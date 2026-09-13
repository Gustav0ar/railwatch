//! The user-session notifier is the only desktop sound owner. Never sends PSU buzzer commands.
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
#[derive(Debug, Subcommand)]
pub enum NotifyCommand {
    /// Run one notification worker for this user session.
    Run {
        #[arg(long)]
        silent: bool,
    },
    /// Play a desktop notification and sound without changing the PSU.
    Test {
        #[arg(long)]
        critical: bool,
    },
    /// Temporarily silence desktop sounds; notifications continue.
    Silence {
        #[arg(long, default_value_t = 15)]
        minutes: u32,
    },
    /// Inspect delivery history and silence expiration.
    Status,
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    silenced_until_ms: i64,
    #[serde(default)]
    deliveries: BTreeMap<String, Delivery>,
}
#[derive(Default, Serialize, Deserialize)]
struct Delivery {
    #[serde(default)]
    delivered: bool,
    attempts: u8,
    last_at_ms: i64,
    error: Option<String>,
}
fn load(path: &Path) -> Result<State> {
    match fs::read(path) {
        Ok(v) => Ok(serde_json::from_slice(&v)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e.into()),
    }
}
fn save(path: &Path, s: &State) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(s)?)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}
fn state_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
        .context("no user state directory")?;
    Ok(base.join("msi-psu"))
}
pub fn run(command: NotifyCommand, runtime: &Path) -> Result<()> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir)?;
    let state_path = dir.join("notifications.json");
    match command {
        NotifyCommand::Status => println!("{}", serde_json::to_string_pretty(&load(&state_path)?)?),
        NotifyCommand::Silence { minutes } => {
            ensure!(minutes <= 1440, "silence must be at most 24 hours");
            let until = chrono::Utc::now().timestamp_millis() + i64::from(minutes) * 60_000;
            fs::write(dir.join("silence"), until.to_string())?;
            println!(
                "Desktop sound silenced until {}",
                chrono::DateTime::from_timestamp_millis(until).unwrap()
            );
        }
        NotifyCommand::Test { critical } => {
            deliver(
                "MSI PSU sound test",
                "Desktop notification and audio test. No PSU settings changed.",
                critical,
                true,
                &dir,
            )?;
            println!("Desktop notification and audio player completed successfully.");
        }
        NotifyCommand::Run { silent } => {
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(dir.join("notifier.lock"))?;
            ensure!(
                unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
                "another notifier already runs for this user"
            );
            let mut state = load(&state_path)?;
            loop {
                let now = chrono::Utc::now().timestamp_millis();
                state.silenced_until_ms = fs::read_to_string(dir.join("silence"))
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                match crate::ipc::call(&runtime.join("monitor.sock"), "Snapshot", json!({})) {
                    Ok(v) => {
                        let mut pending = Vec::new();
                        for incident in v["snapshot"]["active_incidents"]
                            .as_array()
                            .map(Vec::as_slice)
                            .unwrap_or(&[])
                        {
                            let Some(id) = incident["id"].as_str() else {
                                continue;
                            };
                            if !incident["acknowledged_at_ms"].is_null() {
                                continue;
                            }
                            let critical = incident["severity"] == "critical";
                            let d = state.deliveries.entry(id.into()).or_default();
                            let maximum = 3;
                            if (!critical && d.delivered)
                                || d.attempts >= maximum
                                || now - d.last_at_ms < 30_000
                            {
                                continue;
                            }
                            pending.push((
                                id.to_owned(),
                                critical,
                                incident["message"]
                                    .as_str()
                                    .unwrap_or("PSU incident")
                                    .to_owned(),
                            ));
                        }
                        if !pending.is_empty() {
                            let critical = pending.iter().any(|(_, critical, _)| *critical);
                            let message = pending
                                .iter()
                                .take(4)
                                .map(|(_, _, message)| message.as_str())
                                .collect::<Vec<_>>()
                                .join("\n");
                            let sound = !silent && now >= state.silenced_until_ms;
                            let result = deliver(
                                if critical {
                                    "MSI PSU critical alert"
                                } else {
                                    "MSI PSU warning"
                                },
                                &message,
                                critical,
                                sound,
                                &dir,
                            );
                            let error = result.as_ref().err().map(|e| format!("{e:#}"));
                            for (id, _, _) in pending {
                                let d = state.deliveries.get_mut(&id).unwrap();
                                d.attempts += 1;
                                d.last_at_ms = now;
                                d.delivered = result.is_ok();
                                d.error = error.clone();
                            }
                            if let Some(error) = error {
                                eprintln!("desktop alert delivery failed: {error}");
                            }
                            state
                                .deliveries
                                .retain(|_, d| now - d.last_at_ms < 30 * 86_400_000);
                            while state.deliveries.len() > 4096 {
                                let oldest = state
                                    .deliveries
                                    .iter()
                                    .min_by_key(|(_, d)| d.last_at_ms)
                                    .map(|(id, _)| id.clone())
                                    .unwrap();
                                state.deliveries.remove(&oldest);
                            }
                            save(&state_path, &state)?;
                        }
                    }
                    Err(e) => eprintln!("notifier waiting for daemon: {e:#}"),
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
    Ok(())
}
fn deliver(title: &str, message: &str, critical: bool, sound: bool, dir: &Path) -> Result<()> {
    let notification = run_bounded(
        Command::new("notify-send").args([
            "--app-name=MSI PSU",
            "--hint=boolean:suppress-sound:true",
            "--urgency",
            if critical { "critical" } else { "normal" },
            "--",
            title,
            message,
        ]),
        Duration::from_secs(5),
    )
    .context("run notify-send")?;
    ensure!(
        notification.success(),
        "desktop notification delivery failed"
    );
    if sound {
        let path = dir.join(if critical {
            "critical.wav"
        } else {
            "warning.wav"
        });
        fs::write(&path, tone(critical))?;
        let status = run_bounded(Command::new("pw-play").arg(&path), Duration::from_secs(5))
            .context("run PipeWire audio player")?;
        ensure!(status.success(), "PipeWire audio playback failed");
    }
    Ok(())
}
fn run_bounded(command: &mut Command, timeout: Duration) -> Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            child.wait()?;
            anyhow::bail!(
                "desktop delivery process exceeded {} seconds",
                timeout.as_secs_f64()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
pub fn tone(critical: bool) -> Vec<u8> {
    let rate = 24000u32;
    let frames = rate;
    let mut out = Vec::with_capacity(44 + frames as usize * 2);
    out.extend(b"RIFF");
    out.extend((36 + frames * 2).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((frames * 2).to_le_bytes());
    for n in 0..frames {
        let t = n as f64 / rate as f64;
        let phase = t % 0.4;
        let envelope = if phase < 0.25 {
            (phase / 0.02).min(1.) * ((0.25 - phase) / 0.02).min(1.)
        } else {
            0.
        };
        let sample = (envelope
            * 0.16
            * 32767.
            * (std::f64::consts::TAU * if critical { 880. } else { 660. } * t).sin())
            as i16;
        out.extend(sample.to_le_bytes());
    }
    out
}
#[cfg(test)]
mod tests {
    #[test]
    fn hung_delivery_is_terminated_and_reaped() {
        let start = std::time::Instant::now();
        let error = super::run_bounded(
            std::process::Command::new("sleep").arg("30"),
            std::time::Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeded"));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
    #[test]
    fn tones_are_bounded_pcm() {
        for c in [false, true] {
            let t = super::tone(c);
            assert_eq!(t.len(), 48044);
            assert_eq!(&t[..4], b"RIFF");
            let peak = t[44..]
                .chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
                .max()
                .unwrap();
            assert!(peak > 5000 && peak < 5300);
        }
    }
}

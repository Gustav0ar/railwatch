//! Only this transport owns the hidraw descriptor. UI clients never issue USB transactions.
use crate::{model::*, protocol::*};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Discovery {
    pub path: PathBuf,
    pub pid: u16,
    pub model: String,
    pub driver: String,
}

pub fn discover() -> Result<Vec<Discovery>> {
    let mut devices = vec![];
    for e in fs::read_dir("/sys/class/hidraw")? {
        let e = e?;
        let device = e.path().join("device");
        let data = fs::read_to_string(device.join("uevent"))?;
        let Some(id) = data.lines().find_map(|l| l.strip_prefix("HID_ID=")) else {
            continue;
        };
        let parts: Vec<_> = id.split(':').collect();
        if parts.len() != 3 {
            continue;
        }
        let vendor = u16::from_str_radix(parts[1], 16).unwrap_or(0);
        let pid = u16::from_str_radix(parts[2], 16).unwrap_or(0);
        if vendor != VID || family(pid).is_none() {
            continue;
        }
        devices.push(Discovery {
            path: Path::new("/dev").join(e.file_name()),
            pid,
            model: data
                .lines()
                .find_map(|l| l.strip_prefix("HID_NAME="))
                .unwrap_or("MSI PSU")
                .into(),
            driver: fs::read_link(device.join("driver"))?
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
        });
    }
    Ok(devices)
}

pub struct HidDevice {
    file: File,
    pub info: DeviceInfo,
    pub captures: Vec<(u8, Vec<u8>)>,
}
impl HidDevice {
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let devices = discover()?;
        let candidates: Vec<_> = devices
            .into_iter()
            .filter(|d| path.is_none_or(|p| p == d.path))
            .collect();
        ensure!(
            candidates.len() == 1,
            "select exactly one supported PSU with --device; found {}",
            candidates.len()
        );
        let d = &candidates[0];
        ensure!(
            family(d.pid) == Some(Family::Ts),
            "model is detected but its telemetry layout is not yet qualified"
        );
        ensure!(
            d.driver == "hid-generic",
            "PSU has driver {}; use its kernel backend instead of concurrent raw access",
            d.driver
        );
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&d.path)
            .with_context(|| format!("open {}", d.path.display()))?;
        // Advisory ownership prevents two instances of this transport from sharing transactions.
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "PSU is already owned by another process"
        );
        let mut device = Self {
            file,
            info: DeviceInfo {
                id: String::new(),
                model: d.model.clone(),
                pid: d.pid,
                serial: None,
                revision: None,
                path: d.path.display().to_string(),
                backend: "hidraw".into(),
                simulated: false,
                capabilities: vec![
                    "telemetry".into(),
                    "conductor_currents".into(),
                    "hardware_alerts".into(),
                    "device_diagnostics".into(),
                ],
            },
            captures: vec![],
        };
        let reply = device.exchange(0xfa, READ, &[])?;
        let body = response(&reply, 0xfa, READ, 62)?;
        ensure!(body.starts_with(b"MPG "), "unexpected handshake identity");
        let serial = device.read(0x13)?;
        let p = response(&serial, READ, 0x13, 62)?;
        let n = usize::from(p[0]);
        ensure!(n <= 61, "invalid serial length");
        device.info.serial = Some(
            String::from_utf8(p[1..1 + n].to_vec())?
                .trim_end_matches('\0')
                .trim()
                .into(),
        );
        let revision = device.read(0x12)?;
        let p = response(&revision, READ, 0x12, 2)?;
        device.info.revision = Some(String::from_utf8_lossy(p).into());
        let serial = device.info.serial.as_deref().filter(|s| !s.is_empty());
        device.info.id = match serial {
            Some(s) => format!("msi-{:04x}-{s}", d.pid),
            None => format!(
                "msi-{:04x}-{}",
                d.pid,
                d.path.file_name().unwrap().to_string_lossy()
            ),
        };
        Ok(device)
    }

    pub fn exchange(&mut self, opcode: u8, command: u8, payload: &[u8]) -> Result<Vec<u8>> {
        // Drain old unsolicited input before starting a new transaction.
        let mut buffer = [0; 65];
        for _ in 0..32 {
            match self.file.read(&mut buffer) {
                Ok(0) => bail!("PSU disconnected"),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        let tx = request(opcode, command, payload)?;
        self.file.write_all(&tx).context("send HID request")?;
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(400) as i32;
            let mut poll = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut poll, 1, remaining) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e.into());
            }
            if n == 0 {
                break;
            }
            ensure!(
                poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0,
                "PSU disconnected"
            );
            let size = match self.file.read(&mut buffer) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e.into()),
            };
            ensure!(size != 0, "PSU disconnected");
            let r = normalize(&buffer[..size])?;
            if r[0] != opcode || r[1] != command {
                continue;
            }
            ensure!(
                !busy_pattern(r),
                "device returned busy/error sentinel for command {command:#x}"
            );
            let result = buffer[..size].to_vec();
            self.captures.push((command, result.clone()));
            if self.captures.len() > 64 {
                self.captures.remove(0);
            }
            return Ok(result);
        }
        bail!("command {command:#x} timed out; write outcome may be unknown")
    }
    pub fn read(&mut self, command: u8) -> Result<Vec<u8>> {
        self.exchange(READ, command, &[])
    }
    pub fn telemetry(&mut self) -> Result<Telemetry> {
        let mut t = decode_telemetry(&self.read(0xe0)?, Family::Ts)?;
        t.device_id = self.info.id.clone();
        t.captured_at_ms = chrono::Utc::now().timestamp_millis();
        t.monotonic_ms = crate::runtime::monotonic_ms();
        // Each optional group fails independently; absence never means normal.
        match self.read(0xe1) {
            Ok(r) => {
                let p = response(&r, READ, 0xe1, 18)?;
                t.alerts_raw = p.into();
                t.alert_names = p
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| **v != 0)
                    .map(|(i, _)| TS_ALERTS[i].into())
                    .collect();
            }
            Err(e) => t.group_errors.push(format!("hardware_alerts: {e}")),
        }
        match self.read(0xc1) {
            Ok(r) => t.safeguards = decode_safeguards(&r)?,
            Err(e) => t.group_errors.push(format!("safeguards: {e}")),
        }
        match self.read(0x42) {
            Ok(r) => {
                let p = response(&r, READ, 0x42, 4)?;
                if p[..3].iter().all(|v| *v <= 100) && p[3] <= 1 {
                    t.fan.actual_duty_percent = Some(p[0]);
                    t.fan.calculated_duty_percent = Some(p[1]);
                    t.fan.requested_duty_percent = Some(p[2]);
                    t.fan.zero_fan = Some(p[3] == 1)
                } else {
                    t.group_errors.push("invalid fan duty".into())
                }
            }
            Err(e) => t.group_errors.push(format!("fan_duty: {e}")),
        }
        match self.read(0x41) {
            Ok(r) => t.fan.mode_raw = Some(response(&r, READ, 0x41, 3)?[0]),
            Err(e) => t.group_errors.push(format!("fan_setting: {e}")),
        }
        for cmd in [0xd0, 0xd1] {
            match self.read(cmd) {
                Ok(r) => {
                    let n = u32::from_le_bytes(response(&r, READ, cmd, 4)?.try_into().unwrap());
                    if cmd == 0xd0 {
                        t.session_seconds = Some(n)
                    } else {
                        t.lifetime_seconds = Some(n)
                    }
                }
                Err(e) => t.group_errors.push(format!("runtime_{cmd:x}: {e}")),
            }
        }
        Ok(t)
    }
}

pub fn simulated_sample(sequence: u64, fault: bool) -> Telemetry {
    let mut t = decode_telemetry(&request(READ, 0xe0, &[]).unwrap(), Family::Ts).unwrap();
    t.device_id = "demo-ai1600ts".into();
    t.sequence = sequence;
    t.power_uw = 250_000_000 + (sequence % 40) as i64 * 1_000_000;
    t.efficiency_millipercent = 90_000;
    t.temperature_mc = 52_000;
    t.fan.rpm = 700;
    t.fan.mode_raw = Some(0);
    t.fan.actual_duty_percent = Some(35);
    t.fan.calculated_duty_percent = Some(35);
    t.fan.requested_duty_percent = Some(35);
    t.fan.zero_fan = Some(false);
    t.rails = vec![
        Rail {
            label: "12V".into(),
            voltage_mv: 12090,
            current_ma: 18000,
        },
        Rail {
            label: "5V".into(),
            voltage_mv: 5020,
            current_ma: 6000,
        },
        Rail {
            label: "3.3V".into(),
            voltage_mv: 3320,
            current_ma: 1000,
        },
    ];
    t.connector_currents_ma = vec![
        [0; 6],
        if fault {
            [0, 3000, 3000, 3000, 3000, 3000]
        } else {
            [2000, 2100, 1900, 2000, 2000, 2000]
        },
    ];
    t.alerts_raw = vec![0; 18];
    t.safeguards = vec![
        FaultSnapshot {
            status_raw: 0,
            runtime_seconds: 0,
            lifetime_seconds: 0,
            currents_ma: [0; 6]
        };
        2
    ];
    if fault {
        t.alerts_raw[9] = 1;
        t.alert_names.push(TS_ALERTS[9].into());
        t.safeguards[1].status_raw = 2;
    }
    t
}

//! Opt-in endurance check: actual processes, /proc accounting, and a stalled pipe reader.
use serde_json::json;
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let _ = self.0.wait();
    }
}
fn usage(pid: u32) -> (u64, u64) {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let rss = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    (
        rss,
        fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap(),
    )
}
fn descriptors(pid: u32) -> usize {
    fs::read_dir(format!("/proc/{pid}/fd")).unwrap().count()
}
fn cli(dir: &Path, output: Stdio) -> Process {
    Process(
        Command::new(env!("CARGO_BIN_EXE_railwatch"))
            .arg("--runtime-dir")
            .arg(dir.join("run"))
            .arg("plugin-stream")
            .stdout(output)
            .spawn()
            .unwrap(),
    )
}
#[test]
#[ignore = "process/resource validation; run explicitly before release"]
fn persistent_stream_resource_budget_and_slow_consumer() {
    let seconds: u64 = std::env::var("RAILWATCH_SOAK_SECONDS")
        .map(|v| v.parse().expect("invalid soak duration"))
        .unwrap_or(300);
    assert!((40..=86400).contains(&seconds));
    let dir = tempfile::tempdir().unwrap();
    let daemon = Process(
        Command::new(env!("CARGO_BIN_EXE_railwatchd"))
            .args(["--simulate", "--fault-after", "0", "--runtime-dir"])
            .arg(dir.path().join("run"))
            .arg("--database")
            .arg(dir.path().join("history.db"))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.path().join("run/monitor.sock").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(50));
    }
    let stream = cli(dir.path(), Stdio::null());
    thread::sleep(Duration::from_secs(20));
    let baseline = usage(stream.0.id());
    let daemon_baseline = usage(daemon.0.id());
    let stream_fds = descriptors(stream.0.id());
    let daemon_fds = descriptors(daemon.0.id());
    let started = Instant::now();
    let mut samples = vec![];
    for _ in 0..seconds {
        thread::sleep(Duration::from_secs(1));
        samples.push(usage(stream.0.id()).0);
    }
    let final_usage = usage(stream.0.id());
    let daemon_final = usage(daemon.0.id());
    let stream_final_fds = descriptors(stream.0.id());
    let daemon_final_fds = descriptors(daemon.0.id());
    assert!(
        stream_final_fds <= stream_fds + 2,
        "stream descriptors leaked"
    );
    assert!(
        daemon_final_fds <= daemon_fds + 4,
        "daemon descriptors leaked"
    );
    assert!(
        daemon_final.0.saturating_sub(daemon_baseline.0) <= 12 * 1024,
        "daemon RSS growth exceeded 12 MiB"
    );
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let cpu = 100. * (final_usage.1 - baseline.1) as f64 / ticks / started.elapsed().as_secs_f64();
    let daemon_cpu = 100. * (daemon_final.1 - daemon_baseline.1) as f64
        / ticks
        / started.elapsed().as_secs_f64();
    assert!(
        final_usage.0.saturating_sub(baseline.0) <= 2048,
        "stream RSS growth exceeded 2 MiB"
    );
    assert!(
        samples.iter().max().unwrap() < &32768,
        "stream RSS exceeded 32 MiB"
    );
    assert!(cpu < 2., "stream used {cpu:.2}% of one CPU");
    let mut slow = cli(dir.path(), Stdio::piped());
    thread::sleep(Duration::from_secs(20));
    let slow_usage = usage(slow.0.id());
    assert!(
        slow_usage.0 < 32768,
        "slow consumer caused excessive buffering"
    );
    drop(slow.0.stdout.take());
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if slow.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < until,
            "stream did not exit after its reader closed"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let report = json!({"profile":if cfg!(debug_assertions){"debug"}else{"release"},"stream_baseline_rss_kib":baseline.0,"stream_final_rss_kib":final_usage.0,"stream_peak_sampled_rss_kib":samples.iter().max(),"stream_cpu_percent_of_one_core":cpu,"daemon_cpu_percent_of_one_core":daemon_cpu,"daemon_baseline_rss_kib":daemon_baseline.0,"daemon_final_rss_kib":daemon_final.0,"stream_baseline_fds":stream_fds,"stream_final_fds":stream_final_fds,"daemon_baseline_fds":daemon_fds,"daemon_final_fds":daemon_final_fds,"slow_reader_rss_kib":slow_usage.0,"closed_reader_exit":"within 5 seconds","sampled_seconds":seconds,"warmup_seconds":20});
    println!("{report}");
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join(".runtime/performance.json");
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}

use msi_psu::ipc::{INTERFACE, call, read_frame, send};
use serde_json::json;
use std::{
    io::BufReader,
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let _ = self.0.wait();
    }
}
fn start(dir: &Path, extra: &[&str]) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_msi-psud"))
        .args(["--simulate", "--runtime-dir"])
        .arg(dir.join("run"))
        .arg("--database")
        .arg(dir.join("history.db"))
        .args(extra)
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let d = Daemon(child);
    wait_until(|| call(&dir.join("run/monitor.sock"), "Snapshot", json!({})).is_ok());
    d
}
fn wait_until(mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(12);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for daemon");
        thread::sleep(Duration::from_millis(100));
    }
}
#[test]
fn daemon_cli_durability_faults_and_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let d = start(dir.path(), &["--fault-after", "1"]);
    let read = dir.path().join("run/monitor.sock");
    let control = dir.path().join("run/control.sock");
    wait_until(|| {
        call(&read, "Incidents", json!({"active":true,"limit":100}))
            .is_ok_and(|v| v["incidents"].as_array().unwrap().len() == 3)
    });
    let incidents = call(&read, "Incidents", json!({"active":true,"limit":100})).unwrap();
    let id = incidents["incidents"][0]["id"].as_str().unwrap();
    assert!(call(&read, "Acknowledge", json!({"id":id})).is_err());
    call(&control, "Acknowledge", json!({"id":id})).unwrap();
    assert!(!call(&read,"Incidents",json!({"active":true,"limit":100})).unwrap()["incidents"][0]["acknowledged_at_ms"].is_null());
    assert!(
        !call(&read, "Evidence", json!({"id":id})).unwrap()["samples"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let policy = call(&read, "Policy", json!({})).unwrap();
    call(
        &control,
        "SetPolicy",
        json!({"expected_revision":0,"policy":policy["policy"]}),
    )
    .unwrap();
    assert!(
        call(
            &control,
            "SetPolicy",
            json!({"expected_revision":0,"policy":policy["policy"]})
        )
        .is_err()
    );
    assert!(call(&read, "Snapshot", json!({"typo":1})).is_err());
    let before = call(&read, "Snapshot", json!({})).unwrap();
    let from = before["snapshot"]["started_at_ms"].as_i64().unwrap() / 60000 * 60000;
    let q = json!({"device_id":"demo-ai1600ts","from_ms":from,"to_ms":from+120000,"period":"hour","timezone":"UTC"});
    let unpriced = call(&read, "Energy", q.clone()).unwrap();
    assert!(unpriced["buckets"][0]["cost"].is_null());
    call(
        &control,
        "SetTariff",
        json!({"tariff":{"effective_at_ms":from,"microcurrency_per_kwh":1000000,"currency":"BRL"}}),
    )
    .unwrap();
    assert!(call(&read, "Energy", q.clone()).unwrap()["buckets"][0]["cost"].is_string());
    let output = Command::new(env!("CARGO_BIN_EXE_msi-psu"))
        .arg("--runtime-dir")
        .arg(dir.path().join("run"))
        .args(["--json", "watch", "--count", "2"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 2);
    let output = Command::new(env!("CARGO_BIN_EXE_msi-psu"))
        .arg("--runtime-dir")
        .arg(dir.path().join("run"))
        .args(["plugin-stream", "--count", "2"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.iter().filter(|e| e["kind"] == "snapshot").count(), 2);
    assert!(
        events.iter().any(|e| e["kind"] == "history"
            && e["incidents"]["incidents"].as_array().unwrap().len() == 3)
    );
    let mut stream = UnixStream::connect(&read).unwrap();
    send(
        &mut stream,
        &json!({"method":format!("{INTERFACE}.Snapshot"),"parameters":{}}),
    )
    .unwrap();
    let framed = read_frame(&mut BufReader::new(stream), 1_000_000)
        .unwrap()
        .unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&framed)
            .unwrap()
            .get("parameters")
            .is_some()
    );
    drop(d);
    let _restarted = start(dir.path(), &["--fault-after", "0"]);
    wait_until(|| {
        call(&read, "Snapshot", json!({}))
            .is_ok_and(|v| v["snapshot"]["telemetry"]["sequence"].as_u64().is_some())
    });
    let resumed = call(&read, "Incidents", json!({"active":true,"limit":100})).unwrap();
    assert_eq!(resumed["incidents"].as_array().unwrap().len(), 3);
    assert_eq!(resumed["incidents"][0]["id"], id);
    assert!(!resumed["incidents"][0]["acknowledged_at_ms"].is_null());
    assert!(call(&read, "Energy", q).unwrap()["buckets"][0]["cost"].is_string());
}
#[test]
fn disconnection_freezes_values_and_stops_energy() {
    let dir = tempfile::tempdir().unwrap();
    let _d = start(dir.path(), &["--disconnect-after", "2"]);
    let socket = dir.path().join("run/monitor.sock");
    wait_until(|| {
        call(&socket, "Snapshot", json!({}))
            .is_ok_and(|v| v["snapshot"]["last_error"] == "simulated disconnect")
    });
    let v = call(&socket, "Snapshot", json!({})).unwrap();
    assert_eq!(v["stale"], true);
    assert_eq!(v["snapshot"]["telemetry"]["sequence"], 1);
    assert!(v["snapshot"]["telemetry"]["power_uw"].as_i64().unwrap() > 0);
}

#[test]
fn storage_backlog_keeps_monitoring_and_reads_available_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = start(dir.path(), &["--fault-after", "1"]);
    let socket = dir.path().join("run/monitor.sock");
    wait_until(|| {
        call(&socket, "Snapshot", json!({}))
            .is_ok_and(|v| !v["snapshot"]["last_persisted_at_ms"].is_null())
    });
    let writer = rusqlite::Connection::open(dir.path().join("history.db")).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    wait_until(|| {
        call(&socket, "Snapshot", json!({}))
            .is_ok_and(|v| v["snapshot"]["storage_error"].is_string())
    });
    let before = call(&socket, "Snapshot", json!({})).unwrap();
    // WAL readers must work even while another connection prevents all writes.
    call(&socket, "Tariffs", json!({})).unwrap();
    call(&socket, "Incidents", json!({"limit":10})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    let blocked = loop {
        let v = call(&socket, "Snapshot", json!({})).unwrap();
        if v["snapshot"]["dropped_samples"].as_u64().unwrap() > 0 {
            break v;
        }
        assert!(Instant::now() < deadline, "storage queue never filled");
        thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(blocked["stale"], false);
    assert!(
        blocked["snapshot"]["telemetry"]["sequence"]
            .as_u64()
            .unwrap()
            > before["snapshot"]["telemetry"]["sequence"]
                .as_u64()
                .unwrap()
                + 10
    );
    assert_eq!(
        blocked["snapshot"]["active_incidents"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    writer.execute_batch("ROLLBACK").unwrap();
    let at = blocked["snapshot"]["telemetry"]["captured_at_ms"]
        .as_i64()
        .unwrap();
    wait_until(|| {
        call(&socket, "Snapshot", json!({})).is_ok_and(|v| {
            v["snapshot"]["storage_error"].is_null()
                && v["snapshot"]["last_persisted_at_ms"]
                    .as_i64()
                    .is_some_and(|p| p > at)
        })
    });
    let incidents = call(&socket, "Incidents", json!({"active":true,"limit":10})).unwrap();
    assert_eq!(incidents["incidents"].as_array().unwrap().len(), 3);
    for i in incidents["incidents"].as_array().unwrap() {
        assert!(
            !call(&socket, "Evidence", json!({"id":i["id"]})).unwrap()["samples"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn persistent_plugin_stream_reconnects_without_restarting_process() {
    use std::io::BufRead;
    let dir = tempfile::tempdir().unwrap();
    let daemon = start(dir.path(), &[]);
    let child = Command::new(env!("CARGO_BIN_EXE_msi-psu"))
        .arg("--runtime-dir")
        .arg(dir.path().join("run"))
        .arg("plugin-stream")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut client = Daemon(child);
    let stdout = client.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut old_session = String::new();
    while old_session.is_empty() {
        assert!(Instant::now() < deadline);
        let event = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if event["kind"] == "snapshot" {
            old_session = event["data"]["snapshot"]["telemetry"]["session_id"]
                .as_str()
                .unwrap_or("")
                .into();
        }
    }
    drop(daemon);
    let _restarted = start(dir.path(), &[]);
    let mut offline = false;
    let mut recovered = false;
    while !recovered {
        assert!(Instant::now() < deadline, "stream did not recover");
        let event = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        offline |= event["kind"] == "offline";
        if event["kind"] == "snapshot" {
            if let Some(session) = event["data"]["snapshot"]["telemetry"]["session_id"].as_str() {
                recovered = session != old_session;
            }
        }
    }
    assert!(offline, "disconnect must be explicit");
    assert!(
        client.0.try_wait().unwrap().is_none(),
        "the same Rust stream process must survive reconnect"
    );
    drop(client);
    drop(rx);
    reader.join().unwrap();
}

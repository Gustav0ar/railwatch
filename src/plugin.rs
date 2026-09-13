//! One Rust process adapts the Unix socket stream to Noctalia's line-oriented API.
use crate::ipc::{self, INTERFACE};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use chrono_tz::Tz;
use serde_json::{Value, json};
use std::{
    io::{BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

fn publish(output: &mut impl Write, event: Value) -> Result<()> {
    serde_json::to_writer(&mut *output, &event)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

pub fn stream(
    socket: &Path,
    timezone: &str,
    count: Option<usize>,
    output: &mut impl Write,
) -> Result<()> {
    ensure!(count != Some(0), "count must be positive");
    let tz: Tz = timezone.parse()?;
    let mut delivered = 0;
    loop {
        let result = connected(socket, tz, count, &mut delivered, output);
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                // A closed stdout means Noctalia has stopped the child. Do not reconnect forever.
                if e.chain().any(|e| {
                    e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
                }) {
                    return Ok(());
                }
                publish(output, json!({"kind":"offline","message":format!("{e:#}")}))?;
                if count.is_some() {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}
fn connected(
    socket: &Path,
    tz: Tz,
    count: Option<usize>,
    delivered: &mut usize,
    output: &mut impl Write,
) -> Result<()> {
    let mut stream = UnixStream::connect(socket).context("connect to msi-psud")?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    ipc::send(
        &mut stream,
        &json!({"method":format!("{INTERFACE}.Watch"),"parameters":{},"more":true}),
    )?;
    let mut reader = BufReader::new(stream);
    let mut next_history = Instant::now();
    loop {
        let bytes =
            ipc::read_frame(&mut reader, 4 * 1024 * 1024)?.context("msi-psud disconnected")?;
        let reply: Value = serde_json::from_slice(&bytes)?;
        ensure!(reply.get("error").is_none(), "daemon error: {reply}");
        let mut data = reply.get("parameters").context("missing snapshot")?.clone();
        if let Some(captured) = data["snapshot"]["telemetry"]["monotonic_ms"].as_u64() {
            let age = crate::runtime::monotonic_ms().saturating_sub(captured);
            data["age_ms"] = json!(age);
            if age > 3500 {
                data["stale"] = json!(true);
            }
        }
        publish(output, json!({"kind":"snapshot","data":data}))?;
        *delivered += 1;
        if Instant::now() >= next_history {
            next_history = Instant::now() + Duration::from_secs(15);
            if let Some(device) = data["snapshot"]["device"]["id"].as_str() {
                let result = (|| -> Result<Value> {
                    let now = Utc::now().with_timezone(&tz);
                    let (start, _) = crate::calendar::today(now.with_timezone(&Utc), tz)?;
                    let end = now.timestamp_millis() / 60_000 * 60_000 + 60_000;
                    let energy = ipc::call(
                        socket,
                        "Energy",
                        json!({"device_id":device,"from_ms":start,"to_ms":end,"period":"day","timezone":tz.name()}),
                    )?;
                    let incidents = ipc::call(socket, "Incidents", json!({"limit":10}))?;
                    Ok(json!({"kind":"history","energy":energy,"incidents":incidents}))
                })();
                match result {
                    Ok(event) => publish(output, event)?,
                    Err(e) => publish(
                        output,
                        json!({"kind":"history_error","message":format!("{e:#}")}),
                    )?,
                }
            }
        }
        if count.is_some_and(|limit| *delivered >= limit) {
            return Ok(());
        }
    }
}

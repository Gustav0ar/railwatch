//! Bounded Varlink client and wire contract. No hardware or database dependency.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};
pub const INTERFACE: &str = "io.github.railwatch.Monitor1";
pub const MAX_REQUEST: u64 = 64 * 1024;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub method: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub more: bool,
    #[serde(default)]
    pub oneway: bool,
}
pub fn read_frame(reader: &mut impl BufRead, max: u64) -> Result<Option<Vec<u8>>> {
    let mut bytes = vec![];
    let n = reader.take(max + 1).read_until(0, &mut bytes)?;
    if n == 0 {
        return Ok(None);
    }
    ensure!(n as u64 <= max, "message exceeds size limit");
    ensure!(bytes.pop() == Some(0), "unterminated message");
    Ok(Some(bytes))
}
pub fn send(stream: &mut UnixStream, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *stream, value)?;
    stream.write_all(&[0])?;
    Ok(())
}
pub fn call(socket: &Path, method: &str, parameters: Value) -> Result<Value> {
    let mut stream =
        UnixStream::connect(socket).with_context(|| format!("connect {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    send(
        &mut stream,
        &json!({"method":format!("{INTERFACE}.{method}"),"parameters":parameters}),
    )?;
    let bytes = read_frame(&mut BufReader::new(stream), 32 * 1024 * 1024)?
        .context("daemon closed connection")?;
    let reply: Value = serde_json::from_slice(&bytes)?;
    if let Some(e) = reply.get("error") {
        bail!("{}: {}", e, reply["parameters"]["message"])
    }
    reply
        .get("parameters")
        .cloned()
        .context("missing reply parameters")
}

pub const SCHEMA: &str = include_str!("../interfaces/io.github.railwatch.Monitor1.varlink");

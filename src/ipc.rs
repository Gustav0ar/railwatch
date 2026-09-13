//! Versioned Varlink framing over private Unix sockets. Requests never poll USB.
use crate::{
    alerts::Policy,
    history::{History, Tariff},
    runtime::{DeviceCommand, Shared},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc::{self, SyncSender},
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
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Range {
    device_id: String,
    from_ms: i64,
    to_ms: i64,
    #[serde(default = "period_default")]
    period: String,
    #[serde(default = "zone_default")]
    timezone: String,
    #[serde(default = "limit_default")]
    limit: usize,
}
fn period_default() -> String {
    "day".into()
}
fn zone_default() -> String {
    "UTC".into()
}
fn limit_default() -> usize {
    1000
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncidentQuery {
    #[serde(default)]
    active: bool,
    #[serde(default = "limit_default")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rate {
    tariff: Tariff,
    #[serde(default)]
    replace: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyUpdate {
    expected_revision: u64,
    policy: Policy,
}
fn params<T: DeserializeOwned>(v: Value) -> Result<T> {
    Ok(serde_json::from_value(if v.is_null() {
        json!({})
    } else {
        v
    })?)
}
#[derive(Clone)]
pub struct Service {
    pub shared: Shared,
    pub database: PathBuf,
    pub commands: SyncSender<DeviceCommand>,
}
impl Service {
    pub fn dispatch(&self, request: Request, admin: bool) -> Result<Value> {
        ensure!(
            !request.more && !request.oneway,
            "this method requires a regular request; Watch supports streaming"
        );
        let p = request.parameters;
        if request.method == "org.varlink.service.GetInfo" {
            let _: Empty = params(p)?;
            return Ok(
                json!({"vendor":"Railwatch community project","product":"railwatchd","version":env!("CARGO_PKG_VERSION"),"url":"https://github.com/Gustav0ar/railwatch","interfaces":[INTERFACE]}),
            );
        }
        if request.method == "org.varlink.service.GetInterfaceDescription" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Name {
                interface: String,
            }
            let n: Name = params(p)?;
            ensure!(n.interface == INTERFACE, "unknown interface");
            return Ok(
                json!({"description":include_str!("../interfaces/io.github.railwatch.Monitor1.varlink")}),
            );
        }
        let method = request
            .method
            .strip_prefix(&format!("{INTERFACE}."))
            .context("unknown interface")?;
        match method {
            "Snapshot" => {
                let _: Empty = params(p)?;
                let s = self.shared.lock().unwrap().clone();
                let age = s
                    .telemetry
                    .as_ref()
                    .map(|t| crate::runtime::monotonic_ms().saturating_sub(t.monotonic_ms));
                Ok(
                    json!({"snapshot":s,"age_ms":age,"stale":!s.connected||age.is_none_or(|a|a>3500)}),
                )
            }
            "Devices" => {
                let _: Empty = params(p)?;
                let h = History::reader(&self.database)?;
                Ok(json!({"current":self.shared.lock().unwrap().device,"recorded":h.devices()?}))
            }
            "Incidents" => {
                let q: IncidentQuery = params(p)?;
                Ok(
                    json!({"incidents":History::reader(&self.database)?.incidents(q.active,q.limit)?}),
                )
            }
            "Evidence" => {
                let q: Id = params(p)?;
                Ok(json!({"samples":History::reader(&self.database)?.evidence(&q.id)?}))
            }
            "Samples" => {
                let q: Range = params(p)?;
                Ok(
                    json!({"samples":History::reader(&self.database)?.samples(&q.device_id,q.from_ms,q.to_ms,q.limit)?}),
                )
            }
            "Energy" => {
                let q: Range = params(p)?;
                Ok(
                    json!({"from_ms":q.from_ms,"to_ms":q.to_ms,"timezone":q.timezone,"basis":"estimated AC input from DC output / reported efficiency","buckets":History::reader(&self.database)?.summary(&q.device_id,q.from_ms,q.to_ms,&q.period,&q.timezone)?}),
                )
            }
            "Tariffs" => {
                let _: Empty = params(p)?;
                Ok(json!({"tariffs":History::reader(&self.database)?.tariffs()?}))
            }
            "Policy" => {
                let _: Empty = params(p)?;
                let (revision, policy) = History::reader(&self.database)?.policy()?;
                Ok(json!({"revision":revision,"policy":policy}))
            }
            "Acknowledge" => {
                ensure!(admin, "control socket required");
                let q: Id = params(p)?;
                History::open(&self.database)?
                    .acknowledge(&q.id, chrono::Utc::now().timestamp_millis())?;
                let mut shared = self.shared.lock().unwrap();
                for i in &mut shared.active_incidents {
                    if i.id == q.id {
                        i.acknowledged_at_ms = Some(chrono::Utc::now().timestamp_millis());
                    }
                }
                Ok(json!({"acknowledged":q.id}))
            }
            "SetTariff" => {
                ensure!(admin, "control socket required");
                let q: Rate = params(p)?;
                History::open(&self.database)?.set_tariff(&q.tariff, q.replace)?;
                Ok(json!({"tariff":q.tariff}))
            }
            "SetPolicy" => {
                ensure!(admin, "control socket required");
                let q: PolicyUpdate = params(p)?;
                let mut s = self.shared.lock().unwrap();
                let revision =
                    History::open(&self.database)?.set_policy(q.expected_revision, &q.policy)?;
                s.policy = q.policy;
                s.policy_revision = revision;
                Ok(json!({"revision":revision}))
            }
            "Inspect" => {
                let _: Empty = params(p)?;
                ensure!(
                    admin,
                    "control socket required for device diagnostic transactions"
                );
                let (tx, rx) = mpsc::channel();
                self.commands
                    .try_send(DeviceCommand::Inspect(tx))
                    .context("device diagnostic queue busy")?;
                rx.recv_timeout(Duration::from_secs(6))
                    .context("device diagnostic deadline exceeded")?
            }
            _ => bail!("unknown method: {method}"),
        }
    }
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
pub fn serve_client(mut stream: UnixStream, service: &Service, admin: bool) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    for _ in 0..256 {
        let Some(bytes) = read_frame(&mut reader, MAX_REQUEST)? else {
            return Ok(());
        };
        let parsed = serde_json::from_slice::<Request>(&bytes);
        if let Ok(ref r) = parsed {
            if r.method == format!("{INTERFACE}.Watch") && r.more && !r.oneway {
                let _: Empty = params(r.parameters.clone())?;
                while !crate::runtime::STOP.load(std::sync::atomic::Ordering::Relaxed) {
                    let state = service.dispatch(
                        Request {
                            method: format!("{INTERFACE}.Snapshot"),
                            parameters: json!({}),
                            more: false,
                            oneway: false,
                        },
                        false,
                    )?;
                    send(&mut stream, &json!({"parameters":state,"continues":true}))?;
                    std::thread::sleep(Duration::from_secs(1));
                }
                return Ok(());
            }
        }
        let response = match parsed
            .map_err(anyhow::Error::from)
            .and_then(|r| service.dispatch(r, admin))
        {
            Ok(v) => json!({"parameters":v}),
            Err(e) => {
                json!({"error":format!("{INTERFACE}.Failed"),"parameters":{"message":format!("{e:#}")}})
            }
        };
        send(&mut stream, &response)?;
    }
    Ok(())
}

//! One hardware owner and a bounded persistence queue keep clients off the polling path.
use crate::{
    alerts::{AlertEngine, Incident, Policy},
    device::{HidDevice, simulated_sample},
    history::History,
    model::{DeviceInfo, Telemetry},
};
use anyhow::{Result, ensure};
pub use railwatch_core::clock::monotonic_ms;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

pub static STOP: AtomicBool = AtomicBool::new(false);
pub const MAX_GAP_MS: u64 = 3500;
const STORAGE_QUEUE_SIZE: usize = 16;
const MAX_PENDING_EVENTS: usize = 256;
const MAX_PENDING_EVIDENCE: usize = 2048;
const MAX_EVIDENCE_WINDOWS: usize = 32;
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Snapshot {
    pub device: Option<DeviceInfo>,
    pub telemetry: Option<Telemetry>,
    pub connected: bool,
    pub last_error: Option<String>,
    pub storage_error: Option<String>,
    pub dropped_samples: u64,
    pub dropped_incident_updates: u64,
    pub dropped_evidence_samples: u64,
    pub truncated_evidence_windows: u64,
    pub active_incidents: Vec<Incident>,
    pub policy_revision: u64,
    pub policy: Policy,
    pub last_persisted_at_ms: Option<i64>,
    pub started_at_ms: i64,
}
pub type Shared = Arc<Mutex<Snapshot>>;
enum Persist {
    Sample(Box<Telemetry>, Vec<Incident>, Vec<(String, Telemetry)>),
}
pub struct Runtime {
    pub shared: Shared,
    pub commands: SyncSender<DeviceCommand>,
    pub workers: Vec<thread::JoinHandle<()>>,
}
pub enum DeviceCommand {
    Inspect(mpsc::Sender<Result<serde_json::Value>>),
}

pub fn start(
    database: PathBuf,
    path: Option<PathBuf>,
    simulate: bool,
    fault_after: Option<u64>,
    disconnect_after: Option<u64>,
) -> Result<Runtime> {
    ensure!(
        simulate || (fault_after.is_none() && disconnect_after.is_none()),
        "fault injection requires --simulate"
    );
    let history = History::open(&database)?;
    let (revision, policy) = history.policy()?;
    let active = history.incidents(true, 1000)?;
    let shared = Arc::new(Mutex::new(Snapshot {
        started_at_ms: chrono::Utc::now().timestamp_millis(),
        policy_revision: revision,
        policy: policy.clone(),
        active_incidents: active.clone(),
        ..Default::default()
    }));
    let (tx, rx) = mpsc::sync_channel::<Persist>(STORAGE_QUEUE_SIZE);
    let (command_tx, command_rx) = mpsc::sync_channel::<DeviceCommand>(4);
    let storage_shared = shared.clone();
    let storage = thread::spawn(move || {
        let mut h = history;
        while let Ok(Persist::Sample(t, events, evidence)) = rx.recv() {
            loop {
                let result = (|| -> Result<()> {
                    h.record(&t, MAX_GAP_MS)?;
                    for event in &events {
                        h.upsert_incident(event)?;
                    }
                    for (id, sample) in &evidence {
                        h.record_evidence(id, sample)?;
                    }
                    if t.sequence % 60 == 0 {
                        h.prune(t.captured_at_ms)?;
                    }
                    Ok(())
                })();
                {
                    let mut s = storage_shared.lock().unwrap();
                    match &result {
                        Ok(()) => {
                            s.storage_error = None;
                            s.last_persisted_at_ms = Some(t.captured_at_ms);
                        }
                        Err(e) => s.storage_error = Some(format!("{e:#}")),
                    }
                }
                if result.is_ok() {
                    break;
                }
                if STOP.load(Ordering::Relaxed) {
                    eprintln!("history could not be flushed: {}", result.unwrap_err());
                    return;
                }
                pause(Duration::from_secs(1));
            }
        }
    });
    let live_shared = shared.clone();
    let monitor = thread::spawn(move || {
        let mut session = uuid::Uuid::new_v4().to_string();
        let mut sequence = 0;
        let mut device: Option<HidDevice> = None;
        let mut engine = AlertEngine::default();
        let mut current_id = String::new();
        let mut ring: VecDeque<Telemetry> = VecDeque::new();
        let mut capturing: Vec<(String, u64)> = vec![];
        // Retained transitions are retried if storage is temporarily backlogged.
        let mut pending_events = vec![];
        let mut pending_evidence = vec![];
        while !STOP.load(Ordering::Relaxed) {
            let cycle = Instant::now();
            if let Ok(DeviceCommand::Inspect(reply)) = command_rx.try_recv() {
                let result = match &mut device {
                    Some(d) => inspect(d),
                    None => Err(anyhow::anyhow!(
                        "hardware is not connected; simulator has no hardware configuration"
                    )),
                };
                let _ = reply.send(result);
            }
            if !simulate && device.is_none() {
                match HidDevice::open(path.as_deref()) {
                    Ok(d) => {
                        if current_id != d.info.id {
                            current_id = d.info.id.clone();
                            engine = AlertEngine::default();
                            let restored = History::reader(&database)
                                .and_then(|h| h.incidents(true, 1000))
                                .unwrap_or_else(|_| active.clone());
                            for i in restored {
                                if i.device_id == current_id {
                                    engine.active.insert(i.kind.clone(), i);
                                }
                            }
                            capturing.clear();
                        }
                        session = uuid::Uuid::new_v4().to_string();
                        ring.clear();
                        live_shared.lock().unwrap().device = Some(d.info.clone());
                        device = Some(d);
                    }
                    Err(e) => {
                        let mut s = live_shared.lock().unwrap();
                        s.connected = false;
                        s.last_error = Some(format!("{e:#}"));
                        drop(s);
                        pause(Duration::from_secs(2));
                        continue;
                    }
                }
            }
            let mut result = if simulate {
                if disconnect_after.is_some_and(|n| sequence >= n) {
                    Err(anyhow::anyhow!("simulated disconnect"))
                } else {
                    Ok(simulated_sample(
                        sequence,
                        fault_after.is_some_and(|n| sequence >= n),
                    ))
                }
            } else {
                device.as_mut().unwrap().telemetry()
            };
            if simulate && current_id.is_empty() {
                current_id = "demo-ai1600ts".into();
                for i in &active {
                    if i.device_id == current_id {
                        engine.active.insert(i.kind.clone(), i.clone());
                    }
                }
                live_shared.lock().unwrap().device = Some(DeviceInfo {
                    id: current_id.clone(),
                    model: "MPG Ai1600TS (simulated)".into(),
                    pid: 0x808c,
                    serial: None,
                    revision: None,
                    path: "simulator".into(),
                    backend: "simulator".into(),
                    simulated: true,
                    capabilities: vec![
                        "telemetry".into(),
                        "conductor_currents".into(),
                        "hardware_alerts".into(),
                    ],
                });
            }
            match &mut result {
                Ok(t) => {
                    t.session_id = session.clone();
                    t.sequence = sequence;
                    if simulate {
                        t.captured_at_ms = chrono::Utc::now().timestamp_millis();
                        t.monotonic_ms = monotonic_ms();
                    }
                    let policy = {
                        let shared = live_shared.lock().unwrap();
                        for incident in &shared.active_incidents {
                            if let Some(i) = engine.active.get_mut(&incident.kind) {
                                if i.id == incident.id {
                                    i.acknowledged_at_ms = incident.acknowledged_at_ms;
                                }
                            }
                        }
                        shared.policy.clone()
                    };
                    let events = engine.observe(t, &policy, MAX_GAP_MS);
                    for event in &events {
                        if event.recovered_at_ms.is_none() {
                            if capturing.len() == MAX_EVIDENCE_WINDOWS {
                                capturing.remove(0);
                                live_shared.lock().unwrap().truncated_evidence_windows += 1;
                            }
                            for pre in &ring {
                                pending_evidence.push((event.id.clone(), pre.clone()));
                            }
                            capturing.push((event.id.clone(), t.monotonic_ms + 120_000));
                        }
                    }
                    pending_events.extend(events);
                    capturing.retain(|(_, until)| t.monotonic_ms <= *until);
                    for (id, _) in &capturing {
                        pending_evidence.push((id.clone(), t.clone()));
                    }
                    ring.push_back(t.clone());
                    while ring
                        .front()
                        .is_some_and(|p| t.monotonic_ms.saturating_sub(p.monotonic_ms) > 60_000)
                    {
                        ring.pop_front();
                    }
                    bound_pending(&mut pending_events, &mut pending_evidence, &live_shared);
                    let item = Persist::Sample(
                        Box::new(t.clone()),
                        std::mem::take(&mut pending_events),
                        std::mem::take(&mut pending_evidence),
                    );
                    let mut s = live_shared.lock().unwrap();
                    if let Err(e) = tx.try_send(item) {
                        s.dropped_samples += 1;
                        s.storage_error = Some(
                            "storage queue full; live monitoring continues, history has gaps"
                                .into(),
                        );
                        if let mpsc::TrySendError::Full(Persist::Sample(_, events, evidence)) = e {
                            pending_events = events;
                            pending_evidence = evidence;
                        }
                    }
                    s.connected = true;
                    s.last_error = None;
                    s.telemetry = Some(t.clone());
                    s.active_incidents = engine.active.values().cloned().collect();
                }
                Err(e) => {
                    let mut s = live_shared.lock().unwrap();
                    s.connected = false;
                    s.last_error = Some(format!("{e:#}"));
                    if let Some(last) = &s.telemetry {
                        if let Some(event) = engine.monitoring_lost(
                            last,
                            &format!("{e:#}"),
                            chrono::Utc::now().timestamp_millis(),
                        ) {
                            pending_events.push(event.clone());
                            pending_evidence
                                .extend(ring.iter().cloned().map(|t| (event.id.clone(), t)));
                        }
                        let item = Persist::Sample(
                            Box::new(last.clone()),
                            std::mem::take(&mut pending_events),
                            std::mem::take(&mut pending_evidence),
                        );
                        if let Err(mpsc::TrySendError::Full(Persist::Sample(_, events, evidence))) =
                            tx.try_send(item)
                        {
                            pending_events = events;
                            pending_evidence = evidence;
                            s.storage_error =
                                Some("storage queue full; incident persistence pending".into());
                        }
                    }
                    s.active_incidents = engine.active.values().cloned().collect();
                    device = None;
                }
            }
            sequence += 1;
            pause(Duration::from_millis(1000).saturating_sub(cycle.elapsed()));
        }
    });
    Ok(Runtime {
        shared,
        commands: command_tx,
        workers: vec![monitor, storage],
    })
}
// Keep monitoring alive through a prolonged disk failure or alarm storm. Loss is explicit.
fn bound_pending(
    events: &mut Vec<Incident>,
    evidence: &mut Vec<(String, Telemetry)>,
    shared: &Shared,
) {
    let lost_events = events.len().saturating_sub(MAX_PENDING_EVENTS);
    let lost_evidence = evidence.len().saturating_sub(MAX_PENDING_EVIDENCE);
    if lost_events > 0 || lost_evidence > 0 {
        events.drain(..lost_events);
        evidence.drain(..lost_evidence);
        let mut state = shared.lock().unwrap();
        state.dropped_incident_updates += lost_events as u64;
        state.dropped_evidence_samples += lost_evidence as u64;
    }
}
fn pause(d: Duration) {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline && !STOP.load(Ordering::Relaxed) {
        thread::sleep(
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}
fn inspect(d: &mut HidDevice) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(d.diagnostics()?)?)
}

use crate::model::Telemetry;
use anyhow::{Result, ensure};
pub use railwatch_core::measurements::imbalance;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub imbalance_enabled: bool,
    pub minimum_connector_ma: i64,
    pub deviation_percent: u32,
    pub clear_deviation_percent: u32,
    pub minimum_spread_ma: i64,
    pub trigger_ms: u64,
    pub recovery_ms: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            imbalance_enabled: true,
            minimum_connector_ma: 6000,
            deviation_percent: 40,
            clear_deviation_percent: 20,
            minimum_spread_ma: 500,
            trigger_ms: 3000,
            recovery_ms: 5000,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1000..=100_000).contains(&self.minimum_connector_ma),
            "minimum connector load must be 1000..100000 mA"
        );
        ensure!(
            (1..=500).contains(&self.deviation_percent)
                && self.clear_deviation_percent < self.deviation_percent,
            "invalid deviation hysteresis"
        );
        ensure!(
            (1..=20_000).contains(&self.minimum_spread_ma),
            "invalid spread threshold"
        );
        ensure!(
            (500..=60_000).contains(&self.trigger_ms) && (500..=60_000).contains(&self.recovery_ms),
            "durations must be 500..60000 ms"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: String,
    pub device_id: String,
    pub kind: String,
    pub source: String,
    pub severity: String,
    pub message: String,
    pub started_at_ms: i64,
    pub recovered_at_ms: Option<i64>,
    pub acknowledged_at_ms: Option<i64>,
    pub policy: Option<Policy>,
    pub trigger_sample: Telemetry,
}
#[derive(Default)]
pub struct AlertEngine {
    pub active: BTreeMap<String, Incident>,
    pending: BTreeMap<String, u64>,
    recovering: BTreeMap<String, u64>,
    last: Option<(String, u64)>,
}

impl AlertEngine {
    pub fn monitoring_lost(
        &mut self,
        last: &Telemetry,
        message: &str,
        at: i64,
    ) -> Option<Incident> {
        self.pending.clear();
        self.recovering.clear();
        let kind = "monitoring.unavailable";
        if self.active.contains_key(kind) {
            return None;
        }
        let event = Incident {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: last.device_id.clone(),
            kind: kind.into(),
            source: "monitor".into(),
            severity: "warning".into(),
            message: format!("Monitoring interrupted: {message}"),
            started_at_ms: at,
            recovered_at_ms: None,
            acknowledged_at_ms: None,
            policy: None,
            trigger_sample: last.clone(),
        };
        self.active.insert(kind.into(), event.clone());
        Some(event)
    }
    pub fn observe(&mut self, t: &Telemetry, p: &Policy, max_gap_ms: u64) -> Vec<Incident> {
        if self.last.as_ref().is_none_or(|(session, time)| {
            session != &t.session_id || t.monotonic_ms.saturating_sub(*time) > max_gap_ms
        }) {
            self.pending.clear();
            self.recovering.clear();
        }
        self.last = Some((t.session_id.clone(), t.monotonic_ms));
        let mut conditions = BTreeMap::<String, (bool, bool, String, String)>::new();
        if t.alerts_raw.len() == 18
            && !t
                .group_errors
                .iter()
                .any(|e| e.starts_with("hardware_alerts"))
        {
            for (i, name) in crate::protocol::TS_ALERTS.iter().enumerate() {
                conditions.insert(
                    format!("hardware.{name}"),
                    (
                        t.alerts_raw[i] != 0,
                        true,
                        "critical".into(),
                        format!("PSU reports {name}"),
                    ),
                );
            }
        }
        for (c, s) in t.safeguards.iter().enumerate() {
            let desc = match s.status_raw {
                0 => "normal",
                1 => "overcurrent",
                2 => "current imbalance",
                3 => "OCP_18A status",
                _ => "unknown protection status",
            };
            conditions.insert(
                format!("safeguard.connector{}", c + 1),
                (
                    s.status_raw != 0,
                    true,
                    "critical".into(),
                    format!("Connector {}: {desc} ({})", c + 1, s.status_raw),
                ),
            );
        }
        for (c, values) in t.connector_currents_ma.iter().enumerate() {
            let (spread, dev) = imbalance(values);
            let loaded = values.iter().sum::<i64>() >= p.minimum_connector_ma;
            let assertion = p.imbalance_enabled
                && loaded
                && spread >= p.minimum_spread_ma
                && dev >= p.deviation_percent;
            let clear = !p.imbalance_enabled
                || !loaded
                || dev <= p.clear_deviation_percent
                || spread < p.minimum_spread_ma;
            conditions.insert(
                format!("software.connector{}.imbalance", c + 1),
                (
                    assertion,
                    clear,
                    "warning".into(),
                    format!(
                        "Connector {} current deviation {}%, spread {:.3} A",
                        c + 1,
                        dev,
                        spread as f64 / 1000.0
                    ),
                ),
            );
        }
        let mut events = vec![];
        if let Some(mut restored) = self.active.remove("monitoring.unavailable") {
            restored.recovered_at_ms = Some(t.captured_at_ms);
            events.push(restored);
        }
        for (kind, (asserted, clear, severity, message)) in conditions {
            let hardware = !kind.starts_with("software.");
            if asserted {
                self.recovering.remove(&kind);
                let start = *self.pending.entry(kind.clone()).or_insert(t.monotonic_ms);
                if (hardware || t.monotonic_ms.saturating_sub(start) >= p.trigger_ms)
                    && !self.active.contains_key(&kind)
                {
                    let event = Incident {
                        id: uuid::Uuid::new_v4().to_string(),
                        device_id: t.device_id.clone(),
                        kind: kind.clone(),
                        source: if hardware { "hardware" } else { "software" }.into(),
                        severity,
                        message,
                        started_at_ms: t.captured_at_ms,
                        recovered_at_ms: None,
                        acknowledged_at_ms: None,
                        policy: (!hardware).then(|| p.clone()),
                        trigger_sample: t.clone(),
                    };
                    self.active.insert(kind, event.clone());
                    events.push(event);
                }
            } else {
                self.pending.remove(&kind);
                if clear {
                    let at = *self
                        .recovering
                        .entry(kind.clone())
                        .or_insert(t.monotonic_ms);
                    if hardware || t.monotonic_ms.saturating_sub(at) >= p.recovery_ms {
                        if let Some(mut event) = self.active.remove(&kind) {
                            event.recovered_at_ms = Some(t.captured_at_ms);
                            events.push(event);
                        }
                        self.recovering.remove(&kind);
                    }
                } else {
                    self.recovering.remove(&kind);
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(ms: u64, fault: bool) -> Telemetry {
        let mut t = crate::device::simulated_sample(ms, fault);
        t.monotonic_ms = ms;
        t.captured_at_ms = ms as i64;
        t.session_id = "s".into();
        t
    }
    #[test]
    fn hardware_is_immediate_software_waits_and_idle_is_quiet() {
        let mut e = AlertEngine::default();
        let p = Policy::default();
        assert!(e.observe(&sample(0, false), &p, 3000).is_empty());
        let r = e.observe(&sample(500, true), &p, 3000);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|r| r.source == "hardware"));
        e.observe(&sample(2000, true), &p, 3000);
        let r = e.observe(&sample(3500, true), &p, 3000);
        assert!(r.iter().any(|r| r.source == "software"));
        assert!(e.observe(&sample(4000, true), &p, 3000).is_empty());
    }
    #[test]
    fn stale_group_cannot_clear_hardware_fault() {
        let mut e = AlertEngine::default();
        let p = Policy::default();
        e.observe(&sample(0, true), &p, 3000);
        let mut t = sample(500, false);
        t.alerts_raw.clear();
        t.safeguards.clear();
        e.observe(&t, &p, 3000);
        assert_eq!(e.active.len(), 2);
    }
    #[test]
    fn missing_path_and_zero_load() {
        assert_eq!(imbalance(&[0; 6]), (0, 0));
        assert_eq!(imbalance(&[0, 3000, 3000, 3000, 3000, 3000]), (3000, 100));
    }
}

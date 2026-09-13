//! Energy is retained independently of samples and tariffs, so later pricing preserves history.
use crate::model::Telemetry;
use anyhow::{Result, ensure};
use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;
pub use railwatch_core::pricing::{Tariff, parse_price};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
const MAX_SUMMARY_BUCKETS: usize = 10_000;
const MAX_TARIFFS: i64 = 4096;

pub struct History {
    pub connection: Connection,
    previous: Option<Telemetry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergySummary {
    pub period: String,
    pub output_kwh: f64,
    pub estimated_input_kwh: f64,
    pub output_observed_ms: i64,
    pub input_observed_ms: i64,
    pub priced_observed_ms: i64,
    pub cost: Option<String>,
    pub currency: Option<String>,
    pub incomplete_pricing: bool,
}

impl History {
    /// Query connections never initialize schema or acquire a database write lock.
    pub fn reader(path: &Path) -> Result<Self> {
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_millis(200))?;
        Ok(Self {
            connection,
            previous: None,
        })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let c = Connection::open(path)?;
        c.busy_timeout(std::time::Duration::from_millis(200))?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS samples(device TEXT NOT NULL,session TEXT NOT NULL,sequence INTEGER NOT NULL,at_ms INTEGER NOT NULL,json TEXT NOT NULL,PRIMARY KEY(device,session,sequence));
            CREATE INDEX IF NOT EXISTS samples_time ON samples(device,at_ms);
            CREATE TABLE IF NOT EXISTS energy(device TEXT NOT NULL,minute_ms INTEGER NOT NULL,output_uj INTEGER NOT NULL,input_uj INTEGER NOT NULL,observed_ms INTEGER NOT NULL,input_observed_ms INTEGER NOT NULL,PRIMARY KEY(device,minute_ms));
            CREATE TABLE IF NOT EXISTS tariffs(effective_ms INTEGER PRIMARY KEY,rate INTEGER NOT NULL,currency TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS incidents(id TEXT PRIMARY KEY,device TEXT NOT NULL,kind TEXT NOT NULL,start_ms INTEGER NOT NULL,end_ms INTEGER,ack_ms INTEGER,json TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS incidents_device_time ON incidents(device,start_ms);
            CREATE TABLE IF NOT EXISTS evidence(incident TEXT NOT NULL,at_ms INTEGER NOT NULL,json TEXT NOT NULL,PRIMARY KEY(incident,at_ms));")?;
        let version: Option<String> = c
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| {
                r.get(0)
            })
            .optional()?;
        ensure!(
            version.as_deref().is_none_or(|s| s == "1"),
            "unsupported database schema"
        );
        c.execute("INSERT OR IGNORE INTO meta VALUES('schema','1')", [])?;
        Ok(Self {
            connection: c,
            previous: None,
        })
    }

    pub fn record(&mut self, sample: &Telemetry, max_gap_ms: u64) -> Result<()> {
        let tx = self.connection.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO samples VALUES(?1,?2,?3,?4,?5)",
            params![
                sample.device_id,
                sample.session_id,
                i64::try_from(sample.sequence)?,
                sample.captured_at_ms,
                serde_json::to_string(sample)?
            ],
        )?;
        if inserted == 0 {
            return Ok(());
        }
        if let Some(p) = &self.previous {
            let elapsed = sample.monotonic_ms.saturating_sub(p.monotonic_ms);
            let wall = sample.captured_at_ms - p.captured_at_ms;
            if p.session_id == sample.session_id
                && p.device_id == sample.device_id
                && sample.sequence > p.sequence
                && elapsed > 0
                && elapsed <= max_gap_ms
                && wall > 0
                && (wall - elapsed as i64).abs() <= 100
            {
                let mut at = p.captured_at_ms;
                while at < sample.captured_at_ms {
                    let minute = at.div_euclid(60_000) * 60_000;
                    let end = (minute + 60_000).min(sample.captured_at_ms);
                    let integrate = |left: i64, right: i64| -> i64 {
                        let a = (at - p.captured_at_ms) as f64 / wall as f64;
                        let b = (end - p.captured_at_ms) as f64 / wall as f64;
                        let left_at = left as f64 + (right - left) as f64 * a;
                        let right_at = left as f64 + (right - left) as f64 * b;
                        ((left_at + right_at) * 0.5 * (end - at) as f64 / 1000.0).round() as i64
                    };
                    let output = integrate(p.power_uw, sample.power_uw);
                    let input = p
                        .estimated_input_uw()
                        .zip(sample.estimated_input_uw())
                        .map(|(a, b)| integrate(a, b));
                    tx.execute("INSERT INTO energy VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(device,minute_ms) DO UPDATE SET output_uj=output_uj+excluded.output_uj,input_uj=input_uj+excluded.input_uj,observed_ms=observed_ms+excluded.observed_ms,input_observed_ms=input_observed_ms+excluded.input_observed_ms",params![sample.device_id,minute,output,input.unwrap_or(0),end-at,if input.is_some(){end-at}else{0}])?;
                    at = end;
                }
            }
        }
        tx.commit()?;
        self.previous = Some(sample.clone());
        Ok(())
    }

    pub fn tariffs(&self) -> Result<Vec<Tariff>> {
        let tariffs = self
            .connection
            .prepare(
                "SELECT effective_ms,rate,currency FROM tariffs ORDER BY effective_ms LIMIT 4097",
            )?
            .query_map([], |r| {
                Ok(Tariff {
                    effective_at_ms: r.get(0)?,
                    microcurrency_per_kwh: r.get(1)?,
                    currency: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            tariffs.len() <= MAX_TARIFFS as usize,
            "stored tariff history exceeds the supported limit"
        );
        Ok(tariffs)
    }

    pub fn set_tariff(&self, tariff: &Tariff, replace: bool) -> Result<()> {
        ensure!(
            tariff.effective_at_ms.rem_euclid(60_000) == 0,
            "tariff effective time must be at a minute boundary"
        );
        ensure!(
            (0..=1_000_000_000_000).contains(&tariff.microcurrency_per_kwh),
            "invalid electricity price"
        );
        ensure!(
            tariff.currency.len() == 3 && tariff.currency.bytes().all(|b| b.is_ascii_uppercase()),
            "use a three-letter currency such as BRL"
        );
        let tx = self.connection.unchecked_transaction()?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM tariffs", [], |r| r.get(0))?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tariffs WHERE effective_ms=?1)",
            [tariff.effective_at_ms],
            |r| r.get(0),
        )?;
        ensure!(
            count < MAX_TARIFFS || (replace && existing),
            "tariff history is limited to {MAX_TARIFFS} dated rates"
        );
        let current: Option<String> = self
            .connection
            .query_row("SELECT currency FROM tariffs LIMIT 1", [], |r| r.get(0))
            .optional()?;
        ensure!(
            current.as_ref().is_none_or(|c| c == &tariff.currency),
            "cannot combine currencies in one price history"
        );
        if replace {
            tx.execute("INSERT INTO tariffs VALUES(?1,?2,?3) ON CONFLICT(effective_ms) DO UPDATE SET rate=excluded.rate,currency=excluded.currency",params![tariff.effective_at_ms,tariff.microcurrency_per_kwh,tariff.currency])?;
        } else {
            tx.execute(
                "INSERT INTO tariffs VALUES(?1,?2,?3)",
                params![
                    tariff.effective_at_ms,
                    tariff.microcurrency_per_kwh,
                    tariff.currency
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn summary(
        &self,
        device: &str,
        from: i64,
        to: i64,
        period: &str,
        timezone: &str,
    ) -> Result<Vec<EnergySummary>> {
        ensure!(
            from < to && from.rem_euclid(60_000) == 0 && to.rem_euclid(60_000) == 0,
            "range must be increasing and aligned to minutes"
        );
        ensure!(
            ["hour", "day", "week", "month", "year"].contains(&period),
            "invalid period"
        );
        let tz: Tz = timezone.parse()?;
        let tariffs = self.tariffs()?;
        let mut buckets: BTreeMap<String, (EnergySummary, i128)> = BTreeMap::new();
        let mut stmt=self.connection.prepare("SELECT minute_ms,output_uj,input_uj,observed_ms,input_observed_ms FROM energy WHERE device=?1 AND minute_ms>=?2 AND minute_ms<?3 ORDER BY minute_ms")?;
        let rows = stmt.query_map(params![device, from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (at, output, input, observed, input_observed) = row?;
            let t = DateTime::<Utc>::from_timestamp_millis(at)
                .ok_or_else(|| anyhow::anyhow!("invalid stored timestamp"))?
                .with_timezone(&tz);
            let key = match period {
                "hour" => format!("{}T{:02}:00{}", t.date_naive(), t.hour(), t.format("%:z")),
                "day" => t.date_naive().to_string(),
                "week" => format!("{}-W{:02}", t.iso_week().year(), t.iso_week().week()),
                "month" => format!("{}-{:02}", t.year(), t.month()),
                "year" => t.year().to_string(),
                _ => unreachable!(),
            };
            ensure!(
                buckets.len() < MAX_SUMMARY_BUCKETS || buckets.contains_key(&key),
                "energy query exceeds {MAX_SUMMARY_BUCKETS} periods; choose a shorter range or a larger period"
            );
            let (s, cost) = buckets.entry(key.clone()).or_insert((
                EnergySummary {
                    period: key,
                    output_kwh: 0.0,
                    estimated_input_kwh: 0.0,
                    output_observed_ms: 0,
                    input_observed_ms: 0,
                    priced_observed_ms: 0,
                    cost: None,
                    currency: None,
                    incomplete_pricing: false,
                },
                0,
            ));
            s.output_kwh += output as f64 / 3_600_000_000_000.0;
            s.estimated_input_kwh += input as f64 / 3_600_000_000_000.0;
            s.output_observed_ms += observed;
            s.input_observed_ms += input_observed;
            let rate_index = tariffs.partition_point(|r| r.effective_at_ms <= at);
            if let Some(rate) = rate_index.checked_sub(1).map(|i| &tariffs[i]) {
                *cost += i128::from(input) * i128::from(rate.microcurrency_per_kwh);
                s.currency = Some(rate.currency.clone());
                s.priced_observed_ms += input_observed;
            }
        }
        Ok(buckets
            .into_values()
            .map(|(mut s, n)| {
                s.incomplete_pricing = s.priced_observed_ms < s.output_observed_ms;
                if s.priced_observed_ms > 0 {
                    let micro = (n + 1_800_000_000_000) / 3_600_000_000_000;
                    s.cost = Some(format!("{}.{:06}", micro / 1_000_000, micro % 1_000_000));
                }
                s
            })
            .collect())
    }

    pub fn samples(
        &self,
        device: &str,
        from: i64,
        to: i64,
        limit: usize,
    ) -> Result<Vec<Telemetry>> {
        ensure!(
            from < to && (1..=10_000).contains(&limit),
            "invalid sample query"
        );
        let mut stmt=self.connection.prepare("SELECT json FROM samples WHERE device=?1 AND at_ms>=?2 AND at_ms<?3 ORDER BY at_ms LIMIT ?4")?;
        let rows = stmt.query_map(params![device, from, to, i64::try_from(limit)?], |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn prune(&self, now: i64) -> Result<usize> {
        Ok(self.connection.execute("DELETE FROM samples WHERE rowid IN (SELECT rowid FROM samples WHERE at_ms<?1 LIMIT 1000)",[now-48*3_600_000])?)
    }
}

impl History {
    pub fn upsert_incident(&self, i: &crate::alerts::Incident) -> Result<()> {
        self.connection.execute("INSERT INTO incidents VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(id) DO UPDATE SET end_ms=excluded.end_ms,json=excluded.json",params![i.id,i.device_id,i.kind,i.started_at_ms,i.recovered_at_ms,i.acknowledged_at_ms,serde_json::to_string(i)?])?;
        Ok(())
    }
    pub fn incidents(&self, active: bool, limit: usize) -> Result<Vec<crate::alerts::Incident>> {
        ensure!(
            (1..=1000).contains(&limit),
            "incident limit must be 1..1000"
        );
        let mut s=self.connection.prepare("SELECT json,ack_ms FROM incidents WHERE (?1=0 OR end_ms IS NULL) ORDER BY start_ms DESC LIMIT ?2")?;
        let rows = s.query_map(params![active, limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })?;
        rows.map(|r| {
            let (json, ack) = r?;
            let mut i: crate::alerts::Incident = serde_json::from_str(&json)?;
            i.acknowledged_at_ms = ack;
            Ok(i)
        })
        .collect()
    }
    pub fn acknowledge(&self, id: &str, at: i64) -> Result<()> {
        ensure!(
            self.connection.execute(
                "UPDATE incidents SET ack_ms=COALESCE(ack_ms,?1) WHERE id=?2",
                params![at, id]
            )? == 1,
            "incident not found"
        );
        Ok(())
    }
    pub fn evidence(&self, id: &str) -> Result<Vec<Telemetry>> {
        let mut s = self
            .connection
            .prepare("SELECT json FROM evidence WHERE incident=?1 ORDER BY at_ms LIMIT 1000")?;
        let rows = s.query_map([id], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn record_evidence(&self, id: &str, sample: &Telemetry) -> Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO evidence VALUES(?1,?2,?3)",
            params![id, sample.captured_at_ms, serde_json::to_string(sample)?],
        )?;
        Ok(())
    }
    pub fn policy(&self) -> Result<(u64, crate::alerts::Policy)> {
        let p: Option<String> = self
            .connection
            .query_row("SELECT value FROM meta WHERE key='policy'", [], |r| {
                r.get(0)
            })
            .optional()?;
        match p {
            Some(p) => Ok(serde_json::from_str(&p)?),
            None => Ok((0, crate::alerts::Policy::default())),
        }
    }
    pub fn set_policy(&mut self, revision: u64, policy: &crate::alerts::Policy) -> Result<u64> {
        policy.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key='policy'", [], |r| {
                r.get(0)
            })
            .optional()?;
        let current = match old {
            Some(s) => serde_json::from_str::<(u64, crate::alerts::Policy)>(&s)?.0,
            None => 0,
        };
        ensure!(current == revision, "stale policy revision");
        let next = revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("revision overflow"))?;
        tx.execute("INSERT INTO meta VALUES('policy',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(&(next,policy))?])?;
        tx.commit()?;
        Ok(next)
    }
}

impl History {
    pub fn devices(&self) -> Result<Vec<serde_json::Value>> {
        let mut s = self
            .connection
            .prepare("SELECT device,MIN(minute_ms),MAX(minute_ms) FROM energy GROUP BY device")?;
        Ok(s.query_map([],|r|Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"first_recorded_ms":r.get::<_,i64>(1)?,"last_recorded_ms":r.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Family, protocol};
    fn sample(at: i64, seq: u64) -> Telemetry {
        let mut t =
            protocol::decode_telemetry(&protocol::request(0x51, 0xe0, &[]).unwrap(), Family::Ts)
                .unwrap();
        t.device_id = "test".into();
        t.session_id = "session".into();
        t.sequence = seq;
        t.captured_at_ms = at;
        t.monotonic_ms = at as u64;
        t.power_uw = 500_000_000;
        t.efficiency_millipercent = 100_000;
        t
    }
    #[test]
    fn price_precision() {
        assert_eq!(parse_price("1.25").unwrap(), 1_250_000);
        assert_eq!(parse_price("0").unwrap(), 0);
        for s in ["-1", "NaN", "1e3", ".5", "0.0000001"] {
            assert!(parse_price(s).is_err());
        }
    }
    #[test]
    fn energy_survives_pruning_and_late_tariffs() {
        let mut h = History::open(Path::new(":memory:")).unwrap();
        h.record(&sample(0, 1), 3_600_000).unwrap();
        h.record(&sample(3_600_000, 2), 3_600_000).unwrap();
        let s = h.summary("test", 0, 3_600_000, "hour", "UTC").unwrap();
        assert_eq!(s[0].cost, None);
        assert!((s[0].output_kwh - 0.5).abs() < 1e-9);
        h.set_tariff(
            &Tariff {
                effective_at_ms: 0,
                microcurrency_per_kwh: 1_000_000,
                currency: "BRL".into(),
            },
            false,
        )
        .unwrap();
        h.prune(100 * 3_600_000).unwrap();
        for period in ["hour", "day", "week", "month", "year"] {
            let s = h.summary("test", 0, 3_600_000, period, "UTC").unwrap();
            assert_eq!(s[0].cost.as_deref(), Some("0.500000"));
        }
    }
    #[test]
    fn price_change_and_duplicate_sample() {
        let mut h = History::open(Path::new(":memory:")).unwrap();
        h.record(&sample(0, 1), 3_600_000).unwrap();
        h.record(&sample(3_600_000, 2), 3_600_000).unwrap();
        h.record(&sample(3_600_000, 2), 3_600_000).unwrap();
        for (at, rate) in [(0, 1_000_000), (1_800_000, 2_000_000)] {
            h.set_tariff(
                &Tariff {
                    effective_at_ms: at,
                    microcurrency_per_kwh: rate,
                    currency: "BRL".into(),
                },
                false,
            )
            .unwrap();
        }
        assert_eq!(
            h.summary("test", 0, 3_600_000, "day", "UTC").unwrap()[0]
                .cost
                .as_deref(),
            Some("0.750000")
        );
    }
    #[test]
    fn no_integration_over_gap_restart_or_bad_efficiency() {
        let mut h = History::open(Path::new(":memory:")).unwrap();
        h.record(&sample(0, 1), 3000).unwrap();
        h.record(&sample(5000, 2), 3000).unwrap();
        assert!(
            h.summary("test", 0, 60_000, "hour", "UTC")
                .unwrap()
                .is_empty()
        );
        let mut next = sample(6000, 3);
        next.efficiency_millipercent = 0;
        h.record(&next, 3000).unwrap();
        let s = h.summary("test", 0, 60_000, "hour", "UTC").unwrap();
        assert_eq!(s[0].input_observed_ms, 0);
        assert_eq!(s[0].output_observed_ms, 1000);
    }
}

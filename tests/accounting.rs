use railwatch::{
    device::simulated_sample,
    history::{History, Tariff},
};
use std::path::Path;
fn timestamp(v: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(v)
        .unwrap()
        .timestamp_millis()
}
#[test]
fn dst_repeated_hour_is_not_combined() {
    let mut h = History::open(Path::new(":memory:")).unwrap();
    let start = timestamp("2025-11-02T05:00:00Z");
    let mut t = simulated_sample(0, false);
    t.session_id = "dst".into();
    t.power_uw = 500_000_000;
    t.efficiency_millipercent = 100_000;
    t.captured_at_ms = start;
    t.monotonic_ms = 0;
    h.record(&t, 7_200_000).unwrap();
    t.captured_at_ms += 7_200_000;
    t.monotonic_ms += 7_200_000;
    t.sequence = 1;
    h.record(&t, 7_200_000).unwrap();
    let rows = h
        .summary(
            &t.device_id,
            start,
            start + 7_200_000,
            "hour",
            "America/New_York",
        )
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].period.ends_with("-04:00"));
    assert!(rows[1].period.ends_with("-05:00"));
    assert!((rows.iter().map(|r| r.output_kwh).sum::<f64>() - 1.).abs() < 1e-9);
}
#[test]
fn restart_gap_and_partial_prices_remain_visible() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h.db");
    let mut t = simulated_sample(0, false);
    t.session_id = "first".into();
    t.power_uw = 500_000_000;
    t.efficiency_millipercent = 100_000;
    t.captured_at_ms = 0;
    t.monotonic_ms = 0;
    {
        let mut h = History::open(&path).unwrap();
        h.record(&t, 60000).unwrap();
        t.sequence = 1;
        t.monotonic_ms = 60000;
        t.captured_at_ms = 60000;
        h.record(&t, 60000).unwrap();
    }
    let mut h = History::open(&path).unwrap();
    t.session_id = "second".into();
    t.captured_at_ms = 3_600_000;
    t.monotonic_ms = 0;
    h.record(&t, 60000).unwrap();
    t.sequence = 2;
    t.captured_at_ms += 60000;
    t.monotonic_ms += 60000;
    h.record(&t, 60000).unwrap();
    h.set_tariff(
        &Tariff {
            effective_at_ms: 3_600_000,
            microcurrency_per_kwh: 1_000_000,
            currency: "BRL".into(),
        },
        false,
    )
    .unwrap();
    let rows = h.summary(&t.device_id, 0, 7_200_000, "day", "UTC").unwrap();
    assert_eq!(rows[0].output_observed_ms, 120000);
    assert_eq!(rows[0].priced_observed_ms, 60000);
    assert!(rows[0].incomplete_pricing);
    assert_eq!(rows[0].cost.as_deref(), Some("0.008333"));
}

#[test]
fn large_hourly_queries_reject_excess_buckets_without_losing_daily_history() {
    let h = History::open(Path::new(":memory:")).unwrap();
    h.connection.execute_batch("WITH RECURSIVE hours(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM hours WHERE n<10000) INSERT INTO energy SELECT 'history',n*3600000,3600000000,3600000000,60000,60000 FROM hours").unwrap();
    let end = 10_001 * 3_600_000i64;
    let error = h.summary("history", 0, end, "hour", "UTC").unwrap_err();
    assert!(error.to_string().contains("larger period"));
    let days = h.summary("history", 0, end, "day", "UTC").unwrap();
    assert_eq!(
        days.iter().map(|d| d.output_observed_ms).sum::<i64>(),
        10_001 * 60_000
    );
    assert!((days.iter().map(|d| d.output_kwh).sum::<f64>() - 10.001).abs() < 1e-9);
}

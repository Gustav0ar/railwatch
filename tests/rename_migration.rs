use railwatch::{device::simulated_sample, history::History};
use serde_json::json;
use std::path::Path;

#[test]
fn rename_preserves_history_and_is_idempotent() {
    let mut h = History::open(Path::new(":memory:")).unwrap();
    let old = "msi-808c-test";
    let new = "usb-0db0-808c-test";
    let mut t = simulated_sample(0, false);
    t.device_id = old.into();
    t.session_id = "before-rename".into();
    t.captured_at_ms = 0;
    t.monotonic_ms = 0;
    h.record(&t, 3500).unwrap();
    t.sequence += 1;
    t.captured_at_ms = 1000;
    t.monotonic_ms = 1000;
    h.record(&t, 3500).unwrap();
    let c = &h.connection;
    c.execute("INSERT INTO tariffs VALUES(0,1000000,'BRL')", [])
        .unwrap();
    c.execute(
        "INSERT INTO incidents VALUES('fault',?1,'imbalance',0,NULL,500,?2)",
        rusqlite::params![old, json!({"id":"fault","device_id":old}).to_string()],
    )
    .unwrap();
    c.execute(
        "INSERT INTO evidence VALUES('fault',1000,?1)",
        [serde_json::to_string(&t).unwrap()],
    )
    .unwrap();
    let before = h.summary(old, 0, 60000, "hour", "UTC").unwrap();
    for _ in 0..2 {
        c.execute_batch(include_str!("../scripts/migrate-device-ids.sql"))
            .unwrap();
    }
    let after = h.summary(new, 0, 60000, "hour", "UTC").unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(h.samples(new, 0, 2000, 10).unwrap().len(), 2);
    assert!(
        h.samples(new, 0, 2000, 10)
            .unwrap()
            .iter()
            .all(|t| t.device_id == new)
    );
    let incident: (String, String, i64) = c
        .query_row(
            "SELECT device,json_extract(json,'$.device_id'),ack_ms FROM incidents WHERE id='fault'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(incident, (new.into(), new.into(), 500));
    let evidence: String = c
        .query_row(
            "SELECT json_extract(json,'$.device_id') FROM evidence WHERE incident='fault'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(evidence, new);
}

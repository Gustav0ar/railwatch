use msi_psu::{
    model::Family,
    protocol::{READ, decode_safeguards, decode_telemetry, response},
};
use serde::Deserialize;
use std::collections::BTreeMap;
#[derive(Deserialize)]
struct Fixture {
    reports: BTreeMap<String, Vec<u8>>,
}
#[test]
fn captured_linux_ai1600ts_report_decodes() {
    let f: Fixture = serde_json::from_str(include_str!("../fixtures/ai1600ts-linux.json")).unwrap();
    let t = decode_telemetry(&f.reports["e0"], Family::Ts).unwrap();
    assert_eq!(t.power_uw, 180_000_000);
    assert_eq!(t.efficiency_millipercent, 81625);
    assert_eq!(t.temperature_mc, 57000);
    assert_eq!(t.fan.rpm, 0);
    assert_eq!(t.connector_currents_ma[0], [0; 6]);
    assert_eq!(t.connector_currents_ma[1], [375, 375, 375, 375, 375, 437]);
    let snapshots = decode_safeguards(&f.reports["c1"]).unwrap();
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().all(|s| s.status_raw == 0));
    assert_eq!(
        response(&f.reports["c0"], READ, 0xc0, 8).unwrap(),
        [1, 192, 224, 88, 224, 20, 20, 180]
    );
}

#[test]
fn binary_counter_low_byte_is_not_an_error_sentinel() {
    let mut report = [0u8; 64];
    report[0] = READ;
    report[1] = 0xd1;
    report[2..6].copy_from_slice(&11518u32.to_le_bytes());
    assert_eq!(report[2], 0xfe);
    assert_eq!(
        u32::from_le_bytes(
            response(&report, READ, 0xd1, 4)
                .unwrap()
                .try_into()
                .unwrap()
        ),
        11518
    );
}

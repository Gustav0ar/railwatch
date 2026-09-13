//! Convert fixed-size wire readings to the userspace telemetry model.
use anyhow::{Result, ensure};
use railwatch_core::model::{Family, Fan, FaultSnapshot, Rail, Telemetry};
pub use railwatch_protocol::{READ, VID, WRITE, busy_pattern, normalize, request, response};
pub const TS_ALERTS: [&str; 18] = [
    "ocp_12v",
    "ocp_5v",
    "ocp_3v3",
    "connector1_conductor1_ocp",
    "connector1_conductor2_ocp",
    "connector1_conductor3_ocp",
    "connector1_conductor4_ocp",
    "connector1_conductor5_ocp",
    "connector1_conductor6_ocp",
    "connector2_conductor1_ocp",
    "connector2_conductor2_ocp",
    "connector2_conductor3_ocp",
    "connector2_conductor4_ocp",
    "connector2_conductor5_ocp",
    "connector2_conductor6_ocp",
    "over_power",
    "over_temperature",
    "fan_fault",
];
pub fn decode_telemetry(report: &[u8], family: Family) -> Result<Telemetry> {
    ensure!(
        family == Family::Ts,
        "this family requires its own qualified telemetry decoder"
    );
    let raw = railwatch_protocol::decode_ts_telemetry(report)?;
    Ok(Telemetry {
        device_id: String::new(),
        session_id: String::new(),
        sequence: 0,
        captured_at_ms: 0,
        monotonic_ms: 0,
        power_uw: raw.power_uw,
        efficiency_millipercent: raw.efficiency_millipercent,
        temperature_mc: raw.temperature_mc,
        rails: raw
            .rails
            .into_iter()
            .zip(["12V", "5V", "3.3V"])
            .map(|(r, label)| Rail {
                label: label.into(),
                voltage_mv: r.voltage_mv,
                current_ma: r.current_ma,
            })
            .collect(),
        connector_currents_ma: raw.connector_currents_ma.into(),
        fan: Fan {
            rpm: raw.fan_rpm,
            mode_raw: None,
            requested_duty_percent: None,
            actual_duty_percent: None,
            calculated_duty_percent: None,
            zero_fan: None,
        },
        alerts_raw: vec![],
        alert_names: vec![],
        safeguards: vec![],
        session_seconds: None,
        lifetime_seconds: None,
        input_voltage_mv: None,
        group_errors: vec![],
    })
}
pub fn decode_safeguards(report: &[u8]) -> Result<Vec<FaultSnapshot>> {
    Ok(railwatch_protocol::decode_ts_safeguards(report)?
        .into_iter()
        .map(|s| FaultSnapshot {
            status_raw: s.status_raw,
            runtime_seconds: s.runtime_seconds,
            lifetime_seconds: s.lifetime_seconds,
            currents_ma: s.currents_ma,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unqualified_families_never_use_ts_offsets() {
        let report = request(READ, 0xe0, &[]).unwrap();
        assert!(decode_telemetry(&report, Family::P).is_err());
        assert!(decode_telemetry(&report, Family::T).is_err());
        let sample = decode_telemetry(&report, Family::Ts).unwrap();
        assert_eq!(
            sample
                .rails
                .iter()
                .map(|r| r.label.as_str())
                .collect::<Vec<_>>(),
            ["12V", "5V", "3.3V"]
        );
        assert_eq!(sample.connector_currents_ma.len(), 2);
        assert!(sample.fan.mode_raw.is_none());
    }
}

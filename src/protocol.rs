//! The recovered Windows ValueCalc uses an unsigned mantissa. Keep that vendor behavior explicit.
use crate::model::{Family, Fan, FaultSnapshot, Rail, Telemetry};
use anyhow::{Result, bail, ensure};

pub const VID: u16 = 0x0db0;
pub const READ: u8 = 0x51;
pub const WRITE: u8 = 0x50;
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

pub fn request(opcode: u8, command: u8, payload: &[u8]) -> Result<[u8; 65]> {
    ensure!(payload.len() <= 62, "payload exceeds output report");
    let mut result = [0; 65];
    result[1] = opcode;
    result[2] = command;
    result[3..3 + payload.len()].copy_from_slice(payload);
    Ok(result)
}

/// Normalize a captured Windows buffer or a Linux unnumbered report to its 64-byte payload.
pub fn normalize(report: &[u8]) -> Result<&[u8]> {
    match report.len() {
        64 => Ok(report),
        65 if report[0] == 0 => Ok(&report[1..]),
        _ => bail!("invalid HID report length {}", report.len()),
    }
}

/// 0xFE alone is not a sentinel: it is also an ordinary low byte in counters.
/// A reply containing only 0xFE and zero padding remains ambiguous and is not used.
pub fn busy_pattern(report: &[u8]) -> bool {
    report.get(2) == Some(&0xfe) && report[3..].iter().all(|b| *b == 0)
}

pub fn response(report: &[u8], opcode: u8, command: u8, length: usize) -> Result<&[u8]> {
    let r = normalize(report)?;
    ensure!(
        r[0] == opcode && r[1] == command,
        "response does not match outstanding command"
    );
    ensure!(!busy_pattern(r), "device returned busy/error sentinel");
    ensure!(length <= 62, "invalid payload length");
    Ok(&r[2..2 + length])
}

pub fn unsigned_linear11_scaled(bytes: &[u8], scale: i64) -> i64 {
    let raw = u16::from_le_bytes([bytes[0], bytes[1]]);
    let exponent = (raw as i16 >> 11) as i32;
    let mantissa = i64::from(raw & 0x7ff);
    if exponent >= 0 {
        (mantissa * scale) << exponent
    } else {
        (mantissa * scale) >> -exponent
    }
}

pub fn decode_telemetry(report: &[u8], family: Family) -> Result<Telemetry> {
    // Until additional family captures are qualified, do not decode them with TS offsets.
    ensure!(
        family == Family::Ts,
        "this family requires its own qualified telemetry decoder"
    );
    let d = response(report, READ, 0xe0, 44)?;
    let value = |offset, scale| unsigned_linear11_scaled(&d[offset..offset + 2], scale);
    let currents = (0..2)
        .map(|c| std::array::from_fn(|i| value(6 + c * 12 + i * 2, 1000)))
        .collect();
    let rails = [("12V", 38, 0), ("5V", 40, 2), ("3.3V", 42, 4)]
        .into_iter()
        .map(|(label, v, i)| Rail {
            label: label.into(),
            voltage_mv: value(v, 1000),
            current_ma: value(i, 1000),
        })
        .collect();
    let t = Telemetry {
        device_id: String::new(),
        session_id: String::new(),
        sequence: 0,
        captured_at_ms: 0,
        monotonic_ms: 0,
        power_uw: value(30, 1_000_000),
        efficiency_millipercent: value(32, 1000),
        temperature_mc: value(34, 1000),
        rails,
        connector_currents_ma: currents,
        fan: Fan {
            rpm: value(36, 1),
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
    };
    ensure!(
        t.power_uw <= 2_500_000_000
            && t.temperature_mc <= 200_000
            && t.efficiency_millipercent <= 100_000
            && t.rails
                .iter()
                .all(|r| r.voltage_mv <= 20_000 && r.current_ma <= 200_000)
            && t.connector_currents_ma
                .iter()
                .flatten()
                .all(|c| *c <= 50_000),
        "implausible telemetry values"
    );
    Ok(t)
}

pub fn decode_safeguards(report: &[u8]) -> Result<Vec<FaultSnapshot>> {
    let d = response(report, READ, 0xc1, 42)?;
    Ok((0..2)
        .map(|c| {
            let p = &d[c * 21..(c + 1) * 21];
            FaultSnapshot {
                status_raw: p[0],
                runtime_seconds: u32::from_le_bytes(p[1..5].try_into().unwrap()),
                lifetime_seconds: u32::from_le_bytes(p[5..9].try_into().unwrap()),
                currents_ma: std::array::from_fn(|i| {
                    unsigned_linear11_scaled(&p[9 + i * 2..11 + i * 2], 1000)
                }),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_and_lengths() {
        let q = request(READ, 0xe0, &[]).unwrap();
        assert_eq!(&q[..4], &[0, 0x51, 0xe0, 0]);
        assert_eq!(normalize(&q).unwrap(), &q[1..]);
        for n in 0..64 {
            assert!(normalize(&vec![0; n]).is_err());
        }
        assert!(request(WRITE, 0xc0, &[0; 63]).is_err());
    }
    #[test]
    fn vendor_numeric_boundaries() {
        assert_eq!(
            unsigned_linear11_scaled(&0x07ffu16.to_le_bytes(), 1000),
            2_047_000
        );
        assert_eq!(
            unsigned_linear11_scaled(&0xf801u16.to_le_bytes(), 1000),
            500
        );
        assert_eq!(
            unsigned_linear11_scaled(&0x8001u16.to_le_bytes(), 1_000_000),
            15
        );
    }
    #[test]
    fn unknown_safeguard_is_preserved() {
        let mut q = request(READ, 0xc1, &[]).unwrap();
        q[3] = 99;
        assert_eq!(decode_safeguards(&q).unwrap()[0].status_raw, 99);
    }
    #[test]
    fn rejects_wrong_family_busy_and_wrong_echo() {
        let mut q = request(READ, 0xe0, &[]).unwrap();
        assert!(decode_telemetry(&q, Family::P).is_err());
        q[3] = 0xfe;
        assert!(decode_telemetry(&q, Family::Ts).is_err());
        q[3] = 0;
        q[2] = 0xc1;
        assert!(decode_telemetry(&q, Family::Ts).is_err());
    }
}

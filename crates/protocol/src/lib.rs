//! Wire framing and TS decoding only: no allocation, OS calls, serde, or userspace state.
#![no_std]
#![forbid(unsafe_code)]

pub const VID: u16 = 0x0db0;
pub const READ: u8 = 0x51;
pub const WRITE: u8 = 0x50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    PayloadLength,
    ReportLength,
    EchoMismatch,
    Busy,
    ImplausibleTelemetry,
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::PayloadLength => "invalid payload length",
            Self::ReportLength => "invalid HID report length",
            Self::EchoMismatch => "response does not match outstanding command",
            Self::Busy => "device returned busy/error sentinel",
            Self::ImplausibleTelemetry => "implausible telemetry values",
        })
    }
}
impl core::error::Error for Error {}
pub type Result<T> = core::result::Result<T, Error>;

/// Build the unnumbered hidraw write buffer, including its leading report-ID zero.
pub fn request(opcode: u8, command: u8, payload: &[u8]) -> Result<[u8; 65]> {
    if payload.len() > 62 {
        return Err(Error::PayloadLength);
    }
    let mut report = [0; 65];
    report[1] = opcode;
    report[2] = command;
    report[3..3 + payload.len()].copy_from_slice(payload);
    Ok(report)
}
pub fn normalize(report: &[u8]) -> Result<&[u8]> {
    match report.len() {
        64 => Ok(report),
        65 if report[0] == 0 => Ok(&report[1..]),
        _ => Err(Error::ReportLength),
    }
}
/// Binary counters can have low byte FE. Only the otherwise empty pattern is ambiguous.
pub fn busy_pattern(report: &[u8]) -> bool {
    report.get(2) == Some(&0xfe) && report[3..].iter().all(|b| *b == 0)
}
pub fn response(report: &[u8], opcode: u8, command: u8, length: usize) -> Result<&[u8]> {
    let report = normalize(report)?;
    if report[0] != opcode || report[1] != command {
        return Err(Error::EchoMismatch);
    }
    if busy_pattern(report) {
        return Err(Error::Busy);
    }
    if length > 62 {
        return Err(Error::PayloadLength);
    }
    Ok(&report[2..2 + length])
}

#[derive(Clone, Copy)]
pub enum Scale {
    Base = 1,
    Milli = 1000,
    Micro = 1_000_000,
}
/// The vendor format has an unsigned 11-bit mantissa and a signed five-bit exponent.
/// The finite scale set keeps every intermediate within i64 for every u16 input.
pub fn unsigned_linear11(raw: u16, scale: Scale) -> i64 {
    let exponent = (raw as i16 >> 11) as i32;
    let mantissa = i64::from(raw & 0x7ff) * scale as i64;
    if exponent >= 0 {
        mantissa << exponent
    } else {
        mantissa >> -exponent
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rail {
    pub voltage_mv: i64,
    pub current_ma: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TsTelemetry {
    pub power_uw: i64,
    pub efficiency_millipercent: i64,
    pub temperature_mc: i64,
    pub fan_rpm: i64,
    /// Ordered 12V, 5V, 3.3V.
    pub rails: [Rail; 3],
    pub connector_currents_ma: [[i64; 6]; 2],
}
pub fn decode_ts_telemetry(report: &[u8]) -> Result<TsTelemetry> {
    let d = response(report, READ, 0xe0, 44)?;
    let value = |offset: usize, scale| {
        unsigned_linear11(u16::from_le_bytes([d[offset], d[offset + 1]]), scale)
    };
    let t = TsTelemetry {
        power_uw: value(30, Scale::Micro),
        efficiency_millipercent: value(32, Scale::Milli),
        temperature_mc: value(34, Scale::Milli),
        fan_rpm: value(36, Scale::Base),
        rails: [(38, 0), (40, 2), (42, 4)].map(|(v, i)| Rail {
            voltage_mv: value(v, Scale::Milli),
            current_ma: value(i, Scale::Milli),
        }),
        connector_currents_ma: core::array::from_fn(|c| {
            core::array::from_fn(|i| value(6 + c * 12 + i * 2, Scale::Milli))
        }),
    };
    if t.power_uw > 2_500_000_000
        || t.temperature_mc > 200_000
        || t.efficiency_millipercent > 100_000
        || t.rails
            .iter()
            .any(|r| r.voltage_mv > 20_000 || r.current_ma > 200_000)
        || t.connector_currents_ma
            .iter()
            .flatten()
            .any(|c| *c > 50_000)
    {
        return Err(Error::ImplausibleTelemetry);
    }
    Ok(t)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultSnapshot {
    pub status_raw: u8,
    pub runtime_seconds: u32,
    pub lifetime_seconds: u32,
    pub currents_ma: [i64; 6],
}
pub fn decode_ts_safeguards(report: &[u8]) -> Result<[FaultSnapshot; 2]> {
    let d = response(report, READ, 0xc1, 42)?;
    Ok(core::array::from_fn(|c| {
        let p = &d[c * 21..(c + 1) * 21];
        FaultSnapshot {
            status_raw: p[0],
            runtime_seconds: u32::from_le_bytes(p[1..5].try_into().unwrap()),
            lifetime_seconds: u32::from_le_bytes(p[5..9].try_into().unwrap()),
            currents_ma: core::array::from_fn(|i| {
                unsigned_linear11(
                    u16::from_le_bytes([p[9 + i * 2], p[10 + i * 2]]),
                    Scale::Milli,
                )
            }),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_rejects_invalid_reports() {
        let q = request(READ, 0xe0, &[]).unwrap();
        assert_eq!(&q[..4], &[0, 0x51, 0xe0, 0]);
        assert_eq!(normalize(&q).unwrap(), &q[1..]);
        let buffer = [0; 66];
        for n in 0..64 {
            assert_eq!(normalize(&buffer[..n]), Err(Error::ReportLength));
        }
        assert!(normalize(&buffer).is_err());
        assert_eq!(request(WRITE, 0xc0, &[0; 63]), Err(Error::PayloadLength));
    }
    #[test]
    fn numeric_boundaries_and_all_encodings_fit() {
        assert_eq!(unsigned_linear11(0x07ff, Scale::Milli), 2_047_000);
        assert_eq!(unsigned_linear11(0xf801, Scale::Milli), 500);
        assert_eq!(unsigned_linear11(0x8001, Scale::Micro), 15);
        for raw in 0..=u16::MAX {
            assert!(unsigned_linear11(raw, Scale::Micro) >= 0);
        }
    }
    #[test]
    fn fault_status_and_echo_are_preserved() {
        let mut q = request(READ, 0xc1, &[]).unwrap();
        q[3] = 99;
        assert_eq!(decode_ts_safeguards(&q).unwrap()[0].status_raw, 99);
        assert_eq!(decode_ts_telemetry(&q), Err(Error::EchoMismatch));
        q[2] = 0xe0;
        q[3] = 0xfe;
        assert_eq!(decode_ts_telemetry(&q), Err(Error::Busy));
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Ts,
    T,
    P,
}

pub fn family(pid: u16) -> Option<Family> {
    match pid {
        0x808c | 0xaa6f => Some(Family::Ts),
        0xc9eb => Some(Family::T),
        0x56d4 | 0xe749 => Some(Family::P),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rail {
    pub label: String,
    pub voltage_mv: i64,
    pub current_ma: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fan {
    pub rpm: i64,
    pub mode_raw: Option<u8>,
    pub requested_duty_percent: Option<u8>,
    pub actual_duty_percent: Option<u8>,
    pub calculated_duty_percent: Option<u8>,
    pub zero_fan: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FaultSnapshot {
    pub status_raw: u8,
    pub runtime_seconds: u32,
    pub lifetime_seconds: u32,
    pub currents_ma: [i64; 6],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Telemetry {
    pub device_id: String,
    pub session_id: String,
    pub sequence: u64,
    pub captured_at_ms: i64,
    pub monotonic_ms: u64,
    pub power_uw: i64,
    pub efficiency_millipercent: i64,
    pub temperature_mc: i64,
    pub rails: Vec<Rail>,
    pub connector_currents_ma: Vec<[i64; 6]>,
    pub fan: Fan,
    pub alerts_raw: Vec<u8>,
    pub alert_names: Vec<String>,
    pub safeguards: Vec<FaultSnapshot>,
    pub session_seconds: Option<u32>,
    pub lifetime_seconds: Option<u32>,
    pub input_voltage_mv: Option<i64>,
    pub group_errors: Vec<String>,
}

impl Telemetry {
    pub fn estimated_input_uw(&self) -> Option<i64> {
        (self.efficiency_millipercent > 0 && self.efficiency_millipercent <= 100_000)
            .then(|| self.power_uw * 100_000 / self.efficiency_millipercent)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub model: String,
    pub pid: u16,
    pub serial: Option<String>,
    pub revision: Option<String>,
    pub path: String,
    pub backend: String,
    pub simulated: bool,
    pub capabilities: Vec<String>,
}

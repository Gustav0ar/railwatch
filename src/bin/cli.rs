use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use railwatch::{
    alerts::Policy,
    device::{HidDevice, discover},
    history::{Tariff, parse_price},
    ipc::{self, call},
};
use serde_json::{Value, json};
use std::{
    io::{BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Parser)]
#[command(
    version,
    about = "Railwatch monitoring, incidents, and energy costs",
    long_about = "All PSU access belongs to railwatchd. Prices are per kWh. Costs use estimated wall input; incomplete coverage remains visible."
)]
struct Cli {
    #[arg(
        long,
        global = true,
        env = "RAILWATCH_RUNTIME_DIR",
        default_value = "/run/railwatch"
    )]
    runtime_dir: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, ValueEnum)]
enum Period {
    Hour,
    Day,
    Week,
    Month,
    Year,
}
impl Period {
    fn name(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}
#[derive(Subcommand)]
enum Command {
    /// Persistent JSON event stream for Noctalia, including energy and incidents.
    PluginStream {
        #[arg(long)]
        count: Option<usize>,
        #[arg(long)]
        timezone: Option<String>,
    },
    /// Desktop notification worker, sound test, silence, and delivery history.
    Notify {
        #[command(subcommand)]
        command: railwatch::notify::NotifyCommand,
    },
    /// Discover supported USB identities without opening a device.
    Discover,
    /// Direct read-only hardware qualification. The daemon must be stopped.
    Probe {
        #[arg(long)]
        device: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// All telemetry, capabilities, active incidents, freshness, and daemon health.
    Status,
    /// Current and previously recorded device identities.
    Devices,
    /// Stream cached snapshots, one per second; JSON mode is newline-delimited.
    Watch {
        #[arg(long)]
        count: Option<usize>,
    },
    /// Query persistent incident history.
    Incidents {
        #[arg(long)]
        active: bool,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Read retained before/after telemetry for an incident.
    Evidence { id: String },
    /// Acknowledge presentation without clearing a hardware condition.
    Acknowledge { id: String },
    /// Query raw telemetry. Times are RFC3339 with an offset, or Unix milliseconds.
    Samples {
        #[arg(long)]
        device_id: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value_t = 1000)]
        limit: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Energy and estimated cost by calendar period; times must align to minutes.
    Energy {
        #[arg(long)]
        device_id: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, value_enum, default_value = "day")]
        period: Period,
        #[arg(long, default_value = "America/Sao_Paulo")]
        timezone: String,
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// View or set dated price history. Existing energy can be priced retrospectively.
    Tariff {
        #[command(subcommand)]
        command: TariffCommand,
    },
    /// Inspect or atomically replace software alert policy.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Read current raw cooling and safeguard settings with qualification status.
    Inspect,
    /// Print the versioned API definition.
    Interface,
}
#[derive(Subcommand)]
enum TariffCommand {
    List,
    Set {
        #[arg(long)]
        price: String,
        #[arg(long)]
        currency: String,
        #[arg(long)]
        effective: String,
        #[arg(long)]
        replace: bool,
    },
}
#[derive(Subcommand)]
enum PolicyCommand {
    Get,
    Set {
        file: PathBuf,
        #[arg(long)]
        expected_revision: u64,
    },
}
pub fn timestamp(s: &str) -> Result<i64> {
    if let Ok(ms) = s.parse::<i64>() {
        Ok(ms)
    } else {
        Ok(chrono::DateTime::parse_from_rfc3339(s)
            .context("use RFC3339 with a timezone offset, or Unix milliseconds")?
            .timestamp_millis())
    }
}
fn print(v: &Value, json_mode: bool) -> Result<()> {
    if !json_mode && v.get("snapshot").is_some() {
        let s = &v["snapshot"];
        let t = &s["telemetry"];
        println!(
            "{}  {}",
            s["device"]["model"].as_str().unwrap_or("No PSU"),
            if v["stale"] == true {
                "MONITORING INTERRUPTED"
            } else {
                "Monitoring"
            }
        );
        if !t.is_null() {
            println!(
                "Output {:.1} W | efficiency {:.2}% | temperature {:.1} °C | fan {} RPM",
                t["power_uw"].as_f64().unwrap_or(0.) / 1e6,
                t["efficiency_millipercent"].as_f64().unwrap_or(0.) / 1000.,
                t["temperature_mc"].as_f64().unwrap_or(0.) / 1000.,
                t["fan"]["rpm"]
            );
            if let Some(cs) = t["connector_currents_ma"].as_array() {
                for (i, c) in cs.iter().enumerate() {
                    let values = c
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| format!("{:.3}", v.as_f64().unwrap() / 1000.))
                        .collect::<Vec<_>>();
                    println!("Connector {}: {} A", i + 1, values.join("  "));
                }
            }
        }
        if let Some(errors) = t["group_errors"].as_array() {
            for error in errors {
                println!(
                    "Reading unavailable: {}",
                    error.as_str().unwrap_or("unknown")
                );
            }
        }
        for key in ["last_error", "storage_error"] {
            if let Some(e) = s[key].as_str() {
                println!("{key}: {e}");
            }
        }
        println!(
            "{} active incidents | age {} ms | {} dropped history samples",
            s["active_incidents"].as_array().map_or(0, Vec::len),
            v["age_ms"],
            s["dropped_samples"]
        );
    } else {
        println!("{}", serde_json::to_string_pretty(v)?);
    }
    Ok(())
}
fn main() -> Result<()> {
    let c = Cli::parse();
    let monitor = c.runtime_dir.join("monitor.sock");
    let control = c.runtime_dir.join("control.sock");
    let result = match c.command {
        Command::PluginStream { count, timezone } => {
            let timezone = railwatch::calendar::timezone(timezone.as_deref())?;
            railwatch::plugin::stream(
                &monitor,
                timezone.name(),
                count,
                &mut std::io::stdout().lock(),
            )?;
            return Ok(());
        }
        Command::Notify { command } => {
            railwatch::notify::run(command, &c.runtime_dir)?;
            return Ok(());
        }
        Command::Discover => serde_json::to_value(discover()?)?,
        Command::Probe { device, output } => {
            let mut d = HidDevice::open(device.as_deref())?;
            let mut t = d.telemetry()?;
            t.session_id = uuid::Uuid::new_v4().to_string();
            t.captured_at_ms = chrono::Utc::now().timestamp_millis();
            let ocp = d.read(0xc0)?;
            let v = json!({"device":d.info,"telemetry":t,"safeguard_configuration_report":ocp,"reports":d.captures});
            if let Some(p) = output {
                std::fs::write(p, serde_json::to_vec_pretty(&v)?)?;
            }
            v
        }
        Command::Status => call(&monitor, "Snapshot", json!({}))?,
        Command::Devices => call(&monitor, "Devices", json!({}))?,
        Command::Watch { count } => {
            ensure!(count != Some(0), "count must be positive");
            watch(&monitor, count, c.json)?;
            return Ok(());
        }
        Command::Incidents { active, limit } => call(
            &monitor,
            "Incidents",
            json!({"active":active,"limit":limit}),
        )?,
        Command::Evidence { id } => call(&monitor, "Evidence", json!({"id":id}))?,
        Command::Acknowledge { id } => call(&control, "Acknowledge", json!({"id":id}))?,
        Command::Samples {
            device_id,
            from,
            to,
            limit,
            output,
        } => {
            let v = call(
                &monitor,
                "Samples",
                json!({"device_id":device_id,"from_ms":timestamp(&from)?,"to_ms":timestamp(&to)?,"limit":limit}),
            )?;
            if let Some(p) = output {
                std::fs::write(p, serde_json::to_vec_pretty(&v)?)?;
            }
            v
        }
        Command::Energy {
            device_id,
            from,
            to,
            period,
            timezone,
            csv,
        } => {
            let v = call(
                &monitor,
                "Energy",
                json!({"device_id":device_id,"from_ms":timestamp(&from)?,"to_ms":timestamp(&to)?,"period":period.name(),"timezone":timezone}),
            )?;
            if let Some(p) = csv {
                write_csv(&p, &v)?;
            }
            v
        }
        Command::Tariff { command } => match command {
            TariffCommand::List => call(&monitor, "Tariffs", json!({}))?,
            TariffCommand::Set {
                price,
                currency,
                effective,
                replace,
            } => call(
                &control,
                "SetTariff",
                json!({"tariff":Tariff{effective_at_ms:timestamp(&effective)?,microcurrency_per_kwh:parse_price(&price)?,currency},"replace":replace}),
            )?,
        },
        Command::Policy { command } => match command {
            PolicyCommand::Get => call(&monitor, "Policy", json!({}))?,
            PolicyCommand::Set {
                file,
                expected_revision,
            } => {
                let p: Policy = serde_json::from_slice(&std::fs::read(file)?)?;
                p.validate()?;
                call(
                    &control,
                    "SetPolicy",
                    json!({"expected_revision":expected_revision,"policy":p}),
                )?
            }
        },
        Command::Inspect => call(&control, "Inspect", json!({}))?,
        Command::Interface => {
            println!(
                "{}",
                include_str!("../../interfaces/io.github.railwatch.Monitor1.varlink")
            );
            return Ok(());
        }
    };
    print(&result, c.json)
}
fn watch(socket: &Path, count: Option<usize>, json_mode: bool) -> Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    ipc::send(
        &mut stream,
        &json!({"method":format!("{}.Watch",ipc::INTERFACE),"more":true,"parameters":{}}),
    )?;
    let mut r = BufReader::new(stream);
    let mut n = 0;
    while let Some(bytes) = ipc::read_frame(&mut r, 4 * 1024 * 1024)? {
        let v: Value = serde_json::from_slice(&bytes)?;
        ensure!(v.get("error").is_none(), "daemon error: {v}");
        if json_mode {
            println!("{}", v["parameters"]);
        } else {
            print(&v["parameters"], false)?;
        }
        n += 1;
        if count.is_some_and(|c| n >= c) {
            break;
        }
    }
    Ok(())
}
fn write_csv(path: &Path, value: &Value) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    let keys = [
        "period",
        "output_kwh",
        "estimated_input_kwh",
        "output_observed_ms",
        "input_observed_ms",
        "priced_observed_ms",
        "cost",
        "currency",
        "incomplete_pricing",
    ];
    writeln!(file, "{}", keys.join(","))?;
    for row in value["buckets"]
        .as_array()
        .context("invalid energy response")?
    {
        let fields = keys
            .iter()
            .map(|key| {
                let v = &row[*key];
                let s = if v.is_null() {
                    String::new()
                } else {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                };
                format!("\"{}\"", s.replace('"', "\"\""))
            })
            .collect::<Vec<_>>();
        writeln!(file, "{}", fields.join(","))?;
    }
    Ok(())
}

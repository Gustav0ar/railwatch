use anyhow::{Context, Result};
use clap::Parser;
use railwatch_client::call;
use railwatch_core::{
    measurements::imbalance,
    model::Telemetry,
    pricing::{Tariff, parse_price},
};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
slint::include_modules!();
#[derive(Parser)]
#[command(version)]
struct Args {
    #[arg(long, env = "RAILWATCH_RUNTIME_DIR", default_value = "/run/railwatch")]
    runtime_dir: PathBuf,
    #[arg(long)]
    incident: Option<String>,
    /// Calendar time zone; defaults to the system zone.
    #[arg(long)]
    timezone: Option<String>,
    /// Render the real UI with the daemon's current snapshot and exit.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(i32).range(0..=4))]
    page: i32,
}
enum Action {
    Energy(String, String, String, i32),
    Tariff(String, String, String),
    Inspect,
    Acknowledge(String),
    Evidence(String),
    Export,
    Incidents,
}

type UiUpdate = Box<dyn FnOnce(&App) + Send>;
struct UiUpdates(mpsc::SyncSender<UiUpdate>);
impl UiUpdates {
    // Backpressure also applies when the UI thread stalls. Closing the window drops the receiver.
    fn deliver(
        &self,
        update: impl FnOnce(&App) + Send + 'static,
    ) -> Result<(), mpsc::SendError<UiUpdate>> {
        self.0.send(Box::new(update))
    }
}

fn queue_action(tx: &mpsc::SyncSender<Action>, ui: &slint::Weak<App>, action: Action) {
    if let Err(error) = tx.try_send(action)
        && let Some(ui) = ui.upgrade()
    {
        ui.set_notice(
            match error {
                mpsc::TrySendError::Full(_) => {
                    "Still processing earlier requests. Try again shortly."
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "The connection worker stopped. Reopen the application."
                }
            }
            .into(),
        );
    }
}
fn duty(v: Option<u8>) -> String {
    v.map_or("Unavailable".into(), |v| format!("{v}%"))
}
fn strings(values: Vec<String>) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(
        values.into_iter().map(Into::into).collect::<Vec<_>>(),
    ))
}
fn date(s: &str) -> Result<i64> {
    Ok(chrono::DateTime::parse_from_rfc3339(s)?.timestamp_millis())
}
fn update_ui(ui: &App, v: &Value, graph: &[f32]) {
    let s = &v["snapshot"];
    let age = s["telemetry"]["monotonic_ms"]
        .as_u64()
        .map(|at| railwatch_core::clock::monotonic_ms().saturating_sub(at));
    let stale = v["stale"].as_bool().unwrap_or(true) || age.is_none_or(|age| age > 3500);
    ui.set_device_name(s["device"]["model"].as_str().unwrap_or("Railwatch").into());
    ui.set_stale(stale);
    ui.set_status(if stale {
        "Monitoring interrupted. Readings are frozen.".into()
    } else {
        format!("Monitoring · updated {} ms ago", age.unwrap_or(0)).into()
    });
    let active = s["active_incidents"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut problems = active
        .iter()
        .map(|i| i["message"].as_str().unwrap_or("PSU alert").to_string())
        .collect::<Vec<_>>();
    if let Some(e) = s["storage_error"].as_str() {
        problems.push(format!("History recording failed: {e}"));
    }
    if let Some(e) = s["last_error"].as_str() {
        problems.push(e.into());
    }
    for (key, label) in [
        ("dropped_incident_updates", "incident updates lost"),
        ("dropped_evidence_samples", "evidence samples lost"),
        ("truncated_evidence_windows", "evidence windows shortened"),
    ] {
        if let Some(count) = s[key].as_u64().filter(|count| *count > 0) {
            problems.push(format!("History incomplete: {count} {label}"));
        }
    }
    ui.set_problem(problems.join(" · ").into());
    if let Ok(t) = serde_json::from_value::<Telemetry>(s["telemetry"].clone()) {
        if !t.group_errors.is_empty() {
            ui.set_problem(
                format!(
                    "{} · Some readings unavailable: {}",
                    problems.join(" · "),
                    t.group_errors.join("; ")
                )
                .into(),
            );
        }
        ui.set_watts(format!("{:.1} W", t.power_uw as f64 / 1e6).into());
        ui.set_efficiency(format!("{:.2}%", t.efficiency_millipercent as f64 / 1000.).into());
        ui.set_temperature(format!("{:.1} °C", t.temperature_mc as f64 / 1000.).into());
        ui.set_fan(format!("{} RPM", t.fan.rpm).into());
        for (n, c) in t.connector_currents_ma.iter().enumerate() {
            let model = ModelRc::new(VecModel::from(
                c.iter()
                    .map(|v| Conductor {
                        amps: format!("{:.3}", *v as f64 / 1000.).into(),
                        level: (*v as f32 / 8000.).clamp(0., 1.),
                    })
                    .collect::<Vec<_>>(),
            ));
            let total = c.iter().sum::<i64>();
            let (spread, dev) = imbalance(c);
            let summary = if total == 0 {
                "No load observed".into()
            } else {
                format!(
                    "Total {:.3} A · deviation {}% · spread {:.3} A",
                    total as f64 / 1000.,
                    dev,
                    spread as f64 / 1000.
                )
            };
            if n == 0 {
                ui.set_connector_one(model);
                ui.set_connector_one_summary(summary.into());
            } else {
                ui.set_connector_two(model);
                ui.set_connector_two_summary(summary.into());
            }
        }
        ui.set_rails(strings(
            t.rails
                .iter()
                .map(|r| {
                    format!(
                        "{}     {:.3} V     {:.3} A",
                        r.label,
                        r.voltage_mv as f64 / 1000.,
                        r.current_ma as f64 / 1000.
                    )
                })
                .collect(),
        ));
        ui.set_cooling_rows(strings(vec![
            format!("Fan speed: {} RPM", t.fan.rpm),
            format!(
                "Raw mode: {}",
                t.fan
                    .mode_raw
                    .map_or("Unavailable".into(), |v| v.to_string())
            ),
            format!(
                "Requested duty: {}   Calculated: {}   Actual: {}",
                duty(t.fan.requested_duty_percent),
                duty(t.fan.calculated_duty_percent),
                duty(t.fan.actual_duty_percent)
            ),
            format!(
                "Zero fan enabled: {}",
                t.fan
                    .zero_fan
                    .map_or("Unavailable".into(), |v| v.to_string())
            ),
        ]));
    }
    ui.set_graph(ModelRc::new(VecModel::from(graph.to_vec())));
    ui.set_graph_max(graph.iter().copied().fold(100., f32::max) * 1.1);
    ui.set_device_rows(strings(vec![
        format!(
            "Identity: {}",
            s["device"]["id"].as_str().unwrap_or("Unavailable")
        ),
        format!(
            "Backend: {}",
            s["device"]["backend"].as_str().unwrap_or("Unavailable")
        ),
        format!(
            "Firmware revision: {}",
            s["device"]["revision"].as_str().unwrap_or("Unavailable")
        ),
        format!("Capabilities: {}", s["device"]["capabilities"]),
        format!("Policy revision: {}", s["policy_revision"]),
        format!("History samples dropped: {}", s["dropped_samples"]),
        format!("Last history commit: {}", s["last_persisted_at_ms"]),
    ]));
}
fn energy_rows(v: &Value) -> Vec<String> {
    let rows = v["buckets"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if rows.is_empty() {
        return vec!["No recorded energy in this range.".into()];
    }
    rows.iter()
        .map(|r| {
            format!(
                "{}     {:.6} kWh wall     {:.6} kWh output     {} {}     {:.1} min observed{}",
                r["period"].as_str().unwrap_or(""),
                r["estimated_input_kwh"].as_f64().unwrap_or(0.),
                r["output_kwh"].as_f64().unwrap_or(0.),
                r["currency"].as_str().unwrap_or(""),
                r["cost"].as_str().unwrap_or("Unpriced"),
                r["output_observed_ms"].as_f64().unwrap_or(0.) / 60000.,
                if r["incomplete_pricing"] == true {
                    " · partial pricing"
                } else {
                    ""
                }
            )
        })
        .collect()
}
fn incident_rows(v: &Value) -> ModelRc<IncidentRow> {
    ModelRc::new(VecModel::from(
        v["incidents"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|i| IncidentRow {
                id: i["id"].as_str().unwrap_or("").into(),
                title: i["message"].as_str().unwrap_or("Incident").into(),
                detail: format!(
                    "{} · {} · {}",
                    i["source"].as_str().unwrap_or(""),
                    if i["recovered_at_ms"].is_null() {
                        "Active"
                    } else {
                        "Recovered"
                    },
                    chrono::DateTime::from_timestamp_millis(
                        i["started_at_ms"].as_i64().unwrap_or(0)
                    )
                    .map(|v| v.to_rfc3339())
                    .unwrap_or_default()
                )
                .into(),
                critical: i["severity"] == "critical",
                acknowledged: !i["acknowledged_at_ms"].is_null(),
            })
            .collect::<Vec<_>>(),
    ))
}
fn bind_actions(ui: &App, tx: mpsc::SyncSender<Action>) {
    let t = tx.clone();
    let weak = ui.as_weak();
    ui.on_query_energy(move |a, b, z, p| {
        queue_action(&t, &weak, Action::Energy(a.into(), b.into(), z.into(), p));
    });
    let t = tx.clone();
    let weak = ui.as_weak();
    ui.on_save_tariff(move |p, c, e| {
        queue_action(&t, &weak, Action::Tariff(p.into(), c.into(), e.into()));
    });
    let t = tx.clone();
    let weak = ui.as_weak();
    ui.on_inspect(move || {
        queue_action(&t, &weak, Action::Inspect);
    });
    let t = tx.clone();
    let weak = ui.as_weak();
    ui.on_acknowledge(move |id| {
        queue_action(&t, &weak, Action::Acknowledge(id.into()));
    });
    let t = tx.clone();
    let weak = ui.as_weak();
    ui.on_evidence(move |id| {
        queue_action(&t, &weak, Action::Evidence(id.into()));
    });
    let t = tx.clone();
    let weak = ui.as_weak();
    ui.on_export_data(move || {
        queue_action(&t, &weak, Action::Export);
    });
    let weak = ui.as_weak();
    ui.on_refresh_incidents(move || {
        queue_action(&tx, &weak, Action::Incidents);
    });
}
fn main() -> Result<()> {
    let args = Args::parse();
    let timezone = railwatch_core::calendar::timezone(args.timezone.as_deref())?;
    if let Some(path) = args.screenshot {
        return screenshot(path, args.runtime_dir, args.page, timezone.name());
    }
    let ui = App::new()?;
    ui.set_page(args.page);
    let (start, end) = railwatch_core::calendar::today(chrono::Utc::now(), timezone)?;
    let today = chrono::DateTime::from_timestamp_millis(start)
        .context("invalid start")?
        .with_timezone(&timezone);
    let tomorrow = chrono::DateTime::from_timestamp_millis(end)
        .context("invalid end")?
        .with_timezone(&timezone);
    ui.set_timezone(timezone.name().into());
    ui.set_from(today.to_rfc3339().into());
    ui.set_to(tomorrow.to_rfc3339().into());
    ui.set_tariff_effective(today.to_rfc3339().into());
    let (tx, rx) = mpsc::sync_channel(16);
    if let Some(id) = args.incident {
        ui.set_page(2);
        let _ = tx.try_send(Action::Evidence(id));
    }
    bind_actions(&ui, tx);
    let (updates_tx, updates_rx) = mpsc::sync_channel::<UiUpdate>(8);
    let weak_ui = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            if let Some(ui) = weak_ui.upgrade() {
                for update in updates_rx.try_iter().take(8) {
                    update(&ui);
                }
            }
        },
    );
    let updates = UiUpdates(updates_tx);
    let running = Arc::new(AtomicBool::new(true));
    let run = running.clone();
    let worker = std::thread::spawn(move || {
        let socket = args.runtime_dir.join("monitor.sock");
        let control = args.runtime_dir.join("control.sock");
        let mut graph = vec![];
        let mut device = String::new();
        let mut last_seq = Value::Null;
        let mut next = Instant::now();
        let mut energy_tick = 0;
        while run.load(Ordering::Relaxed) {
            if Instant::now() >= next {
                next = Instant::now() + Duration::from_secs(1);
                match call(&socket, "Snapshot", json!({})) {
                    Ok(v) => {
                        device = v["snapshot"]["device"]["id"].as_str().unwrap_or("").into();
                        let seq = json!([
                            v["snapshot"]["telemetry"]["session_id"],
                            v["snapshot"]["telemetry"]["sequence"]
                        ]);
                        if seq != last_seq {
                            graph.push(if v["stale"] == true {
                                -1.
                            } else {
                                v["snapshot"]["telemetry"]["power_uw"]
                                    .as_f64()
                                    .unwrap_or(0.) as f32
                                    / 1e6
                            });
                            if graph.len() > 180 {
                                graph.remove(0);
                            }
                            last_seq = seq;
                        }
                        let g = graph.clone();
                        let _ = updates.deliver(move |ui| update_ui(ui, &v, &g));
                    }
                    Err(e) => {
                        let message = format!("Cannot connect to railwatchd: {e:#}");
                        let _ = updates.deliver(move |ui| {
                            ui.set_stale(true);
                            ui.set_status(message.into());
                        });
                    }
                }
                if energy_tick % 10 == 0 && !device.is_empty() {
                    let now = chrono::Utc::now();
                    let start = railwatch_core::calendar::today(now, timezone)
                        .map(|(start, _)| start)
                        .unwrap_or(now.timestamp_millis() / 60000 * 60000);
                    let end = now.timestamp_millis() / 60000 * 60000 + 60000;
                    if let Ok(v) = call(
                        &socket,
                        "Energy",
                        json!({"device_id":device,"from_ms":start,"to_ms":end,"period":"day","timezone":timezone.name()}),
                    ) {
                        let elapsed = (now.timestamp_millis() - start).max(1) as f64;
                        let _ = updates.deliver(move |ui| {
                            if !ui.get_custom_range() {
                                ui.set_energy_rows(strings(energy_rows(&v)));
                            }
                            if let Some(r) = v["buckets"].as_array().and_then(|r| r.last()) {
                                ui.set_energy(
                                    format!(
                                        "{:.4} kWh estimated wall",
                                        r["estimated_input_kwh"].as_f64().unwrap_or(0.)
                                    )
                                    .into(),
                                );
                                ui.set_cost(
                                    format!(
                                        "{} {}",
                                        r["currency"].as_str().unwrap_or(""),
                                        r["cost"].as_str().unwrap_or("Add a price per kWh")
                                    )
                                    .into(),
                                );
                                ui.set_coverage(
                                    format!(
                                        "{:.1} min observed · {:.1}% of today{}",
                                        r["output_observed_ms"].as_f64().unwrap_or(0.) / 60000.,
                                        100. * r["output_observed_ms"].as_f64().unwrap_or(0.)
                                            / elapsed,
                                        if r["incomplete_pricing"] == true {
                                            " · partial pricing"
                                        } else {
                                            ""
                                        }
                                    )
                                    .into(),
                                );
                            }
                        });
                    }
                    if let Ok(v) = call(&socket, "Tariffs", json!({})) {
                        let _ = updates.deliver(move |ui| {
                            ui.set_tariff_rows(strings(
                                v["tariffs"]
                                    .as_array()
                                    .map(Vec::as_slice)
                                    .unwrap_or(&[])
                                    .iter()
                                    .map(|r| {
                                        format!(
                                            "{}  {:.6} per kWh  from {}",
                                            r["currency"].as_str().unwrap_or(""),
                                            r["microcurrency_per_kwh"].as_f64().unwrap_or(0.) / 1e6,
                                            chrono::DateTime::from_timestamp_millis(
                                                r["effective_at_ms"].as_i64().unwrap_or(0)
                                            )
                                            .map(|t| t.to_rfc3339())
                                            .unwrap_or_default()
                                        )
                                    })
                                    .collect(),
                            ))
                        });
                    }
                    if let Ok(v) = call(&socket, "Incidents", json!({"limit":50})) {
                        let _ = updates.deliver(move |ui| ui.set_incidents(incident_rows(&v)));
                    }
                }
                energy_tick += 1;
            }
            if let Ok(action) = rx.recv_timeout(Duration::from_millis(50)) {
                let result = (|| -> Result<(String, Value)> {
                    match action {
                        Action::Energy(a, b, z, p) => Ok((
                            "energy".into(),
                            call(
                                &socket,
                                "Energy",
                                json!({"device_id":device,"from_ms":date(&a)?,"to_ms":date(&b)?,"timezone":z,"period":(["hour","day","week","month","year"].get(p as usize).context("invalid period")?)}),
                            )?,
                        )),
                        Action::Tariff(p, c, e) => {
                            call(
                                &control,
                                "SetTariff",
                                json!({"tariff":Tariff{effective_at_ms:date(&e)?,microcurrency_per_kwh:parse_price(&p)?,currency:c}}),
                            )?;
                            Ok((
                                "Price saved. Recorded energy will use this dated rate.".into(),
                                Value::Null,
                            ))
                        }
                        Action::Inspect => {
                            Ok(("inspect".into(), call(&control, "Inspect", json!({}))?))
                        }
                        Action::Acknowledge(id) => {
                            call(&control, "Acknowledge", json!({"id":id}))?;
                            Ok((
                                "incidents".into(),
                                call(&socket, "Incidents", json!({"limit":50}))?,
                            ))
                        }
                        Action::Incidents => Ok((
                            "incidents".into(),
                            call(&socket, "Incidents", json!({"limit":50}))?,
                        )),
                        Action::Evidence(id) => Ok((
                            "evidence".into(),
                            call(&socket, "Evidence", json!({"id":id}))?,
                        )),
                        Action::Export => {
                            let v = call(&socket, "Snapshot", json!({}))?;
                            let dir = std::env::var_os("HOME")
                                .map(PathBuf::from)
                                .context("HOME unavailable")?;
                            let path = dir.join(format!(
                                "railwatch-{}.json",
                                chrono::Utc::now().format("%Y%m%dT%H%M%S%3f")
                            ));
                            std::fs::write(&path, serde_json::to_vec_pretty(&v)?)?;
                            Ok((format!("Exported {}", path.display()), Value::Null))
                        }
                    }
                })();
                let _=updates.deliver(move|ui|match result{Ok((kind,v))=>match kind.as_str(){"inspect"=>{let raw=v["safeguard_report"].as_array().map(Vec::as_slice).unwrap_or(&[]);let payload=raw.iter().skip(2).take(8).map(Value::to_string).collect::<Vec<_>>().join(", ");ui.set_configuration_detail(format!("Safeguard configuration bytes: {payload}. Timer units and write semantics are unqualified.").into());ui.set_notice("Device settings read successfully.".into());},"energy"=>ui.set_energy_rows(strings(energy_rows(&v))),"incidents"=>ui.set_incidents(incident_rows(&v)),"evidence"=>{let rows=v["samples"].as_array().map(Vec::as_slice).unwrap_or(&[]);ui.set_incident_detail(format!("{} retained readings. First: {}. Last: {}. Export full evidence with railwatch evidence <incident-id> --json.",rows.len(),rows.first().map(|r|r["captured_at_ms"].to_string()).unwrap_or_default(),rows.last().map(|r|r["captured_at_ms"].to_string()).unwrap_or_default()).into());},_=>ui.set_notice(format!("{kind} {}",if v.is_null(){String::new()}else{v.to_string()}).into())},Err(e)=>ui.set_notice(format!("{e:#}").into())});
            }
        }
    });
    let outcome = ui.run();
    running.store(false, Ordering::Relaxed);
    drop(timer);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("connection worker panicked"))?;
    outcome?;
    Ok(())
}
fn screenshot(path: PathBuf, runtime: PathBuf, page: i32, zone: &str) -> Result<()> {
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use std::{cell::RefCell, io::Write, rc::Rc};
    thread_local! {static WINDOW:RefCell<Option<Rc<MinimalSoftwareWindow>>>=const{RefCell::new(None)};}
    struct Platform;
    impl slint::platform::Platform for Platform {
        fn create_window_adapter(
            &self,
        ) -> std::result::Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError>
        {
            let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            WINDOW.with(|s| *s.borrow_mut() = Some(w.clone()));
            Ok(w)
        }
    }
    slint::platform::set_platform(Box::new(Platform))?;
    let ui = App::new()?;
    ui.set_page(page);
    let v = call(&runtime.join("monitor.sock"), "Snapshot", json!({}))?;
    let socket = runtime.join("monitor.sock");
    let id = v["snapshot"]["device"]["id"]
        .as_str()
        .context("no device")?;
    let now = chrono::Utc::now();
    let timezone = railwatch_core::calendar::timezone(Some(zone))?;
    let (start, _) = railwatch_core::calendar::today(now, timezone)?;
    ui.set_timezone(zone.into());
    let end = now.timestamp_millis() / 60000 * 60000 + 60000;
    let samples = call(
        &socket,
        "Samples",
        json!({"device_id":id,"from_ms":now.timestamp_millis()-180000,"to_ms":now.timestamp_millis()+1,"limit":180}),
    )?;
    let graph = samples["samples"]
        .as_array()
        .context("invalid samples")?
        .iter()
        .map(|s| s["power_uw"].as_f64().unwrap_or(0.) as f32 / 1e6)
        .collect::<Vec<_>>();
    update_ui(&ui, &v, &graph);
    let energy = call(
        &socket,
        "Energy",
        json!({"device_id":id,"from_ms":start,"to_ms":end,"period":"day","timezone":zone}),
    )?;
    ui.set_from(
        chrono::DateTime::from_timestamp_millis(start)
            .unwrap()
            .to_rfc3339()
            .into(),
    );
    ui.set_to(
        chrono::DateTime::from_timestamp_millis(end)
            .unwrap()
            .to_rfc3339()
            .into(),
    );
    ui.set_tariff_effective(ui.get_from());
    ui.set_energy_rows(strings(energy_rows(&energy)));
    if let Some(r) = energy["buckets"].as_array().and_then(|r| r.last()) {
        ui.set_energy(
            format!(
                "{:.5} kWh estimated wall",
                r["estimated_input_kwh"].as_f64().unwrap_or(0.)
            )
            .into(),
        );
        ui.set_cost(
            format!(
                "{} {}",
                r["currency"].as_str().unwrap_or(""),
                r["cost"].as_str().unwrap_or("Add a price per kWh")
            )
            .into(),
        );
        ui.set_coverage(
            format!(
                "{:.1} minutes observed",
                r["output_observed_ms"].as_f64().unwrap_or(0.) / 60000.
            )
            .into(),
        );
    }
    let incidents = call(&socket, "Incidents", json!({"limit":50}))?;
    ui.set_incidents(incident_rows(&incidents));
    ui.show()?;
    let mut pixels = vec![slint::Rgb8Pixel::default(); 1220 * 850];
    WINDOW.with(|s| {
        let w = s.borrow();
        let w = w.as_ref().unwrap();
        w.set_size(slint::PhysicalSize::new(1220, 850));
        w.request_redraw();
        w.draw_if_needed(|r| {
            r.render(&mut pixels, 1220);
        });
    });
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(f, "P6\n1220 850\n255\n")?;
    for p in pixels {
        f.write_all(&[p.r, p.g, p.b])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{PointerEventButton, WindowEvent};
    use std::rc::Rc;
    struct TestPlatform(Rc<MinimalSoftwareWindow>);
    impl slint::platform::Platform for TestPlatform {
        fn create_window_adapter(
            &self,
        ) -> std::result::Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError>
        {
            Ok(self.0.clone())
        }
    }
    #[test]
    fn rendered_navigation_and_forms_dispatch_real_actions() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(window.clone()))).unwrap();
        let app = App::new().unwrap();
        let (tx, rx) = mpsc::sync_channel(16);
        bind_actions(&app, tx);
        app.show().unwrap();
        window.set_size(slint::PhysicalSize::new(1220, 850));
        let mut pixels = vec![slint::Rgb8Pixel::default(); 1220 * 850];
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 1220);
        });
        let position = slint::LogicalPosition::new(85., 181.);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
        assert_eq!(
            app.get_page(),
            1,
            "the rendered energy navigation button must open its page"
        );
        app.invoke_query_energy(
            "2026-09-01T00:00:00-03:00".into(),
            "2026-10-01T00:00:00-03:00".into(),
            "America/Sao_Paulo".into(),
            3,
        );
        match rx.try_recv().unwrap() {
            Action::Energy(from, to, zone, period) => {
                assert!(date(&from).unwrap() < date(&to).unwrap());
                assert_eq!(zone, "America/Sao_Paulo");
                assert_eq!(period, 3);
            }
            _ => panic!("wrong action"),
        }
        app.invoke_save_tariff(
            "1.234567".into(),
            "BRL".into(),
            "2026-09-01T00:00:00-03:00".into(),
        );
        match rx.try_recv().unwrap() {
            Action::Tariff(price, currency, effective) => {
                assert_eq!(parse_price(&price).unwrap(), 1234567);
                assert_eq!(currency, "BRL");
                assert!(date(&effective).is_ok());
            }
            _ => panic!("wrong action"),
        }
        app.invoke_acknowledge("retained-incident".into());
        assert!(
            matches!(rx.try_recv().unwrap(),Action::Acknowledge(id) if id=="retained-incident")
        );
        let t = railwatch_hardware::simulated_sample(2, true);
        update_ui(
            &app,
            &json!({"stale":true,"age_ms":5000,"snapshot":{"device":{"model":"Test PSU"},"telemetry":t,"active_incidents":[],"storage_error":"disk full"}}),
            &[200., -1., 250.],
        );
        assert!(app.get_stale());
        assert!(app.get_problem().contains("disk full"));
        assert_eq!(app.get_watts(), "252.0 W");
        for _ in 0..17 {
            app.invoke_acknowledge("queued".into());
        }
        assert!(app.get_notice().contains("Still processing"));
        drop(rx);
        app.invoke_acknowledge("closed".into());
        assert!(app.get_notice().contains("worker stopped"));
    }

    #[test]
    fn stalled_ui_backpressures_worker_and_close_releases_it() {
        let (tx, rx) = mpsc::sync_channel(8);
        let updates = UiUpdates(tx);
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            for _ in 0..8 {
                updates.deliver(|_| {}).unwrap();
            }
            done_tx.send(()).unwrap();
            assert!(updates.deliver(|_| {}).is_err());
        });
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!worker.is_finished());
        drop(rx);
        worker.join().unwrap();
    }
}

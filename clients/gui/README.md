# Railwatch GUI

Native Slint client using the connector-focused design selected in the local preview. Build with `cargo build --locked --manifest-path clients/gui/Cargo.toml` from the checkout root. Set `RAILWATCH_RUNTIME_DIR` for an isolated daemon.

The UI uses true black, white text, two six-conductor views, calendar energy queries, dated tariffs, incident history, cooling inspection and current-state export. It uses the same daemon API and calculations as the CLI. Hardware writes remain unavailable pending qualification.

`--page 0..4` selects Overview, Energy & cost, Incidents, Cooling or Device. `--incident ID` opens retained incident evidence. `--screenshot OUTPUT.ppm` renders the actual native components using the daemon's current telemetry and history, without opening a desktop window.

The application depends on the independent `railwatch-client` and `railwatch-core` crates. Hardware simulation is a development-only dependency for UI tests. The normal dependency tree contains no daemon, hardware implementation or SQLite. Source is available in the public Railwatch repository; these crates have not been published to a package registry.

# Railwatch GUI

Native Slint client using the connector-focused design selected in the local preview. Build with `cargo build --locked --manifest-path clients/gui/Cargo.toml` from the checkout root. Set `RAILWATCH_RUNTIME_DIR` for an isolated daemon.

The UI uses true black, white text, two six-conductor views, calendar energy queries, dated tariffs, incident history, cooling inspection and current-state export. It uses the same daemon API and calculations as the CLI. Hardware writes remain unavailable pending qualification.

`--page 0..4` selects Overview, Energy & cost, Incidents, Cooling or Device. `--incident ID` opens retained incident evidence. `--screenshot OUTPUT.ppm` renders the actual native components using the daemon's current telemetry and history, without opening a desktop window.

The initial implementation shares the Rust API library from the parent checkout. Publish a versioned client/model crate before moving this application into a separate repository. The repository is currently a local implementation, not a published package.

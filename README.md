# Railwatch for Linux

A local daemon, CLI, desktop notifier, native Slint application, and Noctalia v5 plugin for PSU monitoring.

This is an independent community project. It has no affiliation with MSI and is not sponsored, endorsed, or supported by MSI. Product names identify compatible hardware only.

The daemon owns the HID connection. All clients read its cache and SQLite history. Energy is recorded before an electricity price is configured; a dated price can calculate costs for previously recorded consumption.

## Supported PSUs

| PSU | USB ID | Support status |
| --- | --- | --- |
| MPG Ai1600TS | `0db0:808c` | Monitoring validated on real hardware, including reboot and a move to another USB header. |
| Ai1300TS | `0db0:aa6f` | Recognized by the TS decoder; not yet validated on real hardware. |

T and P families are detected but unsupported for telemetry because their layouts differ. Recognition alone does not establish support.

Available readings include output power, efficiency, temperature, fan RPM, requested/calculated/actual fan duty, zero-fan state, DC rails, twelve conductor currents, hardware alarms, fault snapshots, runtime counters, and raw safeguard configuration.

Hardware writes are unavailable. Fan mode mapping, safeguard timer units, buzzer switching and persistence have unresolved semantics. The CLI exposes the evidence and qualification status rather than issuing guessed writes. The Windows prototype's interpretation of raw fan mode 1 as manual is contradicted by live firmware-controlled fan changes.

## Build and try

Requires Rust 1.95 for the native GUI and the validated toolchain. No service installation is needed for development.

```sh
cargo build --locked
cargo build --locked --manifest-path clients/gui/Cargo.toml

# Terminal 1: isolated simulator with a fault after five samples.
./target/debug/railwatchd --simulate --fault-after 5 \
  --runtime-dir .runtime/demo --database .runtime/demo.db

# Terminal 2
export RAILWATCH_RUNTIME_DIR="$PWD/.runtime/demo"
./target/debug/railwatch status
./target/debug/railwatch watch --json --count 5
./target/debug/railwatch incidents --active
./clients/gui/target/debug/railwatch-gui
```

For a connected PSU, omit `--simulate` and select `--device /dev/hidrawN`. Use `railwatch discover` first. HID read commands require opening the descriptor for both reading and writing. Another raw client or a PSU-specific kernel driver must not own the same device.

## Record electricity costs

Use `railwatch devices` to find the stable device ID. Prices are per **kWh**, with up to six decimal places. Effective times and query boundaries must align to a minute. Times accept RFC3339 with an explicit offset, or Unix milliseconds.

```sh
railwatch tariff set --price 1.00 --currency BRL \
  --effective 2026-09-01T00:00:00-03:00
railwatch energy --device-id demo-ai1600ts \
  --from 2026-09-01T00:00:00-03:00 --to 2026-10-01T00:00:00-03:00 \
  --period day --timezone America/Sao_Paulo --csv september.csv
```

Choose `hour`, `day`, `week`, `month`, or `year`. A new dated price preserves earlier rates. Replacing a rate at the same time requires `--replace`. One currency is supported per history database; currencies are never silently added together.

DC output is measured by the PSU. Wall input is estimated as output divided by reported efficiency. Cost uses that estimated wall input. Missing readings, suspend, reconnects and process restarts are not billed as continuous coverage. Rows report observed and priced durations; absent buckets mean no recorded coverage, not zero consumption. This is not a utility revenue meter and does not include fixed charges or taxes.

## Alerts and sound

Hardware alarms are immediate. The software imbalance rule requires sufficient connector load, percentage deviation, absolute spread, a trigger duration, and recovery hysteresis. Defaults are 6 A total, 40% deviation, 0.5 A spread, 3 seconds to trigger and 5 seconds to recover. These are advisory comparison defaults, not qualified hardware protection limits. An unloaded connector is quiet. Sensor numbering is not a verified physical pin diagram.

```sh
railwatch policy get
railwatch policy set policy.json --expected-revision 0
railwatch incidents --active
railwatch evidence INCIDENT_ID --json
railwatch acknowledge INCIDENT_ID
railwatch notify run
railwatch notify test
railwatch notify silence --minutes 15
railwatch notify status
```

`policy set` accepts the policy object from `policy get`, excluding the surrounding revision wrapper. The revision prevents lost updates.

The notifier uses the desktop notification service through `notify-send` and PipeWire through `pw-play`. One notifier runs per user. Critical incidents repeat at most three times, at least 30 seconds apart. Acknowledgement suppresses future delivery without clearing the condition. Delivery failures are retained in the user's state directory. Silence affects desktop audio only. No PSU buzzer command is sent.

## Clients and packaging

- `clients/gui`: the selected connector-focused native desktop UI. Live overview, calendar energy queries, dated tariffs, incidents, evidence summaries, cooling inspection, and exports. Deep links accept `--incident ID`.
- `clients/noctalia`: a source catalog and Luau plugin for Noctalia v5, plugin API 9. One persistent `railwatch plugin-stream` Rust process supplies live snapshots, energy and incidents. It does not poll HID or produce duplicate sounds.
- `packaging`: systemd, udev, sysusers, desktop entry and an Arch package recipe. See [installation](docs/installation.md) before installing.

Source repository: [Gustav0ar/railwatch](https://github.com/Gustav0ar/railwatch). The daemon, CLI, GUI and Noctalia plugin share this repository. The Noctalia catalog can be published separately. The GUI imports the Rust client/model library by path; extract and version that library before splitting the GUI into a separate repository. No release has been published.

## Verify

```sh
./scripts/check.sh
```

Tests cover Linux capture decoding, report framing, binary counter edge cases, cost arithmetic, delayed pricing, tariff changes, DST, gaps, restart recovery, incident acknowledgements, permission boundaries and the daemon/CLI connection. Noctalia entry tests run without touching an installed shell. [Validation evidence](docs/validation.md) separates hardware observations from simulator and rendering checks.

The [implementation plan](PLAN.md) records the broader roadmap. Kernel upstreaming means a separate hardware driver, while this userspace daemon retains history, policy, pricing, and desktop integration. The Rust-only requirement also applies to that proposal; required kernel bindings and maintainer acceptance must be established separately. See [kernel handoff](docs/kernel-handoff.md).

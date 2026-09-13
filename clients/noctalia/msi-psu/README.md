# MSI PSU

Noctalia v5, plugin API 9. Requires the Rust `msi-psu` CLI, `msi-psud`, and `msi-psu-gui` for detailed views.

The summary widget shows watts, stale state and active incidents. Its panel compares both connectors, shows today's estimated wall energy and cost, and opens incident details in the native GUI. The plugin never accesses HID and never owns desktop alarm sound.

The Luau service starts one persistent `msi-psu plugin-stream` process. Rust handles the Unix connection, reconnects, framing and history queries. `MSI_PSU_RUNTIME_DIR` defaults to `/run/msi-psu`. History refreshes every 15 seconds; a broken live stream becomes explicitly stale after five seconds. Hidden panels do not render updates.

From the development checkout, run `noctalia plugins lint clients/noctalia/msi-psu`. Entry behavior tests and transport tests are included in the checkout's validation script. No active shell configuration is changed by these checks.

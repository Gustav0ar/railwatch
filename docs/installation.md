# Installation

The development run uses private workspace sockets and databases. Nothing in this checkout automatically installs a service, modifies the active Noctalia configuration, or changes PSU settings.

## System package

Build a release archive and package with `packaging/PKGBUILD`, or install the listed files through your distribution's packaging tools. The local archive recipe uses `SKIP` as a development checksum; replace it with the release archive checksum before distributing it.

`scripts/source-archive.sh` creates an archive in `.runtime/dist` from an explicit source allowlist and prints its SHA-256. Local databases, raw captures and build output are excluded. The Arch package includes the daemon, CLI, GUI, desktop entry, service/udev/sysusers definitions, API schema and a Noctalia path source at `/usr/share/msi-psu/noctalia`. It disables makepkg's GCC LTO option because bundled SQLite must link with Rust's LLVM linker. Building the package does not install or enable it.

Use `makepkg --cleanbuild` when building an archive, especially when replacing a development archive at the same version. Archives use fixed timestamps for reproducibility; reusing a previous Cargo target directory can otherwise hide source changes from timestamp-based rebuild detection. Package checks run against optimized binaries.

The service account `msi-psu` owns the database. Its supplementary hardware group can open qualified HID nodes. Readers join `msi-psu-read`; users who may change prices, acknowledge incidents and change software policy also join `msi-psu-control`. Both groups are needed for the graphical application. Do not add desktop users to the hardware group merely to use the GUI.

After installing and reviewing the files, an administrator can run:

```sh
sudo systemd-sysusers
sudo usermod -aG msi-psu-read,msi-psu-control "$USER"
sudo udevadm control --reload-rules
sudo systemctl daemon-reload
sudo systemctl enable --now msi-psud.service
```

Log out and back in to pick up group membership. Reconnect the PSU's USB monitoring cable, or apply the udev rule to its specific device. Do not broadly trigger unrelated hardware during qualification.

The monitor socket is group-readable. The separate control socket belongs to `msi-psu-control`. Group membership authorizes local software mutations; it does not enable unsupported PSU writes. The runtime directory itself is not group-writable.

Install `libnotify` and PipeWire to enable the notifier, then:

```sh
systemctl --user enable --now msi-psu-notify.service
msi-psu notify test
```

A successful player exit verifies the playback path, not that a muted output was audible. The notifier records failed delivery attempts in `$XDG_STATE_HOME/msi-psu/notifications.json`.

## GUI

Build `clients/gui` and install its binary as `/usr/bin/msi-psu-gui`. Install `packaging/msi-psu-gui.desktop` in `/usr/share/applications`. The GUI needs only daemon socket access; it never opens a PSU.

## Noctalia

The plugin targets Noctalia v5's Luau API 9. For a separate development shell configuration, add `clients/noctalia` as a path source and enable `gustav0ar/msi-psu`. For the installed shell, use Noctalia's documented source management workflow after reviewing the plugin.

The service entry maintains the connection. Add the `summary` widget to the desired bar. Clicking it opens the connector panel. The GUI handles detailed history, acknowledgements and pricing. Set `MSI_PSU_RUNTIME_DIR` in the shell's environment only when using development sockets.

Both clients default to the system time zone. The GUI and `plugin-stream` accept `--timezone` to override it. Calendar boundaries follow that zone's clock changes.

## Data and recovery

SQLite uses WAL and `synchronous=FULL`. Every accepted sample commits; a separate bounded queue prevents slow storage from blocking live monitoring. Queue overflow is visible and does not invent missing energy. Raw samples are pruned after 48 hours in batches. Minute energy and incident evidence remain retained.

In-memory limits are explicit: 16 storage messages, 256 pending incident updates, 2,048 pending evidence readings and 32 concurrent evidence windows. A long disk outage or alarm storm can exceed them. The daemon reports lost updates, lost evidence and shortened windows rather than letting memory grow indefinitely. Desktop incidents observed together share one notification and tone. Notification delivery deduplication retains at most 4,096 entries.

Energy replies are limited to 10,000 periods; choose a shorter date range or a coarser period for longer histories. This does not prune stored energy. Price history accepts up to 4,096 dated rates. The GUI holds at most eight pending display updates and sixteen actions, and reports a full action queue. Notification and audio child processes have five-second deadlines.

Stop the daemon before copying its database for an offline backup, or use SQLite's online backup API. Copying only the main database while WAL writes are active is not a valid backup.

On restart, the daemon opens a new integration session, restores active incidents, and reconciles them using fresh hardware readings. Recovery is separate from acknowledgement. Repeated protection snapshots are observations of device-reported state; the hardware's retention and clear behavior remain unqualified.

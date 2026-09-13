# Kernel handoff

The daemon is not code that can be moved into the Linux kernel. Keep USB transport and integer decoding separate for a future Rust HID driver that registers standard hwmon channels. Retain SQLite, software incidents, tariffs, notifications and UI in userspace. The user's Rust-only constraint supersedes the original C-driver proposal. Required Rust bindings and an acceptable upstream design need maintainer review before promising that migration.

## Evidence available for an RFC

- `fixtures/ai1600ts-linux.json` contains sanitized Linux captures with the serial report omitted.
- The unnumbered HID output uses a 65-byte userspace write, including the leading zero report ID. Linux input is 64 bytes; a compatibility normalizer also handles a leading zero in a 65-byte capture.
- The initial handshake is `00 FA 51`, distinct from ordinary `00 51 CMD` reads.
- TS `E0` has a 44-byte payload. Its numeric format uses an unsigned 11-bit mantissa and signed five-bit exponent, as in the recovered vendor implementation. Do not substitute the standard signed PMBus mantissa.
- `E1` has 18 alarm bytes. `C1` has two 21-byte fault snapshots, not a 45-byte payload.
- A payload's low byte `FE` is not by itself an error. Live runtime values naturally cross it. A report consisting only of `FE` and zero padding remains ambiguous and is currently omitted.
- Raw fan mode 1 can coexist with firmware-controlled fan changes and requested duty zero. Writable PWM mapping is not yet qualified.

## Initial patch boundary

Propose a Rust HID driver with a transaction mutex, bounded completion wait, matching echo validation, disconnect cancellation, and one complete-frame cache. Identify any missing Rust HID/hwmon abstractions as part of the RFC. Export the validated TS voltage, current, output power, temperature and fan channels through standard hwmon attributes. Use integer SI units and label conductor sensors without claiming a verified physical orientation.

Do not bind the development driver during normal userspace operation. Hidraw access beside a kernel owner cannot be serialized by the userspace lock. Backend changes require an explicit administrator action.

Before a kernel backend replaces hidraw, resolve efficiency, device fault snapshots, counters and safeguard inspection with HID/hwmon maintainers. Do not create a private ioctl tunnel or require debugfs as the stable application interface. If standard hwmon cannot carry an advanced feature, advertise it as unavailable or keep the documented userspace deployment mode.

## Outstanding hardware qualification

Fan mode/duty writes, zero-fan mutation, safeguard units and ranges, `C2` switch semantics, `F1` nonvolatile commit, firmware behavior after daemon loss, suspend/resume, USB disconnect/reconnect, and Ai1300TS/T/P hardware fixtures need separate qualification. No hardware protection was disabled and no power-cycle or physical disconnection was performed during this implementation.

The next kernel milestone is an RFC patch series with Kconfig/Makefile changes, documentation, MAINTAINERS coverage, strict checkpatch and kernel builds. Loading and testing that driver is a separate administrator operation. No driver has been submitted or accepted upstream.

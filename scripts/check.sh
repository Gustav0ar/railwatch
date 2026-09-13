#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
cargo fmt --manifest-path clients/gui/Cargo.toml -- --check
cargo clippy --workspace --locked --all-targets --manifest-path clients/gui/Cargo.toml -- -D warnings
cargo test --locked --manifest-path clients/gui/Cargo.toml
cargo build --locked --manifest-path clients/gui/Cargo.toml
if command -v noctalia >/dev/null; then noctalia plugins lint clients/noctalia/railwatch; fi
./scripts/check-boundaries.sh

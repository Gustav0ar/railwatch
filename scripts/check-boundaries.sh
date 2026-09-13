#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
protocol_tree=$(cargo tree --locked -p railwatch-protocol --edges normal --prefix none)
[[ $(printf '%s\n' "$protocol_tree" | wc -l) == 1 ]] || { echo 'Protocol must have no runtime dependencies'; exit 1; }
for manifest in crates/client/Cargo.toml clients/gui/Cargo.toml; do
    tree=$(cargo tree --locked --manifest-path "$manifest" --edges normal --prefix none)
    if printf '%s\n' "$tree" | rg '^(railwatch |railwatch-hardware |railwatch-protocol |rusqlite |libsqlite3-sys )'; then
        echo "$manifest depends on daemon, hardware or storage implementation"
        exit 1
    fi
done
echo 'Dependency boundaries verified: protocol has no dependencies; client and GUI have no daemon, hardware or database dependency.'

#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "railwatch") | .version')
destination=".runtime/dist/railwatch-$version.tar.gz"
mkdir -p .runtime/dist
# An allowlist keeps local captures, databases, credentials and build output out of releases.
tar --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
    --exclude='*/target' --exclude='__pycache__' \
    --transform="s,^,railwatch-$version/," \
    -cf - Cargo.toml Cargo.lock LICENSE README.md PLAN.md src tests fixtures crates \
    clients docs packaging scripts | gzip -n > "$destination"
sha256sum "$destination"

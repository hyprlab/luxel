#!/usr/bin/env bash
# Regenerate cargo-sources.json after changing dependencies in Cargo.toml.
# Requires python3 with aiohttp, PyYAML and tomlkit installed.
set -euo pipefail
cd "$(dirname "$0")/.."

generator=$(mktemp)
trap 'rm -f "$generator"' EXIT
curl -sSL -o "$generator" \
    https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py

cargo generate-lockfile
python3 "$generator" Cargo.lock -o build-aux/cargo-sources.json
echo "build-aux/cargo-sources.json updated."

#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
tools=41c20aa10819cdb2a4f3ca171758a96d1955c018
sha256=0a2db6be87d75910facef28ab46d4d6460802e8419ab850d0caa6a364d26b380
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL --retry 5 -o "$work/flatpak-cargo-generator.py" \
    "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$tools/cargo/flatpak-cargo-generator.py"
actual=$( (sha256sum 2>/dev/null || shasum -a 256) <"$work/flatpak-cargo-generator.py" | cut -d' ' -f1)
[ "$actual" = "$sha256" ] || { echo "flatpak-cargo-generator.py has checksum $actual" >&2; exit 1; }
python3 -m venv "$work/venv"
"$work/venv/bin/python" -I -m pip install -q 'aiohttp==3.13.5' 'PyYAML==6.0.3' 'tomlkit==0.13.3'
"$work/venv/bin/python" -I "$work/flatpak-cargo-generator.py" Cargo.lock -o packaging/flatpak/cargo-sources.json

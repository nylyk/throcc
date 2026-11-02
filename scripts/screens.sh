#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q --release -p throcc-client
mkdir -p docs/screens
[ $# -gt 0 ] || set -- join lobby room focus share connection settings appearance members verify keychange
for name in "$@"; do
    ./target/release/throcc-client --mock --screen "$name" >/dev/null 2>&1 &
    pid=$!
    trap 'kill "$pid" 2>/dev/null' EXIT
    sleep 4
    window=$(osascript -l JavaScript scripts/window-id.js throcc-client)
    screencapture -x -o -l "$window" "docs/screens/$name.png"
    kill "$pid"
    wait "$pid" 2>/dev/null || true
done

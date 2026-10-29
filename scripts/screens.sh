#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q --release -p throcc-client
mkdir -p docs/screens
for screen in "${@:-join lobby room share settings connection verify keychange members}"; do
    for name in $screen; do
        ./target/release/throcc-client --mock --screen "$name" >/dev/null 2>&1 &
        pid=$!
        sleep 3
        window=$(osascript -l JavaScript scripts/window-id.js throcc-client)
        screencapture -x -o -l "$window" "docs/screens/$name.png"
        kill "$pid"
        wait "$pid" 2>/dev/null || true
    done
done

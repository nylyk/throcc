#!/usr/bin/env bash
set -euo pipefail
mkdir -p /out
Xvfb :99 -screen 0 1280x800x24 >/dev/null 2>&1 &
export DISPLAY=:99 WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/tmp
sleep 1
for screen in "${@:-join lobby room share settings connection verify keychange members}"; do
    for name in $screen; do
        throcc-client --mock --screen "$name" >"/out/$name.log" 2>&1 &
        pid=$!
        sleep 4
        import -window root "/out/$name.png"
        kill "$pid"
        wait "$pid" 2>/dev/null || true
    done
done

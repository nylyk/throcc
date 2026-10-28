#!/usr/bin/env bash
set -euo pipefail

banned='^(aes-gcm|rsmpeg|rusty_ffmpeg|ffmpeg-sys|ffmpeg-sys-next|ffmpeg-next|cpal|sonora|sonora-common-audio|ropus|neteq|hpke|gpui-pre|gpui-component|screencapturekit|windows-capture|pipewire) '

tree=$(cargo tree -p throcc-server -e normal,build --target all --prefix none -f '{p} [{f}]')

status=0
if grep -E "$banned" <<<"$tree"; then
    echo "error: throcc-server depends on a banned crate (above)" >&2
    status=1
fi
if grep -E '^throcc-crypto .*\[[^]]*\bmedia\b' <<<"$tree"; then
    echo "error: throcc-server enables throcc-crypto's media feature" >&2
    status=1
fi
[ "$status" -eq 0 ] && echo "throcc-server dependency tree is clean"
exit "$status"

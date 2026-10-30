#!/usr/bin/env bash
set -euo pipefail
prefix=$1
out=$2
include="$prefix/include"
wrapper=$(mktemp)
trap 'rm -f "$wrapper"' EXIT
for header in "$include"/lib{avcodec,avdevice,avfilter,avformat,avutil,swresample,swscale}/*.h; do
    name=${header#"$include"/}
    case "$name" in
        */hwcontext_*.h | */d3d1[12]va.h | */dxva2.h | */qsv.h | */vdpau.h | */videotoolbox.h | */xvmc.h) continue ;;
    esac
    echo "#include <$name>" >>"$wrapper"
done
bindgen "$wrapper" --output "$out" \
    --impl-debug --rust-target 1.68 --no-prepend-enum-name \
    --blocklist-type __mingw_ldbl_type_t \
    --blocklist-item 'FP_(NAN|INFINITE|ZERO|SUBNORMAL|NORMAL)' \
    -- -x c -I"$include"

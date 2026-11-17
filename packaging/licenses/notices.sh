#!/usr/bin/env bash
# Writes packaging/licenses/<side>-notices.txt: the native libraries a package ships, from
# native/, then the Rust crates it builds, from cargo-about.
set -euo pipefail
cd "$(dirname "$0")/../.."
side=${1:-}
native_preface="These are the native libraries in this build, with their licenses."
case "$side" in
    client) crate=throcc-client targets=(x86_64-pc-windows-msvc x86_64-unknown-linux-gnu)
        preface=$native_preface natives=(ffmpeg dav1d libvpx nv-codec-headers lucide) ;;
    client-macos) crate=throcc-client targets=(aarch64-apple-darwin)
        preface=$native_preface natives=(dav1d libvpx lucide) ;;
    server) crate=throcc-server targets=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu)
        preface= natives=() ;;
    android) crate=throcc-mobile targets=(aarch64-linux-android x86_64-linux-android)
        preface=$(cat packaging/licenses/android.txt) natives=(dav1d libvpx) ;;
    *) echo "usage: $0 client|client-macos|server|android" >&2; exit 2 ;;
esac
crates=$(mktemp)
trap 'rm -f "$crates"' EXIT
cargo about generate --locked -m "crates/$crate/Cargo.toml" -c packaging/licenses/about.toml \
    "${targets[@]/#/--target=}" -o "$crates" packaging/licenses/about.hbs
out="packaging/licenses/$side-notices.txt"
{
    if [ -n "$preface" ]; then
        printf '%s\n\n' "$preface"
        for native in "${natives[@]}"; do
            cat "packaging/licenses/native/$native.txt"
            echo
        done
    fi
    cat "$crates"
} >"$out"
echo "wrote $out"

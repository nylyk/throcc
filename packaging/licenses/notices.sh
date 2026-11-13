#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
side=${1:-}
case "$side" in
    client) crate=throcc-client preface=native.txt targets=(aarch64-apple-darwin x86_64-pc-windows-msvc x86_64-unknown-linux-gnu) ;;
    server) crate=throcc-server preface= targets=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu) ;;
    android) crate=throcc-mobile preface=android.txt targets=(aarch64-linux-android x86_64-linux-android) ;;
    *) echo "usage: $0 client|server|android" >&2; exit 2 ;;
esac
crates=$(mktemp)
trap 'rm -f "$crates"' EXIT
cargo about generate --locked -m "crates/$crate/Cargo.toml" -c packaging/licenses/about.toml \
    "${targets[@]/#/--target=}" -o "$crates" packaging/licenses/about.hbs
out="packaging/licenses/$side-notices.txt"
{
    [ -z "$preface" ] || { cat "packaging/licenses/$preface"; echo; }
    cat "$crates"
} >"$out"
echo "wrote $out"

#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
side=${1:-}
case "$side" in
    client) targets=(aarch64-apple-darwin x86_64-pc-windows-msvc x86_64-unknown-linux-gnu) ;;
    server) targets=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu) ;;
    *) echo "usage: $0 client|server" >&2; exit 2 ;;
esac
crates=$(mktemp)
trap 'rm -f "$crates"' EXIT
cargo about generate --locked -m "crates/throcc-$side/Cargo.toml" -c packaging/licenses/about.toml \
    "${targets[@]/#/--target=}" -o "$crates" packaging/licenses/about.hbs
out="packaging/licenses/$side-notices.txt"
{
    [ "$side" = server ] || { cat packaging/licenses/native.txt; echo; }
    cat "$crates"
} >"$out"
echo "wrote $out"

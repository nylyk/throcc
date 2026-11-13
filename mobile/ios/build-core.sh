#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
[ $# -gt 0 ] || set -- device simulator

export IPHONEOS_DEPLOYMENT_TARGET=26.0
bindings=target/ios-bindings
headers=$bindings/headers
generated=mobile/ios/throcc/Generated
framework=mobile/ios/ThroccCore.xcframework

targets=()
triples=()
libraries=()
xcframework=()
for slice in "$@"; do
    case $slice in
        device) triple=aarch64-apple-ios ;;
        simulator) triple=aarch64-apple-ios-sim ;;
        *) echo "usage: $0 [device] [simulator]" >&2; exit 2 ;;
    esac
    library=target/$triple/release/libthrocc_mobile.a
    targets+=(--target "$triple")
    triples+=("$triple")
    libraries+=("$library")
    xcframework+=(-library "$library" -headers "$headers")
done

# dav1d and libvpx link statically, built once per slice.
missing=()
for triple in "${triples[@]}"; do
    [ -f "deps/codecs/$triple/lib/libvpx.a" ] || missing+=("$triple")
done
[ ${#missing[@]} -eq 0 ] || packaging/codecs/build.sh deps/codecs "${missing[@]}"
set -a
eval "$(packaging/codecs/env.sh deps/codecs)"
set +a

cargo build --locked -p throcc-mobile --release "${targets[@]}"

rm -rf "$bindings" "$generated" "$framework"
mkdir -p "$headers/throcc_mobileFFI" "$generated"
cargo run --locked -p throcc-mobile --features bindgen --bin uniffi-bindgen -- \
    generate --language swift --no-format --out-dir "$bindings" "${libraries[0]}"
mv "$bindings/throcc_mobile.swift" "$generated/"
mv "$bindings/throcc_mobileFFI.h" "$headers/throcc_mobileFFI/"
mv "$bindings/throcc_mobileFFI.modulemap" "$headers/throcc_mobileFFI/module.modulemap"

xcodebuild -create-xcframework "${xcframework[@]}" -output "$framework"

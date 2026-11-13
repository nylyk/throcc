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
    libraries+=("$library")
    xcframework+=(-library "$library" -headers "$headers")
done

cargo build --locked -p throcc-mobile --release "${targets[@]}"

rm -rf "$bindings" "$generated" "$framework"
mkdir -p "$headers/throcc_mobileFFI" "$generated"
cargo run --locked -p throcc-mobile --features bindgen --bin uniffi-bindgen -- \
    generate --language swift --no-format --out-dir "$bindings" "${libraries[0]}"
mv "$bindings/throcc_mobile.swift" "$generated/"
mv "$bindings/throcc_mobileFFI.h" "$headers/throcc_mobileFFI/"
mv "$bindings/throcc_mobileFFI.modulemap" "$headers/throcc_mobileFFI/module.modulemap"

xcodebuild -create-xcframework "${xcframework[@]}" -output "$framework"

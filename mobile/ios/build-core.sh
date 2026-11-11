#!/usr/bin/env bash
# Builds throcc-mobile for iOS devices and the simulator into ThroccCore.xcframework, and
# generates its Swift bindings into throcc/Generated.
set -euo pipefail
cd "$(dirname "$0")/../.."

export IPHONEOS_DEPLOYMENT_TARGET=26.0
device=target/aarch64-apple-ios/release/libthrocc_mobile.a
simulator=target/aarch64-apple-ios-sim/release/libthrocc_mobile.a
bindings=target/ios-bindings
headers=$bindings/headers
generated=mobile/ios/throcc/Generated
framework=mobile/ios/ThroccCore.xcframework

cargo build --locked -p throcc-mobile --release --target aarch64-apple-ios --target aarch64-apple-ios-sim

rm -rf "$bindings" "$generated" "$framework"
mkdir -p "$headers/throcc_mobileFFI" "$generated"
cargo run --locked -p throcc-mobile --features bindgen --bin uniffi-bindgen -- \
    generate --language swift --no-format --out-dir "$bindings" "$device"
mv "$bindings/throcc_mobile.swift" "$generated/"
mv "$bindings/throcc_mobileFFI.h" "$headers/throcc_mobileFFI/"
mv "$bindings/throcc_mobileFFI.modulemap" "$headers/throcc_mobileFFI/module.modulemap"

xcodebuild -create-xcframework \
    -library "$device" -headers "$headers" \
    -library "$simulator" -headers "$headers" \
    -output "$framework"

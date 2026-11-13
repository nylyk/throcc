#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
notices=packaging/licenses/client-macos-notices.txt
[ -f "$notices" ] || { echo "no $notices; run packaging/licenses/notices.sh client-macos first" >&2; exit 1; }
# dav1d and libvpx link statically; VideoToolbox does the rest.
codecs=${THROCC_CODECS:-deps/codecs}
target=$(rustc -vV | sed -n 's/^host: //p')
[ -f "$codecs/$target/lib/libvpx.a" ] || packaging/codecs/build.sh "$codecs" "$target"
set -a
eval "$(packaging/codecs/env.sh "$codecs")"
set +a
cargo build --release -p throcc-client

version=$(cargo metadata --no-deps --format-version 1 |
    python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "throcc-client"))')
out=target/macos
app="$out/throcc.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed "s/VERSION/$version/g" packaging/macos/Info.plist >"$app/Contents/Info.plist"
cp target/release/throcc-client "$app/Contents/MacOS/"
cp packaging/macos/throcc.icns LICENSE "$app/Contents/Resources/"
cp "$notices" "$app/Contents/Resources/THIRD-PARTY-NOTICES.txt"

# MACOS_SIGN_IDENTITY names a Developer ID Application identity in the keychain; without it the
# app is signed ad hoc. NOTARY_KEY_FILE (an App Store Connect API key), NOTARY_KEY_ID and
# NOTARY_ISSUER have the dmg notarized and stapled.
identity=${MACOS_SIGN_IDENTITY:--}
sign=(codesign --force --sign "$identity")
[ "$identity" = - ] || sign+=(--options runtime --timestamp)
"${sign[@]}" --entitlements packaging/macos/throcc.entitlements "$app"
codesign --verify --strict --deep "$app"
dmg="$out/throcc.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname throcc -srcfolder "$app" -format UDZO "$dmg"
[ "$identity" = - ] || codesign --force --timestamp --sign "$identity" "$dmg"

if [ -n "${NOTARY_KEY_FILE:-}" ]; then
    notary=(--key "$NOTARY_KEY_FILE" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
    submitted=$(xcrun notarytool submit "$dmg" "${notary[@]}" --wait --output-format json)
    status=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])' <<<"$submitted")
    if [ "$status" != Accepted ]; then
        id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$submitted")
        xcrun notarytool log "$id" "${notary[@]}" >&2
        echo "notarization ended $status" >&2
        exit 1
    fi
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
fi
echo "built $app and $dmg"

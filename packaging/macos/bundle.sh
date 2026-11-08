#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
ffmpeg=${FFMPEG_PREFIX:-deps/ffmpeg-lgpl}
[ -f "$ffmpeg/lib/pkgconfig/libavcodec.pc" ] || packaging/ffmpeg/build.sh "$ffmpeg"
ffmpeg=$(cd "$ffmpeg" && pwd)
FFMPEG_PKG_CONFIG_PATH="$ffmpeg/lib/pkgconfig" cargo build --release -p throcc-client --features ffmpeg

version=$(cargo metadata --no-deps --format-version 1 |
    python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "throcc-client"))')
out=target/macos
app="$out/throcc.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
sed "s/VERSION/$version/g" packaging/macos/Info.plist >"$app/Contents/Info.plist"
cp target/release/throcc-client "$app/Contents/MacOS/"
cp packaging/macos/throcc.icns "$app/Contents/Resources/"

# Copies every library outside the OS that `file` needs, and points it at the copy.
bundle() {
    local file=$1
    otool -L "$file" | tail -n +2 | awk '{print $1}' | while read -r lib; do
        case "$lib" in /usr/lib/* | /System/* | @*) continue ;; esac
        local name
        name=$(basename "$lib")
        if [ ! -f "$app/Contents/Frameworks/$name" ]; then
            cp "$lib" "$app/Contents/Frameworks/$name"
            chmod u+w "$app/Contents/Frameworks/$name"
            install_name_tool -id "@rpath/$name" "$app/Contents/Frameworks/$name"
            bundle "$app/Contents/Frameworks/$name"
        fi
        install_name_tool -change "$lib" "@rpath/$name" "$file"
    done
}
bundle "$app/Contents/MacOS/throcc-client"
install_name_tool -add_rpath "@executable_path/../Frameworks" "$app/Contents/MacOS/throcc-client"
for lib in "$app"/Contents/Frameworks/*.dylib; do
    install_name_tool -add_rpath "@loader_path" "$lib" 2>/dev/null || true
done

# MACOS_SIGN_IDENTITY names a Developer ID Application identity in the keychain; without it the
# app is signed ad hoc. NOTARY_KEY_FILE (an App Store Connect API key), NOTARY_KEY_ID and
# NOTARY_ISSUER have the dmg notarized and stapled.
identity=${MACOS_SIGN_IDENTITY:--}
sign=(codesign --force --sign "$identity")
# The hardened runtime loads only libraries of the app's own team, which ad-hoc signatures lack.
[ "$identity" = - ] || sign+=(--options runtime --timestamp)
"${sign[@]}" "$app"/Contents/Frameworks/*.dylib
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

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
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks"
sed "s/VERSION/$version/g" packaging/macos/Info.plist >"$app/Contents/Info.plist"
cp target/release/throcc-client "$app/Contents/MacOS/"

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

codesign --force --sign - "$app"/Contents/Frameworks/*.dylib
codesign --force --sign - "$app"
rm -f "$out/throcc.dmg"
hdiutil create -quiet -volname throcc -srcfolder "$app" -format UDZO "$out/throcc.dmg"
echo "built $app and $out/throcc.dmg"

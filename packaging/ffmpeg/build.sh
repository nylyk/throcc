#!/usr/bin/env bash
set -euo pipefail
version=8.1.3
sha256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
mkdir -p "$1"
prefix=$(cd "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -n "${FFMPEG_TARBALL:-}" ]; then
    cp "$FFMPEG_TARBALL" "$work/ffmpeg.tar.xz"
else
    curl -fsSL --retry 5 --retry-all-errors -o "$work/ffmpeg.tar.xz" "https://ffmpeg.org/releases/ffmpeg-$version.tar.xz"
fi
actual=$( (sha256sum 2>/dev/null || shasum -a 256) <"$work/ffmpeg.tar.xz" | cut -d' ' -f1)
[ "$actual" = "$sha256" ] || { echo "ffmpeg-$version.tar.xz has checksum $actual" >&2; exit 1; }
tar -xf "$work/ffmpeg.tar.xz" -C "$work"
for fix in "$(dirname "$0")"/patches/*.patch; do
    patch -s -p1 -d "$work/ffmpeg-$version" <"$fix"
done

flags=(
    --prefix="$prefix"
    --enable-shared --disable-static --enable-pic
    --disable-programs --disable-doc --disable-debug
    --disable-autodetect --disable-network
    --disable-filters --disable-indevs --disable-outdevs
    --disable-muxers --disable-demuxers --disable-protocols
)
have() { pkg-config --exists "$1" 2>/dev/null; }
have dav1d && flags+=(--enable-libdav1d)
have openh264 && flags+=(--enable-libopenh264)
case "$(uname -s)" in
    Darwin)
        flags+=(--enable-videotoolbox)
        ;;
    Linux)
        have ffnvcodec && flags+=(--enable-ffnvcodec --enable-nvenc)
        have libva && flags+=(--enable-vaapi)
        ;;
    MINGW* | MSYS*)
        flags+=(--toolchain=msvc --enable-d3d11va --enable-mediafoundation)
        have ffnvcodec && flags+=(--enable-ffnvcodec --enable-nvenc)
        ;;
esac

cd "$work/ffmpeg-$version"
./configure "${flags[@]}"
grep -q '^#define CONFIG_GPL 0' config.h
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
make install
echo "ffmpeg $version (LGPL) installed in $prefix"

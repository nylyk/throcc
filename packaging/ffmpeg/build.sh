#!/usr/bin/env bash
# Builds ffmpeg as LGPL shared libraries into the given prefix, with only what throcc uses.
set -euo pipefail
version=8.1.3
sha256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
mkdir -p "$1"
prefix=$(cd "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

curl -fsSL -o "$work/ffmpeg.tar.xz" "https://ffmpeg.org/releases/ffmpeg-$version.tar.xz"
actual=$( (sha256sum 2>/dev/null || shasum -a 256) <"$work/ffmpeg.tar.xz" | cut -d' ' -f1)
[ "$actual" = "$sha256" ] || { echo "ffmpeg-$version.tar.xz has checksum $actual" >&2; exit 1; }
tar -xf "$work/ffmpeg.tar.xz" -C "$work"

flags=(
    --prefix="$prefix"
    --enable-shared --disable-static --enable-pic
    --disable-programs --disable-doc --disable-debug
    --disable-autodetect --disable-network
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
        have vpl && flags+=(--enable-libvpl)
        [ -f /usr/include/AMF/core/Version.h ] && flags+=(--enable-amf)
        ;;
    MINGW* | MSYS*)
        flags+=(--toolchain=msvc --enable-d3d11va --enable-mediafoundation)
        have ffnvcodec && flags+=(--enable-ffnvcodec --enable-nvenc)
        have vpl && flags+=(--enable-libvpl)
        [ -n "${AMF_INCLUDE:-}" ] && flags+=(--enable-amf --extra-cflags="-I$AMF_INCLUDE")
        ;;
esac

cd "$work/ffmpeg-$version"
./configure "${flags[@]}"
grep -q '^#define CONFIG_GPL 0' config.h
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
make install
echo "ffmpeg $version (LGPL) installed in $prefix"

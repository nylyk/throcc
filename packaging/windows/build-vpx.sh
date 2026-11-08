#!/usr/bin/env bash
# Builds libvpx's VP9 encoder as a static MSVC library into deps/, for ffmpeg to link into
# avcodec. Runs under MSYS2 (for make) with the MSVC environment and nasm on PATH.
set -euo pipefail
cd "$(dirname "$0")/../.."
version=1.17.0
[ -d target/libvpx ] ||
    git clone --depth 1 --branch "v$version" https://chromium.googlesource.com/webm/libvpx target/libvpx
test "$(git -C target/libvpx rev-parse HEAD)" = 6df3ec34557879fff673706f4a1d9fbd0f3a6f0e
prefix=$(cygpath -m "$PWD/deps")

mkdir -p target/libvpx/build
cd target/libvpx/build
# ffmpeg's MSVC build links the static CRT, so libvpx does too.
../configure --target=x86_64-win64-vs17 --as=nasm --enable-static-msvcrt \
    --disable-vp8 --disable-vp9-decoder --disable-examples --disable-tools --disable-docs \
    --disable-unit-tests
make dist
MSBuild.exe vpx.vcxproj -nologo -m -p:Configuration=Release -p:Platform=x64

mkdir -p "$prefix/lib/pkgconfig" "$prefix/include/vpx"
cp x64/Release/vpxmt.lib "$prefix/lib/vpx.lib"
cp ../vpx/*.h "$prefix/include/vpx/"
cat >"$prefix/lib/pkgconfig/vpx.pc" <<EOF
prefix=$prefix
libdir=\${prefix}/lib
includedir=\${prefix}/include

Name: vpx
Description: WebM Project VPx codec implementation
Version: $version
Libs: -L\${libdir} -lvpx
Cflags: -I\${includedir}
EOF
echo "libvpx $version installed in $prefix"

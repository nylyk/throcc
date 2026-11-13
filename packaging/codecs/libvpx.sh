#!/usr/bin/env bash
# Builds libvpx's VP9 codec as a static library for one Rust target, into a prefix.
# Run through `build.sh`.
set -euo pipefail
version=1.17.0
commit=6df3ec34557879fff673706f4a1d9fbd0f3a6f0e
target=$1
mkdir -p "$2"
prefix=$(cd "$2" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git clone -q --depth 1 --branch "v$version" https://chromium.googlesource.com/webm/libvpx "$work/libvpx"
actual=$(git -C "$work/libvpx" rev-parse HEAD)
[ "$actual" = "$commit" ] || { echo "libvpx v$version is at $actual, not $commit" >&2; exit 1; }

flags=(
    --prefix="$prefix"
    --enable-static --disable-shared --enable-pic
    --disable-examples --disable-tools --disable-docs --disable-unit-tests
    --disable-vp8 --enable-vp9 --enable-realtime-only --enable-multithread
)
android_api=31
case "$target" in
    aarch64-apple-darwin) flags+=(--target=arm64-darwin20-gcc --extra-cflags=-mmacosx-version-min=14.0) ;;
    x86_64-apple-darwin) flags+=(--target=x86_64-darwin20-gcc --extra-cflags=-mmacosx-version-min=14.0) ;;
    aarch64-apple-ios) flags+=(--target=arm64-darwin-gcc --extra-cflags=-miphoneos-version-min=26.0) ;;
    aarch64-apple-ios-sim)
        # libvpx has no arm64 simulator target: plain C for the simulator, which only runs tests.
        sdk=$(xcrun --sdk iphonesimulator --show-sdk-path)
        export CC="clang -target arm64-apple-ios26.0-simulator -isysroot $sdk" AR=ar
        export CXX="clang++ -target arm64-apple-ios26.0-simulator -isysroot $sdk" LD="$CC"
        flags+=(--target=generic-gnu)
        ;;
    aarch64-linux-android | x86_64-linux-android)
        : "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"
        toolchain=$(echo "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin)
        triple=${target}${android_api}
        export CC="$toolchain/$triple-clang" CXX="$toolchain/$triple-clang++"
        export AR="$toolchain/llvm-ar" AS="$CC" LD="$CC" NM="$toolchain/llvm-nm"
        export STRIP="$toolchain/llvm-strip" RANLIB="$toolchain/llvm-ranlib"
        if [ "$target" = aarch64-linux-android ]; then
            flags+=(--target=arm64-android-gcc)
        else
            flags+=(--target=x86_64-android-gcc --as=nasm)
        fi
        ;;
    x86_64-unknown-linux-gnu) flags+=(--target=x86_64-linux-gcc) ;;
    *) echo "no libvpx build for $target" >&2; exit 1 ;;
esac

cd "$work/libvpx"
./configure "${flags[@]}"
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
make install
echo "libvpx $version for $target installed in $prefix"

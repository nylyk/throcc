#!/usr/bin/env bash
# Builds dav1d, 8-bit only, as a static library for one Rust target, into a prefix.
# Run through `build.sh`.
set -euo pipefail
version=1.5.4
commit=54706fc6bc0cdecab7e9593974a4039cc038fca7
target=$1
mkdir -p "$2"
prefix=$(cd "$2" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git clone -q --depth 1 --branch "$version" https://code.videolan.org/videolan/dav1d.git "$work/dav1d"
actual=$(git -C "$work/dav1d" rev-parse HEAD)
[ "$actual" = "$commit" ] || { echo "dav1d $version is at $actual, not $commit" >&2; exit 1; }

# A meson cross file: C compiler and archiver with their arguments, and the host.
cross() {
    local system=$1 cpu=$2 ar=$3
    shift 3
    local args="" arg
    for arg in "$@"; do args+="'$arg', "; done
    cat >"$work/cross.ini" <<EOF
[binaries]
c = [${args%, }]
ar = '$ar'

[host_machine]
system = '$system'
cpu_family = '$cpu'
cpu = '$cpu'
endian = 'little'
EOF
    echo "--cross-file=$work/cross.ini"
}

android_api=31
apple_sdk() { xcrun --sdk "$1" --show-sdk-path; }
case "$target" in
    aarch64-apple-darwin | x86_64-apple-darwin)
        arch=${target%%-*}
        [ "$arch" = aarch64 ] && clang_arch=arm64 || clang_arch=x86_64
        setup=("$(cross darwin "$arch" ar clang -arch "$clang_arch" -isysroot "$(apple_sdk macosx)" -mmacosx-version-min=14.0)")
        ;;
    aarch64-apple-ios)
        setup=("$(cross darwin aarch64 ar clang -arch arm64 -isysroot "$(apple_sdk iphoneos)" -miphoneos-version-min=26.0)")
        ;;
    aarch64-apple-ios-sim)
        setup=("$(cross darwin aarch64 ar clang -target arm64-apple-ios26.0-simulator -isysroot "$(apple_sdk iphonesimulator)")")
        ;;
    aarch64-linux-android | x86_64-linux-android)
        : "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"
        toolchain=$(echo "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin)
        setup=("$(cross android "${target%%-*}" "$toolchain/llvm-ar" "$toolchain/${target}${android_api}-clang")")
        ;;
    x86_64-unknown-linux-gnu) setup=() ;;
    *) echo "no dav1d build for $target" >&2; exit 1 ;;
esac

meson setup "$work/build" "$work/dav1d" "${setup[@]}" \
    --prefix="$prefix" --libdir=lib --buildtype=release \
    -Ddefault_library=static -Dbitdepths=8 -Denable_asm=true \
    -Denable_tools=false -Denable_tests=false -Denable_examples=false
ninja -C "$work/build" install
echo "dav1d $version for $target installed in $prefix"

#!/usr/bin/env bash
# Builds dav1d and libvpx from source as static libraries for each Rust target given, into
# <root>/<target>, for the platforms whose own codecs lack AV1 or VP9 (Apple, Android):
#   packaging/codecs/build.sh <root> <rust-target>...
# Then point cargo at them with `packaging/codecs/env.sh <root>`; throcc-video links each
# target's own. Needs git, meson, ninja and nasm; Android targets need ANDROID_NDK_HOME,
# Apple targets Xcode.
set -euo pipefail
root=$1
shift
here=$(dirname "$0")
for target in "$@"; do
    "$here/dav1d.sh" "$target" "$root/$target"
    "$here/libvpx.sh" "$target" "$root/$target"
done

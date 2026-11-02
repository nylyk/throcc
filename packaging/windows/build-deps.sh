#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
PATH="$(dirname "$(command -v cl)"):$PATH"
pip install --require-hashes -r packaging/windows/requirements.txt
[ -d target/dav1d ] || git clone --depth 1 --branch 1.5.4 https://github.com/videolan/dav1d.git target/dav1d
# Tags can move; the commit is what code.videolan.org's 1.5.4 tag points at.
test "$(git -C target/dav1d rev-parse HEAD)" = 54706fc6bc0cdecab7e9593974a4039cc038fca7
meson setup target/dav1d/build target/dav1d --prefix="$(pwd -W)/deps" --default-library=shared \
    --buildtype=release -Denable_tools=false -Denable_tests=false
ninja -C target/dav1d/build install

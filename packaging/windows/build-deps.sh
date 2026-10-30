#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
pip install meson==1.12.1 ninja==1.13.2
[ -d target/dav1d ] || git clone --depth 1 --branch 1.5.4 https://github.com/videolan/dav1d.git target/dav1d
meson setup target/dav1d/build target/dav1d --prefix="$(pwd -W)/deps" --default-library=shared \
    --buildtype=release -Denable_tools=false -Denable_tests=false
ninja -C target/dav1d/build install

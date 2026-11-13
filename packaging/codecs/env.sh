#!/usr/bin/env bash
# Prints the environment that has cargo link the codecs `build.sh` put in <root>:
#   set -a; eval "$(packaging/codecs/env.sh <root>)"; set +a
# Under GitHub Actions, append the output to "$GITHUB_ENV" instead.
#
# dav1d-sys and libvpx-native-sys are told to link the static libraries by name, and each
# target built in <root> gets its own library path, so one cargo run can build several.
set -euo pipefail
root=$(cd "$1" && pwd)
echo "SYSTEM_DEPS_DAV1D_NO_PKG_CONFIG=1"
echo "SYSTEM_DEPS_DAV1D_SEARCH_NATIVE=$root"
echo "SYSTEM_DEPS_DAV1D_LIB=dav1d"
echo "SYSTEM_DEPS_DAV1D_LINK=static"
echo "VPX_LIB_DIR=$root"
echo "VPX_VERSION=$(sed -n 's/^version=//p' "$(dirname "$0")/libvpx.sh")"
echo "VPX_STATIC=1"
for lib in "$root"/*/lib; do
    target=$(basename "$(dirname "$lib")")
    name=$(tr 'a-z-' 'A-Z_' <<<"$target")
    echo "CARGO_TARGET_${name}_RUSTFLAGS=-Lnative=$lib"
done

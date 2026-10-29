#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
docker build -q -f tests/screens/Dockerfile -t throcc-screens . >/dev/null
docker run --rm -v "$PWD/docs/screens:/out" throcc-screens "$@"

#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
docker build -q -f tests/netem/Dockerfile -t throcc-netem . >/dev/null
docker run --rm --privileged throcc-netem "$@"

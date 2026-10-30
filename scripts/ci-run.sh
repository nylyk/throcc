#!/usr/bin/env bash
set -o pipefail
"$@" 2>&1 | tee ci-run.log && exit 0
errors=$(grep -vE "^ *(Compiling|Checking|Downloaded|Downloading|Fresh|Running|Finished) " ci-run.log |
    grep -E "^error|error\[|error:|ERROR|panicked|FAILED|^---- |^failures|FAIL|check failed|skipp" -B2 -A8 | tail -150)
[ -n "$errors" ] || errors=$(tail -60 ci-run.log)
printf '%s\n' "$errors" |
    awk 'BEGIN { printf "::error title=%s::", "'"$1 $2"' failed" } { gsub(/%/, "%25"); gsub(/\r/, ""); printf "%s%%0A", $0 } END { print "" }'
exit 1

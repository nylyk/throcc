#!/usr/bin/env bash
set -o pipefail
"$@" 2>&1 | tee ci-run.log && exit 0
annotate() {
    head -c 3500 | awk -v title="$*" 'BEGIN { printf "::error title=%s::", title } { gsub(/%/, "%25"); gsub(/\r/, ""); printf "%s%%0A", $0 } END { print "" }'
}
grep -E "FAIL|panicked|Header path|said|ERROR|fatal|not found" -A3 ci-run.log | head -60 | annotate "$1 $2 summary"
errors=$(grep -vE "^ *(Compiling|Checking|Downloaded|Downloading|Fresh|Running|Finished) " ci-run.log |
    grep -E "^error|error\[|error:|ERROR|panicked|FAILED|^---- |^failures|FAIL|check failed|skipp" -B2 -A8 | head -80)
[ -n "$errors" ] || errors=$(tail -60 ci-run.log)
printf '%s\n' "$errors" | annotate "$1 $2 failed"
exit 1

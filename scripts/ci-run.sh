#!/usr/bin/env bash
# Runs a command and, if it fails, posts its error lines as one annotation, since job logs need a login.
set -o pipefail
"$@" 2>&1 | tee ci-run.log && exit 0
grep -vE "^ *(Compiling|Checking|Downloaded|Downloading|Fresh|Running|Finished) " ci-run.log |
    grep -E "^error|error\[|error:|panicked|FAILED|^---- |^failures|FAIL|check failed|skipp" -B2 -A8 | tail -150 |
    awk 'BEGIN { printf "::error title=%s::", "'"$1 $2"' failed" } { gsub(/%/, "%25"); gsub(/\r/, ""); printf "%s%%0A", $0 } END { print "" }'
exit 1

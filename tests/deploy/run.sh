#!/usr/bin/env bash
# Builds a static server, installs it with the systemd unit on a clean Debian 12 and checks it runs.
set -euo pipefail
cd "$(dirname "$0")/../.."
case "$(docker info --format '{{.Architecture}}')" in
    aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
    *) target=x86_64-unknown-linux-musl ;;
esac
work=$(mktemp -d)
trap 'docker rm -f throcc-deploy >/dev/null 2>&1 || true; rm -rf "$work"' EXIT

docker build -q -f tests/deploy/Dockerfile.static --build-arg TARGET="$target" -t throcc-static . >/dev/null
docker create --name throcc-static-copy throcc-static >/dev/null
docker cp throcc-static-copy:/throcc-server "$work/throcc-server"
docker rm throcc-static-copy >/dev/null
cp packaging/throcc-server.service tests/deploy/Dockerfile.debian "$work/"
docker build -q -f "$work/Dockerfile.debian" -t throcc-deploy "$work" >/dev/null

docker run -d --name throcc-deploy --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw throcc-deploy >/dev/null
for _ in $(seq 1 30); do
    state=$(docker exec throcc-deploy systemctl is-active throcc-server 2>/dev/null || true)
    [ "$state" = active ] && break
    sleep 1
done

fail=0
check() { if eval "$2"; then echo "  ok    $1"; else echo "  FAIL  $1"; fail=1; fi; }
check "the binary is static" "docker exec throcc-deploy sh -c 'ldd /usr/local/bin/throcc-server 2>&1 | grep -Eq \"not a dynamic|statically linked\"'"
check "the unit is active" "[ \"\$(docker exec throcc-deploy systemctl is-active throcc-server)\" = active ]"
check "it runs as a dynamic user, not root" "docker exec throcc-deploy sh -c 'pid=\$(systemctl show -p MainPID --value throcc-server); [ \$(stat -c %u /proc/\$pid) -ne 0 ]'"
check "it listens on UDP 8476" "docker exec throcc-deploy sh -c 'grep -qi \":211C \" /proc/net/udp6'"
check "it logs the first invite" "docker exec throcc-deploy journalctl -u throcc-server --no-pager | grep -q 'becomes admin'"
check "its key is in the state directory" "docker exec throcc-deploy test -s /var/lib/private/throcc/server_key"
docker exec throcc-deploy systemctl restart throcc-server
sleep 2
check "a restart keeps the identity" "[ \"\$(docker exec throcc-deploy journalctl -u throcc-server --no-pager | grep -o 'server fingerprint .*' | sort -u | wc -l)\" -eq 1 ]"
[ "$fail" = 0 ] && echo "all checks passed"
exit "$fail"

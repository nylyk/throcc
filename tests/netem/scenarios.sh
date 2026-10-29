#!/usr/bin/env bash
set -euo pipefail

SERVER=10.0.0.1:8476
DURATION_UNITS=600 # 10 s of video at 60 units/s
VIDEO_SIZE=20833   # 60 units/s of this is 10 Mbps
WORK=$(mktemp -d)
failures=0

ns_ip() {
    case $1 in srv) echo 10.0.0.1 ;; s) echo 10.0.0.2 ;; a) echo 10.0.0.3 ;; b) echo 10.0.0.4 ;; c) echo 10.0.0.5 ;; esac
}

setup() {
    ip link add br0 type bridge
    ip link set br0 up
    for ns in srv s a b c; do
        ip netns add "$ns"
        ip link add "v-$ns" type veth peer name eth0 netns "$ns"
        ip link set "v-$ns" master br0 up
        ip -n "$ns" addr add "$(ns_ip "$ns")/24" dev eth0
        ip -n "$ns" link set eth0 up
        ip -n "$ns" link set lo up
    done
}

teardown() {
    pkill throcc-server 2>/dev/null || true
    for ns in srv s a b c; do ip netns del "$ns" 2>/dev/null || true; done
    ip link del br0 2>/dev/null || true
}

clear_shaping() {
    for ns in s a b c; do
        tc qdisc del dev "v-$ns" root 2>/dev/null || true
        ip netns exec "$ns" tc qdisc del dev eth0 root 2>/dev/null || true
    done
}

# Shapes what `ns` receives (the bridge port's egress).
downlink() {
    local ns=$1
    shift
    tc qdisc add dev "v-$ns" root "$@"
}

# Shapes what `ns` sends.
uplink() {
    local ns=$1
    shift
    ip netns exec "$ns" tc qdisc add dev eth0 root "$@"
}

cli() {
    local ns=$1
    shift
    ip netns exec "$ns" throcc-cli --profile "$WORK/$ns" "$@"
}

start_server() {
    ip netns exec srv throcc-server --data "$WORK/srv" --listen "$SERVER" --congestion "${CONGESTION:-cubic}" >"$WORK/server.log" 2>&1 &
    for _ in $(seq 50); do
        grep -q "becomes admin" "$WORK/server.log" 2>/dev/null && break
        sleep 0.1
    done
    local code
    code=$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/server.log" | grep -o 'invite [0-9A-Z]\{3\}-[0-9A-Z]\{5\}' | cut -d' ' -f2)
    cli s enroll "$SERVER" "$code" >/dev/null
    cli s name "$SERVER" Sender >/dev/null
    cli s create-room "$SERVER" General >/dev/null
    for ns in a b c; do
        cli "$ns" enroll "$SERVER" "$(cli s invite "$SERVER" | awk '{print $2}')" >/dev/null
    done
}

# Runs one scenario: receivers a, b and c watch the sender's 10 Mbps share and hear its mic.
run_share() {
    local name=$1
    local receivers=()
    for ns in a b c; do
        cli "$ns" recv "$SERVER" General --from Sender --with-audio --count "$DURATION_UNITS" --idle 5 \
            >"$WORK/$name-$ns.out" 2>&1 &
        receivers+=($!)
    done
    sleep 2
    timeout 60 ip netns exec s throcc-cli --profile "$WORK/s" send-synthetic "$SERVER" General --size "$VIDEO_SIZE" --rate 60 --count "$DURATION_UNITS" \
        --with-audio >"$WORK/$name-s.out" 2>&1
    wait "${receivers[@]}"
    for ns in s a b c; do
        printf '%-28s %s %s\n' "$name" "$ns" "$(grep RESULT "$WORK/$name-$ns.out" || tail -1 "$WORK/$name-$ns.out")"
    done
}

field() {
    grep RESULT "$WORK/$1.out" | tr ' ' '\n' | grep "^$2=" | cut -d= -f2
}

check() {
    local description=$1
    shift
    if "$@"; then
        echo "  ok    $description"
    else
        echo "  FAIL  $description"
        failures=$((failures + 1))
    fi
}

eq() { [ "$1" = "$2" ]; }
# Keyframe requests leave the server at least 1 s apart; arrival can be a little closer.
spaced() { [ "$1" = none ] || [ "$1" -ge 900 ]; }
gt() { [ "$1" -gt "$2" ]; }

scenario_receiver_limited() {
    downlink b tbf rate 3mbit burst 32kbit latency 100ms
    run_share receiver_limited
    check "a sees no server drops" eq "$(field receiver_limited-a server_dropped_units)" 0
    check "c sees no server drops" eq "$(field receiver_limited-c server_dropped_units)" 0
    check "b sees server drops in LinkStats" gt "$(field receiver_limited-b server_dropped_units)" 0
    check "b gets only whole units" eq "$(field receiver_limited-b corrupt)" 0
    check "audio to b is never dropped by the server" eq "$(field receiver_limited-b audio_server_dropped)" 0
    check "sender gets at most 1 keyframe request per second" spaced "$(field receiver_limited-s min_keyframe_gap_ms)"
}

scenario_random_loss() {
    for ns in a b c; do downlink "$ns" netem loss 2%; done
    run_share random_loss
    for ns in a b c; do
        check "$ns gets only intact units" eq "$(field "random_loss-$ns" corrupt)" 0
    done
    check "sender gets at most 1 keyframe request per second" spaced "$(field random_loss-s min_keyframe_gap_ms)"
}

scenario_jitter() {
    for ns in a b c; do downlink "$ns" netem delay 40ms 30ms distribution normal; done
    run_share jitter
    for ns in a b c; do
        check "$ns gets only intact units" eq "$(field "jitter-$ns" corrupt)" 0
    done
}

scenario_uplink_jitter() {
    uplink s netem delay 40ms 30ms distribution normal
    run_share uplink_jitter
    for ns in a b c; do
        check "$ns gets only intact units" eq "$(field "uplink_jitter-$ns" corrupt)" 0
    done
}

scenario_sender_uplink() {
    uplink s tbf rate 5mbit burst 32kbit latency 100ms
    run_share sender_uplink
    check "the sender drops units locally" gt "$(field sender_uplink-s local_drops)" 0
    for ns in a b c; do
        check "$ns sees no server drops" eq "$(field "sender_uplink-$ns" server_dropped_units)" 0
    done
}

trap teardown EXIT
setup
start_server
scenarios=("$@")
[ ${#scenarios[@]} -eq 0 ] && scenarios=(receiver_limited random_loss jitter uplink_jitter sender_uplink)
for scenario in "${scenarios[@]}"; do
    clear_shaping
    echo "== $scenario"
    "scenario_$scenario"
done
[ "$failures" -eq 0 ] && echo "all checks passed" || echo "$failures checks failed"
exit "$failures"

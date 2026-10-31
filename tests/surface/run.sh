#!/usr/bin/env bash
# Opens the mock client's room under a headless sway on mesa's lavapipe, screenshots it with
# grim and checks the share tile shows VideoFrame::test_pattern through the NV12 surface.
# Needs sway, grim, python3 and lavapipe (Ubuntu: mesa-vulkan-drivers, Arch: vulkan-swrast).
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build -q -p throcc-client
out=target/surface
rm -rf "$out"
mkdir -p "$out"

icd=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
[ -n "$icd" ] || { echo "FAIL  no lavapipe ICD in /usr/share/vulkan/icd.d"; exit 1; }
export VK_ICD_FILENAMES=$icd
export XDG_RUNTIME_DIR=$PWD/$out/xdg WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
mkdir -m 700 "$XDG_RUNTIME_DIR"
printf 'output * resolution 1280x800\ndefault_border none\nxwayland disable\n' >"$out/sway.conf"
# A copy drops sway's file capabilities, which containers refuse.
cp "$(command -v sway)" "$out/sway-nocaps"
"$out/sway-nocaps" -c "$out/sway.conf" >"$out/sway.log" 2>&1 &
sway_pid=$!
client_pid=
trap 'kill $client_pid $sway_pid 2>/dev/null || true' EXIT
for _ in $(seq 50); do
    WAYLAND_DISPLAY=$(ls "$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+$' | head -1) && break
    sleep 0.2
done
[ -n "${WAYLAND_DISPLAY:-}" ] || { echo "FAIL  sway did not start"; tail -20 "$out/sway.log"; exit 1; }
export WAYLAND_DISPLAY

RUST_LOG=gpui_wgpu=info ./target/debug/throcc-client --mock --screen room >"$out/client.log" 2>&1 &
client_pid=$!
adapter=
for _ in $(seq 30); do
    sleep 2
    adapter=$(grep -o 'Selected GPU adapter: .*' "$out/client.log" | head -1 || true)
    [ -n "$adapter" ] && break
done
[ -n "$adapter" ] || { echo "FAIL  the client chose no GPU adapter"; tail -20 "$out/client.log"; exit 1; }
echo "  ok    $adapter"
grep -qi llvmpipe <<<"$adapter" || { echo "FAIL  the adapter is not lavapipe"; exit 1; }

for attempt in $(seq 10); do
    sleep 2
    grim -t ppm "$out/room.ppm"
    if python3 - "$out/room.ppm" <<'EOF'; then
import sys

data = open(sys.argv[1], "rb").read()
magic, size, maxval, pixels = data.split(b"\n", 3)
width, height = map(int, size.split())

def at(x, y):
    i = (y * width + x) * 3
    return tuple(pixels[i:i + 3])

def grey(p, value):
    return all(abs(c - value) <= 1 for c in p)

def fail(message):
    print(f"FAIL  {message}")
    sys.exit(1)

# The test pattern's frame-number stamp is a band of black blocks (Y 16) with the last block
# white (Y 235). Full-range BT.709 shows them as exactly (16,16,16) and (235,235,235).
band = {}
for y in range(height):
    run = start = 0
    for x in range(width):
        if grey(at(x, y), 16):
            run += 1
            if run >= 100 and y not in band:
                start = x - run + 1
                band[y] = start
        else:
            run = 0
if not band:
    fail("no (16,16,16) stamp band, so no test pattern on screen")
top, bottom = min(band), max(band)
left = min(band.values())
if bottom - top < 10:
    fail(f"the stamp band is only {bottom - top + 1} rows tall")
middle = (top + bottom) // 2
print(f"  ok    stamp band at x {left}, rows {top}..{bottom}")

start = band[top + 3]
row = [at(x, top + 3) for x in range(start, width)]
dark = next(i for i, p in enumerate(row) if not grey(p, 16))
if not grey(row[dark], 235) or not grey(row[dark + 5], 235):
    fail(f"the stamp's last block is {row[dark]}, not (235,235,235)")
print(f"  ok    the stamp's last block is (235,235,235) from x {start + dark}")

below = [at(x, y) for y in range(bottom + 4, bottom + 24) for x in range(left + 10, left + 210)]
tinted = [p for p in below if max(p) - min(p) > 2]
outside = [p for p in below if not 60 <= p[0] <= 195]
if tinted:
    fail(f"{len(tinted)} pattern pixels are tinted, e.g. {tinted[0]}")
if outside:
    fail(f"{len(outside)} pattern pixels leave the pattern's 64..191 greys, e.g. {outside[0]}")
print(f"  ok    {len(below)} pattern pixels are neutral greys within the pattern's range")

background = at(left - 4, middle)
corner = at(left, top)
if grey(corner, 16) or any(abs(a - b) > 2 for a, b in zip(corner, background)):
    fail(f"the picture's top-left corner is {corner}, not the tile background {background}")
inside = at(left + 12, top + 12)
if not grey(inside, 16):
    fail(f"inside the rounded corner is {inside}, not the picture")
print(f"  ok    the top-left corner is rounded off to the background {background}")
EOF
        echo "all checks passed"
        exit 0
    fi
    echo "  ..    attempt $attempt failed, retrying"
done
cp "$out/room.ppm" "$out/failed.ppm"
grep -v "^\[" "$out/client.log" | tail -20
exit 1

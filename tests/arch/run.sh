#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
out=${1:-}
mounts=(-v "$PWD":/src:ro)
[ -n "$out" ] && mkdir -p "$out" && mounts+=(-v "$(cd "$out" && pwd)":/out)
docker run --rm --platform linux/amd64 "${mounts[@]}" archlinux:latest bash -euo pipefail -c '
    sed -i "s/^\[options\]/[options]\nDisableSandbox/" /etc/pacman.conf
    sed -i "s|--retry 3|--retry 5 --retry-all-errors|" /etc/makepkg.conf
    pacman -Syu --noconfirm --needed base-devel git rustup >/dev/null
    useradd -m builder
    echo "builder ALL=(ALL) NOPASSWD: ALL" >/etc/sudoers.d/builder
    git -c safe.directory="*" clone -q /src /home/builder/throcc-src
    chown -R builder /home/builder/throcc-src
    su builder -c "
        set -euo pipefail
        cd /home/builder/throcc-src
        git config --global --add safe.directory \"*\"
        rustup default 1.98.1 >/dev/null
        mkdir -p ../pkg && cp packaging/arch/PKGBUILD ../pkg/
        cd ../pkg
        THROCC_GIT=\"file:///home/builder/throcc-src#commit=\$(git -C ../throcc-src rev-parse HEAD)\" makepkg -s --noconfirm || {
            echo \"FAIL  makepkg; the bindings build script said:\"
            cat src/throcc/target/release/build/rusty_ffmpeg-*/stderr | head -40
            echo \"FAIL  pkg-config said: \$(PKG_CONFIG_PATH=src/ffmpeg/lib/pkgconfig pkg-config --cflags libavformat)\"
            ls src/ffmpeg/include/libavformat
            exit 1
        }
    "
    pacman -U --noconfirm /home/builder/pkg/throcc-*.pkg.tar.zst >/dev/null
    missing=$(ldd /usr/bin/throcc /usr/bin/throcc-cli | grep "not found" || true)
    [ -z "$missing" ] || { echo "FAIL  unresolved libraries: $missing"; exit 1; }
    echo "  ok    every library resolves"
    ldd /usr/bin/throcc | grep -q "/usr/lib/throcc/libavcodec" || { echo "FAIL  the system ffmpeg is used"; exit 1; }
    echo "  ok    the private ffmpeg is used"
    if grep -aq -- "--enable-gpl" /usr/lib/throcc/libavcodec.so.*; then echo "FAIL  ffmpeg is GPL"; exit 1; fi
    echo "  ok    ffmpeg is LGPL"
    grep -aq -- "--enable-vaapi" /usr/lib/throcc/libavcodec.so.* || { echo "FAIL  ffmpeg has no VA-API"; exit 1; }
    echo "  ok    ffmpeg encodes through VA-API"
    grep -aq -- "--enable-libvpx" /usr/lib/throcc/libavcodec.so.* || { echo "FAIL  ffmpeg has no libvpx"; exit 1; }
    echo "  ok    ffmpeg has the VP9 software fallback"
    throcc-cli --help >/dev/null
    echo "  ok    throcc-cli runs"
    pacman -S --noconfirm --needed sway vulkan-swrast ttf-dejavu >/dev/null
    cp /usr/bin/sway /usr/local/bin/sway-nocaps
    su builder -c "
        export XDG_RUNTIME_DIR=/tmp/xdg-builder WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
        mkdir -p -m 700 \$XDG_RUNTIME_DIR
        sway-nocaps -c /dev/null >/tmp/sway.log 2>&1 &
        sleep 3
        export WAYLAND_DISPLAY=\$(ls \$XDG_RUNTIME_DIR | grep -E \"^wayland-[0-9]+\$\" | head -1)
        WAYLAND_DEBUG=1 timeout 15 throcc --mock >/tmp/throcc.log 2>&1 || true
    "
    if ! grep -Eq "xdg_toplevel(#[0-9]+)?\.set_title\(\"throcc\"\)" /tmp/throcc.log; then
        echo "FAIL  no Wayland window"
        grep -v "^\[" /tmp/throcc.log | tail -20
        tail -5 /tmp/sway.log
        exit 1
    fi
    echo "  ok    the app opens a Wayland window"
    if [ -d /out ]; then
        cp /home/builder/pkg/throcc-*.pkg.tar.zst /out/throcc-x86_64.pkg.tar.zst
        echo "  ok    package copied out"
    fi
    echo "all checks passed"
'

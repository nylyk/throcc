#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
docker run --rm --platform linux/amd64 -v "$PWD":/src:ro archlinux:latest bash -euo pipefail -c '
    pacman -Syu --noconfirm --needed base-devel git rustup >/dev/null
    useradd -m builder
    echo "builder ALL=(ALL) NOPASSWD: ALL" >/etc/sudoers.d/builder
    cp -r /src /home/builder/throcc-src
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
    throcc-cli --help >/dev/null
    echo "  ok    throcc-cli runs"
    echo "all checks passed"
'

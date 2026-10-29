# syntax=docker/dockerfile:1
FROM rust:1.98.1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends \
        libasound2-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
        libfontconfig-dev libfreetype-dev libx11-xcb-dev libxcb1-dev libxcb-randr0-dev libxcb-shm0-dev \
        libxcb-xfixes0-dev libdbus-1-dev libpipewire-0.3-dev libclang-dev libgbm-dev libegl-dev \
        libssl-dev libzstd-dev pkg-config cmake \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target-screens \
    CARGO_TARGET_DIR=/src/target-screens cargo build --release -p throcc-client \
    && cp /src/target-screens/release/throcc-client /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        xvfb x11-apps imagemagick mesa-vulkan-drivers libvulkan1 libxkbcommon0 libxkbcommon-x11-0 \
        libwayland-client0 libfontconfig1 libfreetype6 libx11-xcb1 libxcb1 libasound2 libdbus-1-3 \
        libpipewire-0.3-0 libgbm1 libegl1 fonts-inter fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/throcc-client /usr/local/bin/
COPY tests/screens/capture.sh /capture.sh
ENTRYPOINT ["/capture.sh"]

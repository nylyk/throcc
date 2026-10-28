# syntax=docker/dockerfile:1
FROM rust:1.98.1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p throcc-server -p throcc-cli \
    && cp target/release/throcc-server target/release/throcc-cli /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends iproute2 procps \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/throcc-server /usr/local/bin/throcc-cli /usr/local/bin/
COPY tests/netem/scenarios.sh /netem/scenarios.sh
ENTRYPOINT ["/netem/scenarios.sh"]

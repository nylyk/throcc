# syntax=docker/dockerfile:1
FROM rust:1.98.1-bookworm AS chef
RUN cargo install cargo-chef --locked --version 0.1.78 \
    && cargo install cargo-about --locked --version 0.9.3 --features cli
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json --bin throcc-server

FROM chef AS build
COPY --from=planner /src/recipe.json recipe.json
COPY third_party third_party
RUN cargo chef cook --release --recipe-path recipe.json -p throcc-server
COPY . .
RUN packaging/licenses/notices.sh server \
    && cargo build --release --locked -p throcc-server \
    && mkdir -p /data \
    && chown 65532:65532 /data \
    && chmod 0700 /data

FROM debian:bookworm-slim AS init
RUN apt-get update && apt-get install -y --no-install-recommends tini

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=init /usr/bin/tini-static /tini
COPY --from=build /src/target/release/throcc-server /throcc-server
COPY --from=build /src/LICENSE /licenses/LICENSE
COPY --from=build /src/packaging/licenses/server-notices.txt /licenses/THIRD-PARTY-NOTICES.txt
COPY --from=build --chown=65532:65532 /data /data
VOLUME /data
EXPOSE 8476/udp
ENTRYPOINT ["/tini", "--", "/throcc-server", "--data", "/data", "--listen", "[::]:8476"]

# syntax=docker/dockerfile:1
FROM rust:1.98.1-bookworm AS chef
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json --bin throcc-server

FROM chef AS build
COPY --from=planner /src/recipe.json recipe.json
COPY third_party third_party
RUN cargo chef cook --release --recipe-path recipe.json -p throcc-server
COPY . .
RUN cargo build --release -p throcc-server \
    && mkdir -p /data \
    && chown 65532:65532 /data

FROM chef AS dev
RUN cargo install watchexec-cli --locked --version 2.8.0
ENV CARGO_TARGET_DIR=/target
VOLUME /src /target /data
EXPOSE 8476/udp
CMD ["watchexec", "--restart", "--exts", "rs,toml", "--", \
     "cargo", "run", "-p", "throcc-server", "--", "--data", "/data", "--listen", "[::]:8476"]

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=build /src/target/release/throcc-server /throcc-server
COPY --from=build --chown=65532:65532 /data /data
VOLUME /data
EXPOSE 8476/udp
ENTRYPOINT ["/throcc-server", "--data", "/data", "--listen", "[::]:8476"]

FROM lukemathwalker/cargo-chef:latest-rust-1.97-slim-bookworm AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS dev
RUN cargo install cargo-watch --locked
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --recipe-path recipe.json -p throcc-server
CMD ["cargo", "watch", "-w", "crates", "-x", "run -p throcc-server"]

FROM chef AS build
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p throcc-server
COPY . .
RUN cargo build --release -p throcc-server
RUN mkdir /data

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=build --chown=nonroot:nonroot /data /data
COPY --from=build /app/target/release/throcc-server /usr/local/bin/throcc-server
CMD ["/usr/local/bin/throcc-server"]

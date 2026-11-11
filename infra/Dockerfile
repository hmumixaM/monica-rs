FROM rust:1.92-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release --locked -p monica-server
FROM debian:bookworm-slim
RUN useradd --create-home --uid 10001 monica
COPY --from=build /src/target/release/monica-server /usr/local/bin/monica-server
USER monica
ENTRYPOINT ["monica-server"]
CMD ["serve", "--listen", "0.0.0.0:17494"]

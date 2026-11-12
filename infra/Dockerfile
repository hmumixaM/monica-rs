FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY apps/web/package.json apps/web/package-lock.json ./
RUN npm ci
COPY apps/web ./
RUN npm run build

FROM rust:1.92-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release --locked -p monica-server
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 monica \
    && mkdir -p /data/media /data/collectors && chown -R monica:monica /data
COPY --from=build /src/target/release/monica-server /usr/local/bin/monica-server
COPY --from=web /web/dist /opt/monica/web
ENV MONICA_WEB_ROOT=/opt/monica/web MONICA_MEDIA_ROOT=/data/media
USER monica
EXPOSE 17494
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=3s --start-period=20s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:17494/api/v1/ready || exit 1
ENTRYPOINT ["monica-server"]
CMD ["serve", "--listen", "0.0.0.0:17494"]

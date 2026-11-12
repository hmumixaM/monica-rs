FROM golang:1.26-bookworm AS build
WORKDIR /src
COPY services/whatsapp/go.mod services/whatsapp/go.sum ./
RUN go mod download
COPY services/whatsapp ./
RUN CGO_ENABLED=1 go build -trimpath -o /monica-whatsapp .
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 monica \
    && mkdir -p /data/media && chown -R monica:monica /data
COPY --from=build /monica-whatsapp /usr/local/bin/monica-whatsapp
COPY infra/whatsapp-entrypoint.sh /usr/local/bin/whatsapp-entrypoint
USER monica
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/whatsapp-entrypoint"]

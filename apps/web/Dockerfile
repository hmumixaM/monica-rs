FROM node:22-alpine AS builder
WORKDIR /web
COPY apps/web/package*.json ./
RUN npm ci
COPY apps/web ./
RUN npm run build
# Static files are copied into monica-server's runtime image; no Node runtime.
FROM scratch
COPY --from=builder /web/dist /web

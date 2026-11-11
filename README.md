# Monica Rust

Independent Rust implementation for macOS and Linux/NAS. **Development foundation, not a complete Monica replacement.** No production data, synchronization or migration is included. External collectors and model calls are disabled by absence of runtime wiring.

## Implemented

- Cargo workspace: shared Rust core and Axum CLI/server.
- PostgreSQL 16: one database, independent crm/wechat/instagram/linkedin schemas.
- Atomic source observations/revisions and durable job insertion; object-scoped row locks.
- Ordered per-object projection, SKIP LOCKED claims, heartbeat, expiring lease tokens and fenced atomic result/ack.
- Synthetic fixtures and isolated PostgreSQL integration tests.
- Existing React frontend and external collectors copied from pinned tracked sources with SHA-256 provenance in docs/sources.json.
- 143 legacy route declarations inventoried in docs/api-coverage.json; all business endpoints remain pending.

## Local validation

Run `sh scripts/test.sh` from the repository. It creates a temporary, Unix-socket-only PostgreSQL cluster, runs the tests, and removes only that temporary cluster. Requires Rust and PostgreSQL 16 on the development machine. Set MONICA_PG_BIN to override the PostgreSQL binary directory.

Set MONICA_DATABASE_URL to a **new, dedicated test database**, then run:

```sh
cargo run -p monica-server -- migrate
python3 scripts/generate-fixtures.py --records 1000 > /tmp/monica-rs-fixtures.json
cargo run -p monica-server -- ingest /tmp/monica-rs-fixtures.json
cargo run -p monica-server -- drain
cargo run -p monica-server -- serve
```

Server binds 127.0.0.1:17494 and currently exposes only health/readiness. Ingestion is a local development CLI, not a replacement for the signed Import API. The record hash/UUID format is provisional and must pass legacy golden vectors before compatibility is claimed.

## Remaining delivery gates

1. Freeze executable OpenAPI and differential fixtures; complete legacy identity/hash compatibility.
2. Full business schema, auth/TOTP/CSRF/HMAC and the 143-route API, media storage and worker lifecycle.
3. CRM projection rules, account relinking locks, source checkpoints, AI/media scheduling and adapter integration.
4. React Router/Vite migration and isolated Tauri tray application with managed PostgreSQL.
5. Multi-architecture images, Mac app bundle, full UI/failure tests and three-round comparative benchmarks.

Copied frontend/collectors are source inputs and are not yet integrated. No Mac app, production image, full API parity or performance improvement is claimed by this initial commit. Current synthetic projection is intentionally separate from the future CRM domain projection. Existing applications and NAS services have not been modified.

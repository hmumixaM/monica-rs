# Monica Rust

Independent Rust implementation for macOS and Linux/NAS. **Development foundation, not a complete Monica replacement.** No production data, synchronization or migration is included. External collectors and model calls are disabled by absence of runtime wiring.

## Implemented

- Cargo workspace: shared Rust core and Axum CLI/server.
- PostgreSQL 16: one database, independent crm/wechat/instagram/linkedin schemas.
- Atomic source observations/revisions and durable job insertion; object-scoped row locks.
- Ordered per-object projection, SKIP LOCKED claims, heartbeat, expiring lease tokens and fenced atomic result/ack.
- Synthetic fixtures and isolated PostgreSQL integration tests.
- Existing React frontend and external collectors copied from pinned tracked sources with SHA-256 provenance in docs/sources.json.
- 143 legacy route declarations inventoried in docs/api-coverage.json; auth routes have HTTP smoke coverage, full differential parity remains pending.
- Bounded immutable message append (1,000 records/4 MiB), streaming JSONL ingestion, generation-coalesced conversation summaries and separate active/history jobs.
- Auth HTTP routes: bootstrap, Argon2 login, database sessions, CSRF, encrypted TOTP, one-use recovery codes, revoke/logout and audit. Password work has a bounded blocking budget; Host/Origin checks protect the browser boundary.

## Local validation

Run `sh scripts/test.sh` from the repository. It creates a temporary, Unix-socket-only PostgreSQL cluster, runs the tests, and removes only that temporary cluster. Requires Rust and PostgreSQL 16 on the development machine. Set MONICA_PG_BIN to override the PostgreSQL binary directory.

Set MONICA_DATABASE_URL to a **new, dedicated test database**, then run:

```sh
cargo run -p monica-server -- migrate
python3 scripts/generate-fixtures.py --records 1000 > /tmp/monica-rs-fixtures.json
cargo run -p monica-server -- ingest /tmp/monica-rs-fixtures.json
cargo run -p monica-server -- drain
# Set MONICA_SESSION_SECRET to a new random secret (at least 32 bytes).
cargo run -p monica-server -- serve
```

Server binds 127.0.0.1:17494 and exposes health/readiness, authentication, manual profile/tag operations and multipart media upload/download/profile attachments. Set MONICA_ORIGIN to the browser origin when using a proxy; session cookies are Secure for HTTPS origins. Ingestion is a local development CLI, not a replacement for the signed Import API. The record hash/UUID format is provisional and must pass legacy golden vectors before compatibility is claimed.

## Remaining delivery gates

1. Freeze executable OpenAPI and differential fixtures; complete legacy identity/hash compatibility.
2. Full business schema, HMAC import, remaining API routes, media storage and worker lifecycle; differential auth parity.
3. CRM projection rules, account relinking locks, source checkpoints, AI/media scheduling and adapter integration.
4. React Router/Vite migration and isolated Tauri tray application with managed PostgreSQL.
5. Multi-architecture images, Mac app bundle, full UI/failure tests and three-round comparative benchmarks.

Copied frontend/collectors are source inputs and are not yet integrated. No Mac app, production image, full API parity or performance improvement is claimed by this initial commit. Current synthetic projection is intentionally separate from the future CRM domain projection. Existing applications and NAS services have not been modified.

## Data-path validation

See docs/DATA_PATHS.md for the immutable/mutable split, coalescing protocol and remaining architectural work. Run `MONICA_BENCH=1 sh scripts/test.sh` for the isolated 10x-scale microbenchmark. The streaming append command is `monica-server append-messages wechat messages.jsonl`; `drain` publishes both object projections and conversation summaries. Dedicated message correction/deletion, full CRM API and a production scheduler are still pending.

Set MONICA_MEDIA_ROOT to an independent test-media directory. The Compose test instance mounts its own named media volume. Multipart uploads stream and deduplicate by SHA-256; downloads authenticate before streaming and support Range/ETag. Complete profile detail, structured filters, source linkage, resumable upload and the rest of the API are still unfinished; see the per-route inventory rather than assuming parity from a route's presence.

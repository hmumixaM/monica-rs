# Monica Rust

Rust backend and React application shared by a native macOS app and Linux/NAS containers. This repository targets isolated testing against generated data. It does not migrate personal data, synchronize Mac and NAS, or replace an existing production service.

The backend uses Tokio, Axum, SQLx and PostgreSQL 16. One database contains CRM and seven independent source schemas: WeChat, Instagram, LinkedIn, WhatsApp, Apple Messages, Apple Contacts and Mail. Native notebook imports remain CRM facts. The **Sources** page is available in both desktop navigation and the mobile bottom bar; Mail has explicit Gmail, Outlook and IMAP connection entries.

## Runtime and data paths

- Immutable messages use bounded bulk append, stable identities and a separate correction path. Mutable facts retain ordered object revisions, including A→B→A. Account and object locks coordinate changes locally.
- Source persistence and derived projection use separate short transactions. Durable queues, expiring leases, heartbeats and commit-time fencing support recovery. Derived work coalesces by generation; unrelated objects can proceed concurrently.
- Page-specific summaries, typed indexes, keyset pagination, scoped cache versions and single-flight rebuilds keep ordinary reads separate from full history. Original JSON and content-addressed media remain available for evidence and rebuilding.
- Interactive and background work have separate connection and resource budgets. Background network and memory reservations leave capacity for interactive work. CPU-heavy and external work uses bounded executors or managed processes.
- Authentication includes sessions, CSRF, TOTP and single-use recovery codes. Signed imports verify the original request bytes and persist replay protection. Installation secrets encrypt source credentials.
- Mail supports Gmail history, Graph delta and read-only IMAP; Apple sources use the native bridge. Browser collectors, Chatlog and the WhatsApp Go bridge are managed adapters. Real sources and model calls require explicit configuration and enabling.
- The Vite/React Router frontend is served by Rust with `/api` on the same origin. The native shell manages PostgreSQL, the core and optional adapters under a separate Monica Rust identity and data directory.

See [multi-source scope and evidence](docs/MULTISOURCE_PARITY.md), [implementation ledger](docs/IMPLEMENTATION.md), [frozen contracts](docs/contracts/README.md) and [optional WAL history](docs/CDC.md). The registration audit currently finds all 166 frozen operations; registration alone does not prove behavioral parity. Differential, PostgreSQL, HTTP, browser and native lifecycle tests provide separate evidence.

The [2026-11-12 review and parity record](docs/reviews/2026-11-12-rust-parity-review.md) records 13 repaired findings, platform capabilities, validation evidence and remaining acceptance limits.

## Model and platform settings

Both Mac and NAS use **Settings → Model settings** to configure an OpenAI-compatible service, model, encrypted API key, browser QA and the separate background chat/note permissions. These settings start disabled and persist across Mac restarts. Saving configuration does not probe a paid model endpoint. Explicit `MONICA_AI_*` environment settings can override the saved configuration; unset/empty Compose overrides preserve the application's saved choices.

Sources reports whether the native Mac reader is present. Linux/NAS disables Apple Contacts/Messages collection controls while keeping stored evidence readable. WhatsApp pause preserves device pairing; disconnect clears its local pairing keys while retaining imported history.

The optional `wechat` Compose profile reads only explicitly published standalone Chatlog SQLite snapshots. Set `MONICA_CHATLOG_SNAPSHOT_ROOT` to an existing absolute export directory, `MONICA_CHATLOG_ACCOUNT` to its independent source anchor and, when known, `MONICA_CHATLOG_SELF_ID` to the owning account identity. The mount is read-only and does not create a missing source directory. Configure the same independent `MONICA_CONTROL_TOKEN` for the server and collector, then explicitly enable the WeChat adapter through the authenticated import controls. Media references, locations, shared links, interactions and exact original XML are retained; the presence of a remote URL does not prove that its original bytes have been archived locally.

Optional collector containers use CPU 0 by default. Set `MONICA_COLLECTOR_CPUSET` to another valid CPU set when needed. This preserves a CPU bound on Synology kernels that support cpusets but cannot apply CFS CPU quotas; the existing 768 MiB memory limit remains in place.

## Local isolated validation

```sh
sh scripts/test.sh
# Focused tests also create and dispose of their own PostgreSQL cluster:
python3 scripts/test-cargo.py -p monica-core --test connections
```

These commands need Rust and PostgreSQL 16. `MONICA_PG_BIN` overrides the PostgreSQL binary directory. Test clusters listen only on a unique Unix socket and contain synthetic data.

To run manually, set `MONICA_DATABASE_URL` to a new dedicated database, `MONICA_SESSION_SECRET` to a new secret of at least 32 bytes, and `MONICA_MEDIA_ROOT` to an independent directory:

```sh
cargo run -p monica-server -- migrate
cargo run -p monica-server -- serve
```

Explicit schema migration has a separate single-connection budget. Its per-statement deadline defaults to 1,800 seconds; use `migrate --statement-timeout-seconds 3600` or `MONICA_MIGRATION_STATEMENT_TIMEOUT_SECONDS=3600` for slower NAS initialization. Valid values are 1–86,400 seconds. Request/background/control query limits remain separate. Compose forwards this optional setting and waits for the final PostgreSQL TCP listener with a bounded cold-start grace.

The default address is `127.0.0.1:17494`. Set `MONICA_WEB_ROOT` to the built `apps/web/dist` directory to serve the UI. Set `MONICA_ORIGIN` to the browser origin if using a proxy. Health and readiness are `/health` and `/api/v1/ready`. `MONICA_START_PAUSED=true` starts background work paused. The control API additionally requires an explicit bearer token and is intended for the native supervisor.

## Packaging and acceptance

The [native application README](apps/native-desktop/README.md) describes the macOS bundle and its managed dependencies. [Compose](infra/compose.yaml) uses separate test database and media volumes, with optional WhatsApp support. Multi-architecture artifact builds target Linux amd64 and arm64. Publishing workflows produce commit-tagged test images.

The [2026-11-13 platform acceptance](docs/reviews/2026-11-13-platform-acceptance.md) records refreshed complete Mac and two-architecture Linux artifacts, actual NAS storage/crash/restart checks, the completed Mac three-round comparison and explicitly canceled NAS matrix/performance gates. The [validation manifest](docs/reviews/2026-11-13-platform-acceptance-validation.json) separates observed successes, retained failures, source/artifact identities and performance targets that were not demonstrated. Existing engineering measurements are not a claim that the requested high-performance thresholds have been reached. The benchmark compares fixed Python main, desktop, multi-source and release Rust using signed synthetic imports, concurrency-eight reads and 1×/10× histories. It reports SQL and WAL per thousand messages as well as latency, throughput and process resources. Runtime, database layout and query changes must be evaluated separately.

Public signing/distribution, legacy data migration and production switching are outside this test delivery. Original worktrees and their private configuration remain untouched. Source provenance is recorded in [docs/sources.json](docs/sources.json).

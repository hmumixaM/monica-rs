# Implementation ledger

Accepted design: native Rust core shared by a Tauri menu-bar/browser application and Linux containers; PostgreSQL 16 with crm/wechat/instagram/linkedin schemas; reuse React UI and managed collector/Chatlog/face adapters. Initial work is isolated testing only. No existing data ingestion, synchronization, NAS deployment or production cutover is authorized in this implementation stage.

## Completed foundation

- Pinned CRM 6d59cfef4b1a12f678d809a00e37d8332be70b06 and Desktop e35b49e4ca33406f0fdd7ecf8767931b44fdac87.
- 294 tracked source files copied, with per-file SHA-256 provenance; 143 API declarations inventoried.
- Rust core/server, new-schema migrations, local ingest/drain CLI, health/readiness.
- Transactional source record/job insertion; immutable observations and revisions; unchanged-content deduplication.
- Per-object ordered jobs, concurrent independent jobs, leases/heartbeat, stale-token fencing and atomic projection/ack.
- PostgreSQL integration tests and a fresh-cluster harness, including message batching and auth HTTP tests; see DATA_PATHS.md and validation commands.
- Dockerfile, isolated Compose configuration, CI test workflow. Docker daemon unavailable locally; image execution not yet verified.

## Next implementation order

1. Freeze executable legacy OpenAPI and identity/hash golden vectors. Source route inventory alone is not a compatibility proof.
2. Implement complete domain schema and authentication, including bootstrap/login/logout, Argon2, session token hashes, CSRF, TOTP/recovery, and raw-body HMAC with persistent nonce replay protection.
3. Port read APIs and profile writes using dedicated interactive/background pool budgets; test through existing UI. Implement durable media files/Range and signed import API.
4. Replace generic test projection with real CRM projection, explicit account/profile lock protocol, manual precedence, group/relink/retirement behavior, durable snapshot cursors and schema-qualified queries.
5. Run scheduler with cooperative cancellation, graceful shutdown, separate DB/AI/network/media limits, NAS I/O=1 and Mac I/O=2; wire managed adapters using fake services first.
6. Complete all 143 API contracts and features: contacts, timelines, messages/search, maps, linking, stats, AI, OCR and faces. Update coverage only after behavioral parity tests.
7. Migrate Next route shell to React Router/Vite, preserve routes/service worker/search/IndexedDB; serve assets through Rust.
8. Tauri tray/settings and managed PostgreSQL under a separate Monica Rust app identifier/data directory; test pause/resume, sleep/wake, startup, crashes, port conflicts and exit.
9. Mac Apple Silicon/macOS 26+ bundle and linux/amd64 + linux/arm64 images; frontend and platform acceptance.
10. Three-round Python main / optimized desktop / Rust benchmarks on identical synthetic data and hardware. Target concurrent-8 interactive p95 <500ms, RSS -30%, import throughput or lock-wait p95 improvement >=30%, no other major p95/p99 regression >10%. Report app/SQL/schema changes separately. No performance claim before measured results.

## Important limits

- Full CRM API/semantics, GUI, adapter lifecycle and Python/NAS benchmark comparison remain pending. Authentication operations have local HTTP regression coverage, with full legacy differential parity pending.
- Current object UUID and JSON hash scheme is provisional; do not load old data or claim legacy compatibility.
- Current projection table is a testable durable storage vertical slice, not the final CRM data model.
- Public distribution signing, old-data migration, two-way synchronization and production rollout remain outside this test stage.
- Preserve original checkouts and never copy their .env, archive/session data, build directories or uncommitted branches into this repository.

## 2026-11-11 follow-up

Implemented the first performance redesign: separate bounded immutable message path, typed compact conversation state/summaries, generation-coalesced work with stale lease fencing, active/history separation for mutable jobs, and removal of the global sequence watermark. Added adaptive streaming batches and three-round SQL/WAL/10x synthetic scaling measurements. Added real authentication HTTP routes and regressions for TOTP, consumed recovery codes, session revocation, Origin/Host/CSRF and login throttling.

The user's full-backend objective remains unfinished. Do not describe these changes as complete backend delivery. DATA_PATHS.md explicitly separates implemented behavior from correction paths, scheduler/resource budgets, media/CRM APIs and cache work still needed.

Manual profile operations now have authenticated HTTP smoke tests: creation/listing, field editing/history/restore, tags and memberships. Full profile details, source provenance/manual-priority integration and structured employment/school filters remain incomplete (unsupported filters return 501 instead of misleading unfiltered results). SQLx migration rebuild tracking now watches the migrations directory, including newly added files.

Media multipart upload and authenticated download now use streamed files, SHA-256 deduplication, atomic no-overwrite blob publication, fsync before DB registration, bounded upload I/O (Mac 2/Linux 1), range responses, ETag/304, conservative attachment disposition, and profile media links. HTTP regressions cover repeated upload identity, bytes/Range/304 and attach/detach. Full format decoding/dimensions, crash-recovery/GC, resumable uploads and adapter media pipelines remain pending.

Validation: 13 PostgreSQL regressions plus one batch-budget unit test; actual HTTP smoke exercises authentication, manual profiles/tags, media, and graceful exit. A 2,500-message streaming input replays with zero new rows. Three-round synthetic SQL/WAL scaling evidence is checked in separately. No full backend completion or overall performance target is claimed.

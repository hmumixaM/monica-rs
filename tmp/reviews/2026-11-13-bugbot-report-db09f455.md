# Bugbot Report

## Report Contract

- Report type: `bugbot`
- Report ID: `bb-20261113-db09f455`
- Review date: `2026-11-13` (America/Chicago)
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-13-bugbot-report-db09f455.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: `Fixed review input; do not rewrite during repair`

## Scope

- Diff mode: `branch changes`
- Requested base: actual PR #4 `main` at `027eaa2e1c113d4345ca82711215cbed4281be45`
- Resolved baseline: `027eaa2e1c113d4345ca82711215cbed4281be45` (verified merge-base)
- Reviewed target: PR head `d1e08cfcf0a3aa1ccef5574f51a309e6e2daa061` / `codex/nas-isolated-migration`; tracked working tree clean; no untracked non-ignored production files.
- Changed files: `75`
- Completion: `Complete within reviewed scope`
- Custom instructions: fresh independent context; Bugbot/code-review/excessive-tests review only; persist initial report before reading prior reports; no source or Git mutation, PR comment, SSH, or existing business database access. Tests only create disposable resources. No canvas or Cursor skills.
- Assumptions: Code review does not certify ongoing personal-data import, real NAS deployment/media completeness, upstream collector recovery, or performance acceptance. Existing compiled Rust finalizer may be reused only after checksum validation.

## Detection Snapshot

- Recommendation: `Changes requested`
- Completion: `Complete within reviewed scope`
- Why now: the normal Chatlog apply path can reuse another target's completed scan receipts and report a partially missing target as converted.
- Findings count: `high 0 | medium 1 | low 0`
- Must-review now: `B1`
- Coverage confidence: `medium`
- Biggest blind spot: real NAS data/media and deployed behavior intentionally not accessed; full-dataset acceptance remains separate.

## Complete Findings Index

| ID | Severity | Category | Location | Title |
| --- | --- | --- | --- | --- |
| B1 | medium | LOGIC_BUG | [scripts/legacy-chatlog-import.py:423-431](/Users/maxhu/play/monica-rs/scripts/legacy-chatlog-import.py:423) | Bind Chatlog scan receipts to target |

## Findings

### B1 medium - Bind Chatlog scan receipts to target

- Category: `LOGIC_BUG`
- Location: [scripts/legacy-chatlog-import.py:423-431](/Users/maxhu/play/monica-rs/scripts/legacy-chatlog-import.py:423)
- Confirmation selector: `B1`

Description: Normal `apply` reopens the caller's state directory without binding its Reader cursor and ACK files to the actual destination database. After a successful import to target A, reusing the same frozen snapshot and `--state` while preparing a second fresh dedicated target B causes Reader to skip all done files (or accept A's ACK for a pending page). B receives no Chatlog messages from those pages, yet archive coverage is counted from A's local `known` table and B is marked `converted` with complete business tables. This affects a rebuild/restore or a second isolated target using the same migration workspace. A later independent `verify` detects missing content, but `apply` already reports a false successful conversion and cannot repair the omission on ordinary resume.

Rationale: This PR introduces the offline Chatlog importer and assigns the normal apply Reader directly to the persistent `state`; the target snapshot ID is deterministic only from the source manifest, so it is identical in A and B. The new target guard validates a dedicated finalized migration, but does not establish target ownership of that local cursor. A state directory is a writable checkpoint, not proof that another target committed its pages.

Evidence:

- `legacy-chatlog-import.py:420-431` creates the target Importer, then reuses `state` for apply. No target database identity is persisted or compared before opening Reader.
- `collector-chatlog.py:137-156` binds the state folder to account/self identity and scan mode only. `pending()` at 175-182 accepts any nonempty receipt ID matching the page SHA; `discover()` at 190-191 skips done files when their source stamp matches.
- `archive_tables()` at 94-96 trusts `scan.count_records()` for business table coverage and writes processed=expected/status=complete; the target summary verifier checks internal summary consistency rather than comparing every source message.
- `legacy-chatlog-import.py:447` marks the target snapshot converted after those checks.
- A disposable PG reproduction created four synthetic source DBs and two independent new `monica_rust*` targets, finalized both from the same source manifest, imported a snapshot with one additional message to A, then ran apply to B with the exact same state. Output: `first_target_chatlog_messages=1`, `fresh_target_chatlog_messages=0`, `fresh_snapshot_status="converted"`, `fresh_business_catalog=[["business",1,1,"complete"]]`. No existing business database was accessed.

Suggested repair: bind apply/reconcile state to an actual target database incarnation (for example cluster system identifier plus database OID, with manifest/snapshot identity), and reject mismatches before consuming any local cursor/ACK. Alternatively, validate every local receipt against B's committed PostgreSQL page receipts and force a complete replay where they are absent. Do not use only the deterministic source migration/snapshot UUID or only the database name. Add a focused two-target or drop/recreate-target regression that preserves same-target crash-resume behavior.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Cookie namespace / CSRF reuse / OAuth session | crates/monica-server/src/auth.rs:88-103; connections.rs:532-539; apps/web/lib/api.ts:3-10 | Reviewed - no bug found | Session and CSRF names consistently selected by installation; origin/host guard retained. | Cookie HTTP regression inspected; no remaining production fixed-name reader found. |
| Saved AI visibility / execution disabled | crates/monica-core/src/ai.rs:215,757,866,925-983 | Reviewed - no bug found | Claims, dispatch, configured provider and browser forwarding gate execution; historical privacy policy remains checked. | ai_settings.rs, ai_browser.rs, importer.analysis and saved-analysis HTTP regression. |
| Four offline sources and destination guards | scripts/legacy-import.py:87-185 | Reviewed - no bug found | Source sessions read-only and local socket; identity/manifest/empty business target checks precede conversion. | Disposable guard tests passed; no existing business DB accessed. |
| Table classification and immutable source fingerprint | scripts/legacy-import.py:27-140 | Reviewed - no bug found | Unknown tables fail; complete ordered JSON row fingerprints and exact manifest comparison. | Unknown-table regression passed; source classification sets traced. |
| Direct CRM entities and extra metadata | scripts/legacy-import.py:355-417 | Reviewed - no bug found | Original IDs/live compatible columns retained; unmapped metadata archived; sessions/worker credentials never installed live. | DIRECT mappings and schema columns traced; TOTP re-encryption and password preservation tested. |
| Fields / structured facts / manual deletion / organization IDs | scripts/legacy-import.py:418-458 | Reviewed - no bug found | Source ownership, notebook pointers and immutable revisions retained; deterministic organization mapping. | linking::rebuild_profile, structured current views and history/restore predicates inspected. |
| Messages and immutable CRM evidence | scripts/legacy-import.py:459-481 | Reviewed - no bug found | Preserves message UUIDs and source IDs/time; imported account attribution remains movable. | Synthetic current body, original body and sender identity regression passed. |
| Provider payloads/revisions/observations | scripts/legacy-import.py:308-353 | Reviewed - no bug found | Cross-store payload references resolved; historical observed_at/source_event retained; same hash recurrence resolves source event. | Synthetic three-revision repeated-hash mapping passed. |
| Social, activities, tags, relationships, historical analyses | scripts/legacy-import.py:481-543 | Reviewed - no bug found | Transformed fields have independent raw history; legacy analyses marked completed with explicit privacy. | Schema compatibility and Rust read consumers traced. |
| Postgres pagination / atomic checkpoints / verify | scripts/legacy-import.py:544-663 | Reviewed - no bug found | 500-row keyset pages commit with target checkpoint; verify replays sources in target read-only mode and ignores write free-space floor. | Interrupted second page/resume, read-only zero-space verify and tamper rejection passed. |
| Deterministic finalization and Memory inputs | crates/monica-server/src/legacy.rs:319-428; migrations/0063_memory_legacy_address_lookup.sql | Reviewed - no bug found | Dedicated target, bounded per-conversation/per-profile commits, identity sequences and queue drainage; no model execution. | Saved finalizer binary SHA verified; exercised twice by synthetic migration contract. |
| Source tags/relationship ownership/manual precedence | migrations/0055_source_history_owners.sql; migrations/0056_legacy_relationship_owners.sql; migrations/0059_legacy_relationship_manual_overrides.sql | Reviewed - no bug found | Views follow current account links, honor removals and manual priority; partial unique conflict target matches API. | Legacy HTTP link/relink/manual override regression inspected; all changed view consumers traced. |
| Group roles and orphan historical field counts | migrations/0053_isolated_legacy_migration.sql; migrations/0055_source_history_owners.sql; migrations/0057_unknown_source_group_roles.sql | Reviewed - no bug found | Unknown source role stays nullable; nullable owner counters filter null subjects. | Importer membership manual/source split and group view inspected. |
| Chatlog manifest/classification/all-table history | scripts/legacy-chatlog-schema.py; scripts/legacy-chatlog-import.py:34-132 | Reviewed - no bug found | Quick-check, full source hashes, known-table dispositions, business scan counts, exact history verification and archive pagination. | Unknown SQLite table rejection and binary-invalid-text source fixture passed. |
| Chatlog message reconciliation / missing sender / UTC fold | scripts/legacy-chatlog-import.py:236-311 | Reviewed - no bug found | Absolute UTC lookup prevents fold duplication; raw string/structured payload comparison normalized; missing sender recorded without inventing owner. | Synthetic DST, corrections, sender gaps, counts and original evidence tests passed. |
| Chatlog post IDs/interactions/revisions | scripts/legacy-chatlog-import.py:313-362 | Reviewed - no bug found | XML canonical ID used; corrected content/interactions retain immutable source versions. | Synthetic post correction and interaction update passed; social_versions union traced. |
| Chatlog revocations and deterministic settlement | scripts/legacy-chatlog-import.py:146-207 | Reviewed - no bug found | Deferred revoke resolves after full source; absent source target explicitly recorded; summary/Memory checked before converted. | Synthetic revoke target/source gaps and last-preview assertions passed. |
| Normal apply scan state can authorize a different DB | scripts/legacy-chatlog-import.py:423-431 | Finding B1 | Reader cursors/ACKs are reused without binding to actual target DB; missing target rows can be marked complete. | Disposable two-target reproduction: first extra message=1, second=0, snapshot=converted and business expected=processed=1. |
| Explicit reconcile / immutable scan count / failure ordering | scripts/legacy-chatlog-import.py:418-448; scripts/collector-chatlog.py:148-183 | Reviewed - no bug found | Reconcile uses separate resumable scan state, bypasses target old-page receipts, converges counters/versions; local count+cursor+pending delete atomic. | Explicit old-receipt correction/repeated page contract passed; ACK-before-cursor ordering inspected; cross-target state root merged into B1. |
| Evidence/content/social/history HTTP access and bounds | crates/monica-server/src/legacy.rs:24-316 | Reviewed - no bug found | Every endpoint authenticates; keyset limit clamps; raw file paths reject traversal and escaping symlinks; range responses retained. | Rust HTTP fixtures for source bodies, revisions, range/path safety, catalogue/history authentication and pagination inspected. |
| Frontend migration/history/media/saved-model UI | apps/web/components/imports/legacy-migrations.tsx; apps/web/components/feed/event-dialog.tsx; apps/web/components/messages/conversation.tsx | Reviewed - no bug found | Text rendering/validated local URLs; paged archive and version views; explicit direction; historical-analysis status reflected. | All web diff hunks read; no new source execution path. |
| Online database export isolation/cancellation | scripts/nas-migration-export.py | Reviewed - no bug found | Read-only pg_dump tagged per operation; bounded remote memory/deadline and guarded cancellation only matches tagged process. | Static remote program/caller error handling reviewed; no SSH or export executed. |
| NAS one-shot copy controller / mount/health boundaries | scripts/nas-migration-copy.py | Reviewed - no bug found | Source allowlist, overlap/symlink guards, read-only no-network worker and exact-label cancellation. | 2 cancellation tests passed; resource limits and process ownership traced. |
| Independent file/WAL backup / resume / cancellation | scripts/migration-file-copy.py | Reviewed - no bug found | Destination is independent inode; committed WAL snapshot pinned and released; journal replay hashes bytes; pause leaves partial unpublished. | 4 focused copy tests passed including live writer commit and tampered destination. |
| Snapshot download validation/resume | scripts/nas-migration-download.py | Reviewed - no bug found | Only completed new archive accepted; private target path guards; SHA/length check before atomic publication; health/free-space controls. | Static stream/read/error paths inspected; no NAS download executed. |
| Go pinned media codecs and independent CAS | tools/migration-media/main.go | Reviewed - no bug found | Canonical disjoint roots, bounded input, file type/codec validation, independent output, existing CAS checksum validation. | go test ./... passed; source/output symlink and tamper fixtures inspected. |
| NAS isolated compose/Caddy ingress | infra/nas.compose.yaml; infra/Caddyfile.rust | Reviewed - no bug found | Only proxy joins ingress; DB/server stay internal; independent roots/cookies/secrets and execution/collection pause defaults. | Static published port/Host/runtime mounts verified; no live NAS operation. |
| CI wiring and test proportionality | scripts/test-legacy-import.py; crates/monica-server/src/legacy_tests.rs; .github/workflows/ci.yml | Reviewed - no bug found | Focused integration/security/data-preservation coverage uses synthetic state; cross-target case absent and required by B1. | No removable redundant tests identified; dedicated code-review/excessive-tests companion follows. |
| Deployment/coverage documentation and codec notices | docs/NAS_ISOLATED_DEPLOYMENT.md; docs/NAS_MIGRATION_COVERAGE.md; tools/migration-media/README.md; tools/migration-media/licenses/* | Reviewed - no bug found | Staged acceptance limits clear; history disposition and source preservation documented; codec dependency/license notices provided. | Read source docs and pinned module metadata; no claim of real-data acceptance. |
| Earlier review artifacts in PR | tmp/reviews/2026-11-13-*-2355f418.md; tmp/reviews/2026-11-13-bugbot-report-426a15f4.md; tmp/reviews/2026-11-13-bugbot-report-8f55023a.md | Not production-relevant | Excluded from recursive production review and not read before this report was persisted. | Historical closure will be recorded in a separate companion after independent detection. |

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| `.github/workflows/ci.yml` | modified | CI coverage or deployment/acceptance documentation |
| `apps/web/components/feed/event-dialog.tsx` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/feed/types.ts` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/imports/import-progress.tsx` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/imports/imports-view.tsx` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/imports/legacy-migrations.tsx` | added | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/messages/conversation.module.css` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/messages/conversation.tsx` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/settings/model-settings.tsx` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/sources/sources-view.tsx` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/components/ui/primitives.tsx` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `apps/web/lib/api.ts` | modified | Web API/CSRF, migration history, message/media and saved-analysis UI |
| `crates/monica-core/src/ai.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-core/src/ai_memory.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-core/src/linking.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/ai.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/ai_browser.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/ai_settings.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/auth.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/connections.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/legacy.rs` | added | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/legacy_tests.rs` | added | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/link_graph.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/main.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/media.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/messages.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/pages/directory.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/pages/engagement.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/pages/metrics.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/pages/overview.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/profile_read.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/profile_relationships.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/profiles.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `crates/monica-server/src/timeline.rs` | modified | Runtime callers, authentication, evidence, projections, ownership, AI gates and focused tests |
| `docs/NAS_ISOLATED_DEPLOYMENT.md` | added | CI coverage or deployment/acceptance documentation |
| `docs/NAS_MIGRATION_COVERAGE.md` | added | CI coverage or deployment/acceptance documentation |
| `infra/Caddyfile.rust` | added | Independent containers/networks/mounts/secrets/port/origin |
| `infra/nas.compose.yaml` | added | Independent containers/networks/mounts/secrets/port/origin |
| `migrations/0053_isolated_legacy_migration.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0054_legacy_evidence.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0055_source_history_owners.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0056_legacy_relationship_owners.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0057_unknown_source_group_roles.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0058_legacy_content_locations.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0059_legacy_relationship_manual_overrides.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0060_offline_chatlog_snapshots.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0061_legacy_attribution_settlement.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0062_chatlog_recovery_evidence.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0063_memory_legacy_address_lookup.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0064_chatlog_interaction_versions.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `migrations/0065_chatlog_archive_catalog.sql` | added | Persistence schema, ownership/override views, historical evidence and settlement |
| `scripts/collector-chatlog.py` | modified | Source/read-only isolation, transformation, checkpoints, copying, cancellation, verification |
| `scripts/legacy-chatlog-import.py` | added | Source/read-only isolation, transformation, checkpoints, copying, cancellation, verification |
| `scripts/legacy-chatlog-schema.py` | added | Source/read-only isolation, transformation, checkpoints, copying, cancellation, verification |
| `scripts/legacy-import.py` | added | Source/read-only isolation, transformation, checkpoints, copying, cancellation, verification |
| `scripts/migration-file-copy.py` | added | Source/read-only isolation, transformation, checkpoints, copying, cancellation, verification |
| `scripts/nas-migration-copy.py` | added | Source/read-only isolation, transformation, checkpoints, copying, cancellation, verification |
| `scripts/nas-migration-download.py` | added | Source/read-only isolation, transformation, checkpoints, copying, cancellation, verification |
| `scripts/nas-migration-export.py` | added | Source/read-only isolation, transformation, checkpoints, copying, cancellation, verification |
| `scripts/test-legacy-import.py` | added | Synthetic contracts and disposable resources |
| `scripts/test-migration-file-copy.py` | added | Synthetic contracts and disposable resources |
| `scripts/test-nas-migration-copy.py` | added | Synthetic contracts and disposable resources |
| `tmp/reviews/2026-11-13-bugbot-report-2355f418.md` | added | Not production-relevant: prior review artifact (excluded from detection) |
| `tmp/reviews/2026-11-13-bugbot-report-426a15f4.md` | added | Not production-relevant: prior review artifact (excluded from detection) |
| `tmp/reviews/2026-11-13-bugbot-report-8f55023a.md` | added | Not production-relevant: prior review artifact (excluded from detection) |
| `tmp/reviews/2026-11-13-code-review-excessive-tests-2355f418.md` | added | Not production-relevant: prior review artifact (excluded from detection) |
| `tools/migration-media/README.md` | added | Pinned codec implementation, files/dependencies/tests/license notices |
| `tools/migration-media/go.mod` | added | Pinned codec implementation, files/dependencies/tests/license notices |
| `tools/migration-media/go.sum` | added | Pinned codec implementation, files/dependencies/tests/license notices |
| `tools/migration-media/licenses/SILK-Skype-BSD.txt` | added | Pinned codec implementation, files/dependencies/tests/license notices |
| `tools/migration-media/licenses/chatlog-MIT.txt` | added | Pinned codec implementation, files/dependencies/tests/license notices |
| `tools/migration-media/licenses/go-lame-MIT.txt` | added | Pinned codec implementation, files/dependencies/tests/license notices |
| `tools/migration-media/licenses/go-silk-MIT.txt` | added | Pinned codec implementation, files/dependencies/tests/license notices |
| `tools/migration-media/main.go` | added | Pinned codec implementation, files/dependencies/tests/license notices |
| `tools/migration-media/main_test.go` | added | Pinned codec implementation, files/dependencies/tests/license notices |

### Verification Commands

- GitHub connector `get_pr_info` for `hmumixaM/monica-rs#4`: base/head exact match, open draft, 75 files, 1 commit. Read-only remote metadata; no PR comments or mutations.
- `git merge-base 027eaa2e1c113d4345ca82711215cbed4281be45 d1e08cfcf0a3aa1ccef5574f51a309e6e2daa061` -> `027eaa2e1c113d4345ca82711215cbed4281be45`; `git diff --name-status 027eaa2e1c113d4345ca82711215cbed4281be45...d1e08cfcf0a3aa1ccef5574f51a309e6e2daa061` -> 75 files; `git status --short` and `git ls-files --others --exclude-standard` -> empty before report artifacts.
- Read the full local Bugbot skill and report contract plus code-review and excessive-tests skills. Independently inspected all production areas and changed tests before reading historical review reports.
- `shasum -a 256 .local-validation/nas-migration-20261113/monica-server-offline` -> `91c0d797df403a36384e2194a5bf87d576236956516f3cbf76655cefefa1122a`.
- `MONICA_TEST_SERVER=.../monica-server-offline uv run --with 'psycopg[binary]==3.2.12' --with cryptography==46.0.3 python scripts/test-legacy-import.py` -> 3 tests passed in 5.789s. Script created/stopped its own `/tmp/monica-legacy-*` PG cluster and synthetic databases.
- `python3 scripts/test-migration-file-copy.py` -> 4 passed; `python3 scripts/test-nas-migration-copy.py` -> 2 passed; `python3 scripts/test-collector-chatlog.py` -> 11 passed.
- `go test ./...` in `tools/migration-media` -> passed.
- Ephemeral Python reproduction using the synthetic `MigrationContract` fixture and a second disposable target -> B1 confirmed with the exact counts above. Disposable cluster stopped and cleaned in finally; no source files changed.
- Full Rust rebuild/workspace/clippy/web rebuild and real NAS/performance tests were intentionally not repeated. Rust legacy HTTP test source was reviewed; prior run logs are supplied context, not independently rerun evidence.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| UTC DST-fold equality duplicates | dismissed | Both indexed target key and incoming timestamp are normalized to absolute UTC; synthetic fold fixture passed. |
| Reconcile repeats counts/revisions for old pages | dismissed | seen receipt suppresses records/unchanged counters; real corrections converge; repeat old receipt test passed. |
| Immutable scan restart double-counts an ACKed page | dismissed within same target | SQLite scan count/cursor/pending deletion is one transaction; SQL target page commits before ACK; cross-target trust issue merged into B1. |
| Read-only legacy verification blocked by low disk or target writes | dismissed | target default_transaction_read_only explicitly enabled; verifying skips the write free-space gate; zero-free mock passed. |
| Source-owned tags/relationships reserve old manual person slot | dismissed | current-link views and partial manual uniqueness/API conflict predicate match; focused HTTP fixture read. |
| Published proxy port on exclusively internal Docker bridge | dismissed | proxy now additionally joins independent project ingress network; app/DB remain isolated. |
| Earlier report files recursively treated as production code | not production-relevant | Explicit Bugbot report exclusion; prior findings not read before this immutable report. |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| Full personal-data table/media correctness and unusual source distributions | Existing business DBs, private snapshots and NAS are outside this review's authorized access; synthetic fixtures cover selected boundaries. | Complete source-to-target verification and per-file hashes by the main migration task, followed by authorized real-page acceptance. |
| Runtime compose ingress, credentials, live image/source equality and collector recovery | No SSH or deployment operation was allowed or performed. | Main task's isolated NAS acceptance and protected upstream spool/recovery evidence. |
| Fresh Rust/web compiler and full suite execution at this head | Avoided redundant rebuilding under constrained disk; no production source edits were made by this review. | CI for this exact head; supplied logs may be checked separately. |
| Full CLI lifecycle for reconcile process interruption / session-lock contention | Static ACK/cursor/transaction/lock ownership inspected; focused contract calls importer/Reader directly. | A small disposable CLI failure/restart/lock regression if changed during B1 repair. |

## Report Self-Check

- yes: Every finding appears in both Complete Findings Index and Findings.
- yes: Every meaningful recursive candidate appears in Detection Coverage Ledger.
- yes: Every Finding B# ledger row maps to a finding card.
- yes: No production-relevant candidate is left Not covered; documented runtime acceptance blind spots are outside reviewed scope.
- yes: Severity counts match the single medium finding card.
- yes: Recommendation follows the mechanical mapping: medium => Changes requested.

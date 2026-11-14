# Bugbot Report

## Report Contract

- Report type: `bugbot`
- Report ID: `bb-20261114-51b745ba`
- Review date: `2026-11-14`
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-51b745ba.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: `Fixed review input; do not rewrite during repair`

## Scope

- Diff mode: `branch changes`
- Requested base: actual PR #4 `main`, resolved through `gh pr view 4`.
- Resolved baseline: `027eaa2e1c113d4345ca82711215cbed4281be45` (also the merge base).
- Reviewed target: PR head `74b9de20b24638cdd1e9fd89948eacbba2724b18`, branch `codex/nas-isolated-migration`; tracked working tree clean and no untracked non-ignored files at scope capture.
- Changed files: `78` (including seven prior review artifacts).
- Completion: `Complete within reviewed scope`.
- Custom instructions: independent fresh-context local Bugbot/code-review/excessive-tests review; no source or Git mutations, no NAS access, no real business databases, no external comments, no performance testing. Focus on target binding and closure of prior confirmed defects while reviewing the full actual PR.
- Assumptions: review is fixed to the captured PR SHA. `scripts/legacy-chatlog-media.py` appeared untracked after scope capture as parallel parent work and is explicitly excluded; it requires a separate review. The supplied offline Rust executable is used for disposable synthetic checks; no new Rust compilation was requested.

## Detection Snapshot

- Recommendation: `Changes requested`
- Completion: `Complete within reviewed scope`
- Why now: the target-binding fix closes its reported defect, but the full PR still loses historical sender attribution in the user-visible evidence API.
- Findings count: `high 0 | medium 1 | low 0`
- Must-review now: `B1`
- Coverage confidence: `high` for the confirmed API defect and reviewed target-binding paths; `medium` for real source-format breadth.
- Biggest blind spot: real snapshot/NAS acceptance is outside this review; synthetic fixtures do not certify full production data reconciliation.

## Complete Findings Index

| ID | Severity | Category | Location | Title |
| --- | --- | --- | --- | --- |
| B1 | medium | LOGIC_BUG | [crates/monica-server/src/legacy.rs:250-252](/Users/maxhu/play/monica-rs/crates/monica-server/src/legacy.rs:250) | Preserve senders in historical evidence |

## Findings

### B1 medium - Preserve senders in historical evidence

- Category: `LOGIC_BUG`
- Location: [crates/monica-server/src/legacy.rs:250-252](/Users/maxhu/play/monica-rs/crates/monica-server/src/legacy.rs:250)
- Confirmation selector: `B1`

Description: `evidence_version` puts participants only in the first tuple's current metadata. Its immutable second payload contains no participants. The existing evidence handler builds response metadata from that payload and copies the first tuple's participants only when the selected revision equals the current revision. After any Chatlog correction, requesting the original or an intermediate message revision therefore returns the correct historical body with no sender. The evidence page consequently drops the author when a user follows the new historical revision links. The same payload is used by the evidence read surface, so historical attribution cannot be recovered from its normal response.

Rationale: the new legacy evidence fallback and historical version navigation introduce this contract mismatch. Ordinary connection revision payloads retain their historical participant fields; the newly created legacy payload omits them. The source sender is available in the original CRM evidence/baseline and selected saved revision. Merely copying the currently queried participants into every payload would also be wrong when a correction changed the authoritative sender: the query at lines 232-233 always uses the current `message["sender_account_id"]`.

Evidence:

- Changed lines 250-252 return participants in current metadata but omit them from the selected immutable payload.
- `crates/monica-server/src/connections.rs:461-479` replaces metadata with the payload and only restores current metadata for the current revision.
- `apps/web/components/sources/sources-view.tsx:164-165` exposes the historical links and renders only `record.metadata.participants` for the sender.
- Disposable PostgreSQL 16 reproduction migrated synthetic CRM rows, finalized them, used the real `Importer.correct` function to create revision 2 with a corrected source sender, then called the actual HTTP evidence endpoints. Revision 1 returned `body="Searchable original text"` and metadata without a `participants` key. Revision 2 returned `body="Corrected text"` and `participants=[{"id":"corrected-sender","role":"sender",...}]`. The original message sender before correction was `synthetic-account`.
- Recommended repair: derive the selected revision's participant identity from its immutable saved attribution/original CRM evidence, include it in the payload contract, and retain live account-link resolution for the current revision. Add assertions for original, intermediate, and current versions around a sender correction; the existing raw-body tests are insufficient to catch this.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Actual baseline, latest remote head and clean initial tree | PR #4 / all changed paths | Reviewed - no bug found | Actual remote base/head match supplied commits; merge-base equals base | `gh pr view 4`, `git merge-base`, initial `git status --short`, initial untracked inventory |
| Target-state marker identity | scripts/legacy-chatlog-import.py:37-66,452 | Reviewed - no bug found | Instance ID, DB name, DB OID and snapshot bind normal/reconcile state before Reader construction | All three migration contract tests pass; different target and recreated same-name target are refused |
| Upgrade of pre-marker state | scripts/legacy-chatlog-import.py:46-66 | Reviewed - no bug found | Existing ACKs require same snapshot identity and matching atomic receipts; absent receipt evidence fails closed | Bounded 500-digest queries; symlink and ACK-size guards; wrong-target legacy-state regression passes |
| Normal receipt replay and recovery | scripts/legacy-chatlog-import.py:389-419,455-477 | Reviewed - no bug found | Apply remains idempotent; explicit reconcile bypasses existing receipts while retaining record counts and original evidence | Synthetic migration test verifies ordinary apply leaves old projection and reconcile repairs it |
| Frozen source format and all-table accounting | scripts/legacy-chatlog-schema.py:1-49; legacy-chatlog-import.py:69-160 | Reviewed - no bug found | Unknown table/path fails; business scan counts, history content/counts and derived dispositions are explicit | Unknown-table test passes; history and business catalog completion tested |
| Source read-only/snapshot guards | scripts/legacy-import.py:91-141,165-194,616-655 | Reviewed - no bug found | Local socket/read-only source, different target, snapshot fingerprint and empty-target guards traced | Migration contract rejects identical target, populated target and unclassified table |
| Conversion ordering and direct metadata | scripts/legacy-import.py:24-89,195-240,277-398,536-615 | Reviewed - no bug found | Content/identity predecessors, batch receipts and extra metadata archives preserve source contracts | Interrupted second page leaves exactly 500 of 501 tags then resumes to 501; big JSON integer and hashes retained |
| CRM/provider transformations | scripts/legacy-import.py:240-276,308-535 | Reviewed - no bug found | Immutable IDs, organization/sequence maps, provider observations, current pointers, original content and historical AI are traced | Synthetic repeated content revisions select exact observation versions; TOTP is re-encrypted and verified |
| String payload reconciliation | scripts/legacy-chatlog-import.py:279-329 | Reviewed - no bug found | Dict/string/other JSON comparison normalization no longer aborts valid legacy strings | Original string payload is exercised by passing Chatlog migration contract |
| Sender correction, gaps, UTC keys and groups | scripts/collector-chatlog.py:46-103,131-173,240-295; legacy-chatlog-import.py:277-342 | Reviewed - no bug found | Authoritative sender corrections and unknown sentinel evidence are preserved; UTC keys handle DST fold | Synthetic tests check corrected sender, retained known sender for source gap, zero sender and fold record |
| Revokes and projection settlement | scripts/legacy-chatlog-import.py:175-245; legacy.rs:320-428 | Reviewed - no bug found | Revokes settle after full scan, preserve notice/evidence, queue Memory and require settled conversation summaries | Synthetic test checks tombstone, five live messages, preview, missing target diagnostics and read-only verification |
| Moments, interaction versions and immutable originals | scripts/legacy-chatlog-import.py:344-388; migrations/0060*,0062*,0064* | Reviewed - no bug found | Canonical XML ID and interaction corrections retain content versions | Synthetic test updates post/comment, verifies original CRM baseline and latest snapshot |
| Historical evidence sender contract | crates/monica-server/src/legacy.rs:199-253; connections.rs:516-523 | Finding B1 | Non-current versions drop the sender because tuple payload lacks participants | Confirmed through real HTTP requests on disposable synthetic cluster |
| Evidence auth, pagination, downloads and path mapping | legacy.rs:23-198,256-319; messages.rs:124-158; timeline.rs:198 | Reviewed - no bug found | Auth required, page sizes capped, composite cursors validated, archive paths canonicalized beneath mapped root | Existing Rust tests inspected for unauthenticated/escape/range/pagination boundaries; synthetic HTTP evidence read exercised |
| Relationship/source-tag ownership and manual slots | migrations/0054-0059; linking.rs; profile_relationships.rs; profile_read.rs; profiles.rs; link_graph.rs; page queries | Reviewed - no bug found | Presented views follow account links and manual priority; partial unique index matches writer conflict target | Dedicated Rust regression inspects relink/unlink/manual slot behavior; callers consistently switched to presented views |
| AI execution/read separation | migrations/0053*; ai.rs; ai_browser.rs; ai_settings.rs; main.rs; model-settings.tsx | Reviewed - no bug found | Execution flag blocks worker/provider/browser calls while historical outputs remain visible | Rust regression for saved analysis inspected; browser/backend predicates and environmental override traced |
| Cookie namespace and CSRF/OAuth | auth.rs; connections.rs; apps/web/lib/api.ts | Reviewed - no bug found | All session/CSRF writes and reads use selected prefix; frontend requests installation token | Same-host separate-port cookie/auth/CSRF regression inspected; historical evidence endpoint authenticated in runtime check |
| Message/media/history/progress frontend | apps/web/components/feed/*; imports/*; messages/*; sources/*; ui/primitives.tsx | Reviewed - no bug found | New links match API paths; bounded history controls and media rendering traced | No separate visual/browser acceptance run; author omission is merged into B1 |
| Isolated Compose/Caddy/CLI | infra/nas.compose.yaml; infra/Caddyfile.rust; main.rs | Reviewed - no bug found | Dedicated DB/mounts/networks, TLS origin, cookie prefix, paused work and optional roots are consistent | Configuration/source review only; no NAS operation or deploy performed |
| Online database export and cancellation | scripts/nas-migration-export.py | Reviewed - no bug found | Read-only logical backup, dedicated process tag, health/disk/time guard and private receipt traced | No remote invocation; cancellation targets only tagged export process |
| Byte/SQLite copying and ownership | scripts/migration-file-copy.py; nas-migration-copy.py | Reviewed - no bug found | Read-only mount, independent output, WAL snapshot, source allowlist, path/ownership and pause boundaries traced | Four file-copy tests and two cancellation-controller tests pass |
| Download source status and transport | scripts/nas-migration-download.py | Reviewed - no bug found | Source-specific status preferred; restricted sudo reads new archive; digest/length guards enforce byte identity | Old status/permission defects are closed in source; no real NAS/download test |
| Offline codec and filesystem output | tools/migration-media/main.go,go.mod,go.sum | Reviewed - no bug found | Canonical disjoint roots, source/output symlink checks, independent CAS output, size/hash validation and pinned codecs traced | `go test ./...` passes; no real encrypted image/global key/voice input exercised |
| Test coverage and CI value | legacy_tests.rs; scripts/test-*.py; main_test.go; .github/workflows/ci.yml | Reviewed - no bug found | Added tests are proportionate to data-loss, attribution, auth and cancellation boundaries | Separate excessive-tests report; focused checks pass; historical participant regression missing |
| Documentation, license notices and prior review files | docs/*; tools/migration-media/README.md,licenses/*; tmp/reviews/* | Not production-relevant | No runtime defect; docs distinguish staged migration work from final acceptance; prior reports are not recursively reviewed as production | No license/legal opinion or release certification given |

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| `.github/workflows/ci.yml` | modified | CI execution and synthetic migration coverage |
| `apps/web/components/feed/event-dialog.tsx` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/feed/types.ts` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/imports/import-progress.tsx` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/imports/imports-view.tsx` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/imports/legacy-migrations.tsx` | added | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/messages/conversation.module.css` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/messages/conversation.tsx` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/settings/model-settings.tsx` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/sources/sources-view.tsx` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/components/ui/primitives.tsx` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `apps/web/lib/api.ts` | modified | Evidence/history/import UI, message media, model controls and namespaced CSRF |
| `crates/monica-core/src/ai.rs` | modified | AI execution gate, Memory attribution, account relinking |
| `crates/monica-core/src/ai_memory.rs` | modified | AI execution gate, Memory attribution, account relinking |
| `crates/monica-core/src/linking.rs` | modified | AI execution gate, Memory attribution, account relinking |
| `crates/monica-server/src/ai.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/ai_browser.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/ai_settings.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/auth.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/connections.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/legacy.rs` | added | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/legacy_tests.rs` | added | Synthetic behavior regression coverage and test isolation |
| `crates/monica-server/src/link_graph.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/main.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/media.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/messages.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/pages/directory.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/pages/engagement.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/pages/metrics.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/pages/overview.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/profile_read.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/profile_relationships.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/profiles.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `crates/monica-server/src/timeline.rs` | modified | Authenticated API, selected evidence versions, source ownership, finalization |
| `docs/NAS_ISOLATED_DEPLOYMENT.md` | added | Migration/release/restore contracts and honest acceptance limitations |
| `docs/NAS_MIGRATION_COVERAGE.md` | added | Migration/release/restore contracts and honest acceptance limitations |
| `infra/Caddyfile.rust` | added | Isolated mounts/networks, cookie namespace, resource controls |
| `infra/nas.compose.yaml` | added | Isolated mounts/networks, cookie namespace, resource controls |
| `migrations/0053_isolated_legacy_migration.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0054_legacy_evidence.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0055_source_history_owners.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0056_legacy_relationship_owners.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0057_unknown_source_group_roles.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0058_legacy_content_locations.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0059_legacy_relationship_manual_overrides.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0060_offline_chatlog_snapshots.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0061_legacy_attribution_settlement.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0062_chatlog_recovery_evidence.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0063_memory_legacy_address_lookup.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0064_chatlog_interaction_versions.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `migrations/0065_chatlog_archive_catalog.sql` | added | DDL, migrated evidence, ownership views, deterministic Memory inputs |
| `scripts/collector-chatlog.py` | modified | File/SQLite copying, isolation, cancellation, source/data integrity |
| `scripts/legacy-chatlog-import.py` | added | Read-only source contracts, conversion, receipts, recovery, content verification |
| `scripts/legacy-chatlog-schema.py` | added | Read-only source contracts, conversion, receipts, recovery, content verification |
| `scripts/legacy-import.py` | added | Read-only source contracts, conversion, receipts, recovery, content verification |
| `scripts/migration-file-copy.py` | added | File/SQLite copying, isolation, cancellation, source/data integrity |
| `scripts/nas-migration-copy.py` | added | File/SQLite copying, isolation, cancellation, source/data integrity |
| `scripts/nas-migration-download.py` | added | File/SQLite copying, isolation, cancellation, source/data integrity |
| `scripts/nas-migration-export.py` | added | File/SQLite copying, isolation, cancellation, source/data integrity |
| `scripts/test-legacy-import.py` | added | Synthetic behavior regression coverage and test isolation |
| `scripts/test-migration-file-copy.py` | added | Synthetic behavior regression coverage and test isolation |
| `scripts/test-nas-migration-copy.py` | added | Synthetic behavior regression coverage and test isolation |
| `tmp/reviews/2026-11-13-bugbot-report-2355f418.md` | added | Not production-relevant: prior immutable review artifact |
| `tmp/reviews/2026-11-13-bugbot-report-426a15f4.md` | added | Not production-relevant: prior immutable review artifact |
| `tmp/reviews/2026-11-13-bugbot-report-8f55023a.md` | added | Not production-relevant: prior immutable review artifact |
| `tmp/reviews/2026-11-13-bugbot-report-db09f455.md` | added | Not production-relevant: prior immutable review artifact |
| `tmp/reviews/2026-11-13-code-review-excessive-tests-2355f418.md` | added | Not production-relevant: prior immutable review artifact |
| `tmp/reviews/2026-11-14-code-review-db09f455.md` | added | Not production-relevant: prior immutable review artifact |
| `tmp/reviews/2026-11-14-excessive-tests-db09f455.md` | added | Not production-relevant: prior immutable review artifact |
| `tools/migration-media/README.md` | added | Offline codecs, path isolation, independent output and pinned dependencies |
| `tools/migration-media/go.mod` | added | Offline codecs, path isolation, independent output and pinned dependencies |
| `tools/migration-media/go.sum` | added | Offline codecs, path isolation, independent output and pinned dependencies |
| `tools/migration-media/licenses/SILK-Skype-BSD.txt` | added | Third-party notices; no executable behavior |
| `tools/migration-media/licenses/chatlog-MIT.txt` | added | Third-party notices; no executable behavior |
| `tools/migration-media/licenses/go-lame-MIT.txt` | added | Third-party notices; no executable behavior |
| `tools/migration-media/licenses/go-silk-MIT.txt` | added | Third-party notices; no executable behavior |
| `tools/migration-media/main.go` | added | Offline codecs, path isolation, independent output and pinned dependencies |
| `tools/migration-media/main_test.go` | added | Synthetic behavior regression coverage and test isolation |

### Verification Commands

- `gh pr view 4 --json baseRefOid,headRefOid,baseRefName,headRefName,title,state`: open PR, actual base `027eaa2e1c113d4345ca82711215cbed4281be45`, actual head `74b9de20b24638cdd1e9fd89948eacbba2724b18`.
- `git merge-base HEAD 027eaa2e1c113d4345ca82711215cbed4281be45`: returns the actual baseline. Initial `git status --short` and `git ls-files --others --exclude-standard`: clean.
- Reviewed explicit base-to-head diff and complete added production/test files; no source or Git state changed by this reviewer.
- `MONICA_TEST_SERVER=.../monica-server-offline uv run --no-project --with 'psycopg[binary]==3.2.12' --with cryptography==46.0.3 python scripts/test-legacy-import.py`: **3 tests passed**, 5.127 s. All database state existed only in a disposable cluster under `/tmp` and was stopped/removed by the harness.
- `python3 scripts/test-collector-chatlog.py`: **11 tests passed**.
- `python3 scripts/test-migration-file-copy.py`: **4 tests passed**.
- `python3 scripts/test-nas-migration-copy.py`: **2 tests passed**.
- `go test ./...` in `tools/migration-media`: **pass** (cached).
- Ad hoc synthetic HTTP reproduction described in B1: **confirmed historical participants omission**, original/current bodies correct, current participants present. Both temporary cluster and temporary server were stopped and deleted after the check.
- Supplied offline server SHA-256: `91c0d797df403a36384e2194a5bf87d576236956516f3cbf76655cefefa1122a`.
- `git diff --check 027eaa2e1c113d4345ca82711215cbed4281be45`: reports pre-existing-in-PR whitespace in one Python blank line, prior reports and third-party notice text; no production finding is based on those style issues.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Prior db09f455 B1: scan state reused on another DB | dismissed | New target identity rejects different and recreated same-name DB; legacy ACK upgrade checks actual target receipts |
| Prior 426a15f4 B1: old receipts prevent explicit repair | dismissed | `reconcile` scans distinct immutable state and processes receipted pages without inflating record/insert counters |
| Prior 2355f418 B1/B2: string payload crash and silent table omission | dismissed | Comparison normalization plus explicit schema/catalog/full scan accounting added and synthetic regression passes |
| Prior 8f55023a B1/B4: archive permissions/download status | dismissed | Download is explicitly restricted sudo; archive subroots are traversable to runtime UID; per-source status is preferred |
| Prior 8f55023a B2/B3: sender correction/settlement | dismissed | Authoritative current attribution is compared and persisted; mandatory finalizer plus projection verification runs before completion |
| Current sender queried while rendering historical revision | merged into B1 | Payload contract first drops historical participants; copying current participants blindly would create wrong old sender |
| Cleanup of parent-created untracked media extractor | not production-relevant | Added after review scope capture by ongoing implementation; untouched and excluded |
| Whitespace and long SQL/React lines | not production-relevant | No verified production error; not a Bugbot finding |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| Full real CRM/provider/Chatlog and media reconciliation | NAS and real business data are explicitly outside reviewer authorization | Parent's separately authorized real-data verification and acceptance evidence |
| Rust unit suite, fresh compilation and browser visual checks | Target directory was cleaned for disk space; this review used supplied offline binary plus source/test inspection | Normal CI/latest source build and targeted Rust/browser checks after repair |
| Actual NAS mounts, ports, health and resource stability | No remote operations requested; no performance testing permitted | Separate deployment acceptance on the NAS without unrelated service changes |
| Real encrypted media/voice coverage | Only deterministic synthetic codec/file tests were run | Explicit offline real-media conversion plus source/destination checksum catalog |

These are validation limits, not unresolved diff-derived defect candidates. They do not turn source-review completion into production-deployment acceptance.

## Report Self-Check

- yes: Every finding appears in both `Complete Findings Index` and `Findings`.
- yes: Every meaningful recursive candidate appears in `Detection Coverage Ledger`.
- yes: Every `Finding B#` ledger row maps to a finding card.
- yes: There are no `Not covered` ledger rows within this fixed review scope.
- yes: Severity counts match the finding cards (`0 high`, `1 medium`, `0 low`).
- yes: Recommendation follows the skill's mechanical mapping (`medium` => `Changes requested`).

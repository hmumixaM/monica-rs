# Bugbot Report

## Report Contract

- Report type: `bugbot`.
- Report ID: `bb-20261114-d38b56a2`.
- Review date: `2026-11-14`.
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-d38b56a2.md`.
- Repository root: `/Users/maxhu/play/monica-rs`.
- Artifact status: `Fixed review input; do not rewrite during repair`.

## Scope

- Diff mode: `branch changes`; independent post-repair review of the explicit-person fallback/precedence fix and affected historical/current evidence caller/importer contracts.
- Requested base: actual PR #4 `main`.
- Resolved baseline: `027eaa2e1c113d4345ca82711215cbed4281be45`; independently confirmed merge-base.
- Reviewed target: `969497f1ca403ebb9c57ad55a535585ff22c4e62`; independently confirmed actual remote PR head and local full SHA.
- Previous reviewed head: `db04fe884b43f7551eba6aeccbe84af026f077d6`. The latest commit changes 2 executable/test paths and adds 6 fixed review artifacts.
- Changed files: 87 in the complete actual base/head inventory. All paths are inventoried below. The fresh executable frontier is the latest fix and directly affected attribution contracts, rather than another full audit of unchanged PR implementation.
- Completion: `Complete within reviewed scope`.
- Custom instructions: use fresh independent context and local bugbot/code-review/excessive-tests; persist immutable reports; no reviewed-source or Git mutations, external comments, NAS/business-database access or performance tests. Private ignored media/restore work is outside this PR review.
- Assumptions: prior full-PR report `bb-20261114-51b745ba` and the focused follow-ups are fixed input evidence for unchanged areas. This run independently evaluates the latest repair, including the confirmed valid profile-only data shape from `bb-20261114-3dfc4585`; it does not claim independent NAS/real-data acceptance.

## Detection Snapshot

- Recommendation: `Pass`.
- Completion: `Complete within reviewed scope`.
- Why now: the explicit saved/current person can now produce a participant without a source account, current explicit attribution overrides the account link, and the independent HTTP regression passes while preserving historical authors.
- Findings count: `high 0 | medium 0 | low 0`.
- Must-review now: `None`.
- Coverage confidence: `high` for the changed attribution behavior and real HTTP caller chain.
- Biggest blind spot: NAS deployment and real-data restoration were deliberately not accessed; full remote CI did not execute on this SHA because of the parent-confirmed Actions budget blocker.

## Complete Findings Index

No introduced production bugs identified in the reviewed scope.

## Findings

None.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Explicit current person versus live account link | legacy.rs:258 | Reviewed - no bug found | Current coalesce gives explicit message person priority and otherwise follows the current account link | Same precedence as messages.rs reader; HTTP case deliberately differs from live link |
| Profile-only current/imported author | legacy.rs:260-274 | Reviewed - no bug found | An empty account result with explicit person now resolves that profile and emits one sender participant | Importer intentionally preserves sender_profile_id when sender_account_id is absent; focused HTTP case clears sender account |
| Historical source-account and saved-person identity | legacy.rs:228-258 | Reviewed - no bug found | Historical selections retain saved profile instead of following current source-account relinks | Three revisions use distinct original/intermediate/current senders; saved original person differs from current link |
| Sparse saved row with immutable external sender | legacy.rs:234-257 | Reviewed - no bug found | Intermediate author resolves using immutable payload sender_external_id when the saved row lacks author fields | Existing three-revision HTTP assertions retain intermediate sender and null person |
| Historical profile-only fallback interaction | legacy.rs:260-274 | Reviewed - no bug found | Fallback uses the selected attribution profile; no current live-link lookup occurs in the non-current CASE branch | Code trace plus existing saved-profile and profile-only HTTP boundaries; exact historical-profile-only combined fixture was not separately added |
| Generic evidence detail reconstruction | connections.rs:442-479,516-521 | Reviewed - no bug found | Legacy payload and metadata both carry participants; historical detail payload reconstruction preserves them | Real authenticated /api/v1/evidence HTTP test asserts metadata for each revision |
| Consumer contract | sources-view.tsx:165; legacy.rs:266 | Reviewed - no bug found | Fallback id/profile_id are JSON UUID strings, role is sender; existing participant consumer handles them | UI iterates participant role/id; no new required client field |
| Query cardinality and authorization | legacy.rs:258-274; connections.rs:441 | Reviewed - no bug found | Provider/external ID uniqueness and account PK bound participant count; fallback is one indexed profile lookup; existing authentication remains before version lookup | Schema account/link uniqueness; query values are bound |
| Test value and execution | legacy_tests.rs:293-406 | Reviewed - no bug found | Added two cases cover different identity boundaries using existing fixture/app; no arbitrary sleeps or external business services | 1 synthetic HTTP regression passed independently in 0.97 s |
| Wider unchanged PR | full base/head inventory | Reviewed - no bug found | No latest-round runtime changes outside active attribution files; earlier full audit and SHA-bound historical repair remain relevant | Fixed prior reports 51b745ba/b1dd56e6/3dfc4585; not a repeated full audit |
| Prior review documents and untracked review artifact | tmp/reviews/* | Not production-relevant | Fixed evidence documents are not recursively reviewed as runtime code | Inventory below; ignored private work excluded |

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| `.github/workflows/ci.yml` | modified | Read supporting caller/importer/rendering/CI contract; unchanged since earlier reviewed fix |
| `apps/web/components/feed/event-dialog.tsx` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/components/feed/types.ts` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/components/imports/import-progress.tsx` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/components/imports/imports-view.tsx` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/components/imports/legacy-migrations.tsx` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/components/messages/conversation.module.css` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/components/messages/conversation.tsx` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/components/settings/model-settings.tsx` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/components/sources/sources-view.tsx` | modified | Read supporting caller/importer/rendering/CI contract; unchanged since earlier reviewed fix |
| `apps/web/components/ui/primitives.tsx` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `apps/web/lib/api.ts` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-core/src/ai.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-core/src/ai_memory.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-core/src/linking.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/ai.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/ai_browser.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/ai_settings.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/auth.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/connections.rs` | modified | Read supporting caller/importer/rendering/CI contract; unchanged since earlier reviewed fix |
| `crates/monica-server/src/legacy.rs` | added | Active latest-fix frontier; sender derivation and HTTP behavioral regression |
| `crates/monica-server/src/legacy_tests.rs` | added | Active latest-fix frontier; sender derivation and HTTP behavioral regression |
| `crates/monica-server/src/link_graph.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/main.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/media.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/messages.rs` | modified | Read supporting caller/importer/rendering/CI contract; unchanged since earlier reviewed fix |
| `crates/monica-server/src/pages/directory.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/pages/engagement.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/pages/metrics.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/pages/overview.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/profile_read.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/profile_relationships.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/profiles.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `crates/monica-server/src/timeline.rs` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `docs/NAS_ISOLATED_DEPLOYMENT.md` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `docs/NAS_MIGRATION_COVERAGE.md` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `infra/Caddyfile.rust` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `infra/nas.compose.yaml` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0053_isolated_legacy_migration.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0054_legacy_evidence.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0055_source_history_owners.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0056_legacy_relationship_owners.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0057_unknown_source_group_roles.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0058_legacy_content_locations.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0059_legacy_relationship_manual_overrides.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0060_offline_chatlog_snapshots.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0061_legacy_attribution_settlement.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0062_chatlog_recovery_evidence.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0063_memory_legacy_address_lookup.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0064_chatlog_interaction_versions.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `migrations/0065_chatlog_archive_catalog.sql` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/collector-chatlog.py` | modified | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/legacy-chatlog-import.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/legacy-chatlog-schema.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/legacy-import.py` | added | Read supporting caller/importer/rendering/CI contract; unchanged since earlier reviewed fix |
| `scripts/migration-file-copy.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/nas-migration-copy.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/nas-migration-download.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/nas-migration-export.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/test-legacy-import.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/test-migration-file-copy.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `scripts/test-nas-migration-copy.py` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tmp/reviews/2026-11-13-bugbot-report-2355f418.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-13-bugbot-report-426a15f4.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-13-bugbot-report-8f55023a.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-13-bugbot-report-db09f455.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-13-code-review-excessive-tests-2355f418.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-bugbot-report-3dfc4585.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-bugbot-report-51b745ba.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-bugbot-report-b1dd56e6.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-code-review-3dfc4585.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-code-review-51b745ba.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-code-review-b1dd56e6.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-code-review-db09f455.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-excessive-tests-3dfc4585.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-excessive-tests-51b745ba.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-excessive-tests-b1dd56e6.md` | added | Not production-relevant; prior fixed review artifact |
| `tmp/reviews/2026-11-14-excessive-tests-db09f455.md` | added | Not production-relevant; prior fixed review artifact |
| `tools/migration-media/README.md` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tools/migration-media/go.mod` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tools/migration-media/go.sum` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tools/migration-media/licenses/SILK-Skype-BSD.txt` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tools/migration-media/licenses/chatlog-MIT.txt` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tools/migration-media/licenses/go-lame-MIT.txt` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tools/migration-media/licenses/go-silk-MIT.txt` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tools/migration-media/main.go` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tools/migration-media/main_test.go` | added | Unchanged since prior full-PR review bb-20261114-51b745ba; inventoried, not independently re-audited in this focused run |
| `tmp/reviews/2026-11-14-bugbot-report-6e62f2e3.md` | untracked | Not production-relevant; independent operational review artifact outside this captured PR SHA |

### Verification Commands

- `gh pr view 4 --json baseRefOid,headRefOid,baseRefName,headRefName,state,url`: actual open PR base `027eaa2e1c113d4345ca82711215cbed4281be45`, remote head `969497f1ca403ebb9c57ad55a535585ff22c4e62`.
- `git rev-parse 969497f`; `git merge-base 969497f 027eaa2e1c113d4345ca82711215cbed4281be45`: full local head and baseline agree with remote PR.
- `git diff 969497f^ 969497f -- crates/monica-server/src/legacy.rs crates/monica-server/src/legacy_tests.rs`: independently reviewed latest production/test repair. `git diff --name-status 027eaa2e1c113d4345ca82711215cbed4281be45 969497f1ca403ebb9c57ad55a535585ff22c4e62` and `git ls-files --others --exclude-standard` resolved the inventory.
- Read the full affected evidence function, generic version/detail callers, message reader precedence, CRM importer transform, participant UI consumer, uniqueness schema and CI entry points.
- `python3 scripts/test-cargo.py --locked -p monica-server imported_messages_expose_bounded_evidence_and_safe_original_files -- --nocapture`: **1 passed, 0 failed**, test runtime **0.97 s**, fresh compilation **35.23 s**. The runner created a temporary socket-only synthetic PostgreSQL16, set its explicit DATABASE_URL, and stopped/deleted it after completion. No NAS or business database access.
- Parent-supplied latest targeted check **1 passed in 0.96 s** and Clippy pass were context, not substituted for this independent test.
- No source edits or Git mutations. Only these fresh review reports are written. No broad suite rerun or performance testing.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Explicit message person loses to current account link | dismissed | coalesce($4::uuid,l.profile_id) now matches message reader precedence; new mismatch case passes |
| Source-account-only query hides profile-only author | dismissed | Empty participant array now triggers selected profile lookup; no-account case passes |
| Fallback rewrites historical person to current link | dismissed | Selected profile is chosen before fallback; historical CASE does not consult live profile link |
| Missing source account creates unbounded work | dismissed | Unique account identity bounds first query; single profile PK lookup is conditional on empty array |
| Reviewer must run full unchanged suite again | dismissed | Focused source-built HTTP verification resolves this small repair; unchanged full suite/deployment remain explicit limits |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| NAS/real migrated content and media completeness | Explicitly outside reviewer access; no production data used | Parent's authorized restoration, deployed-SHA verification and checksum/catalog acceptance |
| Complete unchanged PR implementation | This is a fresh post-repair context, scoped to latest repair and affected contracts | Earlier full PR report plus current repair reports; another full audit only if scope changes |
| Full remote CI at latest SHA | Parent confirmed Actions jobs did not start because of budget | Authorized local build/check evidence and eventual restored CI; this review does not call remote CI green |
| Exact historical plus profile-only combined runtime case | Existing regression separately covers historical selected person and no-account fallback; combined path is statically traced | Optional focused fixture if future changes split these branches; no confirmed uncovered production defect here |

These are scope/validation limits, not unresolved defect candidates within the stated repair frontier.

## Report Self-Check

- yes: Every finding appears in both Complete Findings Index and Findings (none).
- yes: Every meaningful recursive candidate in the repair frontier appears in Detection Coverage Ledger.
- yes: Every Finding B# ledger row maps to a finding card (none).
- yes: No Not covered rows remain inside the stated scope.
- yes: Severity counts match the finding cards (0 high / 0 medium / 0 low).
- yes: Recommendation follows mechanical mapping (complete focused scope and no findings => Pass).

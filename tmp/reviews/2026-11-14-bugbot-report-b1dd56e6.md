# Bugbot Report

## Report Contract

- Report type: `bugbot`
- Report ID: `bb-20261114-b1dd56e6`
- Review date: `2026-11-14`
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-b1dd56e6.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: `Fixed review input; do not rewrite during repair`

## Scope

- Diff mode: `branch changes`, focused independent follow-up after the historical-author repair.
- Requested base: actual PR #4 `main`, verified with `gh pr view 4`.
- Resolved baseline: `027eaa2e1c113d4345ca82711215cbed4281be45`; also the merge base.
- Reviewed target: actual remote PR head and clean local tracked working tree at `db04fe884b43f7551eba6aeccbe84af026f077d6`.
- Changed files: `81` in the complete base-to-head PR inventory; the production/test/runner repair since the prior full review affects `3` files. Three additional new paths in that last commit are immutable prior review reports.
- Completion: `Complete within reviewed scope`.
- Custom instructions: use fresh independent context; review the fix and affected contracts, preserve source/Git state, write three fresh reports, no NAS/business database access and no performance testing.
- Assumptions: this is a focused post-fix re-review, not a claim that every unchanged PR path was independently re-audited from scratch. The previous full-PR report `bb-20261114-51b745ba`, bound to `74b9de20b24638cdd1e9fd89948eacbba2724b18`, remains the evidence for unchanged areas. The actual full-PR inventory below makes that distinction explicit. Ignored `.local-validation` work is excluded.

## Detection Snapshot

- Recommendation: `Pass`.
- Completion: `Complete within reviewed scope`.
- Why now: no new introduced production defect was confirmed in the repair; the prior source-account historical-author finding is closed through the real evidence HTTP path.
- Findings count: `high 0 | medium 0 | low 0`.
- Must-review now: `None`.
- Coverage confidence: `high` for the repaired participant/payload contract; no new full real-data acceptance claim.
- Biggest blind spot: NAS/real-data reconciliation and the previously existing profile-only author case are outside this repair's validation.

## Complete Findings Index

No introduced production bugs identified in the reviewed scope.

## Findings

None.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Actual PR base/head and repair frontier | PR #4 and last-commit diff | Reviewed - no bug found | Remote base/head match local; merge-base is actual base; tracked tree and initial non-ignored untracked inventory were clean | `gh pr view 4`, `git merge-base`, `git status --short`, `git ls-files --others --exclude-standard` |
| Prior B1: historical payload loses participants | legacy.rs:275-278; connections.rs:456-479 | Reviewed - no bug found | Participants now exist in both returned tuple members; non-current detail metadata retains the selected payload participants | Independent fresh-source HTTP regression passes |
| Original sender survives correction | legacy.rs:232-261; legacy_tests.rs:334-384 | Reviewed - no bug found | Revision 1 uses baseline/original CRM attribution rather than today's message sender | Test returns original-sender and saved original_person after live source link points elsewhere |
| Intermediate revision with sparse saved row | legacy.rs:238-263 | Reviewed - no bug found | Selected immutable content's sender_external_id resolves the source identity when saved projection has no sender field | Revision 2 fixture has sparse saved_row and returns intermediate-sender |
| Current revision follows present person association | legacy.rs:232-263 | Reviewed - no bug found | Current message sender is selected and SQL resolves current source_account_links | Revision 3 returns current-sender/current_person |
| External fallback provider isolation | legacy.rs:262-263; migrations/0007_domain.sql:12-20 | Reviewed - no bug found | Provider filter plus unique(provider,external_id) prevents cross-provider fallback or duplicate participants | Schema and importer account contract inspected |
| Missing/null attribution fallback | legacy.rs:238-261; scripts/legacy-chatlog-import.py:251-321 | Reviewed - no bug found | Explicit null remains unknown; sparse projection can recover sender from selected content; original CRM fallback is not mixed into a different known external author | Source inspection of real saved_row and content creation; focused test covers the sparse case |
| Revision persistence shape | scripts/legacy-chatlog-import.py:251-263,294-321; scripts/legacy-import.py:450-466 | Reviewed - no bug found | Corrected versions save merged projection; selected content saves sender_external_id; original CRM evidence retains source fields | Dependency trace only; scripts unchanged by repair |
| Auth, pagination, original file access | connections.rs:440-517; legacy.rs:285-341; legacy_tests.rs:207-430 | Reviewed - no bug found | Repair does not bypass authenticated handler or bounded original body/file handling | Same real HTTP test also verifies body offset, missing revision, auth, range and path escape |
| Runner pin and migration CI wiring | .github/workflows/ci.yml:8,44-54 | Reviewed - no bug found | Test job now uses same fixed Ubuntu version as existing web job; PostgreSQL16 disposable contract remains wired after binary build | Configuration inspected; no unsupported claim about full remote CI completion |
| Regression cost and usefulness | legacy_tests.rs:293-384 | Reviewed - no bug found | Extends an existing SQLx test, uses synthetic IDs and three distinct author boundaries, adds no services or sleeps | Independent focused test passes in 1.09 s after 38.38 s compilation |
| Prior review files | tmp/reviews/* | Not production-relevant | Immutable audit documents do not execute | Not recursively reviewed as production code |

## Evidence Appendix

### Diff Inventory

The complete actual PR inventory is included for SHA binding. Rows marked unchanged were covered by the prior full review and are not presented as newly audited in this focused repair follow-up.

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| `.github/workflows/ci.yml` | modified | Actively re-reviewed: test runner pin and PostgreSQL migration-test entry point |
| `apps/web/components/feed/event-dialog.tsx` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/feed/types.ts` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/imports/import-progress.tsx` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/imports/imports-view.tsx` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/imports/legacy-migrations.tsx` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/messages/conversation.module.css` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/messages/conversation.tsx` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/settings/model-settings.tsx` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/sources/sources-view.tsx` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/components/ui/primitives.tsx` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `apps/web/lib/api.ts` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-core/src/ai.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-core/src/ai_memory.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-core/src/linking.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/ai.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/ai_browser.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/ai_settings.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/auth.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/connections.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/legacy.rs` | added | Actively re-reviewed: selected-revision attribution, participant query and payload contract |
| `crates/monica-server/src/legacy_tests.rs` | added | Actively re-reviewed: original/intermediate/current attribution and relinking regression |
| `crates/monica-server/src/link_graph.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/main.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/media.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/messages.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/pages/directory.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/pages/engagement.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/pages/metrics.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/pages/overview.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/profile_read.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/profile_relationships.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/profiles.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `crates/monica-server/src/timeline.rs` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `docs/NAS_ISOLATED_DEPLOYMENT.md` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `docs/NAS_MIGRATION_COVERAGE.md` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `infra/Caddyfile.rust` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `infra/nas.compose.yaml` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0053_isolated_legacy_migration.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0054_legacy_evidence.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0055_source_history_owners.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0056_legacy_relationship_owners.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0057_unknown_source_group_roles.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0058_legacy_content_locations.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0059_legacy_relationship_manual_overrides.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0060_offline_chatlog_snapshots.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0061_legacy_attribution_settlement.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0062_chatlog_recovery_evidence.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0063_memory_legacy_address_lookup.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0064_chatlog_interaction_versions.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `migrations/0065_chatlog_archive_catalog.sql` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/collector-chatlog.py` | modified | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/legacy-chatlog-import.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/legacy-chatlog-schema.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/legacy-import.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/migration-file-copy.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/nas-migration-copy.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/nas-migration-download.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/nas-migration-export.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/test-legacy-import.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/test-migration-file-copy.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `scripts/test-nas-migration-copy.py` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tmp/reviews/2026-11-13-bugbot-report-2355f418.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-13-bugbot-report-426a15f4.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-13-bugbot-report-8f55023a.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-13-bugbot-report-db09f455.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-13-code-review-excessive-tests-2355f418.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-14-bugbot-report-51b745ba.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-14-code-review-51b745ba.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-14-code-review-db09f455.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-14-excessive-tests-51b745ba.md` | added | Prior immutable review artifact; not production-relevant |
| `tmp/reviews/2026-11-14-excessive-tests-db09f455.md` | added | Prior immutable review artifact; not production-relevant |
| `tools/migration-media/README.md` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tools/migration-media/go.mod` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tools/migration-media/go.sum` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tools/migration-media/licenses/SILK-Skype-BSD.txt` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tools/migration-media/licenses/chatlog-MIT.txt` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tools/migration-media/licenses/go-lame-MIT.txt` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tools/migration-media/licenses/go-silk-MIT.txt` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tools/migration-media/main.go` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |
| `tools/migration-media/main_test.go` | added | Unchanged since prior full PR review; inventoried to bind this follow-up to the actual PR |

### Verification Commands

- `gh pr view 4 --json baseRefName,baseRefOid,headRefName,headRefOid,url`: remote base `027eaa2e1c113d4345ca82711215cbed4281be45`, remote head `db04fe884b43f7551eba6aeccbe84af026f077d6`.
- `git merge-base HEAD 027eaa2e1c113d4345ca82711215cbed4281be45`: resolves `027eaa2e1c113d4345ca82711215cbed4281be45`.
- `git diff 74b9de20b24638cdd1e9fd89948eacbba2724b18 db04fe884b43f7551eba6aeccbe84af026f077d6 -- crates/monica-server/src/legacy.rs crates/monica-server/src/legacy_tests.rs .github/workflows/ci.yml`: reviewed the complete production/test/runner repair.
- Read the full affected evidence function, generic detail/version callers, schema uniqueness constraints and actual CRM/Chatlog importer saved-row/content contracts.
- `python3 scripts/test-cargo.py --locked -p monica-server imported_messages_expose_bounded_evidence_and_safe_original_files -- --nocapture`: **1 test passed, 0 failed**, runtime **1.09 s** after fresh compilation **38.38 s**. The runner created a socket-only temporary PostgreSQL16 under `/tmp`, passed that explicit URL to Cargo and stopped/deleted it afterward. No existing database was used.
- `cargo fmt --all --check`: **pass**.
- No source or Git mutation by reviewer. No NAS access, production service action, business database reads/writes, external comments or performance tests.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Reusing today's sender in all selected versions | dismissed | Historical attribution now comes from selected presentation/content, and three distinct authors are asserted |
| Rebinding an old source account silently rewrites historical profile | dismissed | Non-current query takes saved profile instead of live source_account_links; regression uses deliberately different saved/live profiles |
| Sparse saved_row makes all intermediate authors disappear | dismissed | Immutable payload sender_external_id fallback is now exercised |
| Existing profile-only imported author | not production-relevant to this repair frontier | Before and after db04fe8, source-account lookup cannot represent a message with only sender_profile_id and no source account/external ID. This is not a newly introduced repair defect. It was separately communicated to parent; check real-data prevalence before treating this focused fix approval as all-format acceptance |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| Entire unchanged PR implementation | This run is the fresh independent post-fix review, not a duplicate full audit | Prior immutable full-PR report bb-20261114-51b745ba plus this SHA-bound follow-up |
| Profile-only legacy messages | Existing limitation is unchanged and has no sampled real-data evidence here | Parent's authorized real-data type/count inspection; add a focused fallback/regression if present in release scope |
| Full NAS migration, media bytes and production acceptance | Reviewer was prohibited from NAS/business databases | Parent's authorized deployment acceptance and checksum/catalog reconciliation |
| Entire remote CI suite | Not re-run to assess this focused fix; parent monitors it separately | CI completion on the exact reviewed SHA |

These are explicit validation/scope limits, not uncovered candidates within the repair frontier.

## Report Self-Check

- yes: Every finding appears in both Complete Findings Index and Findings (none).
- yes: Every meaningful recursive candidate in the repair frontier appears in Detection Coverage Ledger.
- yes: Every Finding B# ledger row maps to a finding card (none).
- yes: No Not covered rows remain inside the stated focused scope.
- yes: Severity counts match the finding cards (0 high / 0 medium / 0 low).
- yes: Recommendation follows the mechanical mapping (no findings, complete focused scope => Pass).

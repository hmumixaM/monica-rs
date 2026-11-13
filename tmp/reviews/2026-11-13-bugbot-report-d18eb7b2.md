# Bugbot Report

- Report type: `bugbot`
- Report ID: `bb-20261113-d18eb7b2`
- Review date: `2026-11-13`
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-13-bugbot-report-d18eb7b2.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: Fixed review input; do not rewrite during repair

## Scope

- Diff mode: branch changes
- Requested base: main
- Resolved baseline: 58e2719
- Reviewed target: working tree plus non-ignored untracked source
- Changed files: 37
- Completion: Incomplete — final package, browser fixes and final regression results still pending
- Custom instructions: implement MCP, local password recovery, monitoring and UI integration; no NAS; retain performance fixes
- Assumptions: local synthetic validation only; Instagram stays disconnected

## Detection Snapshot

- Recommendation: Changes requested
- Completion: Incomplete — final package and browser checks pending
- Why now: two telemetry contracts need repair before final validation.
- Findings count: high 0 | medium 2 | low 0
- Must-review now: B1, B2
- Coverage confidence: medium
- Biggest blind spot: final native package and real-provider execution

## Complete Findings Index

| ID | Severity | Category | Location | Title |
| --- | --- | --- | --- | --- |
| B1 | medium | LOGIC_BUG | crates/monica-core/src/connections.rs:226 | Signed pages are counted twice |
| B2 | medium | LOGIC_BUG | migrations/0051_import_activity.sql:50 | Journal completion claims query readiness |

## Findings

### B1 medium - Signed pages are counted twice

- Category: LOGIC_BUG
- Location: crates/monica-core/src/connections.rs:226-229
- Confirmation selector: B1

Description: commit_page writes page telemetry for every call. Signed connection imports already have a receipt tracked by the new batch trigger, so one input produces both batch and page counts.

Rationale: both telemetry paths were introduced in this diff; existing commit_connection_chunk calls commit_page.

Evidence:
- imports.rs:746 calls connections::commit_page for signed connection batches.
- connections.rs:226 inserts page telemetry and the new import_batches trigger separately inserts a batch row.
- Daily/totals sum both kinds, inflating imported records and pending stages.

### B2 medium - Journal completion claims query readiness

- Category: LOGIC_BUG
- Location: migrations/0051_import_activity.sql:50-54
- Confirmation selector: B2

Description: copying a connection batch's complete state and processed_count advertises query readiness before its independent source outbox has been delivered.

Rationale: the new UI labels complete as query-ready, but the original batch contract completes after journal persistence.

Evidence:
- commit_connection_chunk appends revisions/outbox and returns before separate connection delivery.
- track_import_batch copies status/processed_count directly; import-progress maps complete to 已可查询.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Double tracking | connections.rs:226 | Finding B1 | Exclude signed pages from page telemetry | imports::commit_connection_chunk |
| Readiness stages | 0051_import_activity.sql:50 | Finding B2 | Fence completion on pending outbox | bounded delivery contract |
| MCP auth and dispatch | mcp.rs | Reviewed - no bug found | explicit allowlist, scoped state, per-request key authentication | three focused tests, SDK execution pending |
| Password reset | auth.rs/runtime.rs | Reviewed - no bug found | actor retained, sessions revoked, control middleware reused | focused reset test; native package pending |
| Counters and monitor | import_monitor.rs/0052 | Reviewed - no bug found | source/account scope, bounded snapshots, whitelisted strings | final full regression pending |
| UI and native package | web/native desktop | Not covered | browser submit fixes and package validation still in progress | finish browser + packaged runtime checks before final review |
| Baseline read performance | scheduler/cache/query code | Not covered | no final benchmark yet | compare release build to main baseline |
| Review documentation | docs/tmp/reviews | Not production-relevant | evidence only | exclude reports from recursive production review |

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| Cargo.lock | modified/added | auth, persistence, API, UI or tests |
| apps/desktop/collectors/collector.py | modified/added | auth, persistence, API, UI or tests |
| apps/native-desktop/src-tauri/src/native_control.rs | modified/added | auth, persistence, API, UI or tests |
| apps/native-desktop/src-tauri/src/shell.rs | modified/added | auth, persistence, API, UI or tests |
| apps/native-desktop/src-tauri/src/supervisor.rs | modified/added | auth, persistence, API, UI or tests |
| apps/native-desktop/src/main.tsx | modified/added | auth, persistence, API, UI or tests |
| apps/native-desktop/src/styles.css | modified/added | auth, persistence, API, UI or tests |
| apps/native-desktop/tests/settings.spec.ts | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/ask/ask-view.tsx | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/ask/ask.module.css | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/imports/imports-view.tsx | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/imports/imports.module.css | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/settings/settings-view.tsx | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/sources/sources-view.tsx | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/sources/sources.module.css | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/ui/primitives.tsx | modified/added | auth, persistence, API, UI or tests |
| apps/web/tests/pages-real-api.spec.ts | modified/added | auth, persistence, API, UI or tests |
| crates/monica-core/src/connections.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-core/src/lib.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-core/src/scheduler_maintenance.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-server/Cargo.toml | modified/added | auth, persistence, API, UI or tests |
| crates/monica-server/src/auth.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-server/src/imports.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-server/src/main.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-server/src/runtime.rs | modified/added | auth, persistence, API, UI or tests |
| docs/api-registration-audit.json | modified/added | evidence only |
| apps/web/components/imports/import-progress.tsx | modified/added | auth, persistence, API, UI or tests |
| apps/web/components/settings/mcp-settings.tsx | modified/added | auth, persistence, API, UI or tests |
| crates/monica-core/src/import_monitor.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-server/src/import_status.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-server/src/mcp.rs | modified/added | auth, persistence, API, UI or tests |
| crates/monica-server/src/mcp_tests.rs | modified/added | auth, persistence, API, UI or tests |
| migrations/0050_mcp_access.sql | modified/added | auth, persistence, API, UI or tests |
| migrations/0051_import_activity.sql | modified/added | auth, persistence, API, UI or tests |
| migrations/0052_collection_monitor.sql | modified/added | auth, persistence, API, UI or tests |
| scripts/test-mcp-client.mjs | modified/added | auth, persistence, API, UI or tests |
| scripts/test-mcp-http.py | modified/added | auth, persistence, API, UI or tests |

### Verification Commands

- git diff 58e2719; git ls-files --others --exclude-standard
- focused MCP/restore/progress PostgreSQL tests: 3 passed before this repair
- original frozen source HTTP differential: 27 observations passed
- c0007f5 journal/projection/HTTP differential: 48+6 observations passed

### Dismissed Or Merged Candidates

Host validation test initially appended a duplicate Host field; correcting the fixture exercised the intended SDK boundary successfully.

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| Final browser/package/performance | implementation validation in progress | finish tests, real package checks, benchmark and final review |
| Personal providers | no new login or NAS access | authorized existing local sessions only; record unavailable sessions |

## Report Self-Check

- yes: Every finding appears in index and findings.
- yes: Meaningful candidates appear in ledger.
- yes: Finding rows resolve to cards.
- yes: Not covered rows name next steps; completion is Incomplete.
- yes: Severity counts match.
- yes: Recommendation follows mapping.

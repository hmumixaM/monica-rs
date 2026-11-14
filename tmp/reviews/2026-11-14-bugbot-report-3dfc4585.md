# Bugbot Report

## Report Contract

- Report type: `bugbot`
- Report ID: `bb-20261114-3dfc4585`
- Review date: `2026-11-14`
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-3dfc4585.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: `Fixed review input; do not rewrite during repair`

## Scope

- Diff mode: `branch changes`, focused historical/current evidence attribution follow-up with newly supplied real-data shape evidence.
- Requested base: actual PR #4 `main`.
- Resolved baseline: `027eaa2e1c113d4345ca82711215cbed4281be45`.
- Reviewed target: `db04fe884b43f7551eba6aeccbe84af026f077d6`.
- Changed files: 81 in the actual complete PR inventory, including review documents; this report's executable frontier is legacy evidence attribution and its importer/current message contracts.
- Completion: `Complete within reviewed scope`.
- Custom instructions: persist the confirmed profile-only author defect before parent starts authorized repair; no source/Git edits, NAS or business-database access by reviewer, external comments or performance tests.
- Assumptions: the parent supplies an authorized read-only aggregate from the actual Mac copy: **22 message_search rows have sender_profile_id IS NOT NULL and sender_account_id IS NULL**. This reviewer did not access that database or inspect personal records. This newly supplied data-shape evidence expands the focused scope after immutable report `bb-20261114-b1dd56e6` was completed. The earlier report remains fixed; this report supersedes its release recommendation for the expanded attribution scope.

## Detection Snapshot

- Recommendation: `Changes requested`.
- Completion: `Complete within reviewed scope`.
- Why now: the source-account-only participant query drops valid explicitly linked authors that are present in the real migrated data.
- Findings count: `high 0 | medium 1 | low 0`.
- Must-review now: `B1` (the parent refers to this as the next issue, B2; report-local IDs start at B1).
- Coverage confidence: `high` for the source-account-only query and importer propagation; real-row prevalence is parent-supplied aggregate evidence.
- Biggest blind spot: no direct real database validation by reviewer.

## Complete Findings Index

| ID | Severity | Category | Location | Title |
| --- | --- | --- | --- | --- |
| B1 | medium | LOGIC_BUG | [crates/monica-server/src/legacy.rs:262-263](/Users/maxhu/play/monica-rs/crates/monica-server/src/legacy.rs:262) | Preserve explicit person-only message authors |

## Findings

### B1 medium - Preserve explicit person-only message authors

- Category: `LOGIC_BUG`.
- Location: [crates/monica-server/src/legacy.rs:262-263](/Users/maxhu/play/monica-rs/crates/monica-server/src/legacy.rs:262).
- Confirmation selector: `B1`.

Description: the new legacy evidence participant query is anchored on `crm.source_accounts`, so an imported message whose known author is represented only by `sender_profile_id` produces an empty participant array when no source-account/external-ID lookup matches. The code computes the explicit profile but only places it inside a row that must first exist in source_accounts; the current branch additionally ignores that explicit profile in favor of `l.profile_id`. The message list still renders the author through `coalesce(m.sender_profile_id,l.profile_id)`, while the new evidence link loses it or shows the account-linked person instead of the explicit message author.

Rationale: legacy-import.py intentionally preserves sender_profile_id whenever source sender_account_id is null, so this is a valid migrated shape, not corrupted input. The parent confirmed **22 such rows** in the actual Mac copy using an aggregate read-only query. The new legacy evidence route introduced in this PR must handle the importer’s preserved explicit-person contract. db04fe8 fixes source-account historical attribution but leaves this separate branch unsupported. Current explicit sender overrides also have an established precedence contract in the existing message reader.

Evidence:

- `legacy.rs:262-263` selects only `FROM crm.source_accounts a ... WHERE a.provider=$5 AND (...)`; an explicit profile cannot independently produce a participant. Current `CASE WHEN $3 THEN l.profile_id ELSE $4::uuid END` always discards an explicit current profile.
- `scripts/legacy-import.py:463` writes `sender_profile_id=None if r["sender_account_id"] else r["sender_profile_id"]`; profile-only source authors remain valid rows in the new copy.
- `crates/monica-server/src/messages.rs:124` uses `coalesce(m.sender_profile_id,l.profile_id)` for author identity and joins profiles accordingly, so the message reader's existing contract gives an explicit message person precedence.
- `apps/web/components/sources/sources-view.tsx:165` renders only `metadata.participants` for author identity; empty participants hide the preserved author from the new original-evidence view.
- Parent's authorized aggregate evidence supplied after the previous fixed report: `count(*)` of actual Mac `message_search` rows with nonnull sender_profile_id and null sender_account_id is **22**. No personal records were supplied to reviewer.
- Recommended repair: build a participant from the explicitly selected saved/current profile even if no source account exists; for current revisions, prefer explicit message.sender_profile_id and fall back to the present account link. Preserve historical saved-person semantics. Extend the existing HTTP regression with profile-only original/current paths and an explicit current profile differing from the live account link.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Valid profile-only imported author | legacy.rs:262-263; legacy-import.py:450-466 | Finding B1 | Source-account-only lookup cannot emit the preserved explicit author | Static SQL/transform proof plus parent-supplied 22-row aggregate |
| Explicit current-person precedence | legacy.rs:262-263; messages.rs:124 | Finding B1 | Current CASE uses only live link and ignores explicit message profile | Existing message reader establishes explicit profile precedence |
| Historical source-account sender repair | legacy.rs:232-278 | Reviewed - no bug found | Selected immutable participants now survive generic evidence detail handler | Independent source-built HTTP test passed before receipt of new aggregate evidence |
| Current/historical link separation | legacy_tests.rs:293-384 | Reviewed - no bug found | Existing three-revision test verifies source-account variants and historical saved person | 1 passed, 0 failed, 1.09 s test time |
| Profile-only test gap | legacy_tests.rs:293-384 | Reviewed - no bug found | Existing newly extended regression does not represent this distinct author form | Extend same HTTP test; do not remove source-account regressions |
| Prior report artifacts and ignored validation work | tmp/reviews/*; .local-validation/* | Not production-relevant | Documents remain immutable; ignored work excluded | No recursion into audit reports as runtime code |

## Evidence Appendix

### Diff Inventory

The complete 81-path actual PR inventory is recorded in immutable `bb-20261114-b1dd56e6`. This evidence-expansion follow-up actively reviews:

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| crates/monica-server/src/legacy.rs | added in PR | Sender/profile participant derivation and immutable payload |
| crates/monica-server/src/legacy_tests.rs | added in PR | Existing HTTP regression and missing explicit-profile branch |
| scripts/legacy-import.py | added in PR | Preservation of profile-only source attribution |
| crates/monica-server/src/messages.rs | modified in PR | Existing explicit profile precedence and visible message author |
| apps/web/components/sources/sources-view.tsx | modified in PR | Evidence participant rendering |
| tmp/reviews/* | added review artifacts | Not production-relevant; fixed evidence only |

### Verification Commands

- Actual PR base/head were verified by `gh pr view 4` before the first fixed report; local and remote head `db04fe884b43f7551eba6aeccbe84af026f077d6` agree.
- Read the actual current SQL, importer transform and message author query; no source or Git edits.
- Independent `python3 scripts/test-cargo.py --locked -p monica-server imported_messages_expose_bounded_evidence_and_safe_original_files -- --nocapture`: **1 passed**, **1.09 s** after **38.38 s** compile, temporary synthetic PostgreSQL16 cleaned automatically. That test covers source-account authors, not the newly confirmed profile-only shape.
- `cargo fmt --all --check`: pass.
- Parent-supplied read-only real-data count only; reviewer made no NAS/business database connections.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Earlier source-account historical payload omission | dismissed | Prior B1 source-account case is repaired and independently tested |
| Treat missing profile-only participants as invalid source data | merged into B1 | Importer explicitly preserves this valid form, and real-data aggregate shows it is present |
| Separate issue for current explicit profile priority | merged into B1 | Same participant derivation root cause discards the explicit message-person contract |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| Full real-row identity/content | Reviewer prohibited from real business databases | Parent handles authorized real-data acceptance; aggregate suffices to establish shape prevalence |
| Post-repair behavior | Source repair has not started; report must exist first | Parent adds fallback/precedence regression and obtains another fresh independent review of resulting SHA |
| Unchanged wider PR and NAS release | Focused evidence expansion, not a repeated full audit | Previous full report plus exact-head follow-ups and parent's authorized deployment checks |

## Report Self-Check

- yes: Every finding appears in both Complete Findings Index and Findings.
- yes: Every meaningful candidate in this expanded attribution frontier appears in Detection Coverage Ledger.
- yes: Every Finding B1 row maps to the single finding card.
- yes: No Not covered candidates remain within stated scope.
- yes: Severity counts match the finding card (0 high / 1 medium / 0 low).
- yes: Recommendation follows mechanical mapping (medium => Changes requested).

# Bugbot Report

## Report Contract

- Report type: `bugbot`
- Report ID: `bb-20261114-0fe904d1`
- Review date: `2026-11-14`
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-0fe904d1.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: `Fixed review input; do not rewrite during repair`

## Scope

- Diff mode: `branch changes`, a specifically requested fresh independent detection of the sender settlement defect and its causally connected import/read/verification contracts.
- Requested base: the introduction of the suspected settlement SQL (`d1e08cf`), rather than only the most recent PR #4 merge delta.
- Resolved baseline: `027eaa2e1c113d4345ca82711215cbed4281be45` (`d1e08cf^`).
- Reviewed target: local `main` at `2db879447000276195cf89641ace9ebbc1dae839`, including the working-tree/untracked inventory observed before detection. Tracked tree is clean.
- Changed files: `122` in the full historical base-to-target inventory; this deliberately narrow request actively reviews six changed paths listed below and the specified attribution frontier, not the full historical change set.
- Completion: `Complete within reviewed scope`.
- Custom instructions: trace preservation of original/manual person ownership; persist detection before repair; no source edits, Git mutations, NAS/real database access, external comments, or performance tests.
- Assumptions: this report is a narrow defect confirmation, not a fresh merge-safety approval of all 122 historical files. The suspect SQL was introduced by `d1e08cfcf0a3aa1ccef5574f51a309e6e2daa061`, inherited through PR #3, and remains unchanged at the target. It was not introduced by the PR #4 first-parent merge delta. Parent-supplied real-data diagnostics are corroborating context, not independently inspected evidence.

## Detection Snapshot

- Recommendation: `Block`.
- Completion: `Complete within reviewed scope`.
- Why now: the production finalizer was independently reproduced clearing a valid explicit person attribution after the real Chatlog correction method adds a source account.
- Findings count: `high 1 | medium 0 | low 0`.
- Must-review now: `B1`.
- Coverage confidence: `high` for the confirmed SQL/import/read causal path.
- Biggest blind spot: number of affected real rows and post-import manual history were not independently inspected.

## Complete Findings Index

| ID | Severity | Category | Location | Title |
| --- | --- | --- | --- | --- |
| B1 | high | LOGIC_BUG | [crates/monica-server/src/legacy.rs:378](/Users/maxhu/play/monica-rs/crates/monica-server/src/legacy.rs:378) | Preserve explicit people during attribution settlement |

## Findings

### B1 high - Preserve explicit people during attribution settlement

- Category: `LOGIC_BUG`
- Location: [crates/monica-server/src/legacy.rs:378](/Users/maxhu/play/monica-rs/crates/monica-server/src/legacy.rs:378)
- Confirmation selector: `B1`

Description: The settlement query clears `sender_profile_id` for every legacy message whose current projection has both a source account and person. This confuses a source-derived person cache with an original explicit person attribution. A valid CRM message can originally have `sender_account_id = NULL` and a non-null `sender_profile_id`; the CRM importer preserves that explicit person, and Chatlog subsequently fills only the source account. The finalizer then deletes the still-authoritative person. Read paths use `coalesce(message.sender_profile_id, source_account_link.profile_id)`, so the visible author becomes the account's currently linked person, or unknown when unlinked. Running finalization can also clear a later non-null manual person override because the query has no original-value or projection-override guards.

Rationale: The batch was introduced in `d1e08cf`. The same change already normalizes original account-derived attribution in `scripts/legacy-import.py:463` by setting person to NULL only when the *original CRM account* is non-null. Looking only at the *current* account during repeated settlement widens that normalization to explicit original values. Chatlog `finish()` calls this finalizer after corrections, making the damaging order an ordinary import path. This violates the supplied requirement to preserve user ownership and manual values, and is material persisted attribution loss rather than a cosmetic discrepancy.

Evidence:

- `legacy.rs:378` joins evidence only to establish that a message is legacy; it never reads `e.original`, the selected Chatlog baseline, or any override state before unconditionally clearing the person.
- `scripts/legacy-import.py:463-466` maps an account-null/person-non-null original to the same person in `crm.message_search`, and saves the complete original record in `crm.legacy_message_evidence.original`.
- `scripts/legacy-chatlog-import.py:354-372` adds known `sender` / `sender_account_id` to the correction but does not include `sender_profile_id` in `new`. `correct()` at lines 318-326 captures only keys in `new` into `original_row/current_row`, updates only those columns, and leaves the current explicit person intact. A sparse projection therefore does not imply absence of an explicit person.
- `scripts/legacy-chatlog-import.py:157-169` invokes the production `legacy-finalize` command after applying the Chatlog snapshot.
- `scripts/legacy-import.py:192-200` overlays only keys in the Chatlog current projection when rebuilding expected CRM rows during verification. When the sparse projection omits person, expected person remains the original explicit person. Clearing it causes the strict CRM verifier to fail; Chatlog verification does not compare it because that field is not in its correction map.
- `crates/monica-server/src/messages.rs:124`, `legacy.rs:252-262`, and memory read paths give explicit `m.sender_profile_id` precedence over current account links. Removing the explicit value changes author ownership on those surfaces.
- Independent synthetic PostgreSQL 16 run called the real `Importer.correct()` with an original person-only row and then executed the exact SQL extracted from `legacy.rs`. Result: `rows_changed=1`, original person was present before settlement, current person became NULL after settlement, effective person switched to a different linked person, sparse projections omitted person, and immutable evidence still retained the original person. No real database was used.

Repair direction: Remove the blanket pass or gate any compatibility cleanup on proven original account-derived attribution plus unchanged expected current values and absence of manual/profile projection overrides. Restore already-affected values in a new migration or bounded offline settlement from immutable original evidence only for the proven person-only case; preserve any non-null current person and any explicit profile key in projection, including JSON null. Match the current account/sender to the recorded correction before backfilling. Do not edit already-applied migration files. If arbitrary manual NULL writes have no provenance, SQL cannot distinguish them from historical clearing; limit the repair to the known offline import state or use additional evidence rather than claiming that provenance is known.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Current account used as derived-attribution proof | legacy.rs:378 | Finding B1 | Clears explicit original/manual ownership | Real SQL reproduced in isolated PostgreSQL 16 |
| CRM original attribution mapping | scripts/legacy-import.py:463-466 | Reviewed - no bug found | Original person-only values preserved; original account-derived value already omitted | Mapper and immutable evidence inspected |
| Chatlog correction updates and sparse projections | scripts/legacy-chatlog-import.py:316-326,354-372 | Reviewed - no bug found | Correctly retains person column; projections deliberately capture only changed keys | Real correct() method used in reproduction |
| Finalizer invoked after Chatlog corrections | scripts/legacy-chatlog-import.py:157-169 | Finding B1 | Normal import ordering reaches destructive query | Caller traced to main.rs legacy-finalize dispatch |
| CRM vs Chatlog verification disagreement | scripts/legacy-import.py:192-200; scripts/legacy-chatlog-import.py:364-369 | Finding B1 | CRM compares explicit person; Chatlog checks only new-key subset | Comparison contracts inspected |
| Current and historical attribution precedence | legacy.rs:235-262; messages.rs:124 | Reviewed - no bug found | Current read paths already prefer explicit person; immutable historic evidence remains recoverable | Source and SQL inspected |
| Existing attribution test coverage | legacy_tests.rs:321-400; scripts/test-legacy-import.py:95-155,235-270 | Reviewed - no bug found | Tests cover direct read precedence and account-based import but omit person-only -> known account -> finalize ordering | Focused missing regression identified; no test removal suggested |
| Backfill overriding manual/profile projection values | prospective repair only | Reviewed - no bug found | No repair exists yet; preservation guards required in fix | Repair direction above; latest implementation requires a separate fresh review |
| Existing review artifacts in untracked inventory | tmp/reviews/*61b07f50.md | Not production-relevant | Immutable audit reports do not execute | Excluded from recursive production review |
| Other historical changed paths | remaining 122-file inventory | Not production-relevant | Outside the explicitly requested narrow settlement-defect review scope | No full-PR approval is made |

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| crates/monica-server/src/legacy.rs | added since baseline | Settlement SQL and explicit/current/historical evidence reads |
| crates/monica-server/src/legacy_tests.rs | added since baseline | Existing manual attribution coverage and missing settlement order |
| scripts/legacy-chatlog-import.py | added since baseline | Correction projection, saved versions, finish caller, field comparisons |
| scripts/legacy-import.py | added since baseline | Original mapping, immutable evidence and strict verify expected rows |
| scripts/test-legacy-import.py | added since baseline | Synthetic original schema and existing end-to-end import coverage |
| migrations/0061_legacy_attribution_settlement.sql | added since baseline | Partial index predicates; no mutation itself |
| crates/monica-server/src/main.rs; crates/monica-server/src/messages.rs; crates/monica-core/src/ai.rs; crates/monica-core/src/ai_memory_legacy.rs | supporting reads only | Finalize dispatch and current person precedence |
| Other historical changed paths and existing tmp/reviews/* artifacts | outside requested narrow scope / not production-relevant | Inventory acknowledged; no full historical review claim |

### Verification Commands

- `git rev-parse HEAD`, `git status --short`, `git ls-files --others --exclude-standard`: target `2db879447000276195cf89641ace9ebbc1dae839`; no tracked modifications before detection; three prior untracked review reports.
- `git show --no-patch --format='%H%n%P%n%s' HEAD`, `git blame -L 370,387 crates/monica-server/src/legacy.rs`, `git show --no-patch ... d1e08cf`: merge target and true introducing parent confirmed.
- `git diff --name-only 027eaa2e1c113d4345ca82711215cbed4281be45 HEAD | wc -l`: 122 historical paths; selected narrow surfaces above inspected.
- Read exact production mapping, correction, finalizer caller and verifier code with `rg`, `sed`, and `nl`.
- Inline Python via `/tmp/monica-rs-contract-venv/bin/python`: created a fresh PostgreSQL 16 cluster under an automatically cleaned `monica-bb-sender-*` temporary directory using trust auth and no TCP listener. Created only synthetic minimal CRM tables; dynamically loaded current `scripts/legacy-chatlog-import.py`; called its production `Importer.correct()`; extracted the literal batch SQL from `legacy.rs` and executed it. Assertions passed for all six outcomes in B1. Cluster was stopped and directory removed in `finally`.
- Synthetic output: `{"repro":"CONFIRMED","production_correct_called":true,"production_finalizer_SQL_executed":true,"before_explicit_person_preserved":true,"after_explicit_person_cleared":true,"effective_person_switched_to_source_link":true,"projection_omits_profile":true,"immutable_evidence_preserved":true,"rows_changed":1}`.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Chatlog correction itself clears explicit person | dismissed | Correction field map omits person; direct reproduction preserves it until finalizer SQL runs. |
| Historical/raw original evidence destroyed | dismissed | Reproduction confirms original evidence unchanged; loss is in current projection and its ownership precedence. |
| Verification mismatch is harmless bookkeeping | merged into B1 | Reproduced current effective author changes; strict CRM verification correctly detects a material value loss. |
| Account link reassignment is the defect | dismissed | Dynamic account attribution is intentional when no explicit person exists. Blanket deletion wrongly removes the higher-priority explicit value. |
| Duplicate independent bug for manual non-null overrides | merged into B1 | Same unguarded query and same preservation failure. |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| Real affected row count and original/manual provenance | No NAS or real data accessed by design | Parent's read-only diagnostic and post-repair complete CRM verification |
| Complete Rust finalizer CLI and full migration schema reproduction | Synthetic run exercised exact damaging production query plus real correction method, not all unrelated finalizer work | Focused integration regression through actual CLI after repair |
| Later explicit manual NULL without metadata | Current projection does not independently prove intent of arbitrary NULL assignment | Additional persisted provenance or restricting backfill to known offline pre-use state |

## Report Self-Check

- yes Every finding appears in both Complete Findings Index and Findings.
- yes Every meaningful recursive candidate within the requested scope appears in Detection Coverage Ledger.
- yes Every Finding B1 ledger row maps to the B1 finding card.
- yes No in-scope material candidate is left Not covered; limitations are explicit.
- yes Severity counts match the one high finding card.
- yes Recommendation Block follows the skill's mechanical mapping.

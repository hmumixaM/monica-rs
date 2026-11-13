# Bugbot Report

## Report Contract

- Report type: bugbot
- Report ID: bb-20261113-727e65bf
- Review date: 2026-11-13
- Report path: tmp/reviews/2026-11-13-bugbot-report-727e65bf.md
- Repository root: /Users/maxhu/play/monica-rs
- Artifact status: Fixed review input; do not rewrite during repair

## Scope

- Diff mode: branch changes; user-requested committed repair follow-up.
- Requested base: origin/main; latest repair delta3f2c15f..bb69b39.
- Resolved baseline: fetched origin/main and branch merge-base301241bf41e8ba8bb18d981f4e9138fe220e9c18. Previous branch-frontier reviews remain the historical chain, rather than being relabeled as a fresh whole-repository sweep.
- Reviewed target: committed bb69b395803daf1c36f27f226e4ae437585907f3. Working tree and untracked inventory were clean before this report.
- Changed files: four in the committed repair delta (two production/test, two review artifacts).
- Completion: Complete within the selected receipt-replay repair frontier and its causally connected callers/state transitions.
- Custom instructions: Review after commit, repair confirmed defects, preserve real services/data, apply bugbot/code-review/excessive-tests.
- Assumptions: Existing account-free acceptance boundaries apply. Platform performance and final cleanup are execution gates, not evidence of extra code bugs.

## Detection Snapshot

- Recommendation: Pass
- Completion: Complete within reviewed scope
- Why now: The committed repair removes the reproduced writer-lock dependency while retaining atomic first admission and signed-request identity checks.
- Findings count: high0 | medium0 | low0
- Must-review now: None
- Coverage confidence: high for this bounded frontier
- Biggest blind spot: Full NAS logical/benchmark acceptance is still executing; live-provider, personal permissions, vendor security and production migration remain outside this incremental code review.

## Complete Findings Index

No introduced production bugs identified in the reviewed scope.

## Findings

None.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Committed receipt fast path | core imports.rs115-134,154-158 | Reviewed - no bug found | Plain SELECT checks the immutable original hash without locking mutable progress | Old-code regression failed; final held-writer/read-only/xmin regression passed in both timezones. Actual new NAS signed retry and both held-lock paths passed before an unrelated private negative-fixture type error. |
| Concurrent first acceptances | core imports.rs159-168 | Reviewed - no bug found | Unique winner owns input; loser reads and validates winner in a fresh statement | Deterministic test observes the loser lock before winner commit and verifies exactly one receipt/input. All current callers use ordinary READ COMMITTED. |
| Atomic input/media progress | core imports.rs169 onward, migrations0008/0041 | Reviewed - no bug found | Only winning admission writes input/progress, in the same transaction as receipt | Uncommitted winner is invisible; rollback permits the waiting first insert to win. Existing media progress is seeded by its migration and immutable input cannot be rewritten. |
| Hash/nonce/HMAC/token/provider boundaries | server imports.rs accept; adapter publish | Reviewed - no bug found | Existing checks still precede receipt reuse; callers propagate errors and roll back | Signed HTTP import regression in exact-head CI passes; mismatch is rejected by the same helper on both receipt lookup paths. No timeout/security budget changed. |
| First-admission SQL overhead | new indexed lookup | Reviewed - no bug found | One extra indexed query per batch; no per-record loop | Six new release Mac cases completed with zero case/read failures.1x throughput target remains unmet and must be reported, not hidden. |
| Regression value and determinism | append_only.rs24-135 | Reviewed - no bug found | Two distinct lock/read-only and first-admission race boundaries; existing four tests retained | UTC/Chicago focused6 each; exact-head full CI; full repaired workspace177 each timezone before final test-only strengthening. No sleep-based simultaneous-race assumption. |
| Immutable detection and repair notes | tmp/reviews two additions | Not production-relevant | Fixed prior report preserved; repair notes identify reproduced root cause and limits | No recursive production review of review artifacts. |

## Evidence Appendix

### Diff Inventory

| Path | Change status in3f2c15f..bb69b39 | Production surfaces considered |
| --- | --- | --- |
| crates/monica-core/src/imports.rs | modified | Existing and racing first acceptance, immutable identity, atomic inputs/media progress |
| crates/monica-core/tests/append_only.rs | modified | Actual progress lock, read-only reuse, unchanged xmin, strict hash check and deterministic concurrent first insert |
| tmp/reviews/2026-11-13-bugbot-report-53370ca3.md | added | Not production-relevant; immutable source finding |
| tmp/reviews/2026-11-13-receipt-replay-repair.md | added | Not production-relevant; scoped repair/verification record |

### Verification Commands

- git fetch origin main; git merge-base HEAD origin/main:301241bf41e8ba8bb18d981f4e9138fe220e9c18.
- git diff3f2c15f..bb69b39 and caller/schema/progress-writer inspection; no untracked nonignored files before this report.
- Focused final append-only tests UTC and America/Chicago:6 passed each; original production implementation failed the held-writer case.
- Repaired full workspace:177 passed each timezone,0failed,3defaultignored; explicit video1 and performance2 passed. Final test-only strengthening passed the focused matrix and Clippy.
- Exact-head CI43437516559: all six jobs passed. Complete Mac package rebuilt at clean bb69b39 and verified, and native ARM64 Chicago178 including video passed.
- New NAS app exact signed durable retry and two uncommitted lock-holder paths passed. Private negative test supplied an object where cursor_after requires a string and correctly received422; fixture corrected for409 hash-conflict check. Subsequent crash/full NAS tests remain pending at this fixed review snapshot.
- Six new release Mac performance cases completed; original baseline provenance retained. No claim that all original performance targets pass.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Concurrent losing INSERT sees stale statement snapshot | dismissed | Follow-up SELECT is a separate READ COMMITTED statement, explicitly forced by the deterministic race test. |
| Existing receipt returns before preserving its input | dismissed | Every receipt/input is originally committed atomically; migration0041 backfills media progress. No independent receipt insertion or input deletion exists in this frontier. |
| Read-only replay test hides an unnecessary committed rewrite | dismissed | Successful RW replay now commits before xmin comparison; separate read-only transaction also succeeds. |
| Private negative request422 proves application regression | dismissed | cursor_after is Option<String>; a generated object is invalid. Valid different-body conflict must use a string, with original failure retained. |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| Final NAS execution/performance readiness | Long-running gates are still executing | Finish physical restart/recovery, native NAS logical matrices and24 fresh benchmark cases; record limitations and failures. |
| Personal/live-provider and vendor audit | Outside synthetic receipt-repair scope | Separately authorized provider sessions, permissions and dependency/security acceptance. |

## Report Self-Check

- yes Every indexed finding has a matching card (none).
- yes Every meaningful recursive candidate is in the ledger.
- yes Every FindingB# resolves to a card (none).
- yes No material production candidate is left Not covered within this bounded frontier; external execution gates are stated separately.
- yes Counts match high0,medium0,low0.
- yes Mechanical recommendation is Pass.

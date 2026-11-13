# Bugbot Report

## Report Contract
- Report type: `bugbot`
- Report ID:`bb-20261113-a8f8460d`
- Review date:2026-11-13
- Report path:`tmp/reviews/2026-11-13-bugbot-report-a8f8460d.md`
- Repository root:`/Users/maxhu/play/monica-rs`
- Artifact status: Fixed review input; do not rewrite during repair.

## Scope
- Diff mode: branch changes, PR1 actual base93b07a8b93603ad26279da0b9bba9c645258bba7 and head37b07dc8d548ab373c65d7cd1fc1c32f876f262a.
- Completion: Complete within reviewed scope; supplements initial337b890d source review with new runtime evidence.
- Custom instructions: fix import/read performance and re-review before merge; no NAS.
- Assumptions: generated local workload only.

## Detection Snapshot
- Recommendation: Changes requested.
- Completion: Complete within reviewed scope.
- Why now: release-scale execution exposed a confirmed costly plan in the new outbox query.
- Findings count: high0 | medium1 | low0.
- Must-review now:B1.
- Coverage confidence: high.
- Biggest blind spot: NAS timing intentionally excluded.

## Complete Findings Index
| ID | Severity | Category | Location | Title |
|---|---|---|---|---|
| B1 | medium | PERFORMANCE_ISSUE | crates/monica-core/src/ai_memory_legacy.rs:30-35 | Outbox batch repeats message scans |

## Findings
### B1 medium - Outbox batch repeats message scans
- Category: PERFORMANCE_ISSUE
- Location: crates/monica-core/src/ai_memory_legacy.rs:30-35
- Confirmation selector:B1

Description: Joining the unrestricted owner view to a1000-ID array allows a nested-loop plan that reevaluates the selected messages once per profile. This adds population-dependent scans to the ingestion transaction.

Rationale: This bulk outbox query is newly introduced; earlier inline sync did not execute it. A faster durable acceptance does not offset slower full completion.

Evidence:
- Actual release10k-message run: main facts query-ready3.854s but all Memory complete25.789s versus historical18.44s inline baseline.
- Live isolated100k run pg_stat_statements:60 outbox calls19.44s, dominant query.
- Read-only equivalent EXPLAIN ANALYZE saved queue-before-plan.json:4274ms, message-search bitmap scan10000loops and100000 account-link lookups for1000 IDs; profile outer loop100 repeats the bounded subquery.

## Detection Coverage Ledger
| Area | Entry | Status | Result | Evidence |
|---|---|---|---|---|
| Bulk outbox | ai_memory_legacy.rs:30-35 | Finding B1 | repeated scans | runtime plan |
| Memory ownership/fences | imports.rs and maintenance | Reviewed - no bug found | both current/previous owners retained, atomic pending fence |337b890d + scheduler test |
| Remaining source diff: schema/index/epoch, timeline, cache/auth/pools, benchmark/tests | all paths in337b890d inventory | Reviewed - no bug found | original trace remains applicable; actual successful HTTP bodies match | source review + HTTP512requests |
| Review documents | tmp/reviews | Not production-relevant | fixed artifacts | excluded |

## Evidence Appendix
### Diff Inventory
Complete18-file inventory and all changed entry points: immutable337b890d report. Head37b07dc matches that implementation; only new production finding is the queue plan.
### Verification Commands
- `git diff 93b07a8 37b07dc` and source reads.
- Release HTTP probe:7 baseline-success bodies identical, new directory successful where old timed out.
- Benchmark first case complete;100k case deliberately interrupted after diagnosis and controller cleaned its owned local cluster. Interrupted results will not be used as final measurements.
### Dismissed Or Merged Candidates
None beyond prior report ledger.
### Blind Spots
NAS physical timing unverified by explicit user restriction; not a source-review gap.

## Report Self-Check
- yes: finding indexed and one matching card.
- yes: recursive areas accounted for.
- yes: B1 ledger resolves.
- yes: no uncovered production candidate.
- yes: medium1 matches card.
- yes: recommendation follows mapping.

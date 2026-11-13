# Bugbot Report

## Report Contract
- Report type:`bugbot`
- Report ID:`bb-20261113-40dd8b58`
- Review date:2026-11-13
- Report path:`tmp/reviews/2026-11-13-bugbot-report-40dd8b58.md`
- Repository root:`/Users/maxhu/play/monica-rs`
- Artifact status: Fixed review input; do not rewrite during repair.

## Scope
- Diff mode: branch changes including working tree/untracked inventory.
- Requested base: PR1 main.
- Resolved baseline:`93b07a8b93603ad26279da0b9bba9c645258bba7` (fresh fetch).
- Actual PR head:`5feb9f932f3533b684544136ad22566ebc3371a5`.
- Reviewed local target:`41ac3a963cb401fe90f810e330f4e61ff6d9ee69`; only additional local commit41ac3a9 preserves an earlier review document. Application source identical to actual PR head.
- Changed tracked files:25; no untracked production files.
- Completion: Complete within reviewed scope.
- Custom instructions: fix/review/re-review, commit and merge authorized; no NAS connection/testing/deployment; retain data/pagination and macOS/NAS feature behavior.
- Assumptions: generated local performance measurements; no NAS-speed inference or new paid/model service calls.

## Detection Snapshot
- Recommendation: Pass.
- Completion: Complete within reviewed scope.
- Why now: previous owner-plan findings are fixed using batch-before-eligibility selection; all meaningful diff-derived contracts reviewed and scale acceptance complete.
- Findings count: high0 | medium0 | low0.
- Must-review now: None.
- Coverage confidence: high.
- Biggest blind spot: physical NAS latency intentionally outside scope.

## Complete Findings Index
No introduced production bugs identified in the reviewed scope.

## Findings
None.

## Detection Coverage Ledger
| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
|---|---|---|---|---|
| Indexes and epoch migration | 0049:1-71 | Reviewed - no bug found | Actual predicates, ordering and partial filters match reads; sharded committed epochs bounded32/domain, subject tokens retained, deferred flush atomic with rollback and sorted lock order. Metrics global watermark bounded. | Source trace + final tests/plans/benchmark |
| Canonical Memory view split | 0049:73 onward | Reviewed - no bug found | Object/payload UNION byte-identical to0047 (programmatically checked). Shared eligible-profile view preserves person/archive/self rules; current view columns unchanged. Existing core/HTTP Memory tests pass. | Source trace + final tests/plans/benchmark |
| Batch Memory selection and reducer | 0049; ai_memory_legacy.rs | Reviewed - no bug found | Only requested IDs materialized before eligibility; owner/input cleanup, locking, hashes and generation unchanged. Both owner sets fenced atomically. No per-object population join. Generic10k-profile/100k-message probe verifies entire1000-object payload sets equal; selected10.878ms. | Source trace + final tests/plans/benchmark |
| Ingest facts/progress/retries | imports.rs | Reviewed - no bug found | Only normal scheduler path defers; direct core APIs retain immediate Memory contract. Atomic facts/outbox/progress and retry/quarantine retained. | Source trace + final tests/plans/benchmark |
| Scheduler/resource/lifecycle | scheduler.rs; scheduler_maintenance.rs | Reviewed - no bug found | Productive loop releases resources and yields, idle/error backoff only; bounded independent maintenance respects mode/admission/drain/task timeout. Future retry does not starve deterministic Memory recovery. | Source trace + final tests/plans/benchmark |
| Timeline SQL | timeline.rs | Reviewed - no bug found | Limit after all filters and before media hydration; eligible IDs retain original manual/source ownership predicate. Same cursors, NULL ordering, data fields, readiness and metadata; actual router regression passes. | Source trace + final tests/plans/benchmark |
| Read-cache and aggregate HTTP | pages/cache.rs; cache.rs | Reviewed - no bug found | Fresh auth first; committed tokens, hard invalidation and ETags preserved. Only4 aggregates may stale<=30seconds, refresh cadence5s,2 slots/15s timeout and bounded CPU/memory; guard cancellation traced. | Source trace + final tests/plans/benchmark |
| Background read pool | auth.rs; main.rs; pages.rs; engagement.rs; metrics.rs | Reviewed - no bug found | Internal typed extension selects background pool only in refresh; no client override; default constructor compatible. | Source trace + final tests/plans/benchmark |
| Benchmark and readiness | benchmark-backends.py | Reviewed - no bug found | Setup includes attribution recovery after linking. Query-ready and all-required timings distinct; final settlement waits Memory pending0 and preserves monitoring/error accounting. All6 final cases pass; no acceptance-only speed claim. | Source trace + final tests/plans/benchmark |
| Tests and coverage cost | five added tests | Reviewed - no bug found | Focused actual-DB/router coverage of rollback/local invalidation, auth/archive/refresh, admission/stale bounds, durable import/retry/Memory recovery and timeline filter/cursor semantics. No new framework, dependency or shared fixture redesign. | Source trace + final tests/plans/benchmark |
| Report artifacts | tmp/reviews | Not production-relevant | Historical fixed reports excluded from production recursion; later runtime findings are documented rather than rewriting prior Pass reports. | Source trace + final tests/plans/benchmark |

## Evidence Appendix
### Diff Inventory
| Path | Change status | Production surfaces considered |
|---|---|---|
| `crates/monica-core/src/ai_memory_legacy.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-core/src/imports.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-core/src/scheduler.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-core/src/scheduler_maintenance.rs` | A | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-core/tests/scheduler.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-server/src/auth.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-server/src/cache.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-server/src/main.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-server/src/pages.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-server/src/pages/cache.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-server/src/pages/engagement.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-server/src/pages/metrics.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `crates/monica-server/src/timeline.rs` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `migrations/0049_read_access_paths.sql` | A | Reviewed contracts, persistence, async effects, callers and tests |
| `scripts/benchmark-backends.py` | M | Reviewed contracts, persistence, async effects, callers and tests |
| `tmp/reviews/2026-11-13-batch-memory-fix-review.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-benchmark-phase-review.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-bugbot-report-0582e388.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-bugbot-report-337b890d.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-bugbot-report-a8f8460d.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-bugbot-report-dbc5f3d1.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-memory-reducer-review.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-outbox-fix-review.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-read-import-bottlenecks.md` | A | Not production-relevant: immutable review/history |
| `tmp/reviews/2026-11-13-read-import-code-review.md` | A | Not production-relevant: immutable review/history |

### Verification Commands
- Fresh fetch/merge-base and actual PR base/head; full changed-hunk and new-file reads; canonical payload-view branch equality checked.
- Final batch implementation: workspace182passed/3ignored, Clippy warnings denied and release build passed. Earlier implementation also passed182 in UTC/Chicago. Optional historical acceptance remains separately labeled.
- Release API benchmark6/6 complete,3rounds at10k/100k messages: all-required medians2.170697s/21.310110s; pending Memory0 in every case; warm/mixed errors0.
- Current large-population batch probe:10k profiles/100k messages/1000 IDs; forced generic plan; full payload-set equality true. selected10.878ms; per-object7.717ms and legacy bulk5.966ms on this larger population. This microfixture proves bounded semantics/plan, not a universal speedup claim. Full API100-contact workload measures the planner-sensitive import improvement.
- Read HTTP probe37b07dc:401 baseline attempts(395success/6timeouts),451new attempts(451success);7 common successful JSON bodies equal. Read implementation unchanged through5feb9f9; new Memory-view semantics covered by final tests.
- Local Mac bundle rebuild/actual runtime verification and current remote CI are recorded separately when complete.

### Dismissed Or Merged Candidates
| Candidate | Decision | Reason |
|---|---|---|
| Per-object lookup is sufficient for every plan | merged into earlier0582e388/B1 | runtime generic plan demonstrated repeated small-population scans; repaired with batch selection |
| Duplicated privacy/payload policy | dismissed | one canonical objects projection plus one eligibility view; byte-equal payload branches |
| Deferred work hidden outside timing | dismissed | setup costs explicit, Memory settlement explicit; interrupted diagnostic attempts retained separately |
| Stale identity/archive or unauthorized response | dismissed | auth before lookup and hard-token bypass, real router regression |
| Media filtering after pagination | dismissed | EXISTS before LIMIT and focused actual endpoint test |
| Unbounded refresh tasks or leaked resource slot | dismissed |2 slots,15s timeout and RAII cleanup |

### Blind Spots
Physical NAS timing intentionally not measured; no uncovered diff-derived production candidate. Python baseline measurements are historical, pinned, and not rerun or relabeled.

## Report Self-Check
- yes: index and cards agree (none).
- yes: recursive candidates accounted for.
- yes: every Finding B# maps to a card (none).
- yes: no Not covered production rows.
- yes: severity counts match.
- yes: Pass follows mechanical mapping.

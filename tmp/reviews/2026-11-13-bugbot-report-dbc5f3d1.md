# Bugbot Report

## Report Contract
- Report type:`bugbot`
- Report ID:`bb-20261113-dbc5f3d1`
- Review date:2026-11-13
- Report path:`tmp/reviews/2026-11-13-bugbot-report-dbc5f3d1.md`
- Repository root:`/Users/maxhu/play/monica-rs`
- Artifact status: Fixed review input; do not rewrite during repair.

## Scope
- Diff mode: branch changes.
- Requested base: PR1 main.
- Resolved baseline:`93b07a8b93603ad26279da0b9bba9c645258bba7`.
- Reviewed target: PR1 latest head`7793351dbc038479b0e4faa165b8bce0f2b3751c`, confirmed via gh pr view; working tree clean before this report.
- Changed files:22.
- Completion: Complete within reviewed scope.
- Custom instructions: user requested fixes, skill review, commit and merge main; no NAS testing/access or Python-service changes.
- Assumptions: performance measurements are local generated data; physical NAS performance not inferred.

## Detection Snapshot
- Recommendation: Pass.
- Completion: Complete within reviewed scope.
- Why now: previous B1 owner-plan issue repaired and affected reducer/harness changes re-reviewed; no unresolved introduced defect.
- Findings count: high0 | medium0 | low0.
- Must-review now: None.
- Coverage confidence: high for source contracts; final benchmark repetitions recorded separately.
- Biggest blind spot: physical NAS timing deliberately outside scope.

## Complete Findings Index
No introduced production bugs identified in the reviewed scope.

## Findings
None.

## Detection Coverage Ledger
| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
|---|---|---|---|---|
| Object outbox | ai_memory_legacy.rs:30-39 | Reviewed - no bug found | Scalar per-object lookup retains authoritative current-owner view and old owners; conflict unions all pending owners. Initial B1 bulk flattening removed. | Source trace, existing/full tests and release probes |
| Bounded materialization | 0049_read_access_paths.sql:76-102 | Reviewed - no bug found | DISTINCT IDs plus correlated LATERAL OFFSET0 prevent batch/profile flattening; hashing, deletion, locks and generation updates identical to0042. | Source trace, existing/full tests and release probes |
| Facts/progress and retries | imports.rs | Reviewed - no bug found | Only scheduler resume defers Memory. Public direct append/resume remain synchronous. Facts and outbox/progress share transaction; bounded retry/quarantine unchanged. | Source trace, existing/full tests and release probes |
| Pause, drain, resource and task lifecycle | scheduler.rs; scheduler_maintenance.rs | Reviewed - no bug found | One bounded maintenance lane, resource release before media acquisition, mode/admission check and Active coverage retained. Productive chunks immediately yield; only idle/error backoff. Future retries excluded from starvation check. | Source trace, existing/full tests and release probes |
| Indexes and epochs | 0049_read_access_paths.sql:1-71 | Reviewed - no bug found | All8 access paths match actual filters/order. Committed subject tokens retained; one epoch shard/domain/transaction and bounded32/domain global read; rollback/backfill/lock order traced. | Source trace, existing/full tests and release probes |
| Timeline and ownership | timeline.rs | Reviewed - no bug found | Candidate limit after filters, before attachment hydration; original ownership retained after eligible-ID join; media-ready equivalent EXISTS, NULL cursors and metadata fields retained. | Source trace, existing/full tests and release probes |
| HTTP auth, cache, ETags | pages/cache.rs; cache.rs | Reviewed - no bug found | Fresh authentication first; only4 aggregate routes with compatible hard hash and<=30s age,5s refresh cadence,2 refresh slots/15s timeout. Names/archive invalidation immediate; original conditional response retained. | Source trace, existing/full tests and release probes |
| Connection pools | auth.rs; main.rs; pages.rs; engagement.rs; metrics.rs | Reviewed - no bug found | Private typed extension carries background pool; clients cannot select it. Foreground reads retain interactive pool; refresh admission/resources released on cancellation. | Source trace, existing/full tests and release probes |
| Benchmark phases and correctness | benchmark-backends.py | Reviewed - no bug found | Setup settles attribution work after links, included in setup_seconds. No queue deletion; full ingest settlement includes Memory pending0; query-ready separately measured. Historical results not relabeled. | Source trace, existing/full tests and release probes |
| Tests and suite cost | five added tests | Reviewed - no bug found | Actual DB/router tests cover atomic invalidation, auth/archive, bounded stale/task policy, durable backlog/retry/Memory recovery and timeline cursor/filter semantics. No framework/dependency/fixture redesign. | Source trace, existing/full tests and release probes |
| Review artifacts | tmp/reviews | Not production-relevant | Immutable evidence/history; not production behavior | inventoried below |

## Evidence Appendix
### Diff Inventory
| Path | Change status | Production surfaces considered |
|---|---|---|
| `crates/monica-core/src/ai_memory_legacy.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-core/src/imports.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-core/src/scheduler.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-core/src/scheduler_maintenance.rs` | A | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-core/tests/scheduler.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-server/src/auth.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-server/src/cache.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-server/src/main.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-server/src/pages.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-server/src/pages/cache.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-server/src/pages/engagement.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-server/src/pages/metrics.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `crates/monica-server/src/timeline.rs` | M | Production callers, SQL state, error/resource paths and tests traced |
| `migrations/0049_read_access_paths.sql` | A | Production callers, SQL state, error/resource paths and tests traced |
| `scripts/benchmark-backends.py` | M | Production callers, SQL state, error/resource paths and tests traced |
| `tmp/reviews/2026-11-13-benchmark-phase-review.md` | A | Not production-relevant: fixed review/research artifact |
| `tmp/reviews/2026-11-13-bugbot-report-337b890d.md` | A | Not production-relevant: fixed review/research artifact |
| `tmp/reviews/2026-11-13-bugbot-report-a8f8460d.md` | A | Not production-relevant: fixed review/research artifact |
| `tmp/reviews/2026-11-13-memory-reducer-review.md` | A | Not production-relevant: fixed review/research artifact |
| `tmp/reviews/2026-11-13-outbox-fix-review.md` | A | Not production-relevant: fixed review/research artifact |
| `tmp/reviews/2026-11-13-read-import-bottlenecks.md` | A | Not production-relevant: fixed review/research artifact |
| `tmp/reviews/2026-11-13-read-import-code-review.md` | A | Not production-relevant: fixed review/research artifact |

### Verification Commands
- `git diff 93b07a8b93603ad26279da0b9bba9c645258bba7 7793351dbc038479b0e4faa165b8bce0f2b3751c`, full added-file reads, actual PR base/head query and recursive caller/migration traces.
- Full182tests passed with final reducer migration; preceding implementation also passed182 in UTC and America/Chicago. Three optional tests explicitly ignored; historical optional acceptance remains separate.
- Clippy warnings denied passed before SQL-only reducer adjustment; current CI repeats it on latest source.
- Release HTTP before/after:401 baseline attempts (395successes/6timeouts),451new attempts (451successes);7 common successful JSON bodies identical. Directory baseline had no successful body. Unique diagnostic query parameters bypass response caching; DB buffers remain warm.
- Final steady-contact first10k-message run: query-ready1.739s, all-required settlement2.105s with Memory pending0; three-round repetitions pending final evidence report.
- Benchmark parser regression passed; no source-text matching tests added.

### Dismissed Or Merged Candidates
| Candidate | Decision | Reason |
|---|---|---|
| Deferral only hides work | dismissed | pending references are durable and full settlement explicitly includes them; setup costs recorded separately |
| Person privacy changes reuse stale data | dismissed | hard-token mismatch bypass plus actual archive/auth regression |
| LATERAL duplicates IDs or drops multi-owner activity | dismissed | DISTINCT IDs matches ANY semantics; subquery returns all owners |
| Time/filter pagination loses matching media | dismissed | filter before LIMIT and actual router regression |
| Cancellation leaks refresh slot/budget | dismissed | RAII guards and timeout, existing cancellation test |

### Blind Spots
Physical NAS/production-data timing unmeasured by explicit user request; no source coverage candidate remains unresolved. No optional/model paid service called.

## Report Self-Check
- yes: indexed findings/card counts match (none).
- yes: all meaningful recursive candidates accounted for.
- yes: every Finding B# maps to a card (none).
- yes: no Not covered production rows.
- yes: high0/medium0/low0 match cards.
- yes: Pass follows mechanical mapping.

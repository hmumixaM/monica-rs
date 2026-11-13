# Read/import merge review

Base: origin/main93b07a8b93603ad26279da0b9bba9c645258bba7, fetched2026-11-13. Target: full working tree subsequently frozen in the implementation commit; immutable Bugbot report337b890d records tracked diff digest and inventory.

Applied local code-review and excessive-tests skills. Recommendation: pass; no actionable finding after tracing all changed production areas. The migration, latency-sensitive SQL and background lifecycle were reviewed as high-risk surfaces, beyond compilation/CI.

The actual bottlenecks are population-wide validator hashing, missing entity/profile/order access indexes, timeline hydration before an effective page bound and a fixed per-chunk scheduler sleep. The change addresses these without dropping required response fields or replacing pagination. SQL is parameterized. Timeline candidate selection retains original profile/participant ownership, media/provider/date predicates and NULL cursor behavior. Old and new bodies will be compared separately from latency.

Global invalidation uses committed sharded epochs; per-person dependency subjects remain local. Transactions update the old subject tokens and new domain epochs atomically. Rollback and cross-person invalidation are covered. Only four aggregate routes reuse compatible stale count results for at most30seconds; changed identity/archive tokens bypass this path. Every request authenticates before cache lookup. Refresh admission, memory/CPU budgets, background connection pool and cancellation guards are bounded.

Import facts/progress remain atomic. Scheduler drops admission/resources between productive chunks and backs off only without progress. Heavy Memory materialization is referenced durably with previous/current owners, then drained independently with AI disabled. A future retry does not starve that drain. Media recovery remains bounded; periodic metrics/orphan housekeeping leaves the serial ingestion path. Model-enabled lane may also perform its existing bounded recovery; this does not weaken the pending-input publication fence.

## Test-change assessment

Five added focused tests are proportionate:
- Cache invalidation commit/rollback and unrelated-person locality through PostgreSQL.
- Actual authenticated HTTP aggregate refresh, unauthorized access and immediate archival visibility.
- Cache age/hard-token policy plus bounded refresh admission/cancellation RAII.
- Real durable12-batch ingestion backlog, queued Memory recovery after restart, delayed retry and searchable facts; old scheduler negative control fails the4second bound while patched path passes. Timed bound distinguishes the actual11second regression, rather than enforcing a microbenchmark threshold.
- Actual timeline router across page cursors, NULL dates, manual participant ownership, deleted rows, provider and media-ready filtering before LIMIT.

These protect distinct failure modes. No source-text matching, new mocks/frameworks, dependency changes, redundant large matrix or production DB fixtures were added. The existing suite and isolated cluster runner remain unchanged. Benchmark readiness now waits all pending Memory work, so deferral cannot create an acceptance-only performance claim.

Validation: final workspace182passed/3ignored in UTC and America/Chicago; Clippy with warnings denied and diff whitespace check passed. Release HTTP/performance evidence and remote CI are recorded in the final acceptance document, separately from this code review. NAS excluded by user instruction; no NAS access or Python-service changes.

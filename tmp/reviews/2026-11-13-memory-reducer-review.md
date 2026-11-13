# Bounded Memory reducer review

PR1 base93b07a8, prior head31c38be; affected change is the added replacement memory_sync_legacy function in migration0049. This follows the same confirmed owner-view planner failure exposed by scaled import diagnostics. Initial31c38be exploratory runs:10k query-ready1.89s/full24.25s;100k27.16s/full88.22s. Those attempts are retained but excluded from final benchmark results because final reducer changed afterward.

Applied code-review and excessive-tests to the migration diff and all sync callers. Two owner-view selections now run through DISTINCT input IDs and a correlated LATERAL subquery with OFFSET0 as a flattening barrier. Current attribution/privacy rules still come from the authoritative memory_legacy_current view. All payload hashing, old-owner union, ordered Memory-head locks, owner deletion, input deletion/update and generation advancement remain identical to migration0042. No per-record model calls, copied attribution policies or session-wide planner changes.

Deduplicating IDs preserves prior ANY(array) semantics, including empty arrays. Multiple owners for a shared activity remain intact. The1000-input bound remains unchanged. Existing actual-database Memory tests cover source invalidation, group/self/privacy attribution, relink, rollback, head/publication fencing and input hashes; full workspace is being rerun with this replacement function. No extra unit test is needed merely to assert SQL implementation text. Release-scale benchmark will verify the access-cost improvement.

No actionable introduced defect identified in the affected diff. User authorization covers import optimization and final re-review/merge; NAS remains excluded.

# Batch Memory selection repair review

Immutable source report: bb-20261113-0582e388, B1. PR1 base93b07a8/latest reviewed predecessor7793351. The reported per-object plan is still present before repair; user authorized fixing general read/import performance and re-review before merge.

Repair: split the authoritative existing view into its unchanged object/attribution payload projection and a shared eligible-profile view. The existing memory_legacy_current view still joins these and exposes identical columns and privacy rules. A new internal memory_legacy_selected function materializes only the requested object batch before joining eligibility. The outbox and reducer both use this batch function. No duplication of message attribution or privacy predicates, no client-visible data-shape changes, no planner-wide switches and no per-object population joins.

The object UNION branches are copied verbatim from0047 (payload/speaker/direction/group/correction rules preserved). Eligibility remains person, unarchived and not self. ANY(array) retains deduplication semantics, multiple activity owners remain valid and missing/tombstoned objects still allow old-owner cleanup. Existing sync bounds1000IDs, lock order, hash bytes, owner/input deletion and generation changes are unchanged. Deferred outbox references retain both old and current owners atomically.

Code-review: no remaining actionable defect in this affected view/function/query diff. All sync callers, SQL snapshots and model publication pending fences traced. Materialization bounds payload generation to the batch before the shared population join; large-population generic plans and full-payload equivalence will be recorded separately.

Excessive-tests: existing actual-database Memory tests exercise the replaced function through all import/attribution/privacy paths. Full182tests passed with this repair; Clippy warnings denied passed. No implementation-text unit tests, new frameworks or dependency additions. Scaled query plans and release API benchmark are the appropriate performance proof.

Report-scoped repair itself preserved Git/external state; subsequent commit/push/merge follows the user's separate explicit authorization. No NAS access or testing.

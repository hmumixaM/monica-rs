# Bugbot Report

## Report Contract

- Report type: `bugbot`
- Report ID: `bb-20261113-3cd0296a`
- Review date: `2026-11-13`
- Report path: `/Users/maxhu/play/monica-rs-append-only/tmp/reviews/2026-11-13-bugbot-report-3cd0296a.md`
- Repository root: `/Users/maxhu/play/monica-rs-append-only`
- Artifact status: Fixed review input; do not rewrite during repair

## Scope

- Diff mode: branch changes, explicitly scoped to notebook timeline ingestion, immutable content registration and supporting source/schema definitions listed in the inventory.
- Requested base: origin/main
- Resolved baseline: `301241bf41e8ba8bb18d981f4e9138fe220e9c18`
- Reviewed target: clean worktree at `1dfd446f499b0ca905767aa3f878b29dbf2279b4` before this report.
- Changed files in selected scope: 9
- Completion: Complete within the selected source/replay scope; no whole-repository or live-platform acceptance claim.
- Custom instructions: Re-review append-only suitability of imported facts, especially timeline. Implement immutable source storage and suppress unchanged re-import work. Preserve revisions, explicit corrections/deletions, manual data and identity relinking. Work in a separate checkout without changing the active parent worktree.
- Assumptions: Current materialized CRM views and metadata remain mutable; original observations/revision/content rows should be append-only. Existing live databases and source accounts are out of scope.

## Detection Snapshot

- Recommendation: Changes requested
- Completion: Complete within reviewed scope
- Why now: Unchanged notebook post/story imports still update the current object/profile/timeline and rerun interactions, causing repeated writes and scoped invalidations on stable history.
- Findings count: high 0 | medium 1 | low 0
- Must-review now: B1
- Coverage confidence: medium
- Biggest blind spot: Representative production-size ingestion measurements and live provider validation are not part of this isolated review.

## Complete Findings Index

| ID | Severity | Category | Location | Title |
| --- | --- | --- | --- | --- |
| B1 | medium | PERFORMANCE_ISSUE | [imports_notebook.rs:147](/Users/maxhu/play/monica-rs-append-only/crates/monica-core/src/imports_notebook.rs:147) | Skip unchanged notebook timeline imports |

## Findings

### B1 medium - Skip unchanged notebook timeline imports

- Category: PERFORMANCE_ISSUE
- Location: [crates/monica-core/src/imports_notebook.rs:147](/Users/maxhu/play/monica-rs-append-only/crates/monica-core/src/imports_notebook.rs:147)
- Confirmation selector: B1

Description: `changed == false` only suppresses inserting a notebook revision. The path still upserts the notebook head, locks its profile, increments profile generation, upserts timeline content/last_seen, synchronizes Memory and rewrites embedded interactions. Repeating large stable timeline imports causes unnecessary row versions/WAL and invalidates derived/profile caches despite unchanged source facts.

Rationale: The branch introduced the notebook projection path and its unconditional writes. The existing source journal already treats same-head content as unchanged; notebook timeline imports should likewise avoid semantic no-op projection. Comparing content alone is insufficient: top-level timestamps, profile hints, provider and other record metadata must still trigger a real update.

Evidence:

- `imports_notebook::project` computes `changed` near lines 131-145, then unconditionally writes notebook_objects at line 147 and increments profile generation below it.
- `social` unconditionally upserts timeline_events and last_seen_at at line 378; interactions run for every post even when body is unchanged.
- Existing notebook revision insertion is correctly append-only when content changes; this historical record must be preserved for A→B→A while removing replay writes.
- A regression must compare stable timeline/profile/cache state across new import batches, verify metadata-only changes and A→B→A, and protect two different values of the same object within one batch.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Unchanged notebook timeline imports | imports_notebook::project/social | Finding B1 | Head/profile/timeline/interaction writes repeat on unchanged records | Preserve complete-record fingerprint, avoid replay projection, test metadata and ordered reversion |
| Immutable content lookup still reserves writer locks | content::register_many | Reviewed - no bug found | Previously intentional writer registration; user explicitly requests a read-only reuse path as an enhancement | Reuse immutable addresses with SELECT; retain sorted writer locking for missing/colliding values |
| Source payloads/revisions vs mutable heads | domain::observe; connections::commit_page; source migrations | Reviewed - no bug found | Source facts are inserted; object heads, cursors and outbox acknowledgements are mutable | Add user-requested DB rewrite guards to fact tables only |
| Historical timeline bodies | notebook_social_revisions; connection_social_revisions; source revisions | Reviewed - no bug found | Existing immutable source/revision tables retain changed bodies | Enforce append-only without freezing current read models or breaking source correction/relink |
| Current social projections | connections_social.rs; timeline_events; social_interactions | Reviewed - no bug found | Current rows must change for corrections, status and identity projection; their authoritative facts are separate | Do not reject necessary current-row updates |
| Source-row/cursor coordination | connection locks and generation fences | Reviewed - no bug found | Import admission and cursor/order coordination remains necessary | Do not call ingestion fully lock-free |
| Parent worktree changes | separate /Users/maxhu/play/monica-rs | Not production-relevant | Parent has unrelated media-helper/CI/main changes | Never copy, modify, stage or revert them |

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| `crates/monica-core/src/connections_social.rs` | A | source/persistence/replay contract |
| `crates/monica-core/src/content.rs` | A | source/persistence/replay contract |
| `crates/monica-core/src/imports_notebook.rs` | A | source/persistence/replay contract |
| `migrations/0007_domain.sql` | A | source/persistence/replay contract |
| `migrations/0010_timeline.sql` | A | source/persistence/replay contract |
| `migrations/0027_content_objects.sql` | A | source/persistence/replay contract |
| `migrations/0030_connections.sql` | A | source/persistence/replay contract |
| `migrations/0039_notebook_imports.sql` | A | source/persistence/replay contract |
| `migrations/0044_connection_social.sql` | A | source/persistence/replay contract |

### Verification Commands

- Read actual scoped diff against origin/main merge base and current source/DDL/callers.
- `git status --short` in the new worktree: clean before report.
- Traced notebook record hashing, revision insertion, timeline upsert, field/profile generation and embedded interaction projection.
- Inspected content addressing collision handling/sorted multi-value locks and existing concurrent registration tests.
- No source edits or test execution during detection. New isolated regressions will supply executable repair evidence.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Freeze timeline_events against all changes | dismissed | It is a current materialized projection, including mutable ownership/status; authoritative immutable revisions exist separately |
| Drop all source locks | dismissed | Concurrent imports still need stable identity/order/cursor coordination |
| Require generic profile/notebook replay suppression | dismissed | Mutable field restore and manual precedence have distinct semantics; initial replay optimization is deliberately limited to post/story records |
| Missing fact-table rewrite guards | not production-relevant as a pre-existing bug | An explicitly authorized new invariant, implemented as requested hardening rather than inventing a historical failure |

### Blind Spots

No unresolved candidate within this selected review. Full live platform/provider validation and representative-scale benchmarks remain outside this review's scope; do not infer them from the selected result.

## Report Self-Check

- yes: Indexed finding and card agree.
- yes: Every meaningful scoped candidate appears in ledger.
- yes: Finding row maps to B1.
- yes: No Not covered candidate exists inside selected scope.
- yes: Counts match: high 0, medium 1, low 0.
- yes: Changes requested follows medium-finding mapping.

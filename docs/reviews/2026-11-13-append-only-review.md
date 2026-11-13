# Append-only imported facts and timeline re-review — 2026-11-13

## Scope and isolation

The user requested re-review and implementation of append-only storage for mostly unchanged imported data, particularly timeline. The implementation lives in `/Users/maxhu/play/monica-rs-append-only` on `codex/append-only-source-facts`, based on `1dfd446f499b0ca905767aa3f878b29dbf2279b4`. The main worktree is active in another chat and was not modified, staged or reverted. Its unrelated media-helper/CI/runtime changes were not copied. No commit, push, merge, deployment or production data mutation was performed for this task.

Local `bugbot`, `code-review` and `excessive-tests` were used. The immutable scoped Bugbot input is [bb-20261113-3cd0296a](../../tmp/reviews/2026-11-13-bugbot-report-3cd0296a.md). Its medium finding B1 was the repeated physical projection of unchanged notebook posts/stories. B1 is fixed. Append-only database guards and read-only published-content reuse are separately authorized enhancements, not invented historical bug findings.

## Storage policy implemented

| Layer | Policy | Implementation |
| --- | --- | --- |
| Original source payloads, revisions and observations | Append only; no UPDATE or DELETE | Across seven source schemas: payloads, revisions, observations, revision_metadata, messages, message_corrections, events, connection_payloads and connection_revision_refs |
| Shared original bodies and native/structured/social history | Append only; no UPDATE or DELETE | CRM content_objects, notebook_revisions, notebook_social_revisions, connection_social_revisions and source_structured_revisions |
| Original batch inputs and original-file provenance | Append only; no UPDATE or DELETE | CRM import_batch_inputs and source_blob_refs |
| Current heads, timeline/message/interaction read models, identity maps and manual data | Mutable, with existing transaction/generation/identity rules | Do not confuse a materialized current view with original source facts; corrections, tombstones, relinks and manual edits remain available |
| New import receipt, request archive and cursor | Durable import state | New batches still append their original input and update receipt/cursor state, even when all post/story projections are unchanged |

Migration `0048_append_only_facts.sql` installs UPDATE/DELETE rejection triggers on **70 fact tables**. It does not rewrite existing original rows or modify older migrations. These are runtime invariants against accidental DML, not a separate database-privilege security boundary: the database owner can still perform deliberate maintenance, including TRUNCATE/schema changes. Mutable metadata and read models are deliberately excluded.

## Published bodies are reused without writer reservations

`content::register_many` validates and computes the same legacy and typed-fallback addresses, fetches published matches using SELECT, and checks exact canonical content before reuse. If every object already exists, it performs no INSERT and takes no application advisory writer lock. It works in a PostgreSQL READ ONLY transaction. New or colliding addresses retain the existing sorted writer-lock/unique-insert protocol. Reversed batches, typed string/number collisions and concurrent first publication continue to pass.

This makes immutable stored bodies reusable without repeatedly behaving like new content writes. It is not a claim that PostgreSQL has no internal locks, nor that inserting new facts requires no transaction/index coordination.

## Unchanged timeline replay is a no-op projection

Notebook post/story records have a complete-record fingerprint in their small mutable head. It includes record-level timestamp, profile hints, provider and other metadata, rather than comparing body JSON alone. Batch receipt IDs, batch observation time and cursors do not force the same record to reproject.

An unchanged committed record can finish at the initial SELECT without acquiring its notebook/person locks, registering content, updating timeline/interaction rows, incrementing profile generation or invalidating their caches. A changed candidate keeps the existing object lock and is checked again after admission to handle concurrent identical publication.

The filter advances a simulated head while walking a batch, preserving A→B→A order and suppressing consecutive exact duplicates. Content edits append immutable history; metadata/ownership changes still update the current projection. Existing heads have NULL fingerprints after migration and take one normal import to seal their full record; no historical backfill is guessed.

## Verification

- `cargo fmt --all --check`: passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `sh scripts/test.sh`: **174 passed, 0 failed, 3 ignored**; fresh isolated PostgreSQL, plus actual auth/media/CLI/lifecycle HTTP smoke passed.
- `python3 scripts/notebook-import-differential.py --baseline multisource`: passed **104 database observations across 13 signed batches, plus 2 real HTTP timeline details**, against the fixed ordinary multi-source baseline.

Four focused PostgreSQL regressions were added:

1. Every protected fact relation rejects UPDATE/DELETE, while publishing new content/revisions still works and an attempted original-body overwrite leaves the original intact.
2. Published body reuse succeeds in READ ONLY mode while a competing writer holds its content reservation; typed string/number identities remain distinct.
3. A repeated post import completes while its person and notebook reservation are held by another transaction. Actual xmin/state snapshots verify that the timeline row, notebook head, profile generation, cache versions and interactions are unchanged. Timestamp-only changes and A→B→A within a single batch remain correct.
4. Story replay preserves row versions; owner changes, deletion coverage and restoration update the current view while immutable history preserves the originals.

The broader suite also retained the actual same-person/different-post concurrency gate, source replays, message corrections/deletion/restoration, manual precedence, content lock ordering, relinks, Memory, media, auth and source tests. No existing regression was weakened or removed. The new tests assert database behavior and observable physical no-op/recovery contracts rather than source text or incidental private calls; their coverage is proportionate to the persistence/concurrency changes.

## Final affected-diff review

Reviewed the final source, new migration, all four tests and relevant callers after validation. Exact canonical comparison protects reused hash collisions. Only absent values reserve sorted content locks. The social filter preserves within-batch order and rechecks admitted candidates, while metadata is part of its fingerprint. Current read models and manual/source identity semantics remain mutable. Existing history is neither dropped nor rewritten.

No additional confirmed production defect was identified in this affected diff. The statement is limited to this implementation and its isolated evidence; it is not a full platform/security/performance certification. See [validation hashes and logs](2026-11-13-append-only-validation.json).

# Append-only integration review — 2026-11-13

The user explicitly requested merging and pushing the previously reviewed append-only implementation. Target: the active parent/integration branch `codex/complete-multisource-rust`; source: `codex/append-only-source-facts`.

- Integration parent: `92ae99e4d9c291bd380946571d70cdd48ce9e541`.
- Source implementation: `a1236fed50cfc62de31e01fe5797deb087e735f7`.
- Merge commit and reviewed merged production source: `38b7ff6f9250790b16d1d6130962859d76b58afb`.
- Source merge base: `1dfd446f499b0ca905767aa3f878b29dbf2279b4`.
- Remote default/base at fetch: `301241bf41e8ba8bb18d981f4e9138fe220e9c18` (`origin/main`). No remote PR existed at review time.

The integration branch advanced only in media-helper startup, optional collector CPU configuration, CI, README and their review reports since the source branch split. There was no overlap with the append-only core/migration changes. The merge was conflict-free. The merged implementation/test/migration hashes match the previously reviewed source and [validation manifest](2026-11-13-append-only-validation.json) exactly.

## Review

Reconciled the immutable Bugbot finding B1 and its repair against the actual source commit and merged diff; see [the original report](../../tmp/reviews/2026-11-13-bugbot-report-3cd0296a.md). That report remains unchanged. The prior [append-only review](2026-11-13-append-only-review.md) records the narrow detection scope and four focused regressions.

Code-review rechecked the merged seven-file delta, immutable-fact DML guards, read-only body reuse, hash-collision writer coordination and complete-record replay filtering, including ordering, metadata changes and current materialized projection behavior. No additional confirmed defect was identified in the affected integration diff. Existing source and integration branch acceptance limits remain; this is not an unconditional whole-platform release approval.

Excessive-tests review retained all four new regressions: database mutation protection, read-only reuse under writer contention, physical no-op timeline replay with ordered reversion, and Story owner/retraction/restoration. They exercise distinct persistence and concurrency boundaries; none was weakened or removed.

## Fresh merged-source validation

- `cargo fmt --all --check`: passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `sh scripts/test.sh`: **174 passed, 0 failed, 3 ignored**; generated isolated databases only. Actual authentication, media, CLI ingestion/projection and HTTP lifecycle smoke passed.
- `python3 scripts/test-media-helper.py`: **1 passed**, exercising the integration parent's bounded subprocess startup path.
- Merged implementation hashes checked against the source validation manifest: all match.

Logs remain under `.local-validation/append-only-merge-2026-11-13/`. The integration review adds documentation only after the reviewed merge; it introduces no additional production change. The user authorized a Git push of this integration branch. No deployment or service/data migration is included.

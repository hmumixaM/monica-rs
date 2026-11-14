# Independent excessive-tests review

- Repository: `/Users/maxhu/play/monica-rs`.
- PR: https://github.com/hmumixaM/monica-rs/pull/4 (open draft at remote metadata check).
- Actual PR base / verified merge-base: `027eaa2e1c113d4345ca82711215cbed4281be45`.
- Reviewed latest PR head: `d1e08cfcf0a3aa1ccef5574f51a309e6e2daa061`, branch `codex/nas-isolated-migration`.
- Scope: all 75 changed paths; 4 existing review artifacts excluded as non-production. Tracked tree clean at review start; no non-ignored production additions outside PR.
- Fresh independent context; no inherited parent conversation. All three named local skills read and applied by this reviewer. No source edits, Git mutations, PR comments, SSH or existing business DB access.
- Review started 2026-11-13 and completed after local midnight 2026-11-14. Initial immutable Bugbot report was persisted before prior reports were read.
- Bugbot report: [2026-11-13-bugbot-report-db09f455.md](/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-13-bugbot-report-db09f455.md), ID `bb-20261113-db09f455`. Do not rewrite either report during repair.

Conclusion: **Changed tests are proportionate. No redundant-test deletion or consolidation is recommended.** One focused missing regression is linked to the confirmed production finding, separate from test excess.

## Actionable coverage gap

**Medium — Cross-target checkpoint trust has no regression.** [scripts/test-legacy-import.py:198](/Users/maxhu/play/monica-rs/scripts/test-legacy-import.py:198) creates a new Reader state for every phase; it exercises same-target page replay and explicit reconcile but never reopens the same durable state against a new target. This allows Bugbot B1. Add a small disposable test: convert/finalize target A; apply one extra Chatlog record; convert/finalize target B from the same sources; reuse A's `--state`; assert rejection before treating any local ACK as committed in B, or assert correct complete replay into B. A drop/recreate of the same DB name should also be rejected by incarnation identity. Retain the existing same-target duplicate-page, original-body, correction, revocation, DST and tamper assertions so the repair cannot weaken recovery semantics. No existing test is equivalent to that second-target boundary.

## Changed-test value assessment

| Test area | Observable regression protected | Assessment / retained coverage |
| --- | --- | --- |
| scripts/test-legacy-import.py | Read-only sources and identical/existing target rejection; interrupted committed page; version mapping and repeated content hash; source fingerprints; password/TOTP; read-only zero-space verification; source tampering; Chatlog corrections, source gaps, immutable original, old receipt reconcile, counts and DST fold | Three broader integration scenarios share one class-owned disposable PG process but use fresh synthetic DBs per test. Assertions cover real SQL/persistence and actual finalizer behavior. Retain all; cost was 5.789 seconds here. The new target-state case is distinct missing coverage. |
| crates/monica-server/src/legacy_tests.rs | Same-host different-port cookie and CSRF isolation; saved analysis with execution disabled; evidence/current/original revisions; authentication; file ranges/traversal/symlink; archive catalogue/history pagination and incomplete cursor rejection; ownership relink/manual override | Four HTTP/SQL contracts exercise actual routes and persistence. The catalogue/history additions protect private data and multi-page completeness. Their setup volume is useful integration context, not excess. Retain all. Rust source reviewed; not freshly executed here. |
| scripts/test-migration-file-copy.py | Independent inode and SHA; damaged resumed destination; committed WAL and changed WAL; pause before publish; live writer progress during pinned consistent snapshot | Four distinct data-loss/concurrency boundaries. Controlled callbacks avoid timing sleeps. All passed; retain. |
| scripts/test-nas-migration-copy.py | Missing/unrelated containers never stopped; only exact labelled new copy is paused | Mocking subprocess/Path write at the OS boundary is appropriate. Tests execute the generated remote program and inspect selected target; no source-text matching or real NAS stop occurs. Both passed; retain. |
| tools/migration-media/main_test.go | Marked destination, independent source, path/symlink escape, existing CAS tamper; real pinned V4 codec roundtrip | Two distinct contracts use real transforms/bytes. Go test passed; retain. Voice codec full real-data acceptance remains operational, with original blob preservation separate. |
| scripts/test-collector-chatlog.py (existing relevant coverage) | ACK-before-cursor crash replay; group identity/private binary; missing sender and ACK/path failures; source version handling | 11 existing tests passed. They remain valuable for the changed Reader and are not duplicated by target-specific PG semantics. |
| .github/workflows/ci.yml | Builds server then runs synthetic migration/file/cancel contracts; independently runs the new Go module | Correct runner scopes and no external existing DB dependency for this fixture. Previous Go CI gap is closed. No unnecessary repeated broad suite was added by these hunks. |

## Nonblocking limits and future checks

The full CLI reconcile lifecycle, process interruption after SQL commit/before ACK, and concurrent advisory-lock rejection were reasoned from transaction/lock ownership and partially covered by importer/Reader contracts, not driven through a dedicated CLI test in this round. If B1 repair changes state ownership or lock lifetime, a compact disposable CLI restart test would give useful independent signal. Do not add source-text assertions for chmod/network/SQL strings or duplicate every transform as a mock implementation. NAS runtime UID/archive/download, full source/media hashes and rendering are acceptance work, not substitutes for a static test matrix.

No wider slow/flaky/noisy suite problem was established, so `tests-audit` was not invoked. The work here does not remove coverage or change tests/source/Git. No PR comments were sent.

# Committed Rust Monica re-review — 2026-11-12

The user requested committing the implementation and reviewing it again. Local `bugbot`, `code-review` and `excessive-tests` were applied. No PR exists; the reviewed target is the actual local branch, not a remote PR or CI approval.

## Commits and review scope

- Initial implementation commit: `f7c9f01f15b8724d0d1ed0eb2acbafd80d04f70f`.
- Actual branch baseline / merge base: `301241bf41e8ba8bb18d981f4e9138fe220e9c18` (`origin/main`).
- Fresh detection inventory: 439 changed files; clean worktree before detection.
- Immutable detection report: [bb-20261112-ef56c331](../../tmp/reviews/2026-11-12-bugbot-report-ef56c331.md).
- Repair commit and final reviewed production source: `c2470856342bd2ac3379c8c320673e3c2f3bfd2a`.
- Final affected-diff review: `f7c9f01..c247085`. Inspected the committed source/test diff, state transitions, retry admission, generation fences, storage effects and test assertions after committing the fix. No additional confirmed issue was identified in that affected diff.

The original detection recommendation was **Changes requested** (one medium finding). It remains unchanged in the historical report. That finding is fixed in the repair commit. Whole-platform acceptance remains **incomplete** for the specific external/runtime/vendor boundaries below; this document does not claim an unconditional release approval.

## Confirmed finding and repair

**B1 — WhatsApp disconnect intent lost after a storage failure (medium).** The scheduler classified an account as disconnected only while `!enabled && state == 'disconnected'`. Failure to publish control or delete device keys changed the database state to `error`, so the next attempt treated it as a pause. Device keys could survive an explicit Disconnect and be reused on Resume.

The repair preserves the disconnected state while retaining the error for display and retry. Successful publication/cleanup clears the transient error, fenced by connection generation and disabled/disconnected state. The account remains disabled during recovery. The existing success-path pause test continues to verify that pausing retains keys; the new regression verifies that disconnect erases them after a storage error and restart while retaining account history.

## Executable validation

| Check | Result |
| --- | --- |
| New recovery regression against original committed production source | Failed as expected: persisted `error` instead of `disconnected` after `control.json` failure |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `sh scripts/test.sh` | Passed: 170 tests, 0 failures, 3 ignored |
| New recovery regression in full workspace run | Passed for control publication and device-key deletion failure, with restart/recovery |
| Existing WhatsApp pause/resume/disconnect regression | Passed |
| Real isolated HTTP/CLI smoke from `scripts/test.sh` | Passed: authentication/CSRF/TOTP/recovery; actual PNG decode, previews, limits, Range/If-Range, resumable media and dedupe; ingestion/projection and server lifecycle |

Tests use an isolated PostgreSQL cluster, synthetic device files and temporary media roots. No real WhatsApp account or production data was accessed. The final test waits for the completed filesystem **and** database recovery state with a bounded deadline, rather than assuming both asynchronous effects become visible simultaneously.

Validation hashes, commands and local log locations are recorded in [the machine-readable manifest](2026-11-12-committed-rereview-validation.json). The broader frontend, collector, native lifecycle and Python/Rust differential checks from the earlier [parity review](2026-11-12-rust-parity-review.md) are reused for unchanged surfaces and are not represented as freshly rerun here.

## Code-review and excessive-tests assessment

The production fix is limited to error-state persistence and clearing the error after successful retry. It adds no dependency, schema migration, API shape or new external operation. Parameterized SQL and generation predicates remain in place. Retrying the existing idempotent key cleanup is compatible with partially completed filesystem deletion.

The new test covers two distinct failure entry points sharing one root cause. It exercises the real scheduler, PostgreSQL state, atomic control-file publication and key removal across restart. The existing test separately protects the pause-versus-disconnect contract. These are meaningful persistence/data-retention regressions; no test was removed and no source-text or incidental private-call assertion was added. The test changes are proportionate to the failure and platform boundary.

## Remaining acceptance boundaries

- macOS/NAS shared functionality and Mac-dependent disablement retain the evidence and limits in the earlier feature matrix. This fix does not establish new live platform acceptance.
- Real OAuth/device accounts, collector sessions, macOS permissions, model calls and inference were not exercised.
- Docker daemon is unavailable locally; the Linux/NAS image has configuration/static validation but no runtime acceptance in this session.
- Full independent vendor/dependency security review, dataset audit, representative-scale benchmarks, artifact signing and deployment are outside the completed validation.

The implementation and repair were committed locally at the user's request. No push, merge, deployment or external review comment was performed.

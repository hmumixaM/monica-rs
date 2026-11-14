# Excessive Tests Review — PR #4 Actual Main Merge

- Date: `2026-11-14`.
- PR: https://github.com/hmumixaM/monica-rs/pull/4.
- Actual base / merge-base: `99207dcf1ee28f73e211e2e92b81630decc5ba20`.
- Reviewed exact head: `25dbb6c0abc32969d80a7682c38e0186f81b1930`.
- Skill: local `excessive-tests`, fresh independent context.
- Outcome: **No excessive-test findings. One focused missing regression is required with production finding B1.**
- Immutable artifact; no test/source/Git mutation or external comments.

The changed Python contract extends an existing disposable PostgreSQL fixture. DST-fold matching, JSON-string original preservation, read-only zero-free-space verification, replaying committed receipts, explicit reconciliation of stale attribution, another physical database, and a recreated database with the same name are distinct persistence boundaries. They protect against silent migration omissions, original-content loss or duplicate identities. The new shared SERVER environment path enables the parent to test the intended binary; it does not add another harness. Retained main cases still cover historical NUL/BLOB/Infinity encoding, source absence without invented deletion, a later returning revoke target and stale summary detection.

The Rust test changes reuse the authenticated evidence fixture to inspect current, intermediate and original revision senders and person attribution. The account-relink case, explicit person overriding a link, and profile-only author each protect a different behavior. Archive tests use first and subsequent pages at the real 100-table/25-record boundaries, plus unauthorized and incomplete-cursor requests. These are public HTTP contracts, not incidental private-call or source-text assertions. There are no new sleeps, external business services or disproportionate setup costs, and no deletion/consolidation is recommended.

The actionable coverage gap is at `scripts/test-legacy-import.py:213-231,275-299`: foreign-target tests call `bind_state` directly, while the helper uses its supplied state as the Reader root even for reconcile. Production uses the `bind_reader_target` wrapper and nested `state/reconcile`. Therefore the direct-helper checks can pass while the merged wrapper silently adopts another target's interrupted reconcile-only state. Add a focused test through the production wrapper with an existing old target marker and nested partially ACKed Reader. It must reject a different/recreated target and permit same-target resume without dropping the first page. This is missing behavior coverage tied to [B1](/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-173c83b4.md), not evidence that current target-isolation tests are excessive.

The retained live Reader suite ran independently: 11 passed / 0 failed, 0.199 s. A separate two-contact interrupted-reconcile reproduction confirmed B1 despite those passes. Parent reports 3 migration contracts and 6 HTTP tests passed at this SHA; these were not rerun by this reviewer. CI entry points include the Python migration contract, Rust workspace tests and Reader suite. Remote CI did not start according to the parent because of the Actions budget. Full suite restructuring is unwarranted by this delta.

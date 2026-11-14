# Excessive Tests Review — PR #4 Latest Target-Binding Repair

- Date: `2026-11-14`.
- PR: https://github.com/hmumixaM/monica-rs/pull/4.
- Actual base / merge-base: `99207dcf1ee28f73e211e2e92b81630decc5ba20`.
- Reviewed exact head: `a9d2dc39c21d54fdcb18d7bf02b5358d9c5c1a9e`.
- Skill: local `excessive-tests`, fresh independent context.
- Outcome: **Pass. Changed tests are proportionate. No excessive-test finding or remaining B1 coverage blocker.**
- Immutable artifact; no test/source/Git mutation or external comments.

At scripts/test-legacy-import.py:282-296 the added case constructs the legacy reconcile-only layout with target.json and nested raw, then calls the actual production bind_reader_target wrapper against a different physical database. It checks the rejection and absence of chatlog-target.json. The same-target positive case then checks promotion. The previous wrapper would accept the foreign target and fail the negative assertion. This is observable persistence protection for the exact prior B1 defect, rather than a duplicate check of the already-correct bind_state helper.

The fixture reuses the existing temporary PostgreSQL cluster, source import, snapshot and other database. It adds a directory copy and binding calls with no new database setup, sleeps, broad parameter matrix, mock implementation or helper framework. Parent reports the same three contracts passed in 5.264 seconds. An independent actual Reader interrupted-page reproduction additionally checked nested marker and ACK-only rejection, same-target resume and record count two. That experiment used only synthetic SQLite and identity/receipt result rows; broad suites were not run solely to judge test size.

Other actual-base Python cases protect distinct failures: UTC DST-fold identity, immutable JSON-string originals, read-only verification under zero free space, committed-page replay versus explicit correction, recreated database identity, historical encoding and source absence/revocation recovery. Rust public HTTP cases distinguish current, intermediate and original senders, relinked people, explicit person overrides and profile-only attribution; pagination tests cover real 100-table and 25-record boundaries plus authorization/incomplete cursor. They reuse existing integration fixtures. No equivalent coverage warrants removal or consolidation.

CI already invokes the disposable Python contract, Rust workspace and Reader checks. No test framework or runner changes are introduced by this repair. Remote CI was not checked; deployment acceptance remains outside test-scope review. Broader tests-audit work is not indicated by this delta.

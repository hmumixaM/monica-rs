# Excessive Tests Review — PR #4

- Review date: 2026-11-14
- Actual base: `027eaa2e1c113d4345ca82711215cbed4281be45`
- Reviewed head: `74b9de20b24638cdd1e9fd89948eacbba2724b18`
- Skill: local `excessive-tests`; independent fresh-context review.
- Outcome: **Added tests are proportionate; no excessive-test finding.** One concrete missing regression supports the confirmed production defect in [bb-20261114-51b745ba](/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-51b745ba.md).
- Immutable review artifact. No test/source/Git mutation, no external comments or performance testing.

## Changed coverage assessed

| Test area | Concrete protected contract | Assessment |
| --- | --- | --- |
| `crates/monica-server/src/legacy_tests.rs` | Cookie/CSRF installation separation; historical AI readability with no execution; bounded authenticated message/media/evidence reads; file range/path safety; Chatlog table/history pagination; relationship/tag ownership under relinking | Distinct meaningful boundaries; no demonstrated equivalent retained coverage warrants deletion |
| `scripts/test-legacy-import.py` | Interrupted batch resume, JSON fidelity, password/TOTP preservation, observation revision selection, immutable originals, source/target guards, unknown schema rejection, Chatlog messages/posts/interactions/revokes/source gaps/DST, explicit reconciliation and no counter inflation | Broad integration scenario tests real conversion and PG contracts; setup cost buys substantial data-loss coverage |
| Latest target-binding assertions in `scripts/test-legacy-import.py:260-280` | Different target, pre-marker old state with absent target receipts, and recreated same-name target cannot inherit ACK/cursors | Three distinct failure modes; no duplication finding. Same-name DB recreation specifically exercises the OID component |
| `scripts/test-migration-file-copy.py` | Independent bytes/inodes, destination tamper recovery, credential filename exclusion, disjoint state, committed WAL snapshot and live-writer progress, pause before publication | Behavior and filesystem integration assertions; synthetic fixtures; proportionate |
| `scripts/test-nas-migration-copy.py` | Cancellation refuses unrelated/missing containers and permits only exact matching labelled helper | Mocking is appropriate to prevent remote effects and tests the authorization boundary itself |
| `tools/migration-media/main_test.go` | Marked canonical roots, output independence/replay/tamper detection, source/destination symlinks and path escape, pinned encrypted-image codec with unchanged input | Focused codec/path tests; no language/toolchain-only assertions |
| `.github/workflows/ci.yml` | Executes migration contract and filesystem/codec checks against PostgreSQL 16 and pinned Python dependencies | Ties meaningful regressions to CI; no unwarranted suite expansion |

## Missing coverage

The Rust legacy evidence test selects original/current revisions and asserts their bodies, but does not assert historical `metadata.participants`. The real API therefore passes existing tests while dropping the author for every non-current legacy revision. Add focused assertions around one authoritative sender correction: original revision shows original sender, an intermediate revision keeps its saved sender after a later correction, and current revision reflects the current sender/account association. Retain the existing original-body, auth, pagination, range/path and replay checks. No removal or new broad helper framework is recommended.

This is **missing behavior coverage**, not excess tests. It does not justify removing any existing regression.

## Checks performed

- `scripts/test-legacy-import.py`: 3 pass, disposable local PostgreSQL 16 with supplied server.
- `scripts/test-collector-chatlog.py`: 11 pass.
- `scripts/test-migration-file-copy.py`: 4 pass.
- `scripts/test-nas-migration-copy.py`: 2 pass.
- `go test ./...` in `tools/migration-media`: pass (cached).
- Existing Rust tests/fixtures and CI entry points inspected; a complete Rust build/test rerun and browser suite were not run in this space-constrained review. No real services or business databases were used.
- `scripts/legacy-chatlog-media.py`, added after capture as untracked parent work, is excluded and needs separate review.

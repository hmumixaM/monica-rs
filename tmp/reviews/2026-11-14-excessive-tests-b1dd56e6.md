# Excessive Tests Review — PR #4 Historical Evidence Repair

- Date: 2026-11-14.
- Actual base: `027eaa2e1c113d4345ca82711215cbed4281be45`.
- Reviewed remote/local head: `db04fe884b43f7551eba6aeccbe84af026f077d6`.
- Skill: local `excessive-tests`; fresh independent context.
- Outcome: **Proportionate regression extension; no excessive-test finding.**
- Immutable artifact; no test/source/Git edits, external comments, NAS access or performance testing.

## Changed regression assessed

The existing `imported_messages_expose_bounded_evidence_and_safe_original_files` SQLx test is extended rather than introducing another fixture/service layer. Three rows represent distinct observable contracts:

| Scenario | Observable regression caught | Value |
| --- | --- | --- |
| Current revision 3 | Current corrected sender and current person link are returned | Protects live relinking semantics |
| Intermediate revision 2 with sparse saved_row | Sender comes from selected immutable content, not today's sender or original author | Protects fallback contract |
| Original revision 1 with saved profile differing from live link | Original sender and saved original person survive correction/relinking | Direct regression for prior B1 and historical ownership |

The assertions inspect authenticated HTTP JSON rather than private helper calls/source text. The added account/profile inserts express meaningful domain state. UUIDs isolate records; no arbitrary sleeps, network service, mocks that bypass production behavior or wide parameter matrix were added. Existing body, citation, missing revision, original download auth, byte range and path escape checks are retained. No test deletion is justified by equivalent retained coverage.

## Cost and runner

Independent command `python3 scripts/test-cargo.py --locked -p monica-server imported_messages_expose_bounded_evidence_and_safe_original_files -- --nocapture` passes: **1 test, 1.09 s** after **38.38 s** source compilation. The helper owns a disposable PostgreSQL16 and cleans it up. Pinning CI's test runner adds no test workload. The existing migration-contract CI remains valuable for cross-process data preservation and recovery.

## Limits

This follow-up assesses the newly extended regression and affected runner, while the prior `2026-11-14-excessive-tests-51b745ba.md` covers unchanged PR tests. No broad suite was repeated merely to judge test quantity. A profile-only legacy sender would need a distinct regression if that previously unsupported data shape is brought into release scope; it is not equivalent to the source-account cases above.

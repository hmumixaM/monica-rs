# Excessive Tests Review — PR #4 Explicit Person Attribution Repair

- Review date: `2026-11-14`.
- Actual PR base: `027eaa2e1c113d4345ca82711215cbed4281be45`.
- Actual remote/local reviewed head: `969497f1ca403ebb9c57ad55a535585ff22c4e62`.
- Previous head: `db04fe884b43f7551eba6aeccbe84af026f077d6`.
- Skill: local `excessive-tests`, fresh independent context.
- Outcome: **Pass. Changed tests are proportionate; no redundant, brittle or excessively costly test finding.**
- Immutable report; no source/Git mutations or external comments.

The latest executable/test repair changes the existing `imported_messages_expose_bounded_evidence_and_safe_original_files` HTTP regression rather than introducing another harness or a broad matrix. Its two additional cases cover distinct valid attribution boundaries: a manual/explicit sender person overriding the account link, and a message with that person but no source account. Both assertions inspect the public authenticated HTTP response's `metadata.participants[].profile_id`, so breaking the repaired behavior fails the test.

The retained three-revision cases protect different history behavior: distinct immutable authors, saved original person surviving later source-account relink, sparse intermediate saved row falling back to immutable payload, and current live account resolution. These are not equivalent to the two new current explicit/profile-only cases; removing them would lose regression coverage for the previous confirmed history defect. Shared synthetic fixture/app setup keeps the new cost small; there are no sleeps, external business services, source-text assertions, private-call expectations or new helper framework.

The targeted test ran independently through `scripts/test-cargo.py` with a fresh socket-only synthetic PostgreSQL16: **1 passed, 0 failed**, **0.97 s** runtime, **35.23 s** fresh compile. Added requests reuse the existing fixture and test database. CI's Rust workspace test entry includes this test. The CI Ubuntu24/PostgreSQL16 pin is unchanged by this latest commit and remote jobs did not start because of the parent-confirmed budget issue.

The broader PR test strategy was evaluated by the earlier full report; this follow-up covers the exact latest diff and relevant retained coverage. No wider test-suite audit is warranted by this small extension. An exact historical-profile-only combined fixture is not separately present; static trace uses the same selected-profile fallback already exercised by the current no-account case and historical saved-person cases. This is recorded as a validation limit, not a request to grow a combinatorial matrix.

Full base/head inventory, behavioral trace and limits are in [bb-20261114-d38b56a2](/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-d38b56a2.md). No removal or repair is recommended.

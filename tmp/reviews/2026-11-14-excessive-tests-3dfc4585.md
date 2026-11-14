# Excessive Tests Review — PR #4 Explicit Person Attribution

- Review date: 2026-11-14.
- Actual PR base: `027eaa2e1c113d4345ca82711215cbed4281be45`.
- Actual remote/local head: `db04fe884b43f7551eba6aeccbe84af026f077d6`.
- Skill: local `excessive-tests`; fresh independent context.
- Outcome: **No excessive-test finding; one confirmed missing author regression.**
- Immutable report. New evidence expands the coverage assessment in `2026-11-14-excessive-tests-b1dd56e6.md`; that artifact remains unchanged.
- Production finding: report-local B1 in [bb-20261114-3dfc4585](/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-3dfc4585.md).

The three source-account revision cases added to the existing HTTP test are proportionate and must be retained. They protect different current, intermediate sparse-payload and original saved-person behavior. Independent execution passes: **1 test, 1.09 s**, temporary PostgreSQL16 automatically removed.

The parent subsequently confirmed **22** actual Mac-copy messages represented by sender_profile_id with no sender_account_id. The current regression always creates a source account for each asserted author, so it cannot fail when the source-account-only query discards this valid person-only form. Extend the same HTTP fixture to assert a nonempty participant containing the explicit person for a profile-only current/original message. Also assert that an explicit current person different from the live account-linked person takes precedence, matching the existing message reader. These are distinct domain boundaries, not duplicate source-account tests.

No removal, test framework, new service or broad matrix is needed. Existing body/file/auth safety assertions remain valuable. No broad suite was repeated merely to assess test quantity. No test/source/Git edits, external comments, real database access, NAS operations or performance tests by this reviewer.

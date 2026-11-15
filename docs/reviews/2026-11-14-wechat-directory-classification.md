# WeChat directory classification

The directory previously excluded only some `gh_` accounts at collection time,
missed official accounts using ordinary `wxid` identities, and dropped WeCom
`@openim` contacts along with strangers. Retained source evidence shows the
reported public account has `verify_flag=24`; the requested enterprise contact
uses `@openim`, `local_type=5`, and `verify_flag=0`.

Migration 78 builds a derived classification head from the already archived
Chatlog contact rows, saved verification fields and projected native contact
journal/evidence. It does not archive/delete
profiles, rewrite their fields, reimport Next, or change original source rows.
The head retains observation time and a key back to the existing immutable
source evidence. Future contact journal projection and archived contact inserts
update it; older observations do not replace newer evidence.

The classification uses the official-account protocol bit (`verify_flag & 8`)
and `gh_` namespace, with the separate `@openim` enterprise namespace preserved.
Unparseable or absent flags do not reclassify an existing account as a person.
Classification uses source identities, never a display-name blocklist.

A shared directory view hides profiles whose usable identities are exclusively
official accounts. Profiles with another usable identity, deliberately created
notebook people, and enterprise contacts remain visible. Existing archive
decisions remain effective. The default list, directory, person search, matching
suggestions, contact summaries, maps and relationship graph use this policy.
Exact profile reads, original contact evidence and stored relationship history
remain available. Native official contacts retain their journals without
automatically creating notebook people or importing their fields as contact
identifiers. Classification changes invalidate directory/matching caches.

Focused verification uses generated data and disposable local PostgreSQL
databases only:

- One upgrade/HTTP regression starts with migrations 1–77, eight existing
  profiles and archived/native-only contact evidence, then applies 78. It verifies official,
  enterprise, ordinary, archived, mixed, manual, identity-only and native-owner
  cases; malformed flags; profile/list/directory/search/graph/home/metrics reads;
  retained raw rows; correction, stale replay and cached directory invalidation.
  It also exercises the real matching loader/queue to distinguish an archived
  linked account from a genuinely unlinked same-name discovery candidate.
- The 12 connection integration tests pass with `--test-threads=1`, including
  the new real journal-to-projection check for ordinary/`gh_` official accounts,
  enterprise contacts, raw metadata, replay and prevention of automatic people.
- The 12 standalone Chatlog reader tests pass, including preserved official
  classification evidence and `@openim` contact capture.
- Initial concurrent connection tests exhausted the local PostgreSQL lock table
  while applying migration 30. Serial execution passed without any database
  configuration change. This was not a production/NAS test.

Independent review `bb-20261114-908ba597` found two medium issues: a NULL
directory join admitted archived accounts to matching, and upgrade omitted
native-only retained classification evidence. Both have scoped repairs and
regressions. The repaired upgrade/HTTP test passes in 1.03s. Core lib/connection
and server all-target Clippy passed before repair; full-workspace Clippy hits
pre-existing unchanged AI test warnings. The final repaired diff requires a new
independent review before merge; the first review does not certify the repair.

The NAS still runs its pinned old version and schema while the detached copy
chain is active. This change must be reviewed, merged and packaged before a
later safe deployment. Actual NAS profile-list acceptance is pending that
deployment; these tests are not a claim that the live page is already fixed.

Primary protocol reference: [WeixinBot contact verification flags](https://github.com/Urinx/WeixinBot).

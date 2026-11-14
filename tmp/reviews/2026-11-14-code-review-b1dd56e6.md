# Code Review — PR #4 Historical Evidence Repair

- Date: 2026-11-14.
- Actual base: `027eaa2e1c113d4345ca82711215cbed4281be45`.
- Reviewed remote/local head: `db04fe884b43f7551eba6aeccbe84af026f077d6`.
- Skill: local `code-review`; fresh independent context.
- Outcome: **Approve the affected repair; no blocking finding in this follow-up.**
- Full audit detail: [bb-20261114-b1dd56e6](/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-b1dd56e6.md).
- Immutable artifact; no source/Git edits, external comments, NAS operations, business database access or performance testing.

## Behavior and correctness

`evidence_version` now returns selected-version participants in the immutable payload consumed by the generic evidence handler. Original, intermediate and current versions can have different authors. Historical versions derive account identity from saved presentation/selected content and keep a saved profile; the current revision follows the current account link. The external-ID fallback is provider-scoped and matches the schema's uniqueness contract.

The actual importer was traced: `correct()` saves merged changed projection fields, and message content includes `sender_external_id`. This explains why the sparse intermediate test is useful. Original CRM evidence retains the original sender fields for fallback. No auth or pagination bypass was added. Existing evidence body/file safety assertions continue to execute in the expanded test.

Pinning the test job to Ubuntu 24.04 aligns it with the existing web runner and keeps the explicit PostgreSQL16 installation on a fixed OS baseline. This review does not assert that remote CI has finished.

## Verification

- Fresh-source targeted HTTP test: **1 passed, 0 failed, 1.09 s** after **38.38 s** compilation, against a newly created temporary socket-only PostgreSQL16 instance; automatically cleaned up.
- `cargo fmt --all --check`: **pass**.
- Three author revisions and current-versus-historical person-link semantics are asserted through the real authenticated HTTP route.

## Scope and limits

This is a focused post-fix review of the three changed executable/test/runner paths since `74b9de20b24638cdd1e9fd89948eacbba2724b18`, including their callers/persistence contracts. The complete actual base-to-head inventory was reconciled; previous full-PR report `bb-20261114-51b745ba` remains the evidence for unchanged code. Prior B1 is closed for source-account historical evidence.

A profile-only legacy sender with no source account/external ID remains an existing limitation of the source-account-only query, unchanged by this fix. No real business database was inspected to establish whether that data form is present. Parent was told to check prevalence if needed. This focused approval does not certify all source formats, real NAS data completeness, media byte coverage or deployment health.

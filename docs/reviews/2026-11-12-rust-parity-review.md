# Rust Monica Next review and parity verification — 2026-11-12

This review uses the local `bugbot`, `code-review`, and `excessive-tests` skills. The user authorized review, repair, and alignment with the ordinary macOS and NAS functionality, allowing Mac-dependent collection to be unavailable on NAS. Existing services, personal data, source reference worktrees, and account configuration were not modified.

## Reviewed source

- Repository: `/Users/maxhu/play/monica-rs`, branch `codex/complete-multisource-rust`.
- Git HEAD: `5fdd09b` plus the complete tracked/untracked local implementation. No PR exists; this is a local branch/worktree review, not a submitted PR review.
- Actual merge base / remote default: `301241bf41e8ba8bb18d981f4e9138fe220e9c18` (`origin/main`, checked against the remote).
- Ordinary NAS reference: `hmumixaM/monica-next-crm`, `6d59cfef4b1a12f678d809a00e37d8332be70b06`.
- Ordinary Mac reference: `e35b49e4ca33406f0fdd7ecf8767931b44fdac87`; expanded multi-source reference: `1a6e40e20c6ac12aca434b61244059d067847473`, including the accepted conversation-label fix `c0007f50f39e4c38f6dd7b0caef9cf64e1f5c75a`.
- NAS read-only release-state observation: release `abc9cf23ca6f5c1e7b2d8eb883a1cb113dde2d31`; recorded API/Web names retain `6115b1586ea9`. This is release-state evidence, not an independent Docker/Caddy runtime-attestation result.

The initial immutable Bugbot report is `tmp/reviews/2026-11-12-bugbot-report-4e29e212.md` (report ID `bb-20261112-4e29e212`). It records the pre-repair Block recommendation and original coverage limits. Do not rewrite it to describe the repaired tree. Deeper code-review findings are recorded in the two adjacent code-review follow-up reports.

## Confirmed repairs

All 13 identified findings were repaired locally. The severity labels describe the original failures: six high, seven medium.

| Finding | Original failure | Repair and proof |
| --- | --- | --- |
| B1 high | WhatsApp pause erased linked-device keys | Source pause and global pause write a paused control while preserving enabled transport identity. Only explicit disconnect erases keys. A scheduler regression checks pause/resume/disconnect and retained history. |
| B2 medium | Different posts for one person blocked on a shared Memory head | Atomic per-object Memory recovery queue, bounded/coalesced background turns, and publication pending checks. The original failing concurrency regression now passes and verifies both Memory inputs are eventually retained. |
| B3 high | Enabled model settings crashed because OCR response fields did not match the UI | Restore the full typed OCR status contract and connect the OCR limit form submit action. Configuration readiness is separate from the historical successful-call indicator, allowing the first job to be queued. Real HTTP and browser settings checks cover the enabled state. |
| B4 medium | Agent transcripts above 64 KiB were rejected | Give completion requests their explicit 2 MiB boundary. An authenticated 70,000-character request streams from a loopback fake provider and verifies the encrypted configured key is used. |
| B5 medium | Undated evidence repeated the first page | A before-id-only cursor selects the undated tail by descending UUID. A real HTTP test pages the full set without duplicates. |
| B6 medium | Restoring an unchanged contact did not restore removed current fields | Historical hash no-op detection cannot skip an absent current value. Delete/restore regression checks the field returns while retaining history and manual precedence. |
| B7 high | Packaged Mac/NAS model functionality had no usable configuration path | Shared authenticated settings API/UI, encrypted stored key, selective updates and deletion, explicit privacy choices, and environment override support. Mac restarts preserve stored choices. Compose forwards optional model/OCR configuration. No external model call is made merely to validate settings. |
| B8 medium | NAS offered native controls that do not exist there | Runtime capability metadata, clear unavailable text, disabled collection actions, and API rejection of native resume/refresh when no Mac bridge is available. Stored evidence and normal reads remain available. |
| B9 medium | Compose had a WeChat spool but no WeChat reader | Optional WeChat collector profile with an explicit read-only published-snapshot mount, source anchor and own-account identity. The source directory must already exist. |
| C1 high | Generated weekly digests lacked statistics, people and post dates required by the page | SQL-derived frozen week facts, complete read contract, compatibility for older incomplete test records, model-output shape checks, and current-policy visibility. Counts are never accepted from model output. |
| C2 high | Model evidence lost speaker identity and omitted the person's group speech | Preserve sender/direction/participants and explicit citations; include only that person's group speech. Social Memory accepts the author rather than commenters, and public posts do not require private-message consent. |
| C3 medium | Historical profile fields could become this week's update | Weekly evidence contains actual in-week events; no-event subjects cannot enqueue a weekly claim. Explicit weekly instructions preserve the distinction between background and event evidence. |
| C4 high | Chatlog snapshot posts dropped media, location, links and interactions | Use the pinned ordinary parser's pure functions, retain raw XML/hash and literal-reference repair metadata, and project connection-scoped social interactions. Stable interaction identity survives comment-to-reply correction. Update both Mac collector packaging and the container image. |

## Feature alignment matrix

“Verified” below means implementation and the listed isolated contracts passed. It does not imply a real provider/account/permission test.

| Feature area | Mac Rust | Linux/NAS Rust | Evidence |
| --- | --- | --- | --- |
| Authentication, sessions, CSRF, TOTP/recovery and import signatures | Shared | Shared | Workspace tests; actual pinned Python HTTP differentials; HTTP lifecycle smoke |
| Profiles, manual/source fields, typed facts, dates, groups, tags and linking | Shared | Shared | Profile/domain/notebook/connection differentials; PostgreSQL regressions |
| Messages, direct/group speech, corrections, conversation labels and search | Shared | Shared | Source/message tests; signed connection and label differentials; real browser pages |
| Timeline, Stories, comments/likes/replies, media references and original source evidence | Shared | Shared | Timeline/media tests; new Chatlog parser and social-interaction HTTP regression |
| Original uploads/downloads, previews, resumable media, range and conditional reads | Shared | Shared | Media differential; real PNG decode/Range/If-Range and HTTP smoke |
| Directory, home, insights, contact calendar, orbit, maps and social metrics | Shared | Shared | Empty-read differentials and populated actual HTTP/browser fixture |
| Memory, suggestions, weekly notes/digests, reconnect, OCR and browser agent | Shared | Shared | Fake-provider integration, source attribution/privacy tests, complete digest HTTP contract, IndexedDB browser agent checks |
| Gmail, Outlook, IMAP and mailbox rules | Shared | Shared | Transport/source differentials; fake mail integration and Sources browser checks |
| WhatsApp device bridge and phone history imports | Shared | Optional bridge container | Go tests, device lifecycle and phone-export original/replay tests |
| LinkedIn/Instagram collectors and independent LinkedIn message lane | Native supervisor | Optional collector containers | Offline bridge tests, LinkedIn adapter tests and existing native supervisor tests |
| WeChat published snapshots | Native Chatlog + reader | Optional WeChat reader profile | Generated SQLite tests; Moments metadata parser and connection projection tests |
| Apple Contacts / Messages collection | Optional native bridge | Explicitly unavailable | Native bridge/lifecycle tests and unavailable-capability/API regression |
| Existing Apple-source evidence / notebook imports | Shared | Shared | Journal, revisions, original downloads and notebook differentials |
| Face review/device APIs | Shared APIs; optional device/runtime | Shared APIs; optional device/runtime | Signed fake-device, face review and media tests; real inference remains an external acceptance boundary |

## Test review

The added tests protect observed failures across the relevant boundaries. The core weekly test verifies source facts and attribution; the HTTP digest test verifies the response consumed by the UI; browser checks exercise actual forms and rendering. These are distinct contracts. No existing regression was deleted or replaced by source-text matching. The CI adapter job now installs and uses the existing locked collector environment, rather than installing pytest alone and then failing to import Playwright.

Two original empty-read differential failures were oracle mismatches: a wall-clock `as_of` timestamp and Rust's integrated coordinator compared with Python's absent separate worker. Timestamp shape and coordinator identity/state are now checked separately, while business response equality remains exact. Runtime capability fields added to `/sources` are tested independently and excluded only from the old frozen catalogue comparison.

## Acceptance limits

- The static audit finds 166/166 frozen operations registered. Registration is not proof of complete behavioral equivalence.
- Automated validation uses generated data, fake providers and isolated PostgreSQL instances. Real mailboxes, social logins, WhatsApp device pairing, Mac permission dialogs and face inference were not exercised with personal accounts.
- Source media URLs and metadata are retained and rendered. This review does not establish that every original remote asset can still be retrieved from an external source or is already archived locally.
- Local Docker CLI is available, but its daemon socket is unavailable. Compose configuration is validated; final Linux amd64/arm64 image execution and actual NAS runtime acceptance are not certified here.
- Full vendored third-party collector dependency/security review, performance benchmarks, refreshed distributable app/images, signing, personal-data migration and production switching are separate outstanding delivery gates.
- No staging, commits, branch changes, pushes, PR comments, merges, releases or service restarts were performed.

Validation commands, outcomes and reviewed-file hashes are recorded in the accompanying validation JSON and local evidence directory.

## Final validation results

- Rust workspace: **169 passed**, 0 failed; 3 intentionally ignored performance/optional tests. Actual authentication/media/CLI HTTP smoke passed. The latest interaction correction was then rechecked with its focused HTTP regression.
- Root and native Rust formatting checks passed. Root all-targets Clippy and native all-targets Clippy passed with warnings denied.
- Native unit tests: **14 passed**. The explicit disposable PostgreSQL/native server lifecycle test also passed.
- Web: production TypeScript/Vite build passed; router/agent browser checks **5 passed**; real Rust login/settings/OCR form/disabled-native capability browser acceptance passed; populated desktop/mobile pages, evidence and old-digest browser acceptance passed.
- Offline adapters: collector publication **8 passed**; Chatlog **11 passed**; native Messages fixture **1 passed**; locked LinkedIn message adapter **2 passed**; Go WhatsApp tests passed. New Chatlog modules also passed using the staged packaged Python interpreter.
- Pinned Python/Rust differential set: **19 scenarios passed** after correcting the two documented empty-read oracle mismatches. The affected source/HTTP/model-disabled scenarios were rerun after source repairs. These cover authentication, profiles/domain facts, media, empty page contracts, multi-source journals, evidence/identity HTTP, native notebook imports, signed connection imports, conversation labels, mail/phone transports and signed media publication.
- Frozen operation registration: **166/166**, zero missing.
- Compose configuration including the optional WeChat profile passed validation with synthetic values. Running Linux images was not possible through the unavailable local Docker daemon.

Evidence: [validation manifest](2026-11-12-rust-parity-validation.json). Detailed generated-data-only logs are retained under `.local-validation/review-2026-11-12/`. Source edits remain uncommitted; no PR, merge or release was created.

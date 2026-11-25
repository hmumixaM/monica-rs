# Implementation ledger

Current scope is the complete Rust backend and shared Mac/NAS runtime, expanded to the fixed ordinary multi-source Mac baseline. Original worktrees remain read-only. Validation uses generated data, disposable databases and fake external providers.

The subsequent [2026-11-12 skill-based review](reviews/2026-11-12-rust-parity-review.md) records repaired lifecycle, concurrency, model configuration, weekly facts/attribution, native capability and Chatlog metadata defects. Its validation manifest supersedes earlier provisional smoke evidence for the affected behavior. The delivery and real-account gates below still apply.

## Implemented shared scope

- Shared core/server, seven source schemas plus CRM, bounded immutable/mutable paths, typed projections, local lock ordering, fenced queues, generation coalescing and local cache invalidation.
- Auth, manual and source profile fields, structured facts, groups, tags, linking, conversations, timeline, search, maps and page-specific summaries.
- Content-addressed media, range/conditional reads, resumable uploads, durable publication recovery, OCR/face boundaries and managed external processes.
- Multi-account Mail with Gmail/Graph/IMAP, WhatsApp import and bridge, Apple native source pages, evidence/revisions/coverage and account ownership controls.
- Stateless browser model transport, bounded model slots, persistent incremental Memory, classification and derived jobs, tested with fake model servers.
- Vite/React Router same-origin UI, first-class Sources/Mail/Evidence and Ask Monica. Ask runs in the Rust server, saves checkpoints and conversations in PostgreSQL, and runs generated JavaScript in a WebAssembly guest with a read-only query bridge. Tauri tray/settings, PostgreSQL and child process lifecycle, independent data directories and default-off sources.
- Optional real pgoutput WAL capture, independent pipeline status and fenced retries. See CDC.md for exact behavior and maintenance prerequisites.

The static frozen-contract audit covers166 registered operations. This is not a substitute for behavior tests. Evidence is held in the differential scripts, Rust tests, UI tests and isolated validation logs. The final report records the current registration audit and selected behavioral evidence separately; the historical148-operation CRM smoke ledger is not a whole-platform implementation certification.

## Completed account-free execution and remaining external scope

The [2026-11-13 final platform acceptance](reviews/2026-11-13-platform-acceptance.md) and [machine validation](reviews/2026-11-13-platform-acceptance-validation.json) supersede the earlier integration/packaging gates. They record exact committed application source, real complete Mac package verification, both Linux architectures, actual NAS migrations/auth/media/crash persistence, and the complete Mac three-round comparison. The user explicitly canceled further NAS testing. The separate NAS logical matrices did not start because reverse Unix-socket forwarding was rejected; the NAS performance run never started. No SSH service configuration was changed. Failures and functional transport continuations retain their original evidence and are not relabeled as original-command passes.

Measured performance limits remain explicit, including the unmet1x Mac throughput target. Live personal providers/model inference, permission grants, public distribution/notarization, legacy personal-data migration, Mac/NAS synchronization and production cutover remain outside this delivery. No personal data, source worktrees or installed production service was changed.

## Native notebook and multi-source domain completion (2026-11-12)

The accepted ordinary-chat baseline adds four providers to the original three. The single PostgreSQL database now contains `crm` plus `wechat`, `instagram`, `linkedin`, `whatsapp`, `apple_messages`, `apple_contacts`, and `mail`. A signed import's `connection_id` scopes source records, revisions, cursors, conversations, and trusted explicit-person bindings. Native `source=monica` imports remain CRM notebook facts and revisions; they do not create another provider schema or an artificial platform account.

Source journals append a bounded page and cursor atomically. A separate short transaction projects that page and acknowledges its outbox. The append fast path batches heads, revision references and payloads; mutable records retain ordered A-B-A history. Native Contacts reset snapshots preserve their seen-ID set across retries, fence paused generations, and create bounded tombstones only after the final page. Both record count and payload bytes bound recovery batches, with one oversized record admitted to guarantee progress.

The notebook importer handles the pinned `legacy` adapter and public contacts/profiles/people/groups, generic field maps/single fields, structured address/employment/education aliases, messages, posts/stories and their interactions. It preserves manual-current precedence, typed content addressing, source-history deduplication, and Memory inputs. Shared source and notebook structured views use the same canonical organization IDs. Conversation labels additionally match the narrow accepted `c0007f5` ordinary-chat fix.

Evidence: disposable PostgreSQL `domain-final-tests-21.log` contains 9 connection and 12 domain/import passes. Actual Python/Rust notebook differences are checked against all three fixed baselines: 96 database observations plus two HTTP details for each original baseline, 104 plus two details for the multi-source baseline. The newer baseline intentionally updates a repeated message's contents, while the two older baselines used `DO NOTHING`; that explicit behavior change is tested against the newer baseline. Signed connection imports add eight real HTTP observations, and the label fix adds 48 journal/projection plus six HTTP search observations. See the corresponding `notebook-differential-*`, `connection-import-differential-2.log`, and `conversation-labels-differential-4.log` evidence in `.autonomous/complete-monica`.

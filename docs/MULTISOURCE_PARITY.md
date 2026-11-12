# Multisource parity target

User requested parity with the MacOS chat `01a1cb5f-8a1b-7db3-8bf6-0daf6af7bcfa` after the original Rust work began. The additional committed input is `/Users/maxhu/play/monica-multisource` at `1a6e40e20c6ac12aca434b61244059d067847473`, from `hmumixaM/monica-desktop`, branch `codex/multisource-performance`. Original CRM `6d59cfef` and Desktop `e35b49e4` remain regression references. No source worktree, installed service, existing account, archive or credential is modified or copied.

## Added implementation scope

- Seven source schemas (`wechat`, `instagram`, `linkedin`, `whatsapp`, `apple_messages`, `apple_contacts`, `mail`) plus `crm` in one PostgreSQL16 database. Each connection has independent identity, cursors, source versions, failure/retry state and file ownership. Connection IDs scope all external message/account identifiers.
- Gmail/Google Workspace, Graph/Outlook and IMAP TLS multi-account mail, header-first categorization, explicit filtering overrides, recent-first history, cursor-expiry reconciliation, read-only mailbox behavior and retained originals/attachments/account copies.
- Native Contacts change tokens and read-only Messages WAL/edit/retraction/attachment ingestion. LinkedIn's separate message lane. WhatsApp linked-device Go bridge and explicit direct/group TXT/ZIP imports.
- Confirmed/unique identifier ownership, ambiguous shared identifiers as candidates, own-account registration and safe group author attribution. Disconnect preserves evidence and other associations.
- General source/evidence APIs, original downloads, cross-source timeline and person coverage; browser PI agent loop with @people and actual read citations; IndexedDB transcripts/import/export/context compaction. Backend provides stateless model streaming only.
- Four model slots with one interactive and three background slots while QA is enabled; persistent incremental Memory inputs/chunks and reductions, mail classifications and local event candidates. No external sends or calendar writes.
- Selective settings changes and independent source lifecycles, visible history coverage, windowed lists, updated Mac test app and same Linux container core.

## Validation boundary

Implementation and synthetic validation are in progress. Prior completed baseline tests do not establish completion of these additions. Instagram remains disconnected. No real mailbox, address book, Messages database, WhatsApp account or cloud model is used by this task. Formal signing, personal-data migration and production cutover remain excluded. Mac-only native sources expose clear unavailable capability on NAS; stored facts, queries, evidence and browser QA share the same core.


## Supplemental ordinary-version fix

`c0007f50f39e4c38f6dd7b0caef9cf64e1f5c75a` is also included: unlinked conversations show human-readable source labels or phone numbers and their source name. Its SQL label logic and UI change are tracked separately from the original `1a6e40e` contract freeze.

## Mail visibility

Mail is a first-class source on the Sources landing page, with explicit Gmail, Outlook and IMAP connection entries. Sources is directly available in desktop navigation and the mobile bottom bar. Each account has its own state, visible evidence range, saved record count, pending projection count and filtering rules. Progress reads compact connection counters rather than scanning message history. Synthetic Rust-backed desktop/mobile checks exercised adding a fake IMAP account while the server was paused, pausing the connection, rule edits, evidence pagination/revisions and original downloads.

## Current verified integration evidence

- `parent-multisource-test-4.log`: complete fresh PostgreSQL workspace tests and real HTTP authentication/media/CLI smoke passed before subsequent adapter and resource-reservation changes.
- `parent-ai-freeze-core.log` / `parent-ai-freeze-http.log`: incremental Memory, mail AI, adapter and stateless browser-proxy suites passed with fake providers; no paid or personal-account calls.
- `connections-http-differential-3.log`: 27 actual pinned multisource HTTP observations matched; source-journal/projection comparison covered 70 observations.
- `source-transport-differential-3.log`: 45 actual pinned Python/Rust mail and phone-export vectors matched.
- Runtime packaging and final Mac/NAS performance acceptance remain in progress. These functional results do not establish the high-performance designation.

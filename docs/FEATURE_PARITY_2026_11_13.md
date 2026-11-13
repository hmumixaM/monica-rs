# Rust / original Mac feature comparison

Implementation base: Rust main 58e2719 (PR #1). Original references remain CRM
6d59cfef, standalone desktop e35b49e4, multisource 1a6e40e and the unlinked-label
fix c0007f5. Original worktrees are read-only. This is feature-path and selected
behavior verification, not an assertion that all provider inputs were exercised.

| Feature area | Rust path / UI | Evidence in this delivery |
| --- | --- | --- |
| Login, sessions, CSRF, TOTP/recovery | Shared auth, native account section, Web settings | Existing auth/HTTP coverage retained; native password recovery, identity/data preservation and session revocation added |
| Profiles, manual/source fields, structured facts, dates, tags, groups, history | Existing profile/domain APIs and pages | Full workspace regressions; actual browser directory/profile/calendar/map/orbit fixture |
| Identity linking, candidates, own accounts and group attribution | Existing linking and connection ownership handlers | Source/identity regressions; source connection HTTP differential; MCP calls reuse same ownership/audit path |
| Conversations, direct/group messages, edits/retractions, source labels | Shared message/search projections | Workspace regressions; c0007f5 48 journal/projection + 6 HTTP observations |
| Timeline, Stories, reactions, media, original files and versions | Shared timeline/evidence/media routes | Populated browser fixture, evidence original/revision/pagination, Chatlog parser/bridge regressions |
| WeChat | Managed Chatlog and published snapshot reader | Generated-data bridge/supervisor tests; no live database access |
| Instagram | Existing explicit-login collector and source projector | Offline bridge tests and seven-source status; kept disabled, no live login/collection |
| LinkedIn profiles/posts/messages | Independent message lane and existing collector | Source HTTP/transport differentials, scheduler tests; phase/heartbeat/run snapshots added |
| WhatsApp | Linked-device bridge and phone-history imports | Existing Go/source tests and browser fallback; no new personal pairing |
| Apple Contacts / Messages | Native readers and durable page delivery | Native 14-test suite and generated native-source/evidence browser fixture; no permission grant or personal reads |
| Gmail, Outlook, IMAP and classification overrides | Existing multi-account connections and mail rules | 45 pinned transport/phone vectors; real Rust-backed IMAP create/pause and OAuth configuration UI |
| Memory, weekly summaries, suggestions, OCR, faces | Existing shared services and incremental inputs | Workspace tests with synthetic/fake providers retained; no paid model or personal face inference |
| Browser agent | Existing PI loop, multi-person selection, evidence citations, cancellation, IndexedDB | Existing agent/router tests retained; unified shared controls and form submission verified |
| Desktop lifecycle | Existing tray/autostart/supervisor and owned processes | Native unit/UI checks; packaged lifecycle checks recorded in delivery review |
| MCP | New `/mcp`, Settings → AI 访问 / MCP | 19 allowlisted tools; actual official SDK direct/proxy fixture; key rotation/revocation and shared writes |
| Operational visibility | Sources summary, Imports daily/runs/diagnostics | Deduplication, source isolation, three timezone cases, failure transitions, heartbeat replay, desktop/mobile light/dark browser checks |

Confirmed gaps repaired: absent MCP and local password recovery; standalone new UI
controls; missing daily/run telemetry; original-three-only import reconciliation;
signed connection double counting and premature query-ready labels discovered
while implementing monitoring; source-list refresh ordering after account creation.
New migrations are 0050–0052. Migrations through 0049 and the merged read/import
optimization semantics remain unchanged.

No original source feature is intentionally removed. Real-provider account access
is a separate boundary: this Rust data directory has no reusable local collector
authorizations configured. This delivery does not copy legacy app credentials or
data. Instagram stays off. No NAS contact, old Python deployment change, personal
data migration, public endpoint publication or notarization is performed.

# Bugbot Report

## Report Contract

- Report type: `bugbot`
- Report ID: `bb-20261112-ef56c331`
- Review date: `2026-11-12`
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-12-bugbot-report-ef56c331.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: Fixed review input; do not rewrite during repair

## Scope

- Diff mode: branch changes
- Requested base: repository default `origin/main`
- Resolved baseline: `301241bf41e8ba8bb18d981f4e9138fe220e9c18`
- Reviewed target: committed `f7c9f01f15b8724d0d1ed0eb2acbafd80d04f70f`; clean working tree, no untracked non-ignored files at detection start.
- Changed files: 439
- Completion: Incomplete — live external accounts, Linux container execution and independent full vendor/data audit remain uncovered.
- Custom instructions: Review and repair Rust Monica; align macOS/NAS capabilities, allow Mac-dependent collection disabled on NAS; commit then re-review with bugbot/code-review/excessive-tests.
- Assumptions: No PR exists. This is a local branch review. The earlier immutable reports and validation manifest remain historical evidence, not a substitute for live platform acceptance.

## Detection Snapshot

- Recommendation: Changes requested
- Completion: Incomplete — external/platform/vendor acceptance boundaries below.
- Why now: The committed implementation loses durable disconnect intent when WhatsApp cleanup encounters a transient filesystem error.
- Findings count: high 0 | medium 1 | low 0
- Must-review now: B1
- Coverage confidence: medium
- Biggest blind spot: Real provider credentials and Linux/NAS container runtime unavailable locally.

## Complete Findings Index

| ID | Severity | Category | Location | Title |
| --- | --- | --- | --- | --- |
| B1 | medium | LOGIC_BUG | [crates/monica-core/src/scheduler_whatsapp.rs:221-222](/Users/maxhu/play/monica-rs/crates/monica-core/src/scheduler_whatsapp.rs:221) | Preserve disconnect intent after cleanup failure |

## Findings

### B1 medium - Preserve disconnect intent after cleanup failure

- Category: LOGIC_BUG
- Location: [crates/monica-core/src/scheduler_whatsapp.rs:221-222](/Users/maxhu/play/monica-rs/crates/monica-core/src/scheduler_whatsapp.rs:221)
- Confirmation selector: B1

Description: `mark_error` overwrites `state='disconnected'` with `error` after failure to write control or erase the local linked-device keys. The next attempt determines disconnect solely from `!enabled && state == 'disconnected'`. After a temporary permissions/mount/I/O failure recovers, the account is interpreted as paused, `control.json` is published with `enabled:true, paused:true`, and local session erasure is never retried. A later Resume can reuse a device session that explicit Disconnect was meant to erase. Restarting the server does not recover the intent because the wrong state is persisted.

Rationale: The new bridge scheduler introduces both the state interpretation and unconditional error-state update. Its explicit retry bookkeeping promises recovery but loses the operation it is retrying.

Evidence:

- Scheduler lines 63-69 derive the desired control and cleanup operation from the persisted state.
- Lines 83-112 call `mark_error` after control-file publication failure; lines 113-139 do the same after `clear_local_session` failure. Neither path records separate durable disconnect intent.
- Lines 221-222 replace state without checking disabled/disconnected intent; after the 60-second delay (or restart), the same generation takes the paused branch.
- `whatsapp::clear_local_session` returns immediately on a filesystem error before deleting the remaining device DB/WAL/SHM files. `write_control` can fail when a mount is temporarily unavailable or the target is obstructed.
- Existing pause/disconnect test covers success only. A focused recovery regression should obstruct a target, observe the recorded error, restart after restoring storage, then require disabled control, cleared session keys and retained history.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Bridge pause/disconnect and filesystem retry | scheduler_whatsapp.rs | Finding B1 | Error reporting destroys explicit disconnect intent | Trace above; add real scheduler/PostgreSQL/filesystem regression |
| Source journal commit, delivery, identity relink | connections.rs; connections_native.rs; server connections.rs | Reviewed - no bug found | Fenced source commits, bounded delivery, explicit mapping and snapshot recovery traced again | Earlier PostgreSQL and differential evidence; fresh call/transaction inspection |
| Contact identity deactivation uses contact IDs vs participant IDs | server connections.rs; connections.rs | Reviewed - no bug found | Immediate identifier update differs, but queued reproject of contact rows deactivates the matching row and queues identifier relink | No confirmed final-state regression |
| Mail token refresh and source finalization | mail_oauth.rs; mail/sync.rs | Reviewed - no bug found | Generation/token/lease fences retained; response bodies not logged; enabled predicate fences finalization | Fresh inspection of refresh, OAuth state consumption, device status and recovery |
| Memory queue/reducers, privacy and speaker attribution | ai_memory_legacy.rs; ai_memory.rs; ai.rs | Reviewed - no bug found | Per-object pending gate and serialized bounded reducer; source owner/speaker filters consistent | Fresh queue/claim/finalization inspection plus existing focused regressions |
| Model settings and OCR configuration | server ai_settings.rs; core ai.rs | Reviewed - no bug found | Encrypted keys, authenticated updates, no echo, explicit privacy and configuration validation | Fresh settings/provider resolution inspection; prior real browser/API checks |
| Signed media receipt progress and durable publication | media_import.rs; media.rs | Reviewed - no bug found | Lease claim/finish, one attachment at a time, durable publication then dependency progress | Fresh record-media code inspection; previous media differential and smoke |
| Digest/event facts, evidence pagination, profile restore, Chatlog media/social projection | ai_weekly.rs; evidence.rs; domain imports; collector-chatlog.py; connections_social.rs | Reviewed - no bug found | Previous confirmed fixes remain in the committed source | Reconciled prior reports with committed manifest and test records; no new contradiction identified |
| Web/native UI and packaging capability boundaries | apps/web; apps/native-desktop; compose and packaging scripts | Reviewed - no bug found | Existing isolated build/browser/native lifecycle evidence remains valid for committed source | Prior validation manifest and capability API regression; no live account claim |
| Added tests and CI runner configuration | changed test files; .github/workflows/ci.yml | Reviewed - no bug found | Boundary tests are proportionate; DB, HTTP and browser assertions protect different contracts | excessive-tests: retain regressions; new missing recovery coverage is part of B1 |
| Historical review reports and reference docs | tmp/reviews; docs/reviews | Not production-relevant | Immutable historical artifacts | Excluded from recursive production defect sweep |
| Real linked devices, OAuth accounts, Mac permissions/model calls | transport/collector/native integration | Not covered | No real credentials or permissions exercised | Use consenting test accounts and packaged Mac runtime for end-to-end acceptance |
| Linux/NAS image/runtime | Dockerfile; compose; native capability gating | Not covered | Docker daemon unavailable; existing validation was config/static only | Build/run image and isolated PostgreSQL on Linux/NAS; do not infer runtime acceptance from compose rendering |
| Full third-party/vendor/data audit and scale behavior | Go vendor; datasets; native dependencies | Not covered | No independent complete dependency security, dataset correctness or performance audit | Separate dependency provenance/security and representative-scale review |

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| `.dockerignore` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `.github/workflows/artifacts.yml` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `.github/workflows/ci.yml` | `M` | Regression contract / runner / CI; excessive-tests assessment |
| `.gitignore` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `Cargo.lock` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `Cargo.toml` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `README.md` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/desktop/collectors/collector.py` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/desktop/collectors/linkedin_messages.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/desktop/collectors/tests/test_messages_adapter.py` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `apps/native-desktop/.gitignore` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/README.md` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/index.html` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/package-lock.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/package.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/playwright.config.ts` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/Cargo.lock` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/Cargo.toml` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/Info.plist` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/build.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/capabilities/default.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/icons/icon.icns` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/icons/icon.png` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/native/contacts.m` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/native/messages.m` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/src/chatlog.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/src/collectors.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/src/lib.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/src/main.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/src/native_control.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/src/native_sources.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/src/shell.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/src/supervisor.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/tauri.conf.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src-tauri/tests/native_runtime.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `apps/native-desktop/src/main.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/src/styles.css` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/tests/settings.spec.ts` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `apps/native-desktop/tests/test_native_messages.py` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `apps/native-desktop/tsconfig.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/native-desktop/vite.config.ts` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/Dockerfile` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/README.md` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/app/ask/page.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/app/evidence/[id]/page.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/app/layout.tsx` | `D` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/app/page.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/app/profiles/[id]/page.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/app/sources/page.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ai/profile-ai.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ai/suggestion-item.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/app-shell.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ask/ask-view.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ask/ask.module.css` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/charts/monthly-bars.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/contacts/contact-list.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/contacts/contacts-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/contacts/create-profile.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/digest/digest-card.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/digest/digest-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/faces/appearances.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/faces/faces-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/faces/photo-people.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/faces/profile-photos.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/feed/event-dialog.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/feed/masonry.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/feed/social-feed.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/home/home-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/home/legacy-redirect.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/home/reconnect-suggestions.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/home/weekly-highlights.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/imports/history-changes.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/imports/identity-review.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/imports/imports-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/imports/imports.module.css` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/insights/contact-calendar.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/insights/insights-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/linking/confirmed-links.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/linking/link-queue.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/linking/linking-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/login/login-form.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/messages/conversation.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/messages/messages-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/moments-circle.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/contact-events.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/evidence-timeline.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/group-speech.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/hero.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/people.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/profile-page.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/profile-tags.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/records.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/ribbon.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/source-accounts.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/profile/source-coverage.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/relationships/footprint-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/relationships/orbit-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/relationships/relationships-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/search/search-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/settings/ai-status.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/settings/archived-profiles.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/settings/model-settings.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/settings/settings-view.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/shell/command-palette.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/shell/nav.ts` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/social/social-analytics.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/sources/sources-view.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/sources/sources.module.css` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ui/button.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ui/chip.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ui/person-link.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ui/segmented.tsx` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/components/ui/virtual-list.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/index.html` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/lib/agent/runtime.ts` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/lib/agent/sessions.ts` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/lib/ai.ts` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/lib/api-types.ts` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/lib/map.ts` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/lib/search-client.ts` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/lib/search-engine.ts` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/next-env.d.ts` | `D` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/next.config.mjs` | `D` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/package-lock.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/package.json` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/playwright.config.ts` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/public/sw.js` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/src/main.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/src/router/dynamic.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/src/router/link.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/src/router/navigation.tsx` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/src/theme-bootstrap.ts` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/tests/ask-runtime.spec.ts` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `apps/web/tests/pages-real-api.spec.ts` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `apps/web/tests/real-api.spec.ts` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `apps/web/tests/router.spec.ts` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `apps/web/tsconfig.json` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `apps/web/vite.config.ts` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/Cargo.toml` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/data/connection-casefold.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/data/connection-digits.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/data/connection-identifiers.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/data/html-entities.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/data/link-matching/LICENSE.pypinyin` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/data/link-matching/README.md` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/data/link-matching/contract-vectors.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/data/link-matching/pinyin-characters.tsv` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `crates/monica-core/data/link-matching/pinyin-phrases.tsv` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `crates/monica-core/data/unicode-casefold.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/examples/connection_probe.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/examples/source_contract.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/adapter.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/adapter_spool.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/ai.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/ai_mail.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/ai_memory.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/ai_memory_legacy.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/ai_provider.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/ai_slots.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/ai_weekly.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/cdc.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/cdc_codec.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/collector.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/connections.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/connections_fast.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/connections_native.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/connections_social.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/content.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/credentials.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/domain.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/domain_facts.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/faces.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/identity.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/imports.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/imports_notebook.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/lib.rs` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/link_matching.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/linking.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/mail.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/mail/html.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/mail/http.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/mail/imap.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/mail/sync.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/mail_oauth.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/media.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/media_import.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/media_jobs.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/media_probe.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/messages.rs` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/pages.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/pages_presentation_golden.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/resources.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/scheduler.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/scheduler_adapter.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/scheduler_ai.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/scheduler_cdc.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/scheduler_idle.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/scheduler_source.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/scheduler_whatsapp.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/source_blobs.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/source_time.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/whatsapp.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/src/whatsapp_counter.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-core/tests/adapter_connections.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/adapters.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/ai.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/ai_mail.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/ai_memory.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/ai_memory_legacy.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/connections.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/content.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/domain_imports.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/faces.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/identity.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/link_matching.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/mail.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/media.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/performance.rs` | `M` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/scheduler.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-core/tests/storage.rs` | `M` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-server/Cargo.toml` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/activities.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/ai.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/ai_browser.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/ai_settings.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/ai_tests.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-server/src/auth.rs` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/cache.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/cdc.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/collector.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/connections.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/connections_tests.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-server/src/faces.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/faces_tests.rs` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `crates/monica-server/src/imports.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/link_graph.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/linking.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/main.rs` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/media.rs` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/messages.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/cache.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/cities.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/directory.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/engagement.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/geonames/PROVENANCE.json` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/geonames/README.md` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/geonames/cities.tsv` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `crates/monica-server/src/pages/geonames/countries.tsv` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `crates/monica-server/src/pages/geonames/names.tsv` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `crates/monica-server/src/pages/geonames/states.tsv` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `crates/monica-server/src/pages/maps.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/metrics.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/pages/overview.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/phone_export.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/profile_dates.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/profile_details.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/profile_read.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/profile_relationships.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/profile_summary.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/profiles.rs` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/runtime.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/social_metrics.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/timeline.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/timeline_metadata.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `crates/monica-server/src/web.rs` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `docs/AI_ADAPTERS.md` | `A` | Review/reference documentation; not production-relevant |
| `docs/CDC.md` | `A` | Review/reference documentation; not production-relevant |
| `docs/IMPLEMENTATION.md` | `M` | Review/reference documentation; not production-relevant |
| `docs/MULTISOURCE_PARITY.md` | `A` | Review/reference documentation; not production-relevant |
| `docs/api-coverage.json` | `M` | Review/reference documentation; not production-relevant |
| `docs/api-registration-audit.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/README.md` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/auth-validation.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/crm-dates.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/crm-declarations.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/crm-golden.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/crm-http.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/crm-openapi.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/desktop-dates.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/desktop-declarations.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/desktop-golden.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/desktop-http.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/desktop-openapi.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/identity-presentation.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/multisource-dates.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/multisource-declarations.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/multisource-golden.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/multisource-http.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/multisource-openapi.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/profile-presentation.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/contracts/source-times.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/reviews/2026-11-12-rust-parity-review.md` | `A` | Review/reference documentation; not production-relevant |
| `docs/reviews/2026-11-12-rust-parity-validation.json` | `A` | Review/reference documentation; not production-relevant |
| `docs/sources.json` | `M` | Review/reference documentation; not production-relevant |
| `infra/Dockerfile` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `infra/benchmark.Dockerfile` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `infra/collector-supervisor.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `infra/collectors.Dockerfile` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `infra/collectors.Dockerfile.dockerignore` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `infra/compose.yaml` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `infra/whatsapp-entrypoint.sh` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `infra/whatsapp.Dockerfile` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0006_object_kinds.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0007_domain.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0008_imports.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0009_links.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0010_timeline.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0011_profiles.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0012_activities.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0013_pages.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0014_media_full.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0015_ai.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0016_scheduler.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0017_faces.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0018_page_summaries.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0019_adapters.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0020_page_indexes.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0025_auth_compat.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0026_storage_compaction.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0027_content_objects.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0029_cache_scopes.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0030_connections.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0031_mail.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0032_memory.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0033_mail_oauth.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0034_page_metrics.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0035_native_snapshots.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0036_connection_progress.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0037_conversation_labels.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0038_connection_imports.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0039_notebook_imports.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0040_cdc.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0041_record_media.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0042_legacy_memory.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0043_community_ai_cache.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0044_connection_social.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0045_memory_projection_queue.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0046_model_credentials.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `migrations/0047_memory_attribution.sql` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/ai-adapter-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/audit-api-registration.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/auth-contract.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/auth-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/benchmark-backends.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/chatlog_moments.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/collector-bridge.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/collector-chatlog.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/connection-import-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/connections-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/connections-http-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/conversation-labels-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/domain-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/frontend-pages-golden.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/frontend-pages-smoke.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/frontend-smoke.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/import-media-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/legacy-contract.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/macos-bundle.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/macos-chatlog-smoke.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/macos-chatlog.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/macos-collectors-smoke.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/macos-collectors.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/macos-finalize.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/macos-gui-test.py` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `scripts/macos-refresh-collectors.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/macos-verify.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/media-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/notebook-import-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/prepare-benchmark-context.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/profile-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/read-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/reference_backend.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/smoke-auth.py` | `M` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/smoke-media.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/source-transport-differential.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/stage-references.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `scripts/test-cargo.py` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `scripts/test-collector-bridge.py` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `scripts/test-collector-chatlog.py` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `scripts/test-test-runners.py` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `scripts/test.sh` | `M` | Regression contract / runner / CI; excessive-tests assessment |
| `scripts/test_diagnostics.py` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `scripts/whatsapp-licenses.py` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `services/whatsapp/PROVENANCE.md` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `services/whatsapp/control.go` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `services/whatsapp/control_test.go` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `services/whatsapp/go.mod` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `services/whatsapp/go.sum` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `services/whatsapp/licenses/Go-LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/filippo.io_edwards25519@v1.2.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_DATA-DOG_go-sqlmock@v1.5.2/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_agnivade_levenshtein@v1.2.1/License.txt` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_andreyvit_diff@v0.0.0-20170406064948-c7f18ee00883/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_beeper_argo-go@v1.1.2/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_coder_websocket@v1.8.15/LICENSE.txt` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_coreos_go-systemd_v22@v22.7.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_coreos_go-systemd_v22@v22.7.0/NOTICE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_davecgh_go-spew@v1.1.1/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_elliotchance_orderedmap_v3@v3.1.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_golang_protobuf@v1.5.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_google_go-cmp@v0.7.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_google_uuid@v1.6.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_mattn_go-colorable@v0.1.14/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_mattn_go-isatty@v0.0.20/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_mattn_go-sqlite3@v1.14.52/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_petermattis_goid@v0.0.0-20260918085751-abfca077860b/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_pkg_errors@v0.9.1/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_pmezard_go-difflib@v1.0.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_rs_xid@v1.6.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_rs_zerolog@v1.35.1/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_sergi_go-diff@v1.3.1/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_skip2_go-qrcode@v0.0.0-20200617195104-da1b6568686e/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_stretchr_testify@v1.12.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/github.com_vektah_gqlparser_v2@v2.5.27/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/go.mau.fi_libsignal@v0.2.2/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/go.mau.fi_util@v0.11.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/go.mau.fi_whatsmeow@v0.0.0-20261110152934-c88388f983b4/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_crypto@v0.58.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_exp@v0.0.0-20261014143641-f0251eb4aa3e/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_mod@v0.42.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_net@v0.61.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_sync@v0.24.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_sys@v0.49.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_term@v0.47.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_text@v0.43.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_tools@v0.52.0/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_tools_go_expect@v0.1.1-deprecated/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/golang.org_x_tools_go_packages_packagestest@v0.1.1-deprecated/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/google.golang.org_protobuf@v1.36.12/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/gopkg.in_yaml.v3@v3.0.1/LICENSE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/gopkg.in_yaml.v3@v3.0.1/NOTICE` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/licenses/inventory.json` | `A` | Vendored licence/data; correctness/security not independently re-audited |
| `services/whatsapp/main.go` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `services/whatsapp/main_test.go` | `A` | Regression contract / runner / CI; excessive-tests assessment |
| `services/whatsapp/media.go` | `A` | Production runtime/build/config; see area coverage and acceptance limits |
| `tmp/reviews/2026-11-12-bugbot-report-4e29e212.md` | `A` | Review/reference documentation; not production-relevant |
| `tmp/reviews/2026-11-12-code-review-followups.md` | `A` | Review/reference documentation; not production-relevant |
| `tmp/reviews/2026-11-12-code-review-source-parity.md` | `A` | Review/reference documentation; not production-relevant |

### Verification Commands

- `git merge-base HEAD origin/main`: `301241bf41e8ba8bb18d981f4e9138fe220e9c18`.
- `git log -1` and `git status --porcelain`: target `f7c9f01`, clean before report.
- `git diff --name-status <base> HEAD`: 439 paths inventoried below.
- Read-only source/control-flow review of the fresh frontier above. No production edits, staging or Git-state changes during detection.
- Prior executable validation: `docs/reviews/2026-11-12-rust-parity-validation.json` and companion review; those runs preceded the commit and are identified as historical, not newly executed here.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Identity deactivation external-ID mismatch | dismissed | Durable queued contact reproject resolves the matching identifier rows |
| Embedded bridge termination on pause | dismissed | Intentional process boundary; pause retains keys and uses paused control; prior successful regression |
| Error writing control and error clearing device keys | merged into B1 | Same root cause: unconditional state overwrite loses durable disconnect intent |
| Test volume alone | dismissed | Distinct domain, HTTP, platform and browser boundaries justify the existing coverage |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| External providers and packaged Mac permissions | No real account/permission exercise | Dedicated test accounts and packaged runtime acceptance |
| Linux image/NAS runtime | No local Docker daemon | Isolated image build and runtime acceptance on Linux/NAS |
| Vendored code/data and production scale | No independent full audit or representative benchmark | Dependency/data audit and workload profiling |

## Report Self-Check

- yes: Every finding appears in index and card.
- yes: Every meaningful reviewed frontier candidate appears in ledger.
- yes: Every Finding B1 ledger row maps to the card.
- yes: Every Not covered row names blocker/next step, completion is Incomplete.
- yes: Counts are high 0, medium 1, low 0.
- yes: Changes requested follows the medium-finding mapping.

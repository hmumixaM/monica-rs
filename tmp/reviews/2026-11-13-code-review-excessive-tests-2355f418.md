# Independent code-review and excessive-tests

- Date: 2026-11-13.
- Repository: /Users/maxhu/play/monica-rs.
- Branch: codex/nas-isolated-migration.
- Actual base / latest HEAD / merge-base: `027eaa2e1c113d4345ca82711215cbed4281be45` (`origin/main`).
- Target: all tracked changes against actual base plus all 33 non-ignored untracked files present at snapshot time, including one prior report classified non-production. 65 paths total (32 tracked modifications + 33 untracked).
- Dirty snapshot SHA-256: `1f6f1b016a503557fbe073288d7b0e39946907634b1e904b60b4f48fa37493de`.
- Per-file hashes and complete coverage inventory: [immutable independent Bugbot report](2026-11-13-bugbot-report-2355f418.md), ID `bb-20261113-2355f418`.
- Context: fresh delegated review, with no inherited parent conversation. The parent task supplied requirements and scope. No production source, Git state, external comments/PRs, NAS SSH or existing database was modified/accessed.
- No PR exists. This record does not approve a future final PR head.
- Local skills read and applied: /Users/maxhu/.codex/skills/bugbot/SKILL.md (detect only), /Users/maxhu/.codex/skills/code-review/SKILL.md, /Users/maxhu/.codex/skills/excessive-tests/SKILL.md. Complete Bugbot report contract read.
- Immutable review artifact: do not rewrite after fixes; create a new independent review.

## Code-review

Recommendation: **Changes requested**.

Two confirmed medium correctness findings require repair. Their complete evidence and location are in Bugbot B1/B2:

1. `scripts/legacy-chatlog-import.py:210`: existing message payload may be a JSON string accepted by the CRM converter; unconditional `.get` aborts all Chatlog retries for that message.
2. `scripts/legacy-chatlog-import.py:45-46`: Chatlog inspect records names but never classifies tables; unsupported table/file content is silently omitted from Reader business conversion while file completion permits `converted`. This violates explicit all-table classification and unclassified-table failure requirements.

Reviewed architecture/security/persistence boundaries:

- Read-only source sockets; source/target identity and initial-data guards; source manifest fingerprints; atomic bounded checkpoints and retry.
- Original immutable content/metadata, revision relationships, provider payload/revision/observation mapping, TOTP re-encryption and credential/session exclusion from live auth.
- Account-linked versus manual fields, structured values, tags, relationships, membership uncertainty, source relink and manual override readers/writers.
- Chatlog conversation identity, exact source sender, missing sender sentinels, corrected text/attribution, revoke notices and targets, Moments canonical XML ID, contact/interaction history, page receipts, deterministic second settlement.
- Cookie namespace across login/logout/CSRF/session list/OAuth; saved analysis privacy visibility versus claim/dispatch/provider/browser execution gates; collection/OCR/MCP defaults.
- Legacy authenticated history and content endpoints, canonical file containment, bounded pagination, message media/citations, history controls, dashboard/read callers.
- NAS copy read-only bind and DAC read capability, destination traversal, independently written file/temp/CAS inode, SHA checks, WAL backup, resource/health/cancel guards, download/export receipt identity.
- New Compose network/roots/image variables/secrets/cpuset/memory limits and documentation. No LiteLLM restart is part of these files. Documentation reflects cancellation of the earlier fixed 1.5 GiB/five-minute condition and the permitted temporary Monica Next stop/restore.

No additional confirmed runtime bug was found within the reviewed source frontier.

### Prior-report reconciliation after independent detection

The prior report `bb-20261113-8f55023a` was read only after the new independent detection report had been written. Its immutable contents were not edited.

| Prior finding | Latest source result | Evidence / limit |
| --- | --- | --- |
| B1 archive reader permissions | Source remediation present | New outer root stays 0700; bind-visible archive layers and migration status root are 0755; download uses restricted sudo reads; copied files normally created readable. Actual NAS ACL and runtime UID access still require deployment acceptance. |
| B2 sender-only corrections accepted | Repaired in reviewed source and synthetic integration | Messages now compare/write sender and sender_account_id when authoritative identity exists; missing-sender sentinel preserves existing attribution and logs a gap. Existing test changes sender-only data and asserts the new sender. |
| B3 stale conversation/sidebar/Memory after Chatlog | Repaired in reviewed source and synthetic integration | Chatlog apply calls actual legacy-finalize, then checks current conversation/sidebars/pending Memory. Existing integration asserts post-Chatlog message count and preview. |
| B4 downloader depends on global latest source | Repaired in reviewed source | Downloader first chooses copy-status-chatlog-db.json; fallback still validates source. New-source copy no longer hides completed Chatlog state when source-specific document exists. |

### Validation and limits

Passed: 3 synthetic PG migration tests, 4 file-copy tests, 2 labelled-cancellation tests, 11 Chatlog reader tests, Go codec tests, `git diff --check`. Two extra in-memory probes reused the Python fixture-owned disposable PG and temporary SQLite files and reproduced B1/B2.

The Python fixture invokes the existing local monica-server binary for actual SQL migrations/finalization. This reviewer did not independently rebuild that binary or execute Rust sqlx HTTP tests/full CI. Rust added tests were read. No live NAS test, old-service health comparison/stop/restore, browser rendering, complete real source data, media full inventory or final PR-head review is claimed. Final source build, full offline verification and isolated NAS acceptance remain necessary.

## Excessive-tests

Conclusion: **The changed tests are proportionate; no redundant-test deletion or consolidation is recommended.** This is a coverage/value review, separate from Bugbot runtime findings.

| Test area | Concrete protected behavior | Assessment |
| --- | --- | --- |
| scripts/test-legacy-import.py | Independent read-only sources and target rejection; committed page restart; content/revision mapping; password/TOTP preservation; post-import settlement; Chatlog replay/corrections/revokes/sender gaps and tamper rejection | Small disposable cluster shared only within the unittest class; databases and data are synthetic. Three broad integration scenarios cover meaningful cross-component/persistence boundaries. |
| crates/monica-server/src/legacy_tests.rs | Same-host cookie/CSRF independence; historical analysis readable while execution disabled; authenticated bounded evidence/unsafe file paths; source relationship/tag relink with manual override | Four focused HTTP/SQL tests protect distinct contracts; source/assertion volume alone is not excess. No removal proposed. |
| scripts/test-migration-file-copy.py | Independent inode+hash and resume data damage; WAL commits and source consistency; health pause preventing partial publish; live WAL writers unaffected by pinned snapshot | Four short tests cover distinct data-loss/concurrency failure modes. Controlled callbacks avoid sleeps. Retain all. |
| scripts/test-nas-migration-copy.py | Missing/unrelated container never stopped; exact newly labelled worker can be paused | Mocked OS boundary is appropriate because real service stops must never run in local CI. Tests execute generated code and assert actual selected target, rather than source-text matching. |
| tools/migration-media/main_test.go | Destination marking/isolation, symlink/path escape, tamper detection, independent source bytes, pinned V4 codec roundtrip | Two distinct boundaries; byte equality and real codecs provide useful evidence. Retain both. |
| .github/workflows/ci.yml | Runs self-owned disposable PG converter tests and file/cancel tests after server build | Appropriate focused CI addition; no redundant external service added for this fixture. |

### Coverage actions

These are missing-coverage observations, not excessive-test or additional production-bug findings:

1. **Medium, linked to Bugbot B1** — Add a regression beside `scripts/test-legacy-import.py:168` with existing CRM message content stored as a JSON string, then Chatlog reconcile/verify. Preserve existing dict/binary and immutable original assertions. The current fixture always uses object-shaped message payload, so it misses the proven crash.
2. **Medium, linked to Bugbot B2** — Add inspection tests for an unknown SQLite table and an unrecognized *.db file path. Assert rejection or an explicit archival disposition and row/hash accounting. Keep the existing normal replay/attribution test. This protects the explicit completeness gate instead of merely matching implementation text.
3. **Low, tools/migration-media/main_test.go:1 / .github/workflows/ci.yml:98** — The new Go module's tests pass locally, but the existing CI `go test ./...` is scoped to services/whatsapp and does not include tools/migration-media. Add a focused step for the new module with its own go.mod/toolchain context. No tests should be deleted.
4. Actual NAS UID/archive/download acceptance remains an operational check. Do not replace it with source-text assertions about chmod/sudo options.

Tests-audit was not invoked: no broader noisy, slow, flaky or redundant suite problem was established, and the task asks for PR-focused excessive-tests.


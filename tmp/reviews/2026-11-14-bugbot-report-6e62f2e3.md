# Bugbot Report

## Report Contract

- Report type: `bugbot`; independent operational-script review also applying `code-review` and `excessive-tests`.
- Report ID: `bb-20261114-6e62f2e3`
- Review date: `2026-11-14`
- Report path: `/Users/maxhu/play/monica-rs/tmp/reviews/2026-11-14-bugbot-report-6e62f2e3.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: `Fixed review input; do not rewrite during repair`

## Scope

- Diff mode: explicit added-file operational review; no broad PR review.
- Requested base: none; this private ignored script was explicitly selected by the parent task.
- Resolved baseline: no previous tracked version; repository HEAD `db04fe884b43f7551eba6aeccbe84af026f077d6` on `codex/nas-isolated-migration`.
- Reviewed target: entire `.local-validation/nas-migration-20261113/nas-restore-converted-worker.py`, SHA-256 `37d13f4a129399b500fa711a11d4faf914d587af0fa2940e2e09f9657f3ec0d3`.
- Changed files: 1 explicitly selected private operational script. Other working-tree changes and prior reports excluded.
- Completion: Complete within reviewed local/script scope.
- Custom instructions: Read-only review before execution. No NAS or real database access, no reviewed-source edits. Focus original Next data preservation, isolated Rust restoration, atomicity, orphan processes and leakage. Do not require a fixed startup-memory gate. The ordinary Bugbot exclusion of ignored files is overridden only for this explicitly named input.
- Assumptions: Exact NAS PG container and isolated deployment discovered by the parent remain the intended target. Source dump is independently generated on the Mac and uploaded via the reviewed atomic/hash-verifying uploader. No adversarial concurrent file replacement is assumed.

## Detection Snapshot

- Recommendation: Changes requested.
- Completion: Complete within reviewed local/script scope.
- Why now: The low-memory/stop path can exit without stopping its restore, defeating its resource guard and leaving completion state ambiguous.
- Findings count: high 0 | medium 1 | low 0.
- Must-review now: B1.
- Coverage confidence: high for the demonstrated lifecycle defect; medium for environment isolation because runtime access was prohibited.
- Biggest blind spot: NAS container state/mount resolution and real restore behavior were intentionally not accessed.

## Complete Findings Index

| ID | Severity | Category | Location | Title |
| --- | --- | --- | --- | --- |
| B1 | medium | LOGIC_BUG | `.local-validation/nas-migration-20261113/nas-restore-converted-worker.py:43-45` | Cancellation can leave restore running |

## Findings

### B1 medium - Cancellation can leave restore running

- Category: LOGIC_BUG
- Location: `.local-validation/nas-migration-20261113/nas-restore-converted-worker.py:43-45`
- Confirmation selector: B1

Description: Cancellation sends a single `pg_cancel_backend` query, ignores whether it matched any backend, and immediately waits at most 180 seconds. `docker exec` is asynchronous: the first memory check can run before its `pg_restore` database connection exists. When memory is already below the 768 MiB floor, or a stop arrives during startup, cancellation can match zero rows and the restore subsequently starts normally. If it takes longer than 180 seconds, `TimeoutExpired` escapes and there is no cleanup/finally path to stop and reap the exec process or verify its database session is gone. Closing the worker's file objects does not close inherited descriptors in the Docker CLI child. The restore can keep consuming shared NAS resources and eventually commit after monitoring has stopped. If it instead completes successfully within the wait, the script unconditionally reports `database_restore_cancelled`, despite the commit.

Rationale: The new script itself creates this race by starting the restore at line 38 and immediately running a memory/stop check at lines 39-43, without waiting for or acknowledging the matching backend. A fixed startup gate is unnecessary; the cancellation lifecycle must remain responsible for the child until it has verifiably stopped. This is material because retaining collectors while respecting the resource floor is part of the authorized restore's safety contract.

Evidence:

- Lines 38-43: `Popen` returns before any proof the PG session exists; the first memory sample can immediately enter the cancellation branch.
- Line 43: `sql(...)` returns text, but the returned cancellation count/result is ignored.
- Lines 44-45: `wait(timeout=180)` has no exception handling; a successful return is also not checked before claiming cancellation.
- Lines 37-48: the restore lifetime has no `try/finally` cleanup/verification path.
- A pure-memory execution of the exact reviewed source supplied correct identity, zero tables/clients and a matching receipt, then emulated memory 700000 KiB and no restore backend yet. Observed events were `database_restoring`, restore child started, cancellation matched zero, wait(180), unhandled `TimeoutExpired`; the mock restore remained running. No file writes, real processes, services or databases were involved in this reproduction.

Proposed correction: Treat cancellation as a bounded protocol owned by this worker. Use a run-unique application name, keep checking the exact pinned container/database/session while the exec is alive, and retry scoped cancellation or termination until the restore exits and its session is absent. Handle timeout and other monitor errors with the same cleanup protocol. Do not assume killing the Docker CLI stops the container process. Report `cancelled` only after exit and rollback/empty-target evidence; if the restore committed before cancellation, report that actual outcome. If safety cannot be established, report an explicit unresolved state and retain monitoring/escalation responsibility. Re-review the edited script independently before execution.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Old database/container writes | lines 2-5, 9-15, 38, 43 | Reviewed - no bug found | Every database exec targets the same full pinned new-container ID and explicit `monica_rust`; no old container start/stop/volume mutation exists in this script. | Static command argument trace. Actual NAS identity is a parent-owned runtime precondition. |
| Mount/network containment | lines 9-16 | Reviewed - no bug found | Checks exact container name/project and only the isolated network, rejects mount sources outside the new root; root itself must resolve canonically. | `prepare-nas-preview.py:10-18` also rejects aliased initial data directories. Exact runtime mounts were deliberately not fetched. |
| Incomplete/corrupt artifact | lines 17-26 | Reviewed - no bug found | Artifact/receipt size, SHA-256 and direct symlinks are checked before database mutation. | `upload-converted.py:9-36` uses private partial files, streaming SHA and atomic rename; it refuses differing completed artifacts. |
| Nonempty target overwrite | lines 28-30 | Reviewed - no bug found | Refuses user tables or active clients before restore; no drop/clean command. | Target is separate new PG container; `pg_restore` is single-transaction and fails on collisions. No concurrent launch was requested. |
| Restore atomicity on normal SQL failure | line 38 | Reviewed - no bug found | One transaction with exit-on-error; ordinary restore failure rolls back its new target changes. | `--single-transaction --exit-on-error`; no parallel jobs or `--create`. |
| Low-memory/stop startup race and orphan cleanup | lines 39-45 | Finding B1 | A zero-match one-shot cancellation can be ineffective; wait timeout abandons restore, successful wait can mislabel committed work. | Pure-memory exact-source reproduction; fix and fresh review required. |
| Post-restore receipts | lines 49-53 | Reviewed - no bug found | Requires fixed migration/chatlog receipts and no deterministic Memory pending rows, then marks integrity verification pending. | Does not claim full content verification passed or enable collectors/models. |
| Secret/data output | lines 8, 31, 37-38, 45, 48, 53 | Reviewed - no bug found | Printed status includes metadata/hash/identity only. Verbose restore log stays under new data root. | Root is provisioned mode 0700 (`prepare-nas-preview.py:12`); credentials are not embedded in restore command. |
| Changed automated tests | whole scope | Not production-relevant | No test changes in this expressly selected private script scope; no excessive-test finding. | Focused in-memory lifecycle reproduction was proportionate to B1. |

## Code-review Assessment

Changes requested for B1. The bounded command arguments, explicit new DB, complete-ID pin, pre-restore hash, empty-target check and single transaction protect the old application from direct overwrite. The unresolved cancellation ownership is the material execution risk. No style or general refactor request is raised. This report does not approve the separate PR or authorize deployment.

## Excessive-tests Assessment

No changed tests are in this scope, so there is no test bloat finding. Cancellation before backend registration, cancelled-query completion, wait timeout and successful-commit-before-cancel are useful observable lifecycle cases for a small mocked check. Do not require a broad suite or real NAS restore to exercise them. The review's one pure-memory reproduction used the exact source and wrote no test artifacts.

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| `.local-validation/nas-migration-20261113/nas-restore-converted-worker.py` | Explicitly selected ignored private added operational file | New PostgreSQL restore, process cancellation, integrity checks, resource monitoring, status/log files |
| `.local-validation/nas-migration-20261113/upload-converted.py` | Read-only supporting context | Atomic upload and parent-path policy |
| `.local-validation/nas-migration-20261113/dump-converted.py` | Read-only supporting context | Mac-only dump provenance and receipt generation |
| `.local-validation/nas-migration-20261113/prepare-nas-preview.py` | Read-only supporting context | New path preparation and modes |
| `infra/nas.compose.yaml` | Read-only supporting context | New PG volume/network topology and disabled model execution |
| `tmp/reviews/*` | Existing review artifacts | Excluded; not recursively reviewed as production changes |

### Verification Commands

- Read local NAS, Bugbot, code-review, excessive-tests skills, Bugbot report contract and user review memory.
- `git status --short`, `git rev-parse HEAD`, `git branch --show-current`, `git check-ignore -v`: recorded repository state and explicit ignored-file status without modifying Git.
- `nl -ba` and targeted `rg`: traced the entire selected script and only directly supporting operational files/config.
- Python `ast.parse` of reviewed source: syntax parsed successfully without generating bytecode.
- Pure-memory `exec(compile(exact_source, ...))` with fake path/subprocess/signal modules: reproduced B1; no NAS access, real DB queries, real child restore or test-file writes.
- SHA-256 of reviewed source: `37d13f4a129399b500fa711a11d4faf914d587af0fa2940e2e09f9657f3ec0d3`.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Require a fixed memory threshold before any restore startup | dismissed | User explicitly accepts runtime floor without a fixed startup gate; retrying/acknowledging cancellation addresses the defect. |
| Nonzero wait/monitor exception orphaning | merged into B1 | Same missing cancellation ownership/cleanup protocol. |
| Deleting original data on restore error | dismissed | No old PG ID, old DB path, drop, clean or volume delete occurs. |
| Concurrent artifact replacement/symlink attack | dismissed | No evidence of such an actor or concurrent uploader; supporting uploader atomically finalizes and refuses different existing files. |
| Claim full business-data verification after restore | dismissed | Final status explicitly says integrity verification pending. |

### Blind Spots

| Area | Why it remains unverified | What would resolve it |
| --- | --- | --- |
| Actual NAS container, network and mount identity | Runtime/NAS access expressly prohibited for this review. | Parent's read-only inspection immediately before execution. |
| Real archive contents and successful full-data restore | Dump upload was in flight and NAS restoration not started; executing against real data prohibited. | Parent's SHA/receipt check, completed restore and separate content-integrity verification. |

These are stated environment/acceptance limitations, not unreviewed local code candidates. The report is complete for the requested static and simulated operational scope.

## Report Self-Check

- yes: Every finding appears in both Complete Findings Index and Findings.
- yes: Every meaningful recursive candidate appears in Detection Coverage Ledger.
- yes: Every Finding B1 ledger row maps to its finding card.
- yes: No local production-relevant candidate is marked Not covered; runtime limitations are explicit and outside authorized access.
- yes: Severity counts match the finding cards.
- yes: Recommendation follows the skill mapping: one medium finding means Changes requested.

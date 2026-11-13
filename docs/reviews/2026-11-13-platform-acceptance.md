# Monica Rust platform acceptance — 2026-11-13

Authorized local execution is complete. Further NAS execution was stopped at the user’s explicit request. Correctness results, retained failures and measured performance limitations are distinguished below. The [validation manifest](2026-11-13-platform-acceptance-validation.json) records source/artifact identities, exact test counts and SHA256/gzip round trips. [Raw inputs and run logs](evidence/2026-11-13-final/) are committed alongside it.

This records the authorized account-free acceptance from chat `01a1cbb8-f9c2-7f60-8dca-c8601a2161f1`. Tests use generated records, fresh private clusters, fake providers and explicitly owned containers/volumes. Original worktrees, existing services, private accounts and production data are preserved.

## Source, artifacts and review

Frozen application source is `bb69b395803daf1c36f27f226e4ae437585907f3`, with source archive SHA256 `19c28e8cc1ae3098646ba3da39ef5915084a01d5d96a981e252db780cd6b5f72`. The fetched default branch and actual merge-base are `301241bf41e8ba8bb18d981f4e9138fe220e9c18`. The private branch is `codex/complete-multisource-rust`. Later acceptance/report commits do not change application bytes. No PR exists; the actual local/remote branch delta was reviewed. No merge, public image publication or production cutover is part of this test delivery.

[Exact-source CI43437516559](https://github.com/hmumixaM/monica-rs/actions/runs/43437516559) passed all six jobs: backend, Web, adapters, native Mac and native AMD64/ARM64 optional containers. CI is verification alongside the skill-based source reviews, rather than a substitute for review.

The [original parity review](2026-11-12-rust-parity-review.md), [committed follow-up](2026-11-12-committed-rereview.md), [append-only integration review](2026-11-13-append-only-merge.md) and immutable reports under `tmp/reviews/` preserve the repair chain. The [C2 Mac checkpoint](2026-11-13-mac-acceptance.md) remains historical. Final evidence here supersedes its pending gates and pre-replay-repair measurements.

Additional actual-execution repairs covered complete PostgreSQL configuration sharing, synchronous Linux media-helper dispatch before Tokio/RLIMIT_AS (old64-worker stress8/100 failures; repaired100/100 passed), Synology cpuset compatibility, the previously no-op authentication smoke CLI, clean native source CI resources, exact optional-collector Docker context inclusion and private-state exclusion, native ARM64 Chromium validation, benchmark failure accounting, explicit bounded administrative migration deadlines, final TCP PostgreSQL readiness and bounded child-spool test readiness. These did not relax ordinary application query/lock/resource budgets.

The latest real NAS test found a valid already durable signed import returning409: its no-op receipt UPSERT requested a writer lock on mutable processing state. [Immutable reportbb-20261113-53370ca3](../../tmp/reviews/2026-11-13-bugbot-report-53370ca3.md) records the actual failure and two independent lock reproductions. Commitbb69b39 reads/reuses a committed identity and checks its immutable hash. Concurrent first acceptances retain unique arbitration and atomic receipt/input/media-progress creation, with a fresh READ COMMITTED lookup for a loser. HMAC, timestamp, nonce, token, connection/provider checks and timeouts remain. The old implementation failed the held-progress regression; final lock/read-only/xmin and deterministic concurrent-first tests passed in both timezones. [Committed Bugbot re-review](../../tmp/reviews/2026-11-13-bugbot-report-727e65bf.md) found no new bug in the affected frontier; its execution limitations are frozen at review time. Code-review and excessive-tests assessments found the two new tests proportionate and retained the existing four append-only tests.

## Correctness and packages

- Mac repaired workspace:177 passed in UTC and177 in America/Chicago,0failed,3default ignored each. Explicit generated-video1 and performance2 passed. Final test-only strengthening passed all6 append-only tests in both timezones and denied-warning all-target Clippy. Exact committed-source CI reran the full suite.
- Native ARM64 Chicago:178 Rust successes including explicit video,0failed; actual media-helper1, collector-supervisor4 and authenticated HTTP/media/CLI tests passed. Exact-source CI covers native AMD64 and ARM64 optional WhatsApp/browser images.
- Latest Mac native CI:4 UI tests,14 Rust unit tests and1 disposable real PostgreSQL/server lifecycle test passed. Web:5 router/agent tests plus the populated desktop/mobile Rust-backed browser smoke passed. Locked collector8, Chatlog11, supervisor4, LinkedIn2 and the Go suite passed. Original native Messages fixture and differential evidence remain linked in the parity record; no personal native database was read.
- Complete Apple Silicon `Monica Rust.app`:1,318,956,021bytes, backend SHA256 `42079aac513e98fa5edbb38a06c8b7a385c75831091d57f22834362bbb7586be`. Source provenance is clean bb69b39. Deep signatures, relocation, locked Python/offline Chromium, ffprobe, PG16.15, fresh DB, same-origin static Web/auth, pause/suspend/resume, crash recovery, ports and exit verification passed. Signing is ad hoc for testing; public notarization is excluded. Duplicate generated staging was retired after verification; the complete app retains every runtime dependency and staging is reproducible.
- Linux AMD64 backend SHA256 `66ae0fede89a98cd9de8ce3d8560e4fbcef407abb89de474fbd294d6de6ee503`; ARM64 `4fc44a3a05d2c0d9b43f8fc4a4bc38fbfd1491ced1e177cb46a5e382f89a62c4`. Both rebuild core/server and include22 Cargo-selected test executables. Verified image identities and source labels are in the validation manifest. Rebuildable local extraction payloads were retired after successful NAS upload and image verification, retaining hashes and complete images.

Further NAS testing was explicitly canceled by the user: 你别在我 NAS 上测试了. No further NAS connections or tests were made after that instruction. No local NAS validation controller or watcher remains active. The already completed physical fixture's server, PostgreSQL container, hold container and both data volumes were independently confirmed absent before the stop instruction. Built images and uploaded build payloads are retained; no production service was replaced.

Before the stop, the new-source actual NAS image passed exact signed replay with a parent-key-share holder and a progress writer uncommitted (HTTP9.360s/4.065s, including nonce durability), and valid different-body rejection409. A new1000-record batch accepted while paused survived server and PostgreSQL SIGKILL (exit137 each). WAL recovery, resume to exactly2000 messages, duplicate-free signed replay, graceful server exit0 and restart persistence passed. Final counts for profiles/media/messages/receipts/inputs: [3, 2, 2000, 2, 2].

The separate native NAS UTC/Chicago logical matrices did not start: reverse Unix-socket forwarding was rejected before any test process ran. No SSH service configuration was changed. The24-case NAS performance comparison never started. These are unexecuted/canceled gates, not passes. The previously proposed private Mac database transport was not used to run NAS test assertions. Their cancellation does not invalidate the completed physical or local/CI evidence.

## NAS storage and preserved failures

The Synology runs alongside existing services, with private network-none namespaces, no published host ports and only newly owned synthetic volumes. CPU sets and memory bounds are recorded per run. Existing services, storage maintenance, accounts, models and source worktrees were not stopped or modified.

Fresh physical runs applied43 migration files through version48 on actual NAS storage. Early10/300second migration bounds failed on this slow disk; the final administrative deadline is explicitly3600seconds per statement, independently bounded from ordinary interactive10s/background15s/control5s query and1s lock budgets. The failed runs remain evidence. Final follow-ups revalidate checksums and reuse only the already migrated, verified private fixture; no schema templates or fsync changes are used.

The original unchanged10second auth smoke timed out receiving a profile B update. That write had committed. Standalone PostgreSQL without Rust took11.81seconds for one durable single-row transaction. The continuation reauthenticated the same synthetic user, verified durable B and its same-ID no-op replay with two history entries, and executed the exact unchanged original tail AST after the lost response. All144 real auth/media requests and62 auth-tail assertion nodes passed (nested media assertions also ran). Seven requests exceeded10seconds, maximum21.813seconds. Its functional transport alone used90seconds; the original10second failure is retained and is not relabeled as a pass. Authentication/media source was unchanged by the subsequent receipt repair. New signed crash/replay tests use the frozen benchmark client's120second transport; application and timed benchmark budgets remain unchanged.

Private acceptance-driver errors are retained separately: a marker substitution corrupted a PostgreSQL-version identifier before any HTTP checks, and a negative request used an object where cursor_after requires a string and correctly received422. Corrected drivers use the same original business assertions and a valid different-body string for409. Neither failure was evidence of an application defect.

## Performance method

The completed Mac comparison has24 cases: four fixed backends ×1x/10x histories ×three fresh rounds. Inputs are100 contacts,10,000/100,000 initial messages,1000-record signed batches,64 warm samples per route and8 readers; a further import runs concurrently until settlement. Reads cover directory, search, overview, profiles, conversations and messages. Data is established through public signed imports and profile/link APIs. Business outputs are not precomputed.

Mac reran all6 Rust cases at bb69b39 and preserves18 unchanged Python cases from the earlier full run. Row/source/invocation provenance, baseline metadata and raw SHA remain; final Rust/reference rows were not interleaved. The planned24-case NAS comparison was not started and was canceled at the user’s explicit request. Python pins remain CRM `6d59cfef4b1a12f678d809a00e37d8332be70b06`, desktop `e35b49e4ca33406f0fdd7ecf8767931b44fdac87` and multisource `1a6e40e20c6ac12aca434b61244059d067847473`, with unchanged locks.

Acceptance and settlement throughput are distinct. Successful p50/p95/p99 exclude failed reads; failure counts/rates/latency and whole-case failures remain separate, without retries. A first API request is not OS/PostgreSQL cache eviction. Shared RSS can double-count pages; sampled transaction/lock/IO observations miss short events. Sampled blocked-query age is not a true lock-wait duration percentile. Python pool acquisition and unavailable Mac process IO are unmeasured. Rust pool acquisition uses SQLx0.8.6's actual `aquired_after_secs` trace field, with all pools combined and debug logging overhead retained. SQL/WAL include full backend work; mixed figures also include reads. Narrow append/journal microbenchmarks cannot replace these end-to-end figures.

### Mac three-round results

Recorded 24 cases; whole-case failures: 0. Successful-case metrics below retain n/3 rather than treating failed cases as latency samples.

| Backend | History | Successful cases | Settled messages/s | Core peak RSS MiB | SQL calls/1000 | Slowest route mixed p95 ms (median) |
| --- | --- | --- | --- | --- | --- | --- |
|crm|10000|3/3|491.5|233.1|19330.5|23.60|
|crm|100000|3/3|479.2|233.0|19329.2|485.00|
|desktop|10000|3/3|491.2|233.1|19330.5|23.51|
|desktop|100000|3/3|479.8|233.2|19329.2|418.61|
|multisource|10000|3/3|375.4|234.2|23330.5|29.91|
|multisource|100000|3/3|366.5|234.1|23329.2|389.86|
|rust|10000|3/3|542.3|62.4|1286.1|11.32|
|rust|100000|3/3|701.7|64.8|1262.9|12.14|

At 1x: worst measured Rust mixed route/run p95 11.65ms; successful-route read failures {'cold_first_request': 0, 'warm': 0, 'mixed': 0}; median paired RSS ratio to Desktop 0.268; median paired settled-throughput ratio 1.105. Per-round route-quantile regressions over10%: 0, including separate p95/p99 entries. All original targets demonstrated: False.

At 10x: worst measured Rust mixed route/run p95 14.04ms; successful-route read failures {'cold_first_request': 0, 'warm': 0, 'mixed': 0}; median paired RSS ratio to Desktop 0.278; median paired settled-throughput ratio 1.462. Per-round route-quantile regressions over10%: 6, including separate p95/p99 entries. All original targets demonstrated: False.

Mac warm/mixed route quantiles have no paired-round regression over10%; the10x cold-first search calls do. Each cold route has one request per case, so its reported quantiles are individual first-call observations, not a statistically established tail distribution. The1x Mac throughput target of30% improvement is unmet. True lock-wait p95 remains unmeasured. The machine target-evaluation JSON retains every round/route comparison; no claim that all high-performance goals passed is made.

## Feature/contract and remaining external boundaries

The [feature matrix](2026-11-12-rust-parity-review.md) and [multisource record](../MULTISOURCE_PARITY.md) cover shared CRM/notebook functionality, seven sources, evidence/revisions, media, Mail/WhatsApp, model settings, incremental Memory and browser agent/adapters. Linux/NAS disables Apple Contacts/Messages collection without its Mac reader while preserving stored evidence and queries. Mac has the native transport and separate application identity/data directory. Model and adapter calls start disabled and require explicit configuration.

Registration audit:166/166 frozen operations. The earlier `docs/api-coverage.json` is a148-operation historical CRM smoke snapshot; its pending/partial labels are not current platform status. Registration is distinct from selected differential/database/HTTP/browser behavior evidence and is not per-operation exhaustive input parity certification. The original contracts remain immutable baseline evidence.

Live personal OAuth/mail/social/WhatsApp sessions, Mac permission grants, real model/face inference, broad vendor/dependency security review, public signing/notarization, personal-data migration, Mac/NAS synchronization and production cutover remain outside this account-free delivery. No claim of having proved every possible input or eliminated every possible bug is made.

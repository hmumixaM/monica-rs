# Mac acceptance checkpoint — 2026-11-13

**This is a completed Mac/ARM/CI checkpoint. NAS runtime, NAS logical matrices and NAS performance are still pending. It is not a whole-platform readiness report.**

Application and artifact source is `c2e9e2f54e6f0f84188b57b8e8f4d64ebe4b3f23`, based on fetched `origin/main` at `301241bf41e8ba8bb18d981f4e9138fe220e9c18`. The source archive is clean; SHA-256 is `f3458fe176d2bcfbc71645678281afcd6a749712aa225d525006c52d43e74cce`. The final source commit repairs test startup observation; application code is `4c473f6f0c102a8bf8b7f8cdd1a62279435d3378`.

[CI run 43413235968](https://github.com/hmumixaM/monica-rs/actions/runs/43413235968) passed all six jobs: backend, Web, adapters, native Mac, optional containers on native AMD64, and optional containers on native ARM64. There is no submitted PR, merge, GHCR publication or production deployment in this checkpoint.

## Actual tests and packages

- Mac workspace, separately under UTC and America/Chicago: **175 passed, 0 failed, 3 default ignored** in each run. The generated-video test and both performance tests were also explicitly executed: **1 + 2 passed**. All-target Clippy passed with warnings denied.
- Linux ARM64 under America/Chicago: **176 Rust successes including explicit video**, 0 failed. Media helper **1**, collector supervisor **4**, actual authentication/media HTTP and CLI ingestion/projection/lifecycle smoke passed.
- Earlier native UI **3**, native units **14**, real PostgreSQL/server lifecycle **1**, native Messages fixture **1**, Web router/agent **5**, actual API/settings and populated desktop/mobile browser checks retain their evidence. Current CI repeats the corresponding native, Web and adapter jobs. This does not test personal permission grants or live providers.
- Complete Apple Silicon app: **1,318,939,557 bytes**. Bundled backend SHA-256: `0bb13ad07c51f11f1b03883eb83488d46f6913c22ee1ec74a07e9000cb8ffa1c`. Clean source matches the full commit. Deep signatures, relocation, bundled ffprobe/PostgreSQL 16.15, pinned Python/offline Chromium, fresh database, static Web/auth, suspend/resume, pause, crash recovery, port handling and clean exit passed.
- AMD64 backend SHA-256: `6af7d914ec3a61634c52b6fd84024b2e445807b21f691e6457bde611bb641fff`; ARM64: `c1401d2b50550de2c89bb99124f8a29e176e813fba4ffa905b34cc25aa3a1a5e`. Each image/export contains 22 Cargo-selected test executables. Both workspace packages were explicitly rebuilt to avoid stale fingerprints from copied source timestamps.

Host disk exhaustion interrupted Clippy and the ARM export. Those failed attempts were retained locally. Only generated staging/exports and Cargo development caches were retired. Clippy then passed and the complete ARM export was verified. The original user runtime was preserved; no broad Docker prune was used.

## Mac performance

There are three rounds at each history size, 100 contacts, signed 1,000-record batches, 64 warm samples per route and eight readers. The six real reads are directory, search, profile overview/list, conversations and messages. A further 1,000/10,000 messages is imported while readers remain active. Each case uses fresh synthetic databases, public signed imports and public profile/link APIs.

The final comparison has **24 completed cases and zero whole-case/read failures**. Its 18 unchanged Python cases come from the earlier complete three-round run; six Rust cases were freshly measured at the final source. The compressed original, current Rust run and composed comparison are all retained. Composite metadata contains the original baseline metadata/SHA; each row records its actual source/invocation provenance. Final Rust and reference cases were measured in separate invocations, rather than interleaved together.

Fixed Python inputs remain CRM `6d59cfef4b1a12f678d809a00e37d8332be70b06`, desktop `e35b49e4ca33406f0fdd7ecf8767931b44fdac87`, and multisource `1a6e40e20c6ac12aca434b61244059d067847473`.

Three-round medians follow. RSS concerns the core server process, excluding PostgreSQL and optional collectors. The final column is the largest of the six routes' median mixed-phase p95 values.

| Backend | Initial messages | Settled messages/s | Core peak RSS MiB | SQL calls/1,000 | Mixed p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| CRM | 10,000 | 491.5 | 233.1 | 19,330.5 | 23.60 |
| Desktop | 10,000 | 491.2 | 233.1 | 19,330.5 | 23.51 |
| Multisource | 10,000 | 375.4 | 234.2 | 23,330.5 | 29.91 |
| Rust | 10,000 | 550.4 | 61.6 | 1,282.4 | 10.08 |
| CRM | 100,000 | 479.2 | 233.0 | 19,329.2 | 485.00 |
| Desktop | 100,000 | 479.8 | 233.2 | 19,329.2 | 418.61 |
| Multisource | 100,000 | 366.5 | 234.1 | 23,329.2 | 389.86 |
| Rust | 100,000 | 706.1 | 66.1 | 1,261.5 | 11.69 |

Every measured Rust mixed-route p95 is below 500 ms; the worst route/run is 12.17 ms. Paired-round median RSS ratios to desktop are 0.264/0.284; settled-throughput ratios are 1.121/1.475 at 1×/10×. The **1× throughput improvement does not reach the initial 30% target**. All three 10× cold-first search calls are 35.7–50.8% slower than desktop, and one first profiles call is 14.8% slower. Cold has one request per route per run; these are first-call observations, not established latency percentiles. Warm/mixed route p95/p99 show no paired-round regression above 10%.

All initial high-performance targets have **not** been demonstrated. This checkpoint does not claim that designation.

## Measurement limits and evidence

Successful request quantiles exclude failures; failure counts/rates/latencies are separate, with no retries. Cold means the first API call after imports, without evicting OS/PostgreSQL caches. SQL/WAL cover the complete isolated backend; they must not be replaced with the narrower journal microbenchmark's figures. The SQLx pool acquisition trace combines all pools; its `aquired_after_secs` spelling is verified against SQLx 0.8.6 itself. Debug tracing remains enabled and can add overhead. Python pool acquisition and unavailable Mac process I/O remain unmeasured. Sampled blocked-query age is not a true lock-wait p95. Shared RSS pages and short waits limit resource sampling. Coexisting host services were preserved. Differences include schema, projection and query algorithms as well as language/runtime.

The [validation manifest](2026-11-13-mac-acceptance-validation.json) records raw/compressed SHA-256 values. [Raw and summarized evidence](evidence/2026-11-13-mac/) includes all three performance inputs, per-route p50/p95/p99/ranges/error counts, SQL/WAL, process/database resources, scaling, target evaluation, test logs and exact artifact identities.

NAS images have been built and labeled at the current source. The first actual NAS run completed all **43 migration files through version 48**. A private HTTP driver then failed Python parsing before any HTTP assertion ran: broad `BASE` substitution corrupted its `NAS_DATABASE_VERSION` marker. The marker was corrected to `NAS_PG_VERSION`; all six expanded Python programs were syntax-checked before a fresh physical rerun. The failure and successful migration evidence remain preserved. **NAS runtime is not yet certified.**

The [shared feature matrix](2026-11-12-rust-parity-review.md) remains the functional scope. NAS disables native Apple collection and keeps stored evidence readable. Real account sessions, Mac permission grants, actual face/model inference, full third-party dependency/security review, personal-data migration, public distribution signing and production cutover remain outside this generated-data test checkpoint.

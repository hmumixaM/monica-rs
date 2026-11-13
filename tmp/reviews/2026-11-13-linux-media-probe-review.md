# Linux media helper acceptance finding — 2026-11-13

Reviewed source: `1dfd446f499b0ca905767aa3f878b29dbf2279b4`; actual branch merge base `301241bf41e8ba8bb18d981f4e9138fe220e9c18`. This is a confirmed acceptance finding within the user's ongoing review/repair and full-runtime-test request. Historical Bugbot reports are unchanged.

## C2 [medium] Run media helpers before Tokio initialization

Primary changed location: `crates/monica-server/src/main.rs:99-103`. `#[tokio::main]` constructs the multi-thread runtime before the first line of async `main`, so media probe/preview helpers already have worker threads when `probe_entry` applies Linux's 512 MiB RLIMIT_AS. Worker startup races with that process-wide limit. Thread alternate-stack allocation can fail and abort a valid media helper, which `MediaStore::helper_run` reports to the browser as HTTP 422. The bounded helper never needs an async runtime.

Evidence: real Linux ARM64 UTC workspace tests passed 170 assertions, but the following real HTTP upload replay failed at `scripts/smoke-auth.py:157` with 422. Direct CLI validation in the release-image test runner, with valid `TOKIO_WORKER_THREADS=64`, failed 8 of 100 synthetic text probes. Stderr includes `failed to allocate an alternative stack: Cannot allocate memory` and fatal runtime abort. Logs: `.local-validation/acceptance-2026-11-12/linux-arm64-utc.log` and `linux-probe-before-fix-64.log`.

The smallest repair is a synchronous entry function: dispatch the isolated helper first, then construct Tokio only for actual server/CLI application work. Keep CPU/address-space/pixel/allocation limits unchanged. Add a public CLI regression showing the helper works independently of server-runtime environment configuration; rerun the valid high-worker-count Linux stress, real upload smoke, workspace checks and final artifacts. This issue blocks accepting the pre-fix artifacts; earlier Mac checks did not exercise Linux RLIMIT_AS.

Code-review assessment: no API/schema/dependency change is required; preserve runtime defaults (`new_multi_thread`, `enable_all`) for ordinary execution. Test review: real executable/subprocess/environment tests protect the process startup boundary that direct decoder unit tests bypass. Do not replace the existing resource or media HTTP tests.

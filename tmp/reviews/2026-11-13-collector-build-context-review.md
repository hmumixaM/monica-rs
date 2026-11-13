# Optional collector build context finding — 2026-11-13

Reviewed parent `50ce53ad698018dbac99610c1b0dabef0e5f2040`, actual branch base `301241bf41e8ba8bb18d981f4e9138fe220e9c18`.

## C8 [medium] Include collector source without packaging private state

`infra/collectors.Dockerfile` copies `apps/desktop/collectors`, but the root `.dockerignore` excludes the entire `apps/desktop` tree. A clean optional collector image cannot build from that context. Merely allowing the whole tree would risk copying generated environments and login state from local builds.

The root ignore file now permits only the 77 explicitly declared collector source/metadata files and their parent directories, using the same pinned vendor inventory as native packaging. It continues excluding arbitrary children, virtual environments, private state, `.env`, Git metadata and native runtime assets. This uses ordinary Docker ignore rules, so it also works with the NAS legacy builder rather than relying on BuildKit-only per-Dockerfile ignore files.

A real scratch Docker context export verifies all 77 required files and hashes survive, while seven synthetic private-state fixtures are absent. This is an actual build-boundary test, not a regex/source-text assertion. CI also builds the optional WhatsApp and browser collector images and checks help/offline Chromium, with no publishing, accounts or real collection. That boundary is distinct from provider unit tests and the native Mac bundled interpreter.

Code-review inspected filtered source parents, exact allow entries, Docker COPY callers and Docker-only fixture isolation. Excessive-tests assessment: retain this one context-boundary check; no existing test proves local state exclusion or optional image source availability. Separate image build/offline runtime checks remain necessary. Source CI, complete current packages and benchmark results are recorded in the final acceptance report.

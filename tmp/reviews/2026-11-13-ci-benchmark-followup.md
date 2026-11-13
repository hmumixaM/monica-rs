# CI and benchmark acceptance follow-up — 2026-11-13

Reviewed parent `50ce53ad698018dbac99610c1b0dabef0e5f2040`, actual default merge base `301241bf41e8ba8bb18d981f4e9138fe220e9c18`. New append-only source changes were independently reviewed and merged by the user's other chat; preserve them. The previous CB package and partial benchmark remain historical inputs, not current-source acceptance.

## C5 — PostgreSQL 16 missing on changing Ubuntu default

Actual GitHub run 43382684007 web job failed with `Unable to locate package postgresql-16`. Pin only the Web runner to ubuntu-24.04, whose supported package set provides the required PostgreSQL 16. Keep core container database validation on PostgreSQL 16 and preserve actual browser/API checks.

## C6 — Native source checks require an untracked runtime

The same run's native Clippy step failed because `../runtime` does not exist in a clean checkout. The complete runtime is intentionally generated and untracked. Scope `TAURI_CONFIG` to disable bundle resources for the source-validation job, using an empty array override. Its lifecycle test still runs the actual compiled server, installed PG16 and built Web via explicit paths. This does not count as a full bundle test. Actual complete bundles must separately pass macos-verify.py. Local validation removed the generated runtime directory before running the CI override; Clippy passed without creating a fake runtime.

## C7 — Capacity failures abort the entire performance comparison

The first Mac comparison completed six cases, then the fixed multi-source Python baseline returned HTTP 503 (`Database capacity is busy`) during the 100,000-message mixed phase. The harness raised on that one read and discarded the unfinished case, preventing all remaining rounds. The fixed baseline must not be modified to suppress the measured failure.

Read measurements now count failures without retrying them, preserve status codes and failure latency, and compute p50/p95/p99 from successful reads only. A route with no successes has null quantiles rather than a fabricated fast result. Unexpected whole-case/import failures are recorded with diagnostics and the remaining cases continue; any such failure retains a nonzero command outcome. Real local HTTP tests cover successful, 503 and malformed JSON responses in both page and mixed readers; the previous implementation fails the regression. No arbitrary latency expectation or implementation-string assertion was added.

Code-review traced exception boundaries, bounded response reads/closure, import completion, fixed-baseline provenance and failure reporting. Excessive-tests retained one focused real-HTTP regression for two independently used sampling entry points. Existing correctness tests and performance thresholds are unchanged. Fresh CI and complete-source measurements remain required before final acceptance.

# Bugbot Report

## Report Contract

- Report type: `bugbot`
- Report ID: `bb-20261113-111acc3a`
- Review date: `2026-11-13`
- Report path: `tmp/reviews/2026-11-13-bugbot-report-111acc3a.md`
- Repository root: `/Users/maxhu/play/monica-rs`
- Artifact status: Fixed review input; do not rewrite during repair

## Scope

- Diff mode: uncommitted changes
- Requested base: HEAD
- Resolved baseline: `01718e0eba26db50eb7eda3a7b093c054d802e7c`
- Reviewed target: current working tree and untracked non-ignored test script before this report.
- Changed files: 2 (`.github/workflows/ci.yml`, `scripts/test-collector-supervisor.py`)
- Completion: Complete within this narrow test/CI scope.
- Custom instructions: Complete the linked Rust rewrite acceptance tests, commit all changes, preserve production services/data. Prior actual branch-base production review is recorded in `2026-11-12-committed-rereview.md`.
- Assumptions: No remote PR exists. No production implementation changed in this increment.

## Detection Snapshot

- Recommendation: Pass
- Completion: Complete within reviewed scope
- Why now: The missing NAS collector-supervisor runtime tests now have isolated executable coverage and CI wiring.
- Findings count: high 0 | medium 0 | low 0
- Must-review now: None
- Coverage confidence: high for this test-only increment
- Biggest blind spot: This increment does not exercise real collectors/accounts or establish platform acceptance by itself.

## Complete Findings Index

No introduced production bugs identified in the reviewed scope.

## Findings

None.

## Detection Coverage Ledger

| Area / candidate | Changed entry point | Status | Result | Evidence / next step |
| --- | --- | --- | --- | --- |
| Control admission on pause, missing lane, malformed/unavailable/oversized responses | test-collector-supervisor.py Controls | Reviewed - no bug found | Real local HTTP server and real control client; no external endpoint/account | Four tests pass in 4.142 seconds |
| Bearer redirect protection | Controls.test_redirect_never_receives_control_token | Reviewed - no bug found | Request log proves only initial endpoint receives a request; redirect cannot leak token | Synthetic token only |
| Bounded spool backpressure | FilesAndProcesses.test_backpressure_counts_only_complete_ready_pairs | Reviewed - no bug found | Orphan ready markers do not count; seven/eight complete-page boundary covered | Real temporary files |
| Cancellation and browser descendant cleanup | FilesAndProcesses.test_cancel_stops_stubborn_child_and_browser_descendant | Reviewed - no bug found | Dedicated process group, stubborn synthetic parent/descendant; cleanup always kills only owned group | Observable child completion and descendant absent/zombie; bounded startup and shutdown |
| CI runner and test runtime cost | .github/workflows/ci.yml adapters job | Reviewed - no bug found | Standard-library Python test runs before existing locked adapter environment; POSIX group test skips on unsupported OS | Ubuntu supports process groups; no dependency addition |
| Prior runtime/vendor/platform limitations | existing production branch | Not production-relevant | Outside this test-only delta; existing review limits remain | Not relabelled as newly reviewed production code |

## Evidence Appendix

### Diff Inventory

| Path | Change status | Production surfaces considered |
| --- | --- | --- |
| .github/workflows/ci.yml | modified | Existing adapters CI job adds one standard-library test command |
| scripts/test-collector-supervisor.py | untracked | Test-only local HTTP server/files/owned child processes |

### Verification Commands

- `python3 scripts/test-collector-supervisor.py`: 4 tests passed, zero failures, no real collector executed.
- `git diff --check`: passed.
- Inspected the actual current diff, control client/redirect behavior, queue limit and process cleanup implementation.
- Code-review: no new dependency/API/schema change; test resource cleanup and failure paths inspected.
- Excessive-tests: four distinct contracts protect pause/cancellation/token/backpressure boundaries; existing collector publication tests do not cover these controls. No redundant test removal warranted. Polls use bounded observable state, not readiness sleeps; direct child gets reaped, no source-text assertions.

### Dismissed Or Merged Candidates

| Candidate | Decision | Reason |
| --- | --- | --- |
| Real collector/network activity in test | dismissed | Only local HTTP and synthetic sleeping Python processes are created |
| Orphan descendant after a failed assertion | dismissed | finally kills owned process group and waits for child |
| Implementation-coupled token safety assertion | dismissed | Assertion observes real HTTP request destination; no private call or source-string matching |

### Blind Spots

No additional blind spots within this narrow delta. Full branch/provider/NAS/macOS acceptance limits are recorded separately and are not changed by this result.

## Report Self-Check

- yes: zero findings in index and findings.
- yes: all meaningful candidates are in ledger.
- yes: no Finding rows require cards.
- yes: no uncovered production candidate exists in this narrow delta.
- yes: severity counts match.
- yes: Pass follows mechanical recommendation mapping.

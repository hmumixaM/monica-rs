# Artifact smoke entry-point finding — 2026-11-13

Reviewed source: `92ae99e4d9c291bd380946571d70cdd48ce9e541`, actual branch base `301241bf41e8ba8bb18d981f4e9138fe220e9c18`.

## C4 [medium] Execute the artifact workflow's auth smoke

`.github/workflows/artifacts.yml` invokes `python3 scripts/smoke-auth.py http://127.0.0.1:17494`, but the script only defines `run(base)` and has no CLI entry point. That command exits successfully without sending any request, so the manual artifact workflow can report acceptance without testing authentication or media. Confirmation before repair: invoking the same CLI with the unreachable URL `http://127.0.0.1:1` returned exit code zero.

Repair: add a standard `__main__` entry point with a required base URL that calls the existing `run` function. Preserve the runpy/import callers used by the already executed workspace HTTP smoke. Verify that an unreachable server now fails, that imported function callers still pass, and that the CLI runs actual authentication/media checks against the disposable test server. Do not add parser-mirroring unit tests; these observable CLI checks directly protect the workflow contract.

This is a test-entry repair, with no application API, image runtime, dependency or database schema change. Local artifact workflows are not dispatched automatically by pushing the feature branch; the publishing workflow remains manual-only.

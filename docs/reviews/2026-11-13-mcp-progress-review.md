# MCP, recovery and progress review

Actual PR #2 base58e2719, reviewed head `d82c28650103dc0cc1f3a2655d1e4ebe97e9ae37`. Application changes are in df55292; d82c286 adds packaging fixes and stronger packaged recovery tests. No NAS access, personal-data migration or Instagram login.

Local bugbot report `bb-20261113-726c7091`: **Pass**, complete within changed first-party scope, zero new findings. Initial immutable report bb-20261113-d18eb7b2 recorded two medium issues; signed-page double counting and premature query-ready state are repaired and covered by real signed-connection and delivery assertions.

## Code-review

Reviewed authorization, actor propagation, control-origin separation, transactions/lock scope, old/new migration compatibility, source/data preservation, tool allowlisting, cursor/filter handling, UI form behavior and active polling. Existing indexes, aggregate bounded staleness, productive ingestion and incremental Memory semantics remain. No unresolved confirmed merge-safety issue in the reviewed code.

A packaged smoke exposed stale bytecode rewriting signed resources after collector source refresh. Packaging now excludes caches; wrappers and smoke children disable bytecode writes. Verification repeats strict deep codesign after the actual tests, and the final d82c286 bundle passes both seals. This repair is necessary for a repeatably valid delivered app.

## Excessive-tests

Added coverage is proportionate. MCP SQLx tests exercise permissions/shared writes and key lifecycle, recovery tests exercise persisted identity/sessions/TOTP, and monitor tests exercise exactly-once deltas and phase/heartbeat transitions. Official SDK checks transport compatibility; browser/native UI checks interaction and layout; packaged native checks exercise actual private recovery/lifecycle. These cover distinct failure boundaries. Existing regression coverage was retained; no implementation/source-text-only tests were added or recommended for removal.

## Verification

- Workspace186 passed,0 failed,3 optional ignored. Native14 unit,5 UI and1 actual packaged lifecycle/recovery passed. Denied-warning Clippy passed for root/native.
- Actual Rust-backed populated browser/MCP2 passed; router/agent5 passed. Light/dark desktop/mobile screenshots reviewed locally.
- Official SDK1.32.2 direct/proxy fixture and packaged server passed. Frozen source HTTP27, label48+6 and transport45 checks passed.
- Paired local three-round benchmark: main/final10k full settlement2.406/2.141s;100k20.581/20.237s. Both include deterministic Memory completion, all six cases/side succeed, mixed read failures0. No paid model inference. Baseline packaged41ac3a9 is source-identical to main58e2719 for crates/apps/migrations. Host/OS buffers are not cleared; this is not NAS timing.
- Fixed10k-contact fixture: all eight read paths succeed. Exact latencies and limitations are in evidence/2026-11-13-mcp-progress/summary.json. Small sub-ms shifts are retained rather than hidden or claimed as guaranteed speedups.

## Boundaries

No personal providers, permission grants, public HTTPS endpoint, paid models, real face inference or notarization were executed. Existing Rust data is retained; old Python/NAS data and credentials are not imported. Local app installation and final artifact identity are recorded in the delivery report. CI status is reported separately from this source review.

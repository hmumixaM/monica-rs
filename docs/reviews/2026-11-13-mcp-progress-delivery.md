# Local macOS delivery: MCP, recovery and operational progress

Implemented from main58e2719; product source df55292 and packaging/recovery-test follow-up d82c28650103dc0cc1f3a2655d1e4ebe97e9ae37. PR #2 contains the changes; no merge or NAS operation was performed.

The clean d82c286 package is installed at `/Users/maxhu/Applications/Monica Rust.app`.
The new launcher is `/Users/maxhu/Documents/ChatGPT/monica/打开 Monica Rust.command`.
Actual native UI observation confirmed running state, local password recovery,
Contacts/Messages disabled, Instagram/LinkedIn not logged in, WeChat unconfigured,
and zero active jobs. The app is left running for the user. Original `Monica.app`
and its candidate launcher were moved to recoverable system Trash only after the
new installation passed readiness and strict deep signature checks. No legacy
business data or account credentials were imported or deleted. Existing Rust
schema is upgraded normally by the new app.

## Delivered behavior

Settings contains MCP enablement, per-client generated/custom keys, rotation,
revocation, public HTTPS address and copied client configuration.19 tools query
profiles, relationships, Memory, messages, versioned evidence, timeline, coverage
and operational progress; normal profile/notes/tag/link edits reuse business
handlers. The private control plane and credential management are not tools.

Native settings provides forgotten-password recovery with default-off TOTP reset.
It retains the user UUID/business rows/source configuration and revokes browser
sessions. Web password changes require the current password.

Sources, evidence, agent and new settings controls share existing UI primitives
and design tokens. Sources gives a concise operational summary; Imports shows
source/account stages, daily counters, paginated runs and safe diagnostics. Signed
connection receipts count once and remain saved until source projection finishes.
Media, Memory inputs and model jobs remain independent stages. Events expire
through bounded idle maintenance after90days; compact daily counters persist.

## Validation and limits

-186 workspace tests passed,0failed,3 optional ignored; root/native Clippy with warnings denied passed.
-14 native unit tests and5 native UI tests passed. The actual packaged native PostgreSQL/server lifecycle test also exercises private password recovery, rejected old sessions and stable user UUID.
-2 populated/MCP Rust-backed browser tests and5 router/agent checks passed. Desktop/mobile, light/dark screenshots were reviewed locally.
-Official MCP SDK1.32.2 passed direct and forwarded-public-Host proxy calls, both against release and packaged backend. No public TLS endpoint was created.
-Frozen differentials:27 source HTTP observations,48 label journal/projection plus6 HTTP observations,45 source/phone transport vectors. Offline supervisor4, bridge8, Chatlog11 and locked packaged Chromium/source status checks passed.
-Bugbot final726c7091, code-review and excessive-tests reviews found no unresolved confirmed issue in the actual changed first-party scope. Initial findings and their repair evidence are retained separately.

Paired three-round local benchmark (100contacts, signed1000-record batches):

| Messages | main query-ready | new query-ready | main full settlement | new full settlement |
| --- | --- | --- | --- | --- |
|10,000|1.996s|1.771s|2.406s|2.141s|
|100,000|12.742s|12.052s|20.581s|20.237s|

All12paired cases complete, mixed read failures0, final Memory queues0. Full
settlement includes deterministic Memory inputs; external inference is disabled.
The baseline binary from the prior clean Mac package is application-source-identical
to main58e2719; actual binary SHA/selected path and harness provenance are retained.
The earlier provisional100k35s median coincided with compilation and is preserved,
not substituted for this same-session comparison. No claim about NAS timing or
universal10x improvement is made.

On the fixed10k-contact read fixture, all8routes return successfully. New medians:
home2.83ms, profile1.28ms, overview2.44ms, global timeline5.01ms, full directory613ms.
Main comparison and all successful/failure samples remain in the machine summary;
small sub-ms shifts and host/cache limitations are explicit.

An initial repeated package verification caught stale Python bytecode being
rewritten inside the signed resources after collector source refresh. Final
packaging excludes bytecode caches, wrappers/children disable bytecode writes, and
verification checks the signature both before and after actual execution. The
final1312088362-byte clean bundle passes both seals. It is ad-hoc signed for local
use; notarization/public distribution is outside this delivery.

Live providers, new permission grants, real face inference and paid model calls
were not performed. This Rust directory has no reusable social authorization;
Instagram remains off. Old data directories and the Python/NAS service remain
untouched. Full data/feature comparison is in `../../FEATURE_PARITY_2026_11_13.md`;
MCP operation is described in `../../MCP.md`.

Evidence: `evidence/2026-11-13-mcp-progress/summary.json`, runtime manifest,
installation receipt, compressed logs/benchmarks and SHA256 inventory. CI is
tracked on PR #2 and is separate from the local skill review and package checks.

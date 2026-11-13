# General read latency and import optimization

User authorized Rust read/import fixes, skill-based review and merge to main. No NAS access, test or deployment was performed. The live Python NAS service and Python source are untouched. Required response data and pagination remain intact.

## Source and method

PR1 base93b07a8b93603ad26279da0b9bba9c645258bba7. Final application source5feb9f932f3533b684544136ad22566ebc3371a5; benchmark/Mac source41ac3a963cb401fe90f810e330f4e61ff6d9ee69 adds only a review artifact. The measured release hash and signed bundle's pre-sign build hash match in the [runtime manifest](evidence/2026-11-13-read-import/runtime-manifest.json). Subsequent evidence/document commits do not change application code.

All new measurements use isolated local socket-only PostgreSQL16.15, generated data, enabled durability and no model calls. Owned databases/processes were cleaned up. Unrelated host services were preserved. This measures SQL/schema/scheduler improvements as well as Rust; it is not a language-only benchmark or a claim about actual NAS timing.

## Uncached HTTP reads

Fixture:10,000 contacts,100,000 identities,20,000 posts,50,000 media links and20,000 conversations. Unique ignored diagnostic query parameters bypass the existing response cache; PostgreSQL/OS buffers are not evicted. Each route uses64requests except full directory3. Before/after is on the same fixture. Baseline binarybb69b39 has identical application source to base93b07a8. Read probe source37b07dc is byte-identical to final5feb9f9 throughout monica-server; later changes are internal Memory projection/maintenance work, with equivalent semantics covered by final tests.

| Route | Before median | After median | Ratio | Success/failure before → after |
|---|---:|---:|---:|---|
| home | 76.359ms | 2.418ms | 31.58x | 64/0 → 64/0 |
| insights | 146.605ms | 61.551ms | 2.38x | 64/0 → 64/0 |
| profile | 10.152ms | 1.081ms | 9.39x | 64/0 → 64/0 |
| profile-summary | 9.965ms | 1.793ms | 5.56x | 64/0 → 64/0 |
| overview | 42.74ms | 1.840ms | 23.23x | 64/0 → 64/0 |
| timeline-profile | 4.824ms | 0.796ms | 6.06x | 64/0 → 64/0 |
| timeline-global | 95.766ms | 4.018ms | 23.83x | 11/3 → 64/0 |
| directory | 3/3 timed out at10s | 607.148ms | all3 succeeded | 0/3 → 3/0 |

The7 common successful response bodies have equal normalized JSON SHA256. The baseline directory had no successful body to compare; the final actual endpoint and existing directory tests verify its contract. Successful latency excludes failures; failures are retained separately. Global timeline repeatedly hit its10-second database statement deadline under the old generic prepared plan; final64/64requests succeeded. Directory still returns all required10kcontacts and costs about607ms including serialization/transfer/client decode; no new pagination or hidden field dropping was introduced.

SQL diagnostic medians: validator92.706→0.266ms, identity lookup11.153→0.129ms, entity-media lookup2.580→0.039ms, global conversation ordering3.273→0.036ms. Final plans explicitly force generic planning. Raw plans and exact statements are in the [evidence directory](evidence/2026-11-13-read-import/README.md).

## Import and deterministic background completion

Same established signed-HTTP workload:100 linked contacts,10k/100kmessages, batches1000,3rounds,64warm requests/route and8mixed readers. Final setup includes contact linking and its attribution recovery in setup_seconds. Message-import timing begins after setup is fully ready. No pending queue is deleted or marked complete artificially. Separate diagnostics with a linking backlog are preserved; they are not final measurements.

Acceptance is durable receipt only. Query-ready waits typed facts/search plus jobs/conversation work; full settlement additionally waits pending Memory records/scopes0. Model inference is disabled, so full settlement means deterministic inputs/projections are ready, not external LLM summaries. Every final case verifies100linked conversations and exact message counts. All6cases pass, all final Memory queues are0, warm/mixed read failures are0.

| Messages | Durable acceptance | Query-ready | All required work | Previous Rust reported settled | Historical Python Mac reported settled | Python/new ratio |
|---|---:|---:|---:|---:|---:|---:|
| 10,000 | 0.280s | 1.757s | 2.171s | 18.440s | 20.360s | 9.38x |
| 100,000 | 2.732s | 12.407s | 21.310s | 142.518s | 208.399s | 9.78x |

These are three-round medians. Final full-settlement samples:10k[2.171,2.118,2.186]s;100k[21.310,21.011,21.659]s. Against previous Rust reported settlement, full completion improves8.49x/6.69x. Against the historical Python Mac desktop measurements, it is9.38x/9.78x: close to10x, not a strict universal10x guarantee. Historical baselines were reused unchanged with original source pins/metadata; they were not rerun or relabeled. Their prior reported-settlement metric did not explicitly wait attribution scopes. Current measurements include all such work and charge initial linking recovery to setup. Differences include projection/layout algorithms, scheduler and readiness accounting; use the raw records to assess exact scope.

Final cumulative SQL execution drops to about1.71s/19.29s for10k/100k. Peak server RSS median is68.1/77.4MiB. Raw SQL/WAL/pool/resource/mixed-route data remain in the compressed benchmark records. These are local measurements under coexisting host services, not dedicated hardware or network/NAS guarantees.

## Changes

- Migration0049 adds eight indexes matching profile/identity/media/group/order predicates, including DESC NULLS LAST ordering. Existing data/fields/cursors remain.
- Global cache validation uses at most32committed epoch shards/domain rather than hashing every subject token. Person routes retain scoped subjects. Epochs and subject invalidations commit/roll back with facts; one shard/domain/transaction avoids a single global writer row.
- Timeline applies all ownership/date/provider/cursor/media filters and LIMIT before hydrating attachment arrays. Manual participants and NULL cursors remain covered.
- Only home, insights, people-overview and summary aggregates may reuse compatible stale counts for at most30seconds. Refresh is request-driven, bounded to2tasks and15seconds with background pool/budgets; minimum refresh cadence5seconds. Profile/identity/archive/media hard invalidation remains immediate. Person/timeline/message facts do not opt into aggregate staleness.
- Productive import chunks release admission/resources and immediately continue. Idle/error polling backs off; the fixed1second-per-chunk limit is removed. Media recovery/periodic metrics/expiry/orphan work run in an independent bounded maintenance lane.
- Normal scheduler imports commit Memory references with facts and defer materialization. Old/current owners fence stale publication. Maintenance drains with AI disabled, after ready imports; future retry batches do not starve it. Direct public core append/resume calls retain their immediate Memory contract.
- Canonical Memory object payloads and eligible profiles are shared views. Bounded batch selection precedes eligibility joins, removing planner-sensitive repeated owner/batch scans. Generic10k-profile/100k-message tests verify full1000-object payload-set equivalence. This fixture's per-object7.717ms/old-bulk5.966ms/selected10.878ms are a plan/semantics guard, not a universal microquery speed claim.

Search, conversation and month fact projections still commit atomically with typed messages. This change does not claim that every SQL projection moved out of the ingestion transaction. The principal deferred work is Memory materialization and housekeeping; aggregate responses refresh separately.

## Review and validation

Local bugbot, code-review and excessive-tests skills were applied to the actual PR base/latest application head and affected repairs. Earlier immutable reports preserve runtime findings and their fixes. Final Bugbot report40dd8b58 recommends Pass,0findings; no unresolved confirmed finding. Focused five-test additions cover actual persistence/auth/privacy/lifecycle/filter contracts; no framework or suite redesign.

Final batch implementation:182workspace tests pass,3optional tests remain explicitly ignored; Clippy with warnings denied and release build pass. The preceding implementation also passed UTC and America/Chicago. Actual old-pacer negative control fails the backlog bound; restored/final implementation passes. Benchmark parser regression passes. Optional video/scaling historical acceptance remains separately labeled rather than claimed as a new rerun.

The complete Mac app was rebuilt from clean41ac3a9, locally ad-hoc signed, and verified for native-library relocation, ffprobe, locked collectors/Chatlog, fresh bundled PG16, static Web/auth, suspend/resume, pause, crash recovery, ports and clean exit. App path:apps/native-desktop/src-tauri/target/release/bundle/macos/Monica Rust.app. It is a local test artifact, not notarized. Application source5feb9f9 passed all6 [CI jobs](https://github.com/hmumixaM/monica-rs/actions/runs/43469081701):Linux Rust/auth, Web, native, adapters and optional AMD64/ARM64 containers. Latest evidence-head CI and merge commit are recorded in the completion handoff; no NAS image deployment occurs.

[Machine summary](evidence/2026-11-13-read-import/summary.json), [evidence inventory and caveats](evidence/2026-11-13-read-import/README.md), [SHA256 manifest](evidence/2026-11-13-read-import/SHA256.json).

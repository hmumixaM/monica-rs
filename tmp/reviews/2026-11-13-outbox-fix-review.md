# Outbox finding repair and affected-change review

Source report: bb-20261113-a8f8460d, B1 (immutable). Existing user request authorizes fixing import performance. The source at PR1 head37b07dc is still the reported query, with no checkout drift.

Repair: correlate ownership lookup to one unique object ID with a scalar ARRAY subquery. PostgreSQL can use object equality indexes instead of flattening the1000-ID owner view across every profile. Reuse the authoritative owner view, including current attribution, archived/self filtering and group rules; retain previous-owner union and pending-owner upsert union. No copied attribution rules or planner-wide session switches.

Verification: scheduler committed-ingest-backlog regression passed in0.59s, including facts searchable, durable Memory recovery and delayed retry. The initial runtime regression was diagnosed from an actual4274ms read-only ownership plan with10000 message bitmap loops. Interrupted initial benchmark artifacts are retained as failed-attempt evidence and excluded from final timings. Fresh release-scale results follow in the acceptance report.

Applied code-review and excessive-tests to the affected query: correlation changes access strategy, not owner-set semantics, empty-owner queue references or transactional fences. Existing regression remains appropriate; no new timing-only unit test or fixture framework. No unrelated source, dependencies or NAS changes.

During report-scoped local repair no Git or external state was mutated. Subsequent commits, PR updates and main merge are separately authorized by the user. Final PR-head re-review records the fixed source commit.

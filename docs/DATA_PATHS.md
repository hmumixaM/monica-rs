# Backend performance rules

This supplements the accepted implementation plan. Full backend parity is still in progress.

## Implemented paths

- Immutable messages use a dedicated typed source table. Stable account/conversation/message keys deduplicate facts. Conflicting identities fail the entire batch; append never silently overwrites a saved message. Dedicated correction/deletion revisions are still required.
- Each append accepts at most 1,000 records and 4 MiB encoded input, prepares data before the transaction, performs set-based insertion, and groups only newly inserted rows into conversation deltas. There is no per-message revision/current-object lock/job.
- The streaming JSONL CLI bounds each input line and resident batch. It starts at 500 records, reduces the next batch after >200 ms and grows gradually below 50 ms, retaining hard caps. Prior committed batches can be safely replayed after failure; the CLI does not yet maintain a durable file checkpoint.
- Conversation cumulative state and active work are separate from source bodies. Counts and last-message previews update from inserted facts without scanning history. Old historical inserts do not replace the newest preview.
- One work row per conversation coalesces batches. The worker snapshots a committed generation; finish atomically publishes that summary and only clears work through that generation. New arrivals during execution survive as a further run. Lease takeover rejects stale results.
- Mutable objects still retain ordered revisions. Claim checks the object's committed projection version instead of searching earlier job history. Completed jobs move into a separate history table in the result transaction.
- The global projection sequence is removed. Conversation cache versions come from committed summary generations; mutable-object versions are local to that object. No cache consumer may treat an allocated sequence or maximum event ID as proof that earlier transactions have committed.

## Required next stages

- Extend immutable paths to calls and interaction events; implement explicit correction/tombstone revisions with reversible aggregate deltas.
- Finish account/conversation mappings, profile-linked monthly aggregates, media content/reference separation and keyset page APIs. Generic payload projections remain experimental and must not become the page read model.
- Fixed execution partitions, bounded worker queues, separate CPU/I/O/memory budgets and interaction-latency feedback; Mac SSD/HDD NAS budgets share logic.
- COPY staging for large historical imports, measured against the current bounded set-based path. Preserve constraints, WAL durability and short transactions.
- Scoped cache invalidation and per-key singleflight. Manual edits and linking must be immediately readable; derived summaries may lag with visible progress.
- Audit indices against actual queries, benchmark HOT/WAL behavior, and decide physical partitioning from demonstrated pruning. No speculative all-field indexes or monthly partitioning default.

## Measurements

Run `MONICA_BENCH=1 sh scripts/test.sh`. Three rounds compare 10,000 and 100,000 messages with a fixed 100 conversations. The test records pg_stat_statements calls per 1,000 messages, WAL bytes per 1,000 messages and compact-summary query p95/p99. Stored evidence is in `docs/benchmarks/2026-11-11-message-batches.json`.

This is a warm SQL-level microbenchmark on isolated local PostgreSQL, using a debug Rust build. It is not a browser/API result, NAS result, Python comparison or proof of the final performance target. SQL counts are PostgreSQL observed statement calls, not protocol roundtrips; WAL probes are excluded from call counts and WAL bytes are cluster-wide. Broader page/10x-scale and mixed interactive-load measurements remain required.

References: [PostgreSQL bulk loading](https://www.postgresql.org/docs/16/populate.html), [HOT](https://www.postgresql.org/docs/16/storage-hot.html), [partitioning](https://www.postgresql.org/docs/16/ddl-partitioning.html).

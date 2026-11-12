# Optional WAL history and pipeline status

The default is disabled. Source import ACKs and projection recovery do not depend on CDC. All business facts remain durable without it.

To enable in a dedicated installation, configure PostgreSQL 16 `wal_level=logical` and a replication slot budget, restart PostgreSQL, run the same binary's `cdc-initialize` command after `migrate`, then set `MONICA_CDC_ENABLED=true` when serving. Initialization is an explicit maintenance operation because it changes replica identities and the publication. The role must own published tables and have logical replication permissions.

One database-scoped `pgoutput` slot covers the seven source schemas and CRM mutable objects. Append-only message bodies and revision archives are excluded. `REPLICA IDENTITY FULL` supplies before images. Each bounded capture persists history and its consumer checkpoint before advancing the slot; the `(provider, commit_lsn, ordinal)` constraint makes a crash before advancement safe to replay. A missing established slot or lost WAL fails closed and requires explicit recovery; startup never silently recreates it. A malformed or oversized transaction remains unacknowledged. Pausing stops capture too, so operators can inspect retained WAL in status.

Authenticated routes preserve the ordinary app contracts:

- `GET /api/v1/cdc/events`: descending IDs, provider filter, `before` cursor, limit 1–200.
- `GET /api/v1/cdc/events/{event}`: before/after and sorted business field differences; refresh-only timestamps are excluded.
- `GET /api/v1/cdc/status`: consumer and pipeline state. CDC routes return 404 while disabled.
- `GET /api/v1/pipeline/status`: independent of CDC, reads actual durable queue, media, AI and adapter states.
- `POST /api/v1/pipeline/tasks/{event}/retry`: addresses actual Rust projection task IDs, clears quarantine and fences the previous lease token. Audit event IDs are unrelated.

Database topology differs deliberately: the consumer is `database`, rather than four independent database consumers. Event providers retain their source schema names. A single bounded capture accepts at most 32 MiB of protocol data. Large maintenance transactions that exceed this bound stop audit progress rather than silently dropping history; monitor retained WAL and perform controlled recovery before exhausting disk.

Evidence: core PostgreSQL tests cover persisted-before-ACK crash replay, before/after images, append-message exclusion and missing-slot refusal. HTTP tests cover enabled/disabled behavior, paging, field differences, actual queue counts, quarantine retry and token fencing. Decoder tests reject truncated, unknown and incomplete transactions.

Protocol references: [PostgreSQL 16 message formats](https://www.postgresql.org/docs/16/protocol-logicalrep-message-formats.html) and [replication management functions](https://www.postgresql.org/docs/16/functions-admin.html#FUNCTIONS-REPLICATION).

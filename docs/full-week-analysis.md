# Complete weekly evidence and bounded inference

Person weeklies and digests capture the complete eligible week in PostgreSQL.
The former 80/120/40/160 row limits no longer truncate weekly input. Providers
receive bounded Unicode fragments, cached batch reductions, then a person or
writer result using the configured roles. Each lane turn makes at most one
provider request. Progress is durable across lease recovery and process restart.
Changing only the final person/writer model reuses compatible batch reductions.

The window includes nondeleted posts and retained Stories, opted-in messages
and notes, and genuine manual/native-connection profile updates. Group messages
belong to their identified speaker. Privacy and source revisions are checked
again before publication. Changed input cancels the obsolete result and records
a durable refresh request. Missing observation context waits for deterministic
background work rather than consuming inference. Existing published reports are
still dated results: later imports do not automatically regenerate every closed
historical week. An explicit weekly dispatch can regenerate a requested period.

Discovery dates come only from retained source revision observation timestamps.
Unknown dates remain unknown; migration time and last-seen time are not discovery
dates. Newly discovered old content preserves its original date. Profile updates
and old-content discovery cannot become new life events. Citations resolve against
the captured window; published excerpts are short, while originals stay intact.
Saved OCR and confirmed face context are included without running OCR again.

`GET /api/v1/imports/status` includes observation-context and weekly-refresh
backlogs plus up to 20 active weekly windows. For each window `sources` is the
captured denominator (null before capture), and `processed` counts sources whose
batch reduction is complete. `writing` means final writing remains; it does not
mean the final report is complete. `analysis_execution_enabled` distinguishes
waiting while disabled. Completed results carry explicit coverage. Dispatch
completion only means subjects were arranged, never that all analysis finished.

Detailed captured inputs, buckets and cached model parts are pruned in bounded
batches after 90 days once their job is terminal. Original evidence and published
reports are retained. In-progress work is retained. Only mutable queue/run records
use short explicit locks; append-only source evidence and revisions use MVCC.

Validation uses an isolated SQLx database and fake providers: a 377-source week,
late tail text and saved OCR, private-data opt-out, role/cache reuse, genuine and
unknown observation dates, source changes, lease recovery and retention. No real
model call was made. On the NAS, AI, OCR and the new group-photo backfill remain
off until all imports finish; this source change does not enable them.

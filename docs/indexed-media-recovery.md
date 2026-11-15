# Recover failed indexed WeChat media

`scripts/retry-wechat-media.py` repairs only `decode_failed` image records in
the independent Rust migration's `wechat-file-assets.sqlite`. It does not
restart the original copier, reread Next, change encryption algorithms, run
group-image discovery, invoke a model, or register media in PostgreSQL.
Successful assets and `source_missing` records are retained unchanged. Missing
source files still require the separate filename/fallback coverage stages.

Before either `inspect` or `retry`, the client acquires the existing copy
control-file lock and checks the original container's actual identity and
terminal state. A matching live PID, a running/OOM worker, an incomplete indexed
copy, a changed frozen index, or a mismatched snapshot refuses recovery. A
finished checkpoint or status JSON alone is not evidence of worker termination.
The caller supplies the observed original PID/start ticks, immutable container
ID, and baseline manifest hash. An inspection timeout is a failure to observe;
it does not authorize restarting anything.

The required paths are explicit: `--raw`, `--media`, `--state`, `--index`,
`--copy-lock`, `--codec`, and `--docker`. Roots must be canonical and disjoint;
the media root must already contain the migration marker. State and index must
have the original snapshot/account identity. Pin `--index-sha256`,
`--codec-sha256`, `--codec-commit`, `--image`, and `--expected-records` from the
reviewed migration artifacts. The codec must be a separately built executable
readable/executable by the selected container UID/GID (10001 by default).

`inspect` reports bounded candidate counts without reading a key or running a
codec. `retry` reads one private credential JSON frame from stdin. Do not put
credentials in arguments, Docker environment configuration, or a file. The
client sends this frame directly to the helper's `--credentials-stdin` batch
protocol; the helper removes credential variables before any FFmpeg subprocess.

Each batch defaults to 16 failed records (maximum 128). Before conversion,
every selected raw file is checked against both the frozen index and its
independent size/SHA-256 copy receipt. The codec runs in one new labelled
container, using the pinned runtime image with FFmpeg. It has no network, a
read-only root and raw archive, a writable marked CAS, bounded memory/CPU/tmpfs,
and Docker logging disabled. The current Rust application image can supply this
runtime; no application/database/proxy container is replaced.

Every result must match a requested ID and the expected codec build. Failure
diagnostics accept only fixed codes; arbitrary stdout/stderr, upstream errors,
paths, and decrypted data are never saved as diagnostics. Success additionally
requires a canonical CAS object with the claimed length/hash. The client makes
the object readable by the application, flushes file/directory metadata, then
atomically publishes the recovered asset and an append-only retry record in
the same SQLite transaction. The WAL is checkpointed before a successful
summary, because the later link importer opens a completed standalone snapshot.

Attempts are grouped by the immutable codec/runtime fingerprint. A previously
recorded failed attempt is skipped on ordinary resume. Use `--repeat` only after
an explicit diagnosis or changed circumstances; successful rows are still
skipped. A crash after CAS creation but before publication is safe to repeat.
Completed batches do not require another scan/copy of all successful media.

The inherited codec can leave a temporary hardlink when interrupted immediately
after CAS linking. Recovery handles only `.migration-media-<digits>` aliases in
the marked media root, after original and previous retry workers are stopped.
It verifies the full content hash and canonical CAS inode, accounts for **every**
link, flushes the canonical name, then removes only redundant temporary names.
The bytes remain in CAS and the raw archive is unchanged. Any unaccounted link
still refuses recovery; it is never treated as independent data. Even a retry
that selects no new rows must finish the WAL checkpoint/fsync/status publication
left by a previously committed batch. A busy checkpoint remains a failure.

The private `codec-retry-container.json` records a creation intent before Docker
is called, then the exact owned container ID. After an interrupted attach,
recovery validates its labels, image, mounts, and network before stopping only
that container. A create timeout without a resolved ID requires operator
inspection of the recorded name; the client does not guess or create duplicates.
Stopped containers are retained as evidence. No service or unrelated container
is stopped or removed.

The client writes `codec_retry_attempts` and `codec-retry-status.json`, retaining
original source keys and raw receipts. It does **not** rewrite the copier's
status/history or claim complete migration. After failed assets are resolved,
the NAS continuation handoff must preserve the original failure and recovery
evidence before advancing to media linking and inventory. Real recovery runs
only after the active frozen copier has exited; do not hot-patch it.

Local checks: `python3 scripts/test-retry-wechat-media.py` and, in
`tools/migration-media`, `go test -mod=readonly ./...`. Both the default Go build
and `CGO_ENABLED=0` image-only build are supported. CGO-disabled voice support
remains unavailable; this recovery client selects only failed images.

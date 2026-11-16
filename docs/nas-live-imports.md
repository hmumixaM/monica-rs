# Existing NAS collector ingestion

`scripts/nas-live-imports.py` receives the existing LinkedIn, Instagram and
Chatlog archives. It never starts a collector, signs in, decrypts files or calls
an AI model. Next can remain stopped. The standalone worker is independent of
the Mac and uses the isolated Rust network and SSD only for its writable state.

The worker is intentionally bound to the current isolated NAS database identity
and schema 86. It must not target Monica Next or another restored database.
Run the reviewed source from a read-only `/work` mount, with these mounts:

- Existing Next `data` → `/source`, **read-only**.
- Existing Chatlog `work/db_storage` → `/chatlog`, **read-only**.
- Rust SSD `data/media` → `/data/media`, writable.
- New Rust SSD `data/live-imports` → `/data/live-imports`, writable, private.
- Rust SSD `data/.monica-rs-ssd-target` → `/data/.monica-rs-ssd-target`, read-only.
- The verified running Rust PostgreSQL Unix socket directory → `/pg`, read-only.

Do not mount Next's database, browser sessions or WeChat decryption keys. Pass
`MONICA_DATABASE_URL` for `monica_rust` on `/pg`, the Rust installation's
`MONICA_IMPORT_HMAC_SECRET`, and dedicated per-source
`MONICA_IMPORT_TOKEN_LINKEDIN` / `MONICA_IMPORT_TOKEN_INSTAGRAM` credentials.
Only their SHA-256 digests belong in `crm.import_service_tokens`. Keep the
worker's credential file private. Never put the NAS login password in a file.

Example command inside the isolated worker:

```sh
python3 -u scripts/nas-live-imports.py --run \
  --source /source --chatlog /chatlog --wechat-self VERIFIED_EXISTING_SELF_ID \
  --media /data/media --state /data/live-imports \
  --since 2026-11-13T00:00:00Z --interval 300
```

The cutoff is an explicit conservative backfill boundary, not a fabricated
historical collection statistic. The first social pass includes records dated
or captured since that boundary; subsequent passes deliver changed records.
LinkedIn/Instagram records use the existing signed batch API and normal Rust
projection, preserving migrated public IDs. Media are SHA-256 verified and
published into the Rust CAS before the batch is sent. An interrupted request
has no successful local receipt and retries the same idempotent batch. Original
JSON bytes are archived separately in the Rust state directory.

Chatlog has a different compatibility boundary. SQLite's backup API copies the
active plaintext databases including committed WAL, checks integrity, and
atomically publishes standalone Rust-owned exports. The existing reconciler
adopts old conversations and message evidence instead of generating a second
set of native connection IDs. It scans the first exports fully, then retains
Reader state across changed exports. Existing bodies, corrections, revocations
and explicit person ownership use the same legacy rules as the original
migration. Current message statistics and conversation heads are updated before
ACK. Receipt counters enter the ordinary import center atomically with each
page, using the date of this import. They are not backdated as old collection
telemetry. This ongoing business-page stream is not a claim that all historical
SQLite tables were re-archived.

`status.json`, `consumer.sqlite`, `raw-json/`, `chatlog-export/` and
`chatlog-reader/` are persistent worker state. Keep them with Rust backups.
The status file retains each provider's last state, including retry errors;
source inputs and unacknowledged records remain for the next pass. A provider
error does not stop checks for other providers. `--once` exits nonzero if any
provider failed. SIGTERM waits for a safe operation/page boundary. Do not use a
short forced shutdown timeout during a SQLite backup or database commit.

Use one worker with a small CPU/memory limit. Do not run it during the release's
database backup or a conflicting legacy reconciliation. It takes the existing
legacy control lock for Chatlog writes; immutable source facts receive no new
explicit row locks. Model/OCR/QA execution and new network media backfills stay
disabled independently of deterministic import processing.

Local focused validation, against disposable PostgreSQL only:

```sh
MONICA_LIVE_TEST_ADMIN=DISPOSABLE_ADMIN_URL python3 scripts/test-nas-live-imports.py
```

The tests cover a lost HTTP response/retry and shared CAS reuse, plus a live WAL
backup, stable message identity, correction, revocation, replay and import
counter de-duplication. The production target guard is not relaxed for testing.

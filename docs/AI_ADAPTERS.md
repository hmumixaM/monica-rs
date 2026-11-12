# AI and managed collector boundaries

All opt-ins are disabled on a new installation. The normal scheduler never
uses the fake model provider to manufacture user-facing results. Tests inject
fake providers and generated source fixtures explicitly. No live accounts,
production databases, collectors or paid model calls are needed for the tests
below.

## AI configuration and execution

The persisted singleton `crm.ai_settings` controls enabled/provider/model and
privacy. Startup applies only environment variables that are explicitly set;
absent values preserve saved settings. The supported variables are:

| Variable | Purpose |
| --- | --- |
| `MONICA_AI_ENABLED` | Enable scheduled background AI |
| `MONICA_AI_PROVIDER` | `openai` for configured execution; `fake` is test configuration |
| `MONICA_AI_MODEL` | OpenAI-compatible model identifier |
| `MONICA_AI_BASE_URL` | API base, including `/v1` when required |
| `MONICA_AI_API_KEY` | Runtime-only transport key; never stored in job JSON or database settings |
| `MONICA_AI_INCLUDE_PRIVATE_NOTES` | Consent to include private notes |
| `MONICA_AI_INCLUDE_MESSAGES` | Consent to include chat and mail content |
| `MONICA_OCR_ENABLED` | Enable image-reading jobs |
| `MONICA_OCR_MODEL` | Image-capable model |
| `MONICA_OCR_DAILY_LIMIT` | Daily reserved-call budget |
| `MONICA_OCR_INCLUDE_CHAT` | Separate consent for private chat images |
| `MONICA_AI_QA_ENABLED` | Enable the authenticated stateless browser transport |
| `MONICA_AI_QA_CONTEXT_WINDOW` | Browser model's declared context budget |
| `MONICA_AI_QA_MAX_TOKENS` | Maximum generated tokens for browser requests |

The transport accepts HTTPS, or HTTP on localhost/loopback for an explicitly
configured local compatible service. Redirects and inherited HTTP proxies are
disabled. Retries are bounded; errors returned to clients and durable jobs omit
upstream bodies, keys and arbitrary exception strings.

`crm.ai_jobs` stores the immutable input snapshot, model, prompt version, privacy
policy and hash. Subject heads, current-input checks, cancellation and lease tokens
fence publication. A changed source or consent policy prevents a stale worker
from publishing. A canceled or dropped task releases in-process admission and can
resume from durable state after its lease expires. Reverting A→B→A can reuse
immutable cached work while restoring the correct visible output.

Profile Memory uses source-addressed inputs and persisted reductions. Unchanged
buckets reuse cached parts; dirty buckets load their current source bodies under
one repeatable-read snapshot. That database snapshot ends before provider I/O.
Large inputs split into bounded leaves and hierarchical reductions. A worker turn
performs at most one new Memory model part call; durable parts resume on later
turns. Mail headers, profile facts, source authorship and private content have
separate rules. Suggestions retain references to submitted evidence; accepting a
field/date/meetup suggestion commits the resulting write, audit and decision in
one transaction. Generated mail events are quoted candidates, never automatic
people creation or external actions.

The process has one reserved interactive model slot and three background slots.
Actual background concurrency is further constrained by shared CPU/network/memory
admission, leaving network and memory capacity for interactive requests. The
browser receives SSE through `POST /api/v1/ai/chat/completions`; its agent loop and
session history remain in the browser. The backend forwards a bounded request
and stream, releases database connections before upstream I/O, and drops the
upstream stream when the browser disconnects.

## Collector process protocol

Each provider has persistent enabled/paused controls and a bounded run lease.
The four ordinary controls (`wechat`, `instagram`, `linkedin`, `chatlog`) default
to disabled. The internal `linkedin_messages` lane also requires the parent
LinkedIn control to be enabled and unpaused. Its own run and cooldown are
independent of LinkedIn profile collection.

An external executable must be explicitly configured with absolute paths:

- `MONICA_ADAPTER_<PROVIDER>_EXECUTABLE`
- `MONICA_ADAPTER_<PROVIDER>_WORK_DIR`
- Optional `MONICA_ADAPTER_<PROVIDER>_ARGS_JSON`, a JSON array of arguments
- Optional `MONICA_ADAPTER_<PROVIDER>_ENABLED`, `_PAUSED`, `_MIN_INTERVAL_SECONDS`

Native bundles provide a relocatable `MONICA_COLLECTOR_BRIDGE` wrapper and
`MONICA_COLLECTOR_DATA_ROOT` as defaults. The wrapper locates its own Python,
locked dependencies, browser, vendored collector and companion bridge modules.
The core clears the child environment and invokes it without a shell. No old
installation sessions, source directories or credentials are discovered.
Cancellation kills the owned process group, including grandchildren. Numeric
progress and fixed phase names are whitelisted; raw stderr and arbitrary command
arguments do not enter API status responses.

The executable writes bounded newline-delimited JSON frames to stdout:

- `snapshot`: one ordinary signed-import-shaped `batch`.
- `connection_snapshot`: explicit owning `account`, one `batch`, and optionally
  `page_sha256` plus root-relative `raw/.../*.ack` path. The core creates or finds
  the connection without re-enabling an existing disabled connection. It checks
  provider controls, lease and source connection state before storing the
  receipt. The filesystem ACK is written and synced only after that receipt
  commits. CRM projection remains independently recoverable from the journal.
- `media`: root-confined `path`, lowercase SHA-256 and actual `byte_length`.
  Durable media ingestion verifies and archives the actual bytes separately.
- `progress`: fixed `phase` and numeric `processed`/`total`.

A frame is at most 4 MiB; per-process output and runtime are bounded. Published
snapshot identities are immutable. Reusing an identity with a different body
fails rather than silently replacing the earlier receipt.

## External container shared-volume IPC

`collector-bridge.py --spool-dir /data/spool` writes durable IPC instead of source
frames on container stdout. The spool and source media must be inside the
explicit data volume. The Rust consumer uses:

- `MONICA_ADAPTER_<PROVIDER>_SPOOL=/absolute/data/spool`
- `MONICA_ADAPTER_<PROVIDER>_WORK_DIR=/absolute/data`

A page is named `<20-digit-monotonic-sequence>-<64-lowercase-sha256>.jsonl`.
Publication writes and syncs a temporary file, atomically renames it, then
publishes a synced `.ready` JSON marker containing version, SHA and byte length.
The consumer validates regular files, source root, digest, size and stable file
identity. It handles at most 25 frames per turn, committing its byte offset only
after each durable source receipt. A crash between receipt and checkpoint
replays the same idempotent frame. Completed pages receive `.ack` and are renamed
to `.consumed` after the final checkpoint. Corrupt pages fail independently and
remain inspectable. Relative media paths resolve inside the shared work root.

LinkedIn's message lane reads its own
`raw/linkedin/messages/<mailbox>/pending` artifacts. An identical normalized
message page reuses a content-addressed batch, including the original observation
time. The original pending artifact moves to archive only after a matching
receipt ACK and page hash; printing the frame never advances its checkpoint.

## Chatlog snapshots

The native supervisor owns the pinned patched Chatlog daemon, explicit data/key
settings, loopback port, startup/pause/restart and shutdown. The reader does not
extract keys, open the encrypted source or restart an existing daemon. The daemon
atomically publishes consistent standalone SQLite snapshots, including WAL, into
its app-owned `work/db_storage` directory.

The snapshot bridge requires an explicit `MONICA_CHATLOG_SNAPSHOT_ROOT` and
`MONICA_CHATLOG_ACCOUNT` in addition to the bundled wrapper/data root. Native
creates an opaque stable account anchor from the explicitly configured source
path and archives its owned work when that source changes. An optional
`MONICA_CHATLOG_SELF_ID` is an authoritative own WeChat identifier. Without it,
direction remains unknown; display names and source status flags are not used
to guess the user.

The reader is `collector-bridge.py --provider wechat --data-root ABS
--snapshot-root ABS --account ANCHOR [--self-id ID]`. `collector-chatlog.py` is a
required bundled companion. It reads only `contact/contact.db`,
`message/message_N.db` and `sns/sns.db` under the supplied export root. It pins
one exported inode across passes, using a synced copy for cross-filesystem
volumes. A newer export cannot shift an active scan's pagination.

Each pass reads at most 500 source records or prepares 1,000 Name2Id entries.
Pages are also bounded to 3 MiB of source JSON. Contacts use stable usernames;
messages use Name2Id sender/talker IDs and stable server/local source IDs.
Moments preserve the original XML, author and absolute source time. Private
binary messages retain bytes as base64 instead of inventing text. Missing sender
identity, malformed schema, invalid time, symlink escape or a mismatched ACK fails
closed and preserves the checkpoint. Other files receive independent turns and
failed files back off.

Changed snapshots reconcile corrections and deletions. Reused local row IDs
retire the former external identity. Unacknowledged pages remain byte-identical
across restarts. Local cursor and known-content changes commit only after the
Rust receipt ACK. Actual backfill pages can continue at one-second intervals;
fully unchanged sources and errors use a sixty-second adapter backoff.

## Reproducible checks

These commands use synthetic fixtures and fake providers only:

```sh
PYTHONPYCACHEPREFIX=/tmp/monica-ai-pycache python3 scripts/test-collector-bridge.py
PYTHONPYCACHEPREFIX=/tmp/monica-ai-pycache python3 scripts/test-collector-chatlog.py
cargo test --offline -p monica-core --test adapter_connections --no-run --locked
cargo clippy --offline --workspace --all-targets --locked -- -D warnings
```

Database test execution requires a **new disposable PostgreSQL cluster**. The
supervising fresh-cluster harness executes `ai`, `ai_mail`, `ai_memory`,
`adapters` and `adapter_connections`, including real child-process and fake HTTP
boundaries. Never point that harness or SQLx tests at an existing installation.
Compilation alone does not count as database or provider transport evidence.

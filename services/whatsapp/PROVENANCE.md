The linked-device reader is copied from the tracked source at
`monica-multisource@1a6e40e20c6ac12aca434b61244059d067847473`.
It is an optional external Go process; Rust imports its durable pending pages.
No send-message command is exposed. Every account must use its own directory.
Build with `go build -o monica-whatsapp .` (CGO SQLite required).
Run only after explicit account connection; offline tests use `go test ./...`.

The NAS adapter runs as the optional `whatsapp` Compose profile. Create a WhatsApp
source explicitly in Sources, then set `MONICA_WHATSAPP_CONNECTION_ID` to its UUID
and start that profile. Its session and durable pages use the shared media volume
under `sources/whatsapp/<connection UUID>`. Rust acknowledges a pending page only
after its source transaction is durable. A disabled source does not ingest pages.
The profile is off by default. Multiple accounts need distinct adapter service
instances and connection UUIDs; never share one WhatsApp session directory.

The bridge waits for the server's atomic `control.json` (`enabled`, `paused`)
before connecting. Pause keeps local session keys. Disconnect clears only local
linked-device keys; it retains imported evidence, pending events and attachments.
An external reader remains offline until the connection is explicitly enabled
again. The embedded reader uses one process slot, a separate process group,
`GOMAXPROCS=1` and a 32 MiB Go GC target (a soft runtime target, not an OS RSS cap).
Multiple external readers must use separate connection directories.

Rust changes also fsync publication directories, cancel downloads on pause,
and preserve pending work when cancellation interrupts a download. These changes
are tested without a WhatsApp login. Generate bundle license files with
`python3 scripts/whatsapp-licenses.py`; `licenses/inventory.json` records each
actual pinned dependency and copied notice. No session material is bundled.

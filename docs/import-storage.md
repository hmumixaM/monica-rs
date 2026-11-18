# Import storage

The reusable storage code lives in `scripts/monica_imports/`. Normal source
imports no longer load migration repair command-line programs as libraries.

`sqlite_snapshot.publish_snapshot` produces a standalone, integrity-checked
SQLite snapshot from a read-only source. A normal live source uses one pinned
read transaction for the entire incremental backup. When a fresh export lacks
a WAL index or includes a rollback journal, a stable copy of its database and
journal files is recovered in private writable storage. Changing files reject
that capture and leave the previous published target intact. Source database
and WAL bytes are never modified. The caller records the captured source
signature only after atomic publication and directory synchronization.

`media_store.MediaStore` owns content-addressed file publication. It checks a
staged object's size and SHA-256, synchronizes it, and publishes with a
no-replace link into `sha256/<prefix>/<prefix>/<digest>`. A repeated import uses
the same object. Conflicting existing bytes fail instead of being overwritten.
The existing media-root marker remains valid, so existing files need no move.

`sns_publication.publish` validates the decoded media and current source event,
then publishes its asset, provenance, slot and link under the database's existing
publication/garbage-collection lock contract. The caller supplies a physical
`DatabaseTarget`; there are no compiled-in NAS system identifiers or database
OIDs. Publication is idempotent if a file has been written before a transaction
fails. A source page is acknowledged only after normal publication succeeds.

`source_media.link_existing` can reuse an already registered, verified file with
the same source URL. It does not scan historical migration tables to invent
bindings. New source pages use the ordinary importer. There are no startup
version markers that initiate historical image/video repairs.

The previous snapshot-specific re-decode, correction publishing, duplicate-post
reconciliation and archived-link repair commands have been retired. Applied SQL
migrations, stored provenance and imported aliases remain part of the existing
database contract; deleting the commands does not undo historical data changes.

The focused regressions are `test-sqlite-snapshot.py`, `test-media-store.py`,
`test-import-media.py` and `test-nas-live-imports.py`. They use synthetic files
and disposable PostgreSQL databases.

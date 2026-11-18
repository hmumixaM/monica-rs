"""Source-owned SQLite coordination never leaks into the read-only receiver."""
from contextlib import closing
import hashlib
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from monica_imports import sqlite_snapshot as snapshots


class SnapshotContract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root/'source'
        self.target = self.root/'published'
        self.source.mkdir()
        self.target.mkdir()
        self.path = self.source/'messages.db'
        self.output = self.target/'messages.db'

    def test_committed_wal_is_read_without_source_shm_or_write_permission(self):
        with closing(sqlite3.connect(self.path)) as writer:
            writer.execute('PRAGMA journal_mode=WAL')
            writer.execute('CREATE TABLE messages(body)')
            writer.execute("INSERT INTO messages VALUES('committed in WAL')")
            writer.commit()
            Path(str(self.path)+'-shm').unlink()
            paths = [self.path, Path(str(self.path)+'-wal')]
            before = [hashlib.sha256(p.read_bytes()).hexdigest() for p in paths]
            for p in paths:
                p.chmod(0o444)
            self.source.chmod(0o555)
            try:
                snapshots.publish_snapshot(self.path, self.output)
                with closing(sqlite3.connect(self.output)) as result:
                    self.assertEqual(result.execute('SELECT * FROM messages').fetchall(), [('committed in WAL',)])
                self.assertFalse(Path(str(self.path)+'-shm').exists())
                self.assertEqual(before, [hashlib.sha256(p.read_bytes()).hexdigest() for p in paths])
            finally:
                self.source.chmod(0o755)
                for p in paths:
                    p.chmod(0o644)

    def test_writer_can_commit_during_pinned_snapshot_without_restarting_it(self):
        with closing(sqlite3.connect(self.path)) as writer:
            writer.execute('PRAGMA journal_mode=WAL')
            writer.execute('CREATE TABLE messages(body)')
            writer.execute("INSERT INTO messages VALUES('before')")
            writer.executemany('INSERT INTO messages VALUES(?)', [('x' * 4000,)] * 300)
            writer.commit()
            committed = []
            def progress(_status, remaining, _total):
                if remaining and not committed:
                    writer.execute("INSERT INTO messages VALUES('after')")
                    writer.commit()
                    committed.append(True)
            snapshots.publish_snapshot(self.path, self.output, progress=progress)
            self.assertTrue(committed, 'writer must commit between incremental backup steps')
            with closing(sqlite3.connect(self.output)) as result:
                self.assertEqual(result.execute('SELECT count(*) FROM messages').fetchone()[0], 301)
                self.assertEqual(result.execute("SELECT count(*) FROM messages WHERE body='after'").fetchone()[0], 0)
            self.assertEqual(writer.execute('SELECT count(*) FROM messages').fetchone()[0], 302)

    def test_changed_capture_and_cancellation_keep_previous_publication(self):
        with closing(sqlite3.connect(self.path)) as writer:
            writer.execute('PRAGMA journal_mode=WAL')
            writer.execute('CREATE TABLE messages(body)')
            writer.commit()
            snapshots.publish_snapshot(self.path, self.output)
            previous = self.output.read_bytes()
            Path(str(self.path)+'-shm').unlink()
            original = snapshots.copy_file
            def copy_then_replace(source, target, cancelled):
                result = original(source, target, cancelled)
                if source == self.path:
                    replacement = self.source/'replacement.db'
                    replacement.write_bytes(self.path.read_bytes())
                    replacement.replace(self.path)
                return result
            with patch.object(snapshots, 'copy_file', side_effect=copy_then_replace):
                with self.assertRaises(snapshots.SourceChanged):
                    snapshots.publish_snapshot(self.path, self.output)
            self.assertEqual(self.output.read_bytes(), previous)
            with self.assertRaises(InterruptedError):
                snapshots.publish_snapshot(self.path, self.output, cancelled=lambda: True)
            self.assertEqual(self.output.read_bytes(), previous)
            self.assertFalse(list(self.target.glob('.sqlite-snapshot-*')))

    def test_exclusive_uncommitted_writer_does_not_block_committed_snapshot(self):
        with closing(sqlite3.connect(self.path)) as writer:
            writer.execute('CREATE TABLE messages(body)')
            writer.commit()
            writer.execute('BEGIN EXCLUSIVE')
            writer.execute("INSERT INTO messages VALUES('not committed')")
            snapshots.publish_snapshot(self.path, self.output)
            with closing(sqlite3.connect(self.output)) as result:
                self.assertEqual(result.execute('SELECT count(*) FROM messages').fetchone()[0], 0)
            self.assertTrue(writer.in_transaction)
            writer.commit()
            self.assertEqual(writer.execute('SELECT count(*) FROM messages').fetchone()[0], 1)


if __name__ == '__main__':
    unittest.main()

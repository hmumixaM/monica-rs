from contextlib import closing
import hashlib
import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('readiness',Path(__file__).with_name('nas-chatlog-readiness.py'))
readiness=importlib.util.module_from_spec(spec);spec.loader.exec_module(readiness)

class ReadOnlyCoordination(unittest.TestCase):
    def test_live_wal_reader_leaves_database_and_committed_wal_bytes_unchanged(self):
        with tempfile.TemporaryDirectory()as directory:
            path=Path(directory)/'contact.db'
            with closing(sqlite3.connect(path))as writer:
                writer.execute('PRAGMA journal_mode=WAL');writer.execute('CREATE TABLE records(id PRIMARY KEY,value)');writer.execute("INSERT INTO records VALUES(1,'committed')");writer.commit()
                files=[path,Path(str(path)+'-wal')]
                before=[hashlib.sha256(p.read_bytes()).hexdigest()for p in files]
                result=readiness.probe(path)
                self.assertTrue(result['main_file_unchanged'])
                self.assertEqual(before,[hashlib.sha256(p.read_bytes()).hexdigest()for p in files])
                self.assertEqual(writer.execute('SELECT * FROM records').fetchall(),[(1,'committed')])

    def test_busy_source_is_bounded_and_sidecar_alias_is_rejected(self):
        with tempfile.TemporaryDirectory()as directory:
            path=Path(directory)/'contact.db'
            with closing(sqlite3.connect(path))as writer:
                writer.execute('CREATE TABLE records(id)');writer.commit();writer.execute('BEGIN EXCLUSIVE')
                with self.assertRaises(sqlite3.OperationalError):readiness.probe(path,timeout=0.01)
                writer.rollback()
            Path(str(path)+'-shm').symlink_to(path)
            with self.assertRaises(ValueError):readiness.probe(path)

if __name__=='__main__':unittest.main()

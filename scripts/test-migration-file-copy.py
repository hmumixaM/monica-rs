#!/usr/bin/env python3
"""Verify independent byte copies, resume and committed WAL snapshot semantics."""
import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest

spec=importlib.util.spec_from_file_location("copy_worker",Path(__file__).with_name("migration-file-copy.py"))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)


class CopyContract(unittest.TestCase):
    def test_byte_copies_are_independent_and_resume_checks_destination(self):
        with tempfile.TemporaryDirectory(prefix="monica-copy-") as folder:
            root=Path(folder);source=root/"source";source.mkdir();target=root/"target"
            (source/"photo").write_bytes(b"synthetic original photo")
            (source/"photo.migration-partial").write_bytes(b"another original file")
            (source/"chatlog.json").write_text('{"synthetic":"excluded runtime configuration"}')
            original=m.sha256(source/"photo")
            worker=m.Copier(source,target,root/"journal.sqlite",rate=10**9)
            try:
                result=worker.run()
                self.assertEqual(result["files_verified"],2)
                self.assertEqual(result["excluded_credentials"],1)
                self.assertFalse((target/"chatlog.json").exists())
                self.assertEqual(m.sha256(target/"photo"),original)
                self.assertNotEqual((target/"photo").stat().st_ino,(source/"photo").stat().st_ino)
                (target/"photo").write_bytes(b"destination changed")
                # Model persisted journal/inode metadata with damaged data;
                # recovery must check bytes rather than trust that metadata.
                worker.journal.execute("UPDATE files SET destination_stamp=? WHERE path='photo'",(__import__('json').dumps(m.fingerprint((target/"photo").stat())),))
                worker.run()
                self.assertEqual(m.sha256(target/"photo"),original)
                self.assertEqual(m.sha256(source/"photo"),original)
                self.assertEqual((target/"photo.migration-partial").read_bytes(),b"another original file")
            finally:worker.journal.close()
            with self.assertRaisesRegex(ValueError,"disjoint"):
                m.Copier(source,source/"nested",root/"bad-journal.sqlite")
            with self.assertRaisesRegex(ValueError,"read-only source"):
                m.Copier(source,target,source/"unsafe-state.sqlite")
            self.assertFalse((source/"unsafe-state.sqlite").exists())

    def test_snapshot_includes_committed_wal_and_rechecks_a_changed_wal(self):
        with tempfile.TemporaryDirectory(prefix="monica-sqlite-") as folder:
            root=Path(folder);source=root/"source";source.mkdir();target=root/"target"
            path=source/"messages.db";writer=sqlite3.connect(path)
            writer.execute("PRAGMA journal_mode=WAL");writer.execute("PRAGMA wal_autocheckpoint=0")
            writer.execute("CREATE TABLE messages(id integer PRIMARY KEY,body text)")
            writer.execute("INSERT INTO messages VALUES(1,'committed')");writer.commit()
            writer.execute("INSERT INTO messages VALUES(2,'not yet committed')")
            worker=m.Copier(source,target,root/"journal.sqlite",rate=10**9)
            try:
                worker.run(sqlite_only=True)
                with sqlite3.connect(target/"messages.db") as read:
                    self.assertEqual(read.execute("SELECT count(*) FROM messages").fetchone()[0],1)
                writer.commit()
                worker.run(sqlite_only=True)
                with sqlite3.connect(target/"messages.db") as read:
                    self.assertEqual(read.execute("SELECT count(*) FROM messages").fetchone()[0],2)
                    self.assertEqual(read.execute("PRAGMA integrity_check").fetchall(),[("ok",)])
                self.assertEqual(writer.execute("SELECT count(*) FROM messages").fetchone()[0],2)
            finally:
                writer.close();worker.journal.close()

    def test_health_pause_does_not_publish_an_incomplete_file(self):
        with tempfile.TemporaryDirectory(prefix="monica-pause-") as folder:
            root=Path(folder);source=root/"source";source.mkdir();target=root/"target"
            original=b"synthetic photo"*40000
            (source/"photo").write_bytes(original)
            calls=0
            def pause():
                nonlocal calls
                calls+=1
                if calls==2:raise RuntimeError("synthetic health pause")
            worker=m.Copier(source,target,root/"journal.sqlite",rate=10**9,guard=pause)
            try:
                with self.assertRaisesRegex(RuntimeError,"health pause"):worker.run()
                self.assertFalse((target/"photo").exists())
                self.assertEqual(worker.status()["files_verified"],0)
                self.assertEqual((source/"photo").read_bytes(),original)
                worker.guard=lambda:None
                self.assertEqual(worker.run()["files_verified"],1)
                self.assertEqual((target/"photo").read_bytes(),original)
            finally:worker.journal.close()

    def test_live_wal_writer_can_commit_during_a_consistent_snapshot(self):
        with tempfile.TemporaryDirectory(prefix="monica-live-wal-") as folder:
            root=Path(folder);source=root/"source";source.mkdir();target=root/"target"
            writer=sqlite3.connect(source/"messages.db")
            writer.execute("PRAGMA journal_mode=WAL");writer.execute("PRAGMA wal_autocheckpoint=0")
            writer.execute("CREATE TABLE messages(id INTEGER PRIMARY KEY,body BLOB)")
            writer.executemany("INSERT INTO messages VALUES(?,?)",[(i,b"synthetic"*32768) for i in range(8)]);writer.commit()
            committed=False
            def append():
                nonlocal committed
                if worker.bytes and not committed:
                    writer.execute("INSERT INTO messages VALUES(9,'new live message')");writer.commit();committed=True
            worker=m.Copier(source,target,root/"journal.sqlite",rate=10**9,guard=append)
            try:
                worker.run(sqlite_only=True)
                self.assertTrue(committed)
                with sqlite3.connect(target/"messages.db") as snapshot:
                    self.assertEqual(snapshot.execute("SELECT count(*) FROM messages").fetchone()[0],8)
                worker.run(sqlite_only=True)
                with sqlite3.connect(target/"messages.db") as snapshot:
                    self.assertEqual(snapshot.execute("SELECT count(*) FROM messages").fetchone()[0],9)
            finally:worker.journal.close();writer.close()


if __name__=="__main__":unittest.main()

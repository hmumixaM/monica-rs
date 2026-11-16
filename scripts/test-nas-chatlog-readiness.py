from contextlib import closing
import hashlib
import importlib.util
import json
import os
from types import SimpleNamespace
from unittest.mock import MagicMock,patch
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

    def test_atomic_refresh_after_probe_is_checked_next_cycle(self):
        with tempfile.TemporaryDirectory()as directory:
            root=Path(directory);source=root/'source';(source/'contact').mkdir(parents=True)
            path=source/'contact/contact.db';replacement=root/'replacement.db'
            for target,tables in((path,1),(replacement,2)):
                with closing(sqlite3.connect(target))as db:
                    for i in range(tables):db.execute('CREATE TABLE table_'+str(i)+'(id)')
            (root/'data').mkdir();(root/'data/.monica-rs-ssd-target').write_text('monica-rs-ssd-target-v1\n');(root/'state').mkdir()
            guarded=MagicMock(wraps=root);guarded.resolve.return_value=guarded;guarded.stat.return_value=SimpleNamespace(st_dev=2081);guarded.__truediv__.side_effect=lambda key:root/key
            original=readiness.probe;versions=[];sleeps=[]
            def replace_after_probe(target):
                result=original(target);versions.append(result['schema_version'])
                if len(versions)==1:os.replace(replacement,path)
                return result
            def tick(_):
                sleeps.append(1)
                if len(sleeps)==61:readiness.STOP=True
            with patch.object(readiness,'ROOT',guarded),patch.object(readiness,'SOURCE',source),patch.object(readiness,'STOP',False),patch.object(readiness.os,'geteuid',return_value=0),patch.object(readiness.signal,'signal'),patch.object(readiness,'probe',side_effect=replace_after_probe),patch.object(readiness.time,'sleep',side_effect=tick),patch('sys.argv',['readiness','--watch']):
                readiness.main()
            self.assertEqual(versions,[1,2])
            self.assertEqual(json.loads((root/'state/chatlog-sqlite-readiness/status.json').read_text())['files']['contact/contact.db']['schema_version'],2)

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

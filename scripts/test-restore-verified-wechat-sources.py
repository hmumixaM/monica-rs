import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('restore', Path(__file__).with_name('restore-verified-wechat-sources.py'))
restore = importlib.util.module_from_spec(spec)
spec.loader.exec_module(restore)


class RestoreSourceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name).resolve()
        stage, media, state = [base / p for p in ('stage', 'media', 'state')]
        for p in (stage, media, state):
            p.mkdir()
        (media / '.monica-rust-migration-media').write_text(restore.helper.MARKER)
        (stage / '.verified-source-stage').write_text('monica-verified-source-stage-v1\n')
        payload = b'Synthetic exact source bytes.\n'
        self.sha = hashlib.sha256(payload).hexdigest()
        md5 = hashlib.md5(payload).hexdigest()
        (stage / self.sha).write_bytes(payload)
        index = base / 'index.sqlite'
        metadata = {'complete': '1', 'snapshot': 'synthetic-snapshot', 'own_id': 'synthetic-account'}
        with sqlite3.connect(index) as d:
            d.executescript('CREATE TABLE metadata(key TEXT,value TEXT); CREATE TABLE files(source_key TEXT,kind TEXT,md5 TEXT,expected_bytes INT,candidates TEXT);')
            d.executemany('INSERT INTO metadata VALUES(?,?)', metadata.items())
            d.execute('INSERT INTO files VALUES(?,?,?,?,?)', ('missing', 'file', md5, len(payload), '["msg/synthetic.txt"]'))
        index_hash = restore.helper.checksum(index)
        with sqlite3.connect(state / 'wechat-file-assets.sqlite') as d:
            d.executescript('CREATE TABLE metadata(key TEXT,value TEXT); CREATE TABLE assets(source_key TEXT PRIMARY KEY,kind TEXT,md5 TEXT,source_path TEXT,raw_sha256 TEXT,asset TEXT,status TEXT);')
            d.executemany('INSERT INTO metadata VALUES(?,?)', [('snapshot', metadata['snapshot']), ('own_id', metadata['own_id']), ('input_sha256', index_hash)])
            d.executemany('INSERT INTO assets VALUES(?,?,?,?,?,?,?)', [
                ('missing', 'file', md5, None, None, None, 'source_missing'),
                ('old-ready', 'file', 'a' * 32, 'unchanged', 'b' * 64, 'unchanged-asset', 'ready'),
                ('other-missing', 'file', 'c' * 32, None, None, None, 'source_missing')])
        (stage / 'manifest.json').write_text(json.dumps(dict(version=1, index_sha256=index_hash, metadata=metadata, entries=[
            dict(key='missing', kind='file', md5=md5, bytes=len(payload), sha256=self.sha, scope='same_account', media_type='application/octet-stream', width=0, height=0)])))
        self.args = SimpleNamespace(stage=stage, media=media, state=state, index=index,
                                    copy_lock=base / 'copy.lock', index_sha256=index_hash,
                                    expected_records=3, owner_uid=os.getuid(), owner_gid=os.getgid())
        self.args.copy_lock.touch()

    def rows(self):
        with sqlite3.connect(self.args.state / 'wechat-file-assets.sqlite') as d:
            return d.execute('SELECT * FROM assets ORDER BY source_key').fetchall()

    def test_exact_recovery_retains_history_and_resumes_without_duplication(self):
        before = self.rows()
        result = restore.restore(self.args, lambda: None)
        self.assertEqual(result['recovered'], 1)
        after = self.rows()
        self.assertEqual(before[1:], after[1:])
        self.assertIsNone(after[0][3])
        self.assertEqual(after[0][6], 'ready')
        with sqlite3.connect(self.args.state / 'wechat-file-assets.sqlite') as d:
            receipt = d.execute('SELECT original_row,raw_sha256 FROM verified_source_recoveries').fetchone()
        self.assertEqual(json.loads(receipt[0]), list(before[0]))
        self.assertEqual(receipt[1], self.sha)
        self.assertEqual(restore.restore(self.args, lambda: None)['recovered'], 0)

    def test_corrupt_staging_and_symlink_refuse_before_ledger_change(self):
        before = self.rows()
        source = self.args.stage / self.sha
        source.write_bytes(b'corrupt')
        with self.assertRaises(Exception):
            restore.restore(self.args, lambda: None)
        self.assertEqual(self.rows(), before)
        other = self.args.stage / 'other'
        source.rename(other)
        source.symlink_to(other)
        with self.assertRaises(Exception):
            restore.restore(self.args, lambda: None)
        self.assertEqual(self.rows(), before)


    def test_interrupted_link_publication_can_resume(self):
        original_link = os.link
        def interrupted_link(source, target):
            original_link(source, target)
            raise RuntimeError('synthetic interruption after linking')
        before = self.rows()
        original_unlink = Path.unlink
        def interrupted_unlink(path, *args, **kwargs):
            if path.name.startswith('.source-recovery-'):
                raise RuntimeError('synthetic interruption before unlinking')
            return original_unlink(path, *args, **kwargs)
        with patch.object(restore.os, 'link', interrupted_link), patch.object(Path, 'unlink', interrupted_unlink):
            with self.assertRaises(RuntimeError):
                restore.restore(self.args, lambda: None)
        self.assertEqual(self.rows(), before)
        self.assertEqual(restore.restore(self.args, lambda: None)['recovered'], 1)
        target = self.args.media / 'sha256' / self.sha[:2] / self.sha[2:4] / self.sha
        self.assertEqual(target.stat().st_nlink, 1)
        self.assertEqual(list(target.parent.glob('.source-recovery-*')), [])


if __name__ == '__main__':
    unittest.main()

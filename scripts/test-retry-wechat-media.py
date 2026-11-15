#!/usr/bin/env python3
"""Offline recovery contracts; temporary copies only, no Docker/NAS/credentials."""
import base64
from contextlib import closing
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location('retry_media', Path(__file__).with_name('retry-wechat-media.py'))
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)
PNG = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=')


class Codec:
    runtime_id = 'c' * 64

    def __init__(self, args):
        self.args, self.calls, self.mode = args, 0, 'ready'

    def terminal(self):
        pass

    def recover(self, inspect_only=False):
        pass

    def convert(self, requests, credentials):
        self.calls += 1
        values = []
        for request in requests:
            if self.mode == 'failed':
                value = {'error_code': 'wxgf_conversion_failed'}
            elif self.mode == 'private':
                return b'synthetic PRIVATE_KEY_OR_BODY is not a protocol record'
            else:
                key = hashlib.sha256(PNG).hexdigest()
                relative = 'sha256/' + key[:2] + '/' + key[2:4] + '/' + key
                path = self.args.media / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                if not path.exists():
                    path.write_bytes(PNG)
                value = {'asset': {'sha256': key, 'byte_length': len(PNG), 'media_type': 'image/png',
                                   'object_path': relative, 'width': 1, 'height': 1}}
                if self.mode == 'invalid':
                    value['asset']['byte_length'] += 1
            value.update(id=request['id'], codec_commit=self.args.codec_commit)
            values.append(value)
        return ('\n'.join(json.dumps(v) for v in values) + '\n').encode()


class Recovery(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='monica-retry-test-')
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name).resolve()
        self.args = a = SimpleNamespace(raw=root / 'raw', media=root / 'media', state=root / 'state',
                                       index=root / 'index.sqlite', codec=root / 'codec',
                                       copy_lock=root / 'copy.lock', action='retry', expected_records=3,
                                       codec_commit='a' * 40, limit=16, repeat=False,
                                       owner_uid=os.getuid(), owner_gid=os.getgid())
        for path in (a.raw, a.media, a.state):
            path.mkdir()
        (a.media / '.monica-rust-migration-media').write_text(worker.MARKER)
        a.copy_lock.write_text('')
        a.codec.write_text('synthetic pinned codec')
        a.codec.chmod(0o700)
        a.codec_sha256 = worker.checksum(a.codec)
        raw = a.raw / 'attach' / 'copy.dat'
        raw.parent.mkdir()
        raw.write_bytes(b'synthetic original encrypted bytes')
        self.raw = raw
        self.raw_sha = worker.checksum(raw)
        with closing(sqlite3.connect(a.index)) as db:
            db.executescript('CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT);'
                             'CREATE TABLE files(source_key TEXT PRIMARY KEY,kind TEXT,md5 TEXT,candidates TEXT);')
            db.executemany('INSERT INTO metadata VALUES(?,?)',
                           [('snapshot', 'b' * 64), ('own_id', 'synthetic-self'), ('complete', '1')])
            db.executemany('INSERT INTO files VALUES(?,?,?,?)',
                           [(key, 'image', 'd' * 32, '["msg/attach/copy.dat"]') for key in ['failed', 'ready', 'missing']])
            db.commit()
        a.index_sha256 = worker.checksum(a.index)
        with closing(sqlite3.connect(a.state / 'wechat-file-assets.sqlite')) as db:
            db.executescript('CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT);'
                             'CREATE TABLE assets(source_key TEXT PRIMARY KEY,kind TEXT,md5 TEXT,source_path TEXT,raw_sha256 TEXT,asset TEXT,status TEXT);')
            db.executemany('INSERT INTO metadata VALUES(?,?)',
                           [('snapshot', 'b' * 64), ('own_id', 'synthetic-self'), ('input_sha256', a.index_sha256)])
            db.executemany('INSERT INTO assets VALUES(?,?,?,?,?,?,?)', [
                ('failed', 'image', 'd' * 32, 'msg/attach/copy.dat', self.raw_sha, None, 'decode_failed'),
                ('ready', 'image', 'd' * 32, 'msg/attach/copy.dat', self.raw_sha, '{"keep":"original"}', 'ready'),
                ('missing', 'image', 'd' * 32, None, None, None, 'source_missing')])
            db.commit()
        with closing(sqlite3.connect(a.state / 'wechat-files.sqlite')) as db:
            db.executescript('CREATE TABLE policy(name TEXT,value TEXT);'
                             'CREATE TABLE files(path TEXT PRIMARY KEY,sha256 TEXT,bytes INTEGER,mode TEXT);')
            db.executemany('INSERT INTO policy VALUES(?,?)', [('source', '/source'), ('target', '/target/snapshots/wechat-files/msg')])
            db.execute('INSERT INTO files VALUES(?,?,?,?)', ('attach/copy.dat', self.raw_sha, raw.stat().st_size, 'file'))
            db.commit()
        (a.state / 'copy-status-wechat-files.json').write_text(json.dumps(
            {'processed_records': 3, 'indexed_records': 3, 'state': 'paused'}))
        self.codec = Codec(a)

    def rows(self):
        with closing(sqlite3.connect(self.args.state / 'wechat-file-assets.sqlite')) as db:
            return list(db.execute('SELECT source_key,raw_sha256,asset,status FROM assets ORDER BY source_key'))

    def test_failed_only_recovery_and_replay_preserve_originals(self):
        before = self.rows()
        result = worker.retry(self.args, self.codec, {'image_key': 'synthetic-private-key'})
        self.assertEqual((result['selected'], result['recovered'], result['remaining_decode_failed']), (1, 1, 0))
        after = self.rows()
        self.assertEqual(after[1:], before[1:])
        self.assertEqual(after[0][1], self.raw_sha)
        self.assertEqual(worker.checksum(self.raw), self.raw_sha)
        result = worker.retry(self.args, self.codec, {})
        self.assertEqual((result['selected'], self.codec.calls), (0, 1))
        with closing(sqlite3.connect(self.args.state / 'wechat-file-assets.sqlite')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM codec_retry_attempts').fetchone()[0], 1)

    def test_recorded_failure_then_transaction_interruption_resume(self):
        self.codec.mode = 'failed'
        result = worker.retry(self.args, self.codec, {})
        self.assertEqual(result['codes'], {'wxgf_conversion_failed': 1})
        self.assertEqual(worker.retry(self.args, self.codec, {})['selected'], 0)
        self.args.repeat = True
        self.codec.mode = 'ready'
        path = self.args.state / 'wechat-file-assets.sqlite'
        with closing(sqlite3.connect(path)) as db:
            db.execute("CREATE TRIGGER interrupt_retry BEFORE INSERT ON codec_retry_attempts BEGIN SELECT RAISE(ABORT,'synthetic interruption'); END")
        with self.assertRaises(sqlite3.IntegrityError):
            worker.retry(self.args, self.codec, {})
        self.assertEqual(self.rows()[0][3], 'decode_failed', 'asset publication must roll back with retry history')
        with closing(sqlite3.connect(path)) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM codec_retry_attempts').fetchone()[0], 1)
            db.execute('DROP TRIGGER interrupt_retry')
        self.assertEqual(worker.retry(self.args, self.codec, {})['recovered'], 1)

    def test_raw_tampering_active_lock_and_invalid_output_are_rejected(self):
        before = self.rows()
        with self.args.copy_lock.open('rb') as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(worker.Failure, 'original_media_operation_active'):
                worker.retry(self.args, self.codec, {})
        self.assertEqual(self.codec.calls, 0)
        self.raw.write_bytes(b'changed raw copy')
        with self.assertRaises(worker.Failure):
            worker.retry(self.args, self.codec, {})
        self.raw.write_bytes(b'synthetic original encrypted bytes')
        for mode in ['invalid', 'private']:
            self.codec.mode = mode
            with self.assertRaises((worker.Failure, json.JSONDecodeError)):
                worker.retry(self.args, self.codec, {})
        self.assertEqual(self.rows(), before)
        self.assertNotIn('PRIVATE_KEY_OR_BODY', (self.args.state / 'wechat-file-assets.sqlite').read_bytes().decode('latin1'))

    def test_empty_resume_still_finalizes_committed_wal_for_standalone_reader(self):
        path = self.args.state / 'wechat-file-assets.sqlite'
        with closing(sqlite3.connect(path)) as holder, closing(sqlite3.connect(path)) as reader:
            self.assertEqual(holder.execute('PRAGMA journal_mode=WAL').fetchone()[0], 'wal')
            reader.execute('BEGIN')
            self.assertEqual(reader.execute("SELECT status FROM assets WHERE source_key='failed'").fetchone()[0], 'decode_failed')
            for _ in range(2):
                with self.assertRaisesRegex(worker.Failure, 'retry_checkpoint_pending'):
                    worker.retry(self.args, self.codec, {})
            self.assertEqual(self.codec.calls, 1, 'resume must retain the committed conversion')
            reader.rollback()
            result = worker.retry(self.args, self.codec, {})
            self.assertEqual((result['selected'], result['remaining_decode_failed']), (0, 0))
            with closing(sqlite3.connect(path.as_uri() + '?mode=ro&immutable=1', uri=True)) as standalone:
                self.assertEqual(standalone.execute("SELECT status FROM assets WHERE source_key='failed'").fetchone()[0], 'ready')

    def test_only_fully_accounted_published_temporary_links_are_recovered(self):
        key = hashlib.sha256(PNG).hexdigest()
        cas = self.args.media / 'sha256' / key[:2] / key[2:4] / key
        cas.parent.mkdir(parents=True)
        cas.write_bytes(PNG)
        temporary = self.args.media / '.migration-media-123456'
        os.link(cas, temporary)
        outside = self.args.state / 'unrelated-link'
        os.link(cas, outside)
        with self.assertRaisesRegex(worker.Failure, 'unaccounted_media_hardlinks'):
            worker.retry(self.args, self.codec, {})
        self.assertTrue(temporary.exists())
        self.assertEqual(outside.read_bytes(), PNG)
        outside.unlink()
        result = worker.retry(self.args, self.codec, {})
        self.assertEqual(result['recovered_temporary_links'], 1)
        self.assertEqual(result['recovered'], 1)
        self.assertFalse(temporary.exists())
        self.assertEqual((cas.stat().st_nlink, cas.read_bytes()), (1, PNG))

    def test_capture_drains_output_while_writing_and_bounds_diagnostics(self):
        code, output, errors = worker.capture([sys.executable, '-c',
            'import sys;sys.stdout.buffer.write(b"y"*100000);sys.stdout.flush();assert len(sys.stdin.buffer.read())==100000'],
            input_data=b'x' * 100000, deadline=5, limit=110000)
        self.assertEqual((code, len(output), errors), (0, 100000, b''))
        with self.assertRaisesRegex(worker.Failure, 'codec_output_limit'):
            worker.capture([sys.executable, '-c', 'import sys;sys.stdout.buffer.write(b"x"*100000)'], limit=32)

    def test_timeout_stops_only_verified_retry_and_recovers_its_receipt(self):
        self.args.image = 'sha256:' + 'f' * 64
        self.args.docker = Path('/unused-docker')
        test = self

        class Runtime(worker.DockerCodec):
            def __init__(self, args):
                super().__init__(args)
                self.item, self.commands = None, []

            def command(self, args, **kwargs):
                self.commands.append(list(args))
                if args[0] == 'create':
                    intent = worker.read_json(self.args.state / 'codec-retry-container.json')
                    test.assertIsNone(intent['id'])
                    self.item = {'Id': 'e' * 64, 'Image': self.args.image,
                                 'Config': {'Labels': {'monica.media-retry': intent['token']}},
                                 'Mounts': [{'Destination': k, 'Source': v[0], 'RW': v[1]} for k, v in self.mounts().items()],
                                 'NetworkSettings': {'Networks': {'none': {}}},
                                 'State': {'Running': False}}
                    return 0, self.item['Id'].encode(), b''
                if args[:2] == ['container', 'inspect']:
                    return 0, json.dumps([self.item]).encode(), b''
                if args[0] == 'start':
                    self.item['State']['Running'] = True
                    raise worker.Failure('codec_command_timeout')
                if args[0] == 'stop':
                    test.assertEqual(args[-1], self.item['Id'])
                    self.item['State']['Running'] = False
                    return 0, b'', b''
                raise AssertionError(args)

        runtime = Runtime(self.args)
        with self.assertRaisesRegex(worker.Failure, 'codec_command_timeout'):
            runtime.convert([{'id': 'a' * 64, 'path': 'attach/copy.dat', 'kind': 'image'}],
                            {'image_key': 'PRIVATE_KEY_MUST_NOT_APPEAR_IN_ARGUMENTS'})
        self.assertFalse(runtime.item['State']['Running'])
        self.assertNotIn('PRIVATE_KEY', repr(runtime.commands))
        receipt = self.args.state / 'codec-retry-container.json'
        self.assertEqual(receipt.stat().st_mode & 0o777, 0o600)
        runtime.item['State']['Running'] = True
        runtime.recover()
        self.assertFalse(runtime.item['State']['Running'])
        stop_count = sum(c[0] == 'stop' for c in runtime.commands)
        runtime.item['Config']['Labels']['monica.media-retry'] = 'changed-owner'
        with self.assertRaisesRegex(worker.Failure, 'retry_container_owner_changed'):
            runtime.recover()
        self.assertEqual(sum(c[0] == 'stop' for c in runtime.commands), stop_count)


if __name__ == '__main__':
    unittest.main()

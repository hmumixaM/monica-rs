#!/usr/bin/env python3
"""Focused, offline regressions. Optional PG contract uses a disposable database."""
import base64
from contextlib import closing, redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('group_images', ROOT / 'scripts/wechat-group-images.py')
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)
PNG = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a5n8AAAAASUVORK5CYII=')


class Images(unittest.TestCase):
    def test_only_image_attributes_are_references_and_quality_is_explicit(self):
        a, b = 'a' * 32, 'b' * 32
        self.assertEqual(worker.image_references(a, {'text': f'wxid:\n<msg><img md5="{a}" thumbmd5="{b}" /></msg>'}), [a, b])
        self.assertEqual(worker.image_references(f'<msg><img md5="{a}" /></msg><broken>'), [])
        self.assertEqual(worker.image_references(f'<!DOCTYPE msg [<!ENTITY x "{a}">]><msg><img md5="&x;"/></msg>'), [])
        self.assertEqual(worker.quality(a + '_t.dat'), ('thumbnail', 1))
        self.assertEqual(worker.quality(a + '_h.dat'), ('high', 4))
        self.assertEqual(worker.quality(a + '.dat'), ('unknown', 2))

    def test_no_run_does_not_open_database_or_source(self):
        result = subprocess.run([sys.executable, str(ROOT / 'scripts/wechat-group-images.py')],
                                env={'PATH': os.environ['PATH']}, capture_output=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn(b'--run', result.stdout)

    @unittest.skipUnless(os.getenv('MONICA_GROUP_IMAGE_TEST_ADMIN'), 'optional isolated PostgreSQL contract')
    def test_group_coverage_resume_reuse_upgrade_and_pause(self):
        import psycopg
        from psycopg import sql
        from psycopg.conninfo import conninfo_to_dict, make_conninfo
        from psycopg.types.json import Jsonb
        name = 'monica_rust_group_images_test_' + uuid.uuid4().hex
        admin = psycopg.connect(os.environ['MONICA_GROUP_IMAGE_TEST_ADMIN'], autocommit=True)
        db = None
        try:
            admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(name)))
            options = conninfo_to_dict(admin.info.dsn)
            options['dbname'] = name
            db = psycopg.connect(make_conninfo(**options), autocommit=True)
            for migration in sorted((ROOT / 'migrations').glob('*.sql')):
                with db.transaction():
                    db.execute(migration.read_text())
            db.execute('CREATE TABLE _sqlx_migrations(version bigint,success boolean)')
            db.execute('INSERT INTO _sqlx_migrations VALUES(68,true)')
            system = db.execute('SELECT system_identifier::text FROM pg_control_system()').fetchone()[0]
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                source, media, state = (root / p for p in ('source', 'media', 'state'))
                for path in (source / 'attach/a/2026/Img', media, state):
                    path.mkdir(parents=True)
                (media / '.monica-rust-migration-media').write_text(worker.MARKER)
                decoder = root / 'codec'
                decoder.write_text('#!' + sys.executable + '''
import hashlib,json,os,pathlib,sys
r=json.load(sys.stdin);source=pathlib.Path(sys.argv[2]);target=pathlib.Path(sys.argv[4]);data=(source/r['path']).read_bytes()
if not data.startswith(bytes.fromhex('89504e47')):sys.exit(1)
h=hashlib.sha256(data).hexdigest();p='sha256/'+h[:2]+'/'+h[2:4]+'/'+h
out=target/p;out.parent.mkdir(parents=True,exist_ok=True)
if not out.exists():
 with out.open('wb') as f:f.write(data);f.flush();os.fsync(f.fileno())
print(json.dumps(dict(sha256=h,byte_length=len(data),media_type='image/png',object_path=p,width=1,height=1)))
''')
                decoder.chmod(0o700)
                a, alias, filename = 'a' * 32, 'b' * 32, 'c' * 32
                folder = source / 'attach/a/2026/Img'
                (folder / (a + '_t.dat')).write_bytes(PNG)
                high = folder / (a + '_h.dat')
                high.write_bytes(b'invalid encrypted cache')
                (folder / (filename + '_h.dat')).write_bytes(PNG + b'alias')
                initial = {str(p.relative_to(source)): worker.digest(p) for p in source.rglob('*') if p.is_file()}
                index = root / 'metadata.sqlite'
                with closing(sqlite3.connect(index)) as meta:
                    meta.executescript('CREATE TABLE metadata(key TEXT,value TEXT);CREATE TABLE files(kind TEXT,md5 TEXT,candidates TEXT)')
                    meta.executemany('INSERT INTO metadata VALUES(?,?)', [('own_id', 'synthetic-self'), ('complete', '1')])
                    meta.execute('INSERT INTO files VALUES(?,?,?)', ('image', alias, json.dumps(['msg/attach/a/2026/Img/' + filename + '_h.dat'])))
                    meta.commit()
                group, unlinked, direct, other = [uuid.uuid4() for _ in range(4)]
                for gid, kind, account in ((group, 'group', 'synthetic-self'), (unlinked, 'group', 'synthetic-self'), (direct, 'direct', 'synthetic-self'), (other, 'group', 'other-account')):
                    db.execute("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type) VALUES(%s,'wechat',%s,%s,%s)", (gid, account, str(gid), kind))
                ids = []
                for gid, text, kind, tombstone in ((group, a, '3', False), (unlinked, alias, '4294967299', False), (group, 'd' * 32, '3', False), (group, None, 'image', False), (direct, a, '3', False), (other, a, '3', False), (group, a, '3', True), (group, a, 'text', False)):
                    mid, co = uuid.uuid4(), uuid.uuid4()
                    ids.append(mid)
                    body = f'<msg><img md5="{text}"/></msg>' if text else '[图片]'
                    db.execute("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body,message_type,tombstone) VALUES(%s,%s,'wechat','synthetic-self',%s,'unknown','2026-01-01T23:59:59Z','[图片]',%s,%s)", (mid, gid, str(mid), kind, tombstone))
                    db.execute("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json) VALUES(%s,'message',%s,1,%s)", (co, str(co), Jsonb({'text': body})))
                    db.execute("INSERT INTO crm.legacy_message_evidence(message_id,content_object_id,original) VALUES(%s,%s,'{}')", (mid, co))
                args = SimpleNamespace(account='synthetic-self', source_owner=None, decoder=decoder, expected_database=name, expected_system_id=system, file_index=index, retry=False)
                ledger = sqlite3.connect(state / 'state.sqlite')
                w = worker.Worker(db, ledger, args, (source, media, state), {'PATH': os.environ['PATH']})
                with self.assertRaises(worker.Paused):
                    w.open()
                self.assertEqual(db.execute('SELECT count(*) FROM crm.wechat_image_runs').fetchone()[0], 0)
                db.execute('UPDATE crm.wechat_image_control SET enabled=true,paused=false')
                with redirect_stdout(io.StringIO()):
                    w.open(); worker.catalog(source, ledger, w.guard)
                    worker.metadata_aliases(source, ledger, index, args.account, w.guard)
                    w.discover(); w.item(ids[0])
                    # Re-open a worker at the committed checkpoint, as after interruption.
                    w = worker.Worker(db, ledger, args, (source, media, state), {'PATH': os.environ['PATH']})
                    w.run()
                counts = db.execute('SELECT counts FROM crm.wechat_image_runs WHERE id=%s', (w.run_id,)).fetchone()[0]
                self.assertEqual({k: counts.get(k, 0) for k in ('total', 'available', 'quality_thumbnail', 'quality_high', 'source_missing', 'no_reference', 'pending')}, {'total': 4, 'available': 2, 'quality_thumbnail': 1, 'quality_high': 1, 'source_missing': 1, 'no_reference': 1, 'pending': 0})
                self.assertEqual(db.execute('SELECT sum((counts->>\'total\')::int) FROM crm.wechat_image_daily').fetchone()[0], 4)
                before = db.execute('SELECT count(*) FROM crm.media_links').fetchone()[0]
                with redirect_stdout(io.StringIO()):
                    w.run()
                self.assertEqual(db.execute('SELECT counts FROM crm.wechat_image_runs WHERE id=%s', (w.run_id,)).fetchone()[0], counts)
                self.assertEqual(db.execute('SELECT count(*) FROM crm.media_links').fetchone()[0], before)
                self.assertEqual(initial, {str(p.relative_to(source)): worker.digest(p) for p in source.rglob('*') if p.is_file()})
                high.write_bytes(PNG + b'upgraded')
                args.retry = True
                with redirect_stdout(io.StringIO()):
                    w.run()
                after = db.execute('SELECT counts FROM crm.wechat_image_runs WHERE id=%s', (w.run_id,)).fetchone()[0]
                self.assertEqual(after['total'], 4)
                self.assertEqual(after['quality_high'], 2)
                self.assertEqual(after['quality_thumbnail'], 0)
                db.execute('UPDATE crm.wechat_image_control SET paused=true')
                with self.assertRaises(worker.Paused):
                    w.item(ids[0])
                self.assertEqual(db.execute('SELECT count(*) FROM crm.message_search').fetchone()[0], 8)
                self.assertEqual(db.execute('SELECT count(*) FROM crm.profiles').fetchone()[0], 0)
                ledger.close()
                # Native account UUID and source owner are separate namespaces.
                db.execute('UPDATE crm.wechat_image_control SET paused=false')
                connection, native_group, native_mid = [uuid.uuid4() for _ in range(3)]
                db.execute("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES(%s,'wechat','synthetic-self','Synthetic','chatlog')", (connection,))
                db.execute("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type,connection_id) VALUES(%s,'wechat',%s,'native-group','group',%s)", (native_group, str(connection), connection))
                db.execute("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body,message_type) VALUES(%s,%s,'wechat',%s,'native-message','unknown','2026-01-02T00:00:00Z','[图片]','3')", (native_mid, native_group, str(connection)))
                payload_a = {'body': f'<msg><img md5="{a}"/></msg>'}
                payload_b = {'body': f'<msg><img md5="{alias}"/></msg>', 'deleted': True, 'included': False}
                for key, payload in [('projected-a', payload_a), ('undelivered-b', payload_b)]:
                    db.execute('INSERT INTO wechat.connection_payloads(content_hash,payload) VALUES(%s,%s)', (key, Jsonb(payload)))
                db.execute("INSERT INTO wechat.connection_record_heads(connection_id,external_id,revision,content_hash) VALUES(%s,'native-message',2,'undelivered-b')", (connection,))
                db.execute("INSERT INTO crm.connection_evidence(id,connection_id,external_id,revision,content_hash,kind,included,message_id,conversation_id) VALUES(%s,%s,'native-message',1,'projected-a','message',true,%s,%s)", (native_mid, connection, native_mid, native_group))
                native_state = root / 'native-state'; native_state.mkdir()
                native_ledger = sqlite3.connect(native_state / 'state.sqlite')
                args.account, args.source_owner, args.retry = str(connection), None, False
                native = worker.Worker(db, native_ledger, args, (source, media, native_state), {'PATH': os.environ['PATH']})
                with self.assertRaises(ValueError):
                    native.open()
                args.source_owner = 'other-owner'
                with self.assertRaises(ValueError):
                    native.open()
                args.source_owner = 'synthetic-self'
                with redirect_stdout(io.StringIO()):
                    native.run()
                sha_a = worker.digest(high)
                current_sha = lambda: db.execute('SELECT a.sha256 FROM crm.wechat_group_images i JOIN crm.media_assets a ON a.id=i.media_id WHERE i.run_id=%s AND i.message_id=%s', (native.run_id, native_mid)).fetchone()[0]
                self.assertEqual(current_sha(), sha_a)  # CE revision1, not newer deleted head2.
                old_asset = db.execute('SELECT media_id FROM crm.wechat_group_images WHERE run_id=%s AND message_id=%s', (native.run_id, native_mid)).fetchone()[0]
                db.execute("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('message_event',%s,%s,'manual-image')", (native_mid, old_asset))
                # A same-tier replacement must select B and preserve manual A.
                db.execute("INSERT INTO wechat.connection_payloads(content_hash,payload) VALUES('projected-b',%s)", (Jsonb({'body': payload_b['body'], 'included': True, 'deleted': False}),))
                db.execute("UPDATE wechat.connection_record_heads SET revision=3,content_hash='projected-b' WHERE connection_id=%s", (connection,))
                db.execute("UPDATE crm.connection_evidence SET revision=3,content_hash='projected-b' WHERE id=%s", (native_mid,))
                args.retry = True
                with redirect_stdout(io.StringIO()):
                    native.run()
                expected = worker.digest(folder / (filename + '_h.dat'))
                self.assertEqual(current_sha(), expected)
                self.assertEqual(db.execute("SELECT count(*) FROM crm.media_links WHERE entity_id=%s AND media_id=%s AND role LIKE 'wechat-group-image:%%'", (native_mid, old_asset)).fetchone()[0], 0)
                self.assertEqual(db.execute("SELECT count(*) FROM crm.media_links WHERE entity_id=%s AND media_id=%s AND role='manual-image'", (native_mid, old_asset)).fetchone()[0], 1)
                # Simulate restart after decoding, before public-link commit, with
                # a damaged derived receipt. The healthy source must be decoded.
                native_ledger.execute("UPDATE decoded SET asset='invalid json' WHERE path=?", ('attach/a/2026/Img/' + filename + '_h.dat',)); native_ledger.commit()
                db.execute("DELETE FROM crm.media_links WHERE entity_id=%s AND role LIKE 'wechat-group-image:%%'", (native_mid,))
                with redirect_stdout(io.StringIO()):
                    native.run()
                self.assertEqual(current_sha(), expected)
                receipt = native_ledger.execute('SELECT asset FROM decoded WHERE path=?', ('attach/a/2026/Img/' + filename + '_h.dat',)).fetchone()[0]
                self.assertEqual(json.loads(receipt)['sha256'], expected)
                self.assertEqual(db.execute('SELECT counts FROM crm.wechat_image_runs WHERE id=%s', (native.run_id,)).fetchone()[0]['total'], 1)
                moved_group = uuid.uuid4()
                db.execute("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type,connection_id) VALUES(%s,'wechat',%s,'corrected-native-group','group',%s)", (moved_group, str(connection), connection))
                db.execute("UPDATE crm.message_search SET conversation_id=%s,occurred_at='2026-01-03T00:00:00Z' WHERE message_id=%s", (moved_group, native_mid))
                db.execute('UPDATE crm.connection_evidence SET conversation_id=%s WHERE id=%s', (moved_group, native_mid))
                for _ in range(2):
                    with redirect_stdout(io.StringIO()):
                        native.run()
                    buckets = db.execute('SELECT conversation_id,day::text,counts FROM crm.wechat_image_daily WHERE run_id=%s', (native.run_id,)).fetchall()
                    self.assertEqual(len(buckets), 1)
                    self.assertEqual(buckets[0][:2], (moved_group, '2026-01-03'))
                    self.assertEqual(buckets[0][2]['total'], 1)
                    self.assertEqual(buckets[0][2]['available'], 1)
                    self.assertEqual(db.execute('SELECT counts FROM crm.wechat_image_runs WHERE id=%s', (native.run_id,)).fetchone()[0]['total'], 1)
                # Real lock wait: a projector changes only raw XML while the
                # visible body remains [图片]. The final read must see its commit.
                editor = psycopg.connect(db.info.dsn, autocommit=True)
                editor.execute('BEGIN')
                editor.execute('UPDATE crm.message_search SET body=body WHERE message_id=%s', (native_mid,))
                editor.execute("UPDATE crm.connection_evidence SET revision=4,content_hash='projected-a' WHERE id=%s", (native_mid,))
                editor.execute("UPDATE wechat.connection_record_heads SET revision=4,content_hash='projected-a' WHERE connection_id=%s", (connection,))
                waited, errors = [], []
                def release_projector():
                    try:
                        deadline = time.monotonic() + 5
                        while time.monotonic() < deadline:
                            event = admin.execute('SELECT wait_event_type FROM pg_stat_activity WHERE pid=%s', (db.info.backend_pid,)).fetchone()
                            if event and event[0] == 'Lock':
                                waited.append(True)
                                editor.execute('COMMIT')
                                return
                            time.sleep(0.01)
                        errors.append('worker did not reach a lock wait')
                        editor.execute('ROLLBACK')
                    except Exception as error:
                        errors.append(str(error))
                thread = threading.Thread(target=release_projector)
                thread.start()
                try:
                    native.item(native_mid)
                finally:
                    thread.join(timeout=6)
                    editor.close()
                self.assertFalse(thread.is_alive())
                self.assertEqual(errors, [])
                self.assertEqual(waited, [True])
                self.assertEqual(db.execute('SELECT state,media_id FROM crm.wechat_group_images WHERE run_id=%s AND message_id=%s', (native.run_id, native_mid)).fetchone(), ('source_changed', None))
                with redirect_stdout(io.StringIO()):
                    native.run()
                self.assertEqual(current_sha(), sha_a)
                # An interrupted decode can leave an old, unregistered CAS blob.
                # Orphan GC wins while this worker waits for its message lock.
                orphan_key = 'e' * 32
                orphan_path = folder / (orphan_key + '_h.dat')
                orphan_path.write_bytes(PNG + b'orphan-resume')
                orphan = worker.decode(source, media, decoder, str(orphan_path.relative_to(source)), {'PATH': os.environ['PATH']})
                orphan_blob = media / orphan['object_path']
                os.utime(orphan_blob, (time.time() - 25 * 3600,) * 2)
                native_ledger.execute('INSERT INTO decoded VALUES(?,?,?)', (str(orphan_path.relative_to(source)), worker.digest(orphan_path), json.dumps(orphan))); native_ledger.commit()
                db.execute("INSERT INTO wechat.connection_payloads(content_hash,payload) VALUES('orphan-ref',%s)", (Jsonb({'body': f'<msg><img md5="{orphan_key}"/></msg>'}),))
                db.execute("UPDATE crm.connection_evidence SET revision=5,content_hash='orphan-ref' WHERE id=%s", (native_mid,))
                db.execute("UPDATE wechat.connection_record_heads SET revision=5,content_hash='orphan-ref' WHERE connection_id=%s", (connection,))
                db.execute("UPDATE crm.wechat_group_images SET state='pending',quality='none',media_id=NULL WHERE run_id=%s AND message_id=%s", (native.run_id, native_mid))
                worker.catalog(source, native_ledger, native.guard)
                editor = psycopg.connect(db.info.dsn, autocommit=True)
                editor.execute('BEGIN')
                editor.execute('UPDATE crm.message_search SET body=body WHERE message_id=%s', (native_mid,))
                waited, errors = [], []
                def collect_while_waiting():
                    try:
                        deadline = time.monotonic() + 5
                        while time.monotonic() < deadline:
                            event = admin.execute('SELECT wait_event_type FROM pg_stat_activity WHERE pid=%s', (db.info.backend_pid,)).fetchone()
                            if event and event[0] == 'Lock':
                                waited.append(True)
                                editor.execute("SELECT pg_advisory_xact_lock(hashtextextended('media-gc:'||%s,0))", (orphan['sha256'],))
                                protected = editor.execute('SELECT EXISTS(SELECT 1 FROM crm.media_assets WHERE sha256=%s) OR EXISTS(SELECT 1 FROM crm.media_publications WHERE sha256=%s)', (orphan['sha256'], orphan['sha256'])).fetchone()[0]
                                if protected:
                                    raise AssertionError('fixture must be an unregistered orphan')
                                orphan_blob.unlink()
                                editor.execute('COMMIT')
                                return
                            time.sleep(0.01)
                        errors.append('worker did not reach GC lock boundary')
                        editor.execute('ROLLBACK')
                    except Exception as error:
                        errors.append(str(error))
                        editor.execute('ROLLBACK')
                thread = threading.Thread(target=collect_while_waiting)
                thread.start()
                try:
                    with self.assertRaises(FileNotFoundError):
                        native.item(native_mid)
                finally:
                    thread.join(timeout=6)
                    editor.close()
                self.assertFalse(thread.is_alive())
                self.assertEqual(errors, [])
                self.assertEqual(waited, [True])
                self.assertEqual(db.execute('SELECT state,media_id FROM crm.wechat_group_images WHERE run_id=%s AND message_id=%s', (native.run_id, native_mid)).fetchone(), ('pending', None))
                self.assertFalse(orphan_blob.exists())
                with redirect_stdout(io.StringIO()):
                    native.run()
                self.assertEqual(current_sha(), orphan['sha256'])
                self.assertTrue(orphan_blob.is_file())
                # A native projector may own c before it updates m. Publishing
                # this image must finish while that conversation lock is held.
                editor = psycopg.connect(db.info.dsn, autocommit=True)
                editor.execute('BEGIN')
                editor.execute("UPDATE crm.conversations SET source_display_name='Corrected group label' WHERE id=%s", (moved_group,))
                db.execute("SET lock_timeout='300ms'")
                try:
                    native.item(native_mid)
                    editor.execute("SET LOCAL lock_timeout='300ms'")
                    editor.execute('UPDATE crm.message_search SET body=body WHERE message_id=%s', (native_mid,))
                    editor.execute('COMMIT')
                finally:
                    db.execute("SET lock_timeout='0'")
                    editor.close()
                self.assertEqual(current_sha(), orphan['sha256'])
                native_ledger.close()
        finally:
            if db:
                db.close()
            admin.execute(sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(name)))
            admin.close()


if __name__ == '__main__':
    unittest.main()

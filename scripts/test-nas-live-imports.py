#!/usr/bin/env python3
"""Focused offline contract: durable social retry and live SQLite reconciliation."""
import base64
from contextlib import closing
from datetime import datetime, timezone
import hashlib
import hmac
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch
import uuid

import psycopg
from psycopg import sql

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('live_imports', ROOT/'scripts/nas-live-imports.py')
live = importlib.util.module_from_spec(spec)
spec.loader.exec_module(live)


class LiveImportContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.admin_url = os.environ.get('MONICA_LIVE_TEST_ADMIN')
        if cls.admin_url:
            if not psycopg.conninfo.conninfo_to_dict(cls.admin_url).get('host','').startswith('/'):
                raise ValueError('test administrator must use an isolated Unix socket')
            return
        cls.pg = tempfile.TemporaryDirectory(prefix='monica-live-pg-')
        cls.addClassCleanup(cls.pg.cleanup)
        root = Path(cls.pg.name).resolve()
        binary = Path(os.environ.get('MONICA_PG_BIN','/opt/homebrew/opt/postgresql@16/bin'))
        subprocess.run([str(binary/'initdb'),'-D',str(root/'db'),'-A','trust','--no-locale','-E','UTF8'],check=True,stdout=subprocess.DEVNULL)
        subprocess.run([str(binary/'pg_ctl'),'-D',str(root/'db'),'-l',str(root/'pg.log'),'-o',"-h '' -k "+str(root),'-w','start'],check=True,stdout=subprocess.DEVNULL)
        cls.addClassCleanup(subprocess.run,[str(binary/'pg_ctl'),'-D',str(root/'db'),'-m','fast','-w','stop'],check=True,stdout=subprocess.DEVNULL)
        cls.admin_url = 'dbname=postgres host='+str(root)

    def setUp(self):
        admin_url = self.admin_url
        self.admin = psycopg.connect(admin_url,autocommit=True)
        self.addCleanup(self.admin.close)
        name = 'monica_rust_live_test_'+uuid.uuid4().hex[:12]
        self.admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(name)))
        self.addCleanup(self.admin.execute,sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(name)))
        self.db = psycopg.connect(**dict(self.admin.info.get_parameters(),dbname=name),autocommit=True)
        self.addCleanup(self.db.close)
        for migration in sorted((ROOT/'migrations').glob('*.sql')):
            with self.db.transaction():
                self.db.execute(migration.read_text(),prepare=False)
        self.temp = tempfile.TemporaryDirectory(prefix='monica-live-test-')
        self.addCleanup(self.temp.cleanup)
        folder = Path(self.temp.name).resolve()
        self.args = types.SimpleNamespace(source=folder/'source',state=folder/'live-imports',media=folder/'media',chatlog=folder/'chatlog',wechat_self='synthetic-self',since='2026-11-13T00:00:00Z',url='http://monica-rs-server-1:17494')
        for path in (self.args.source,self.args.state,self.args.media,self.args.chatlog):
            path.mkdir()
        (folder/'.monica-rs-ssd-target').write_bytes(b'monica-rs-ssd-target-v1\n')
        self.consumer = live.Consumer(self.args,self.db)
        self.addCleanup(self.consumer.close)
        # Never relax the production identity guard; this test uses another DB.
        self.consumer.guard = lambda: None

    def test_social_ack_retry_cas_reuse_and_raw_bytes(self):
        folder = self.args.source/'raw/linkedin'
        (folder/'assets').mkdir(parents=True)
        image = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII=')
        (folder/'assets/photo.png').write_bytes(image)
        url = 'https://example.invalid/avatar'
        document = dict(profiles=[dict(provider_user_id='synthetic-peer',profile=dict(name='Synthetic',avatar_url=url,about='Original\0control and visible ␀'),captured_at=datetime.now(timezone.utc).isoformat(),posts=[])],local_assets=[dict(source_url=url,local_path='photo.png',content_hash=hashlib.sha256(image).hexdigest())])
        self.args.linkedin_self = 'urn:li:member:synthetic-self'
        self.args.linkedin_connection = uuid.uuid4()
        self.db.execute("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES(%s,'linkedin',%s,'Existing archive','archive')",(self.args.linkedin_connection,self.args.linkedin_self))
        document['conversations'] = [dict(conversation_urn='urn:li:conversation:direct',participants=['urn:li:member:peer'],messages=[dict(message_urn='message-1',sender_urn=self.args.linkedin_self,incoming=False,body='Outbound',delivered_at=datetime.now(timezone.utc).isoformat())]),dict(conversation_urn='urn:li:conversation:group',participants=['urn:li:member:peer','urn:li:member:other'],messages=[dict(message_urn='message-2',sender_urn='urn:li:member:peer',incoming=True,body='Inbound',delivered_at=datetime.now(timezone.utc).isoformat())])]
        original = json.dumps(document,indent=3).encode()+b'\n'
        (folder/'linkedin_enrichment.incoming.json').write_bytes(original)
        sent = []
        class Transport:
            fail = True
            def __init__(self,*args,**kwargs):
                pass
            def request(self,method,path,body,headers):
                self.body,self.headers = body,headers
            def getresponse(self):
                if self.fail:
                    raise ConnectionError('synthetic interrupted request')
                body,headers = self.body,self.headers
                expected = hmac.new(b'synthetic-hmac',headers['x-import-timestamp'].encode()+b'.'+headers['x-import-nonce'].encode()+b'.'+body,hashlib.sha256).hexdigest()
                assert headers['x-import-signature'] == expected
                sent.append(json.loads(body))
                return types.SimpleNamespace(status=201,read=lambda n:json.dumps(dict(id=str(uuid.uuid4()))).encode())
            def close(self):
                pass
        original_umask = os.umask(0o077)
        try:
            with patch.dict(os.environ,{'MONICA_IMPORT_HMAC_SECRET':'synthetic-hmac','MONICA_IMPORT_TOKEN_LINKEDIN':'synthetic-token'}),patch.object(live.http.client,'HTTPConnection',Transport):
                with self.assertRaises(ConnectionError):
                    self.consumer.social('linkedin')
                self.assertEqual(self.consumer.journal.execute('SELECT count(*) FROM delivered').fetchone()[0],0)
                Transport.fail = False
                self.consumer.social('linkedin')
                self.consumer.social('linkedin')
        finally:
            os.umask(original_umask)
        self.assertEqual(len(sent),3)
        self.assertEqual(sent[0]['records'][0]['external_id'],'synthetic-peer')
        self.assertEqual(sent[0]['records'][0]['content']['about'],'Original␀control and visible ␀')
        self.assertEqual(sent[0]['records'][0]['archive_normalization']['fields'],['/about'])
        self.assertEqual(self.db.execute('SELECT %s::jsonb',(json.dumps(sent[0]),)).fetchone()[0],sent[0])
        self.assertEqual(sent[0]['raw_objects'][0]['sha256'],hashlib.sha256(original).hexdigest())
        self.assertEqual((self.args.state/sent[0]['raw_objects'][0]['archive_relative_path']).read_bytes(),original)
        messages = [b for b in sent if b['stream']=='messages']
        self.assertTrue(all(b['connection_id']==str(self.args.linkedin_connection) for b in messages))
        self.assertEqual([b['records'][0]['conversation_type'] for b in messages],['direct','group'])
        self.assertEqual([b['records'][0]['content']['direction'] for b in messages],['sent','received'])
        self.assertTrue(all(p.get('id') and 'is_self' in p for b in messages for p in b['records'][0]['content']['participants']))
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_assets').fetchone()[0],1)
        copied = next((self.args.media/'sha256').rglob(hashlib.sha256(image).hexdigest()))
        self.assertEqual(copied.read_bytes(),image)
        for directory in [copied.parent,copied.parent.parent,copied.parent.parent.parent]:
            self.assertEqual(directory.stat().st_mode & 0o005,0o005)
        self.assertEqual(next((self.args.state/'raw-json').glob('*.json')).read_bytes(),original)
        orphan = self.args.source/'orphan.png'
        orphan.write_bytes(image+b'distinct')
        key = hashlib.sha256(orphan.read_bytes()).hexdigest()
        target = self.args.media/'sha256'/key[:2]/key[2:4]/key
        target.parent.mkdir(parents=True,exist_ok=True)
        target.write_bytes(orphan.read_bytes())
        os.utime(target,(1,1))
        original_digest = live.digest_file
        with psycopg.connect(**self.db.info.get_parameters(),autocommit=True) as gc:
            def interleaved_hash(path):
                result = original_digest(path)
                if path==target and gc.execute("SELECT pg_try_advisory_lock(hashtextextended('media-gc:'||%s,0))",(key,)).fetchone()[0]:
                    try:
                        if not gc.execute('SELECT 1 FROM crm.media_assets WHERE sha256=%s',(key,)).fetchone():
                            path.unlink()
                    finally:
                        gc.execute("SELECT pg_advisory_unlock(hashtextextended('media-gc:'||%s,0))",(key,))
                return result
            manifest = self.consumer.deferred_media(orphan,'source_account','synthetic-peer','avatar')
            with patch.object(live,'digest_file',interleaved_hash):
                self.consumer.materialize(manifest,'linkedin')
        self.assertTrue(target.is_file())
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_assets WHERE sha256=%s',(key,)).fetchone()[0],1)
        identity = self.db.execute('SELECT id FROM crm.media_assets WHERE sha256=%s',(key,)).fetchone()[0]
        self.db.execute("UPDATE crm.media_assets SET media_type='application/octet-stream',object_path=%s WHERE id=%s",(str(target),identity))
        synced = set()
        original_open = live.os.open
        def record_directory(path,flags,*args,**kwargs):
            if flags & os.O_DIRECTORY:
                synced.add(Path(path))
            return original_open(path,flags,*args,**kwargs)
        with patch.object(live.os,'open',record_directory):
            self.consumer.materialize(manifest,'linkedin')
        self.assertEqual(self.db.execute('SELECT id,media_type FROM crm.media_assets WHERE sha256=%s',(key,)).fetchone(),(identity,'image/png'))
        self.assertTrue({target.parent,target.parent.parent,target.parent.parent.parent,self.args.media}.issubset(synced))
        self.assertEqual(self.db.execute('SELECT object_path FROM crm.media_assets WHERE id=%s',(identity,)).fetchone()[0],str(target))
        self.db.execute('UPDATE crm.media_assets SET object_path=%s WHERE id=%s',('/outside/'+key,identity))
        with self.assertRaisesRegex(ValueError,'identity differs'):
            self.consumer.materialize(manifest,'linkedin')



    def test_only_selected_archive_documents_are_written(self):
        folder=self.args.source/'staging/instagram-current'
        folder.mkdir(parents=True)
        path=folder/'profile.json'
        raw=json.dumps(dict(node=dict(__typename='GraphUser',id='synthetic-old',username='old'))).encode()
        path.write_bytes(raw);os.utime(path,(1,1))
        received=[]
        with patch.object(self.consumer,'accept',side_effect=lambda batch:received.append(batch)or str(uuid.uuid4())):
            self.consumer.social('instagram')
            self.assertEqual(received,[])
            self.assertFalse((self.args.state/'raw-json').exists())
            # A capture on/after the explicit boundary becomes selected.
            os.utime(path,(self.consumer.cutoff+1,self.consumer.cutoff+1))
            self.consumer.social('instagram')
        self.assertEqual(len(received),1)
        self.assertEqual((self.args.state/received[0]['raw_objects'][0]['archive_relative_path']).read_bytes(),raw)
        self.assertEqual(path.read_bytes(),raw)

    def test_invalid_new_capture_is_archived_and_nested_ids_are_not_changed(self):
        folder=self.args.source/'raw/linkedin';folder.mkdir(parents=True)
        path=folder/'linkedin_enrichment.incoming.json'
        raw=json.dumps(dict(profiles=[dict(profile={'name':'Missing stable ID'})])).encode()
        path.write_bytes(raw)
        with self.assertRaises(ValueError):self.consumer.social('linkedin')
        self.assertEqual((self.args.state/'raw-json'/(hashlib.sha256(raw).hexdigest()+'.json')).read_bytes(),raw)
        for content in [dict(sender='peer\0id'),dict(participants=[dict(id='peer\0id',name='Text')]),dict(participants=['peer\0id'])]:
            original=json.dumps(content)
            with self.assertRaisesRegex(ValueError,'identity contains NUL'):live.postgres_content(content)
            self.assertEqual(json.dumps(content),original)
        self.assertEqual(live.postgres_content(dict(participants=[dict(id='peer-id',name='Display\0name')]))[0]['participants'][0],dict(id='peer-id',name='Display␀name'))

    def test_busy_sqlite_backup_times_out_without_changing_source(self):
        path=self.args.source/'busy.db'
        with closing(sqlite3.connect(path))as writer:
            writer.execute('CREATE TABLE messages(id)');writer.commit();writer.execute('BEGIN EXCLUSIVE')
            with closing(sqlite3.connect(path.as_uri()+'?mode=ro',uri=True,timeout=0.01))as source,closing(sqlite3.connect(self.args.state/'unfinished.db'))as target:
                reports=[]
                with self.assertRaises(sqlite3.OperationalError):live.backup_with_progress(source,target,lambda *row:reports.append(row),stall_seconds=0.02)
                self.assertFalse(source.in_transaction)
            writer.rollback();self.assertEqual(writer.execute('SELECT count(*) FROM messages').fetchone()[0],0)

    def test_backup_keeps_one_read_snapshot_while_wal_writer_commits(self):
        path=self.args.source/'wal-snapshot.db'
        with closing(sqlite3.connect(path))as writer:
            writer.execute('PRAGMA journal_mode=WAL');writer.execute('CREATE TABLE messages(id INTEGER PRIMARY KEY,body TEXT)')
            writer.executemany('INSERT INTO messages(body)VALUES(?)',[('x'*4000,)]*2500);writer.commit()
            inserted=[]
            def report(*_):
                if not inserted:
                    writer.execute("INSERT INTO messages(body)VALUES('arrived during copy')");writer.commit();inserted.append(True)
            with closing(sqlite3.connect(path.as_uri()+'?mode=ro',uri=True,timeout=.01))as source,closing(sqlite3.connect(self.args.state/'snapshot.db'))as target:
                live.backup_with_progress(source,target,report)
                self.assertTrue(inserted)
                self.assertEqual(target.execute('SELECT count(*) FROM messages').fetchone()[0],2500)
                self.assertFalse(source.in_transaction)
            self.assertEqual(writer.execute('SELECT count(*) FROM messages').fetchone()[0],2501)

    def test_database_disconnect_exits_for_a_fresh_supervised_connection(self):
        argv = ['nas-live-imports.py','--run','--once','--source',str(self.args.source),'--state',str(self.args.state),'--media',str(self.args.media),'--since',self.args.since]
        def disconnected(consumer,provider):
            self.admin.execute('SELECT pg_terminate_backend(%s)',(consumer.db.info.backend_pid,))
            consumer.db.execute('SELECT 1')
        with patch.object(sys,'argv',argv),patch.object(live.os,'geteuid',return_value=0),self.assertRaises(SystemExit):
            live.main()
        with patch.dict(os.environ,{'MONICA_DATABASE_URL':self.db.info.dsn}),patch.object(sys,'argv',argv),patch.object(live.signal,'signal'),patch.object(live.os,'geteuid',return_value=10001),patch.object(live.Consumer,'guard',lambda consumer:None):
            with patch.object(live.Consumer,'social',disconnected):
                self.assertEqual(live.main(),1)
            self.assertEqual(live.main(),0)

    def test_wechat_live_wal_backup_edit_replay_and_existing_identity(self):
        self.db.execute("INSERT INTO crm.legacy_migrations(id,manifest_hash,source_manifest,status) VALUES(%s,'synthetic','{}','database_ready')",(uuid.uuid4(),))
        self.db.execute("INSERT INTO crm.legacy_chatlog_snapshots(id,manifest,own_id,status) VALUES(%s,'{}','synthetic-self','converted')",(uuid.uuid4(),))
        root = self.args.chatlog
        (root/'contact').mkdir()
        (root/'message').mkdir()
        with closing(sqlite3.connect(root/'contact/contact.db')) as contact,contact:
            contact.execute('CREATE TABLE contact(username TEXT,remark TEXT,nick_name TEXT,local_type INTEGER)')
            contact.execute("INSERT INTO contact VALUES('synthetic-peer','Synthetic','Synthetic',0)")
        table = 'Msg_'+hashlib.md5(b'synthetic-peer').hexdigest()
        with closing(sqlite3.connect(root/'message/message_0.db')) as source:
            source.execute('PRAGMA journal_mode=WAL')
            source.execute('CREATE TABLE Name2Id(user_name TEXT)')
            source.execute("INSERT INTO Name2Id VALUES('synthetic-peer')")
            source.execute(f'CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,create_time INTEGER,local_type TEXT,real_sender_id INTEGER,message_content TEXT,compress_content BLOB)')
            source.execute(f"INSERT INTO {table} VALUES(1,7,1794780000,'text',1,'Original',NULL)")
            source.commit()
            self.consumer.wechat()
            first = self.db.execute("SELECT message_id,conversation_id FROM crm.message_search WHERE body='Original'").fetchone()
            self.assertIsNotNone(first)
            source.execute(f"UPDATE {table} SET message_content='Corrected' WHERE local_id=1")
            source.execute(f"INSERT INTO {table} VALUES(2,8,1794780001,'text',1,'Downtime backlog',NULL)")
            source.execute(f"INSERT INTO {table} VALUES(3,9,1794780002,'10000',1,'<sysmsg><revokemsg><newmsgid>7</newmsgid></revokemsg></sysmsg>',NULL)")
            source.commit()
            self.assertTrue(Path(str(root/'message/message_0.db')+'-wal').exists())
            self.consumer.wechat()
            self.consumer.wechat()
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.message_search').fetchone()[0],3)
        self.assertEqual(self.db.execute('SELECT body FROM crm.message_search WHERE message_id=%s',(first[0],)).fetchone()[0],'Corrected')
        self.assertTrue(self.db.execute('SELECT tombstone FROM crm.message_search WHERE message_id=%s',(first[0],)).fetchone()[0])
        self.assertEqual(self.db.execute('SELECT message_count FROM crm.conversations WHERE id=%s',(first[1],)).fetchone()[0],2)
        self.assertEqual(self.db.execute("SELECT sum(records),sum(processed) FROM crm.import_activity WHERE id LIKE 'nas-chatlog:%'").fetchone(),(5,5))
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.ai_jobs').fetchone()[0],0)


if __name__ == '__main__':
    unittest.main()

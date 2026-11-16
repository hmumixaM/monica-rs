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
    def setUp(self):
        admin_url = os.environ.get('MONICA_LIVE_TEST_ADMIN')
        if not admin_url:
            self.skipTest('explicit disposable PostgreSQL administrator required')
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
        self.args = types.SimpleNamespace(source=folder/'source',state=folder/'state',media=folder/'media',chatlog=folder/'chatlog',wechat_self='synthetic-self',since='2026-11-13T00:00:00Z',url='http://monica-rs-server-1:17494')
        for path in (self.args.source,self.args.state,self.args.media,self.args.chatlog):
            path.mkdir()
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
        document = dict(profiles=[dict(provider_user_id='synthetic-peer',profile=dict(name='Synthetic',avatar_url=url),captured_at=datetime.now(timezone.utc).isoformat(),posts=[])],local_assets=[dict(source_url=url,local_path='photo.png',content_hash=hashlib.sha256(image).hexdigest())])
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
        with patch.dict(os.environ,{'MONICA_IMPORT_HMAC_SECRET':'synthetic-hmac','MONICA_IMPORT_TOKEN_LINKEDIN':'synthetic-token'}),patch.object(live.http.client,'HTTPConnection',Transport):
            with self.assertRaises(ConnectionError):
                self.consumer.social('linkedin')
            self.assertEqual(self.consumer.journal.execute('SELECT count(*) FROM delivered').fetchone()[0],0)
            Transport.fail = False
            self.consumer.social('linkedin')
            self.consumer.social('linkedin')
        self.assertEqual(len(sent),1)
        self.assertEqual(sent[0]['records'][0]['external_id'],'synthetic-peer')
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_assets').fetchone()[0],1)
        copied = next((self.args.media/'sha256').rglob(hashlib.sha256(image).hexdigest()))
        self.assertEqual(copied.read_bytes(),image)
        self.assertEqual(next((self.args.state/'raw-json').glob('*.json')).read_bytes(),original)

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

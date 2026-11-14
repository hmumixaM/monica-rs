#!/usr/bin/env python3
"""Exercise data preservation, snapshot guards and recovery on synthetic PG copies."""
import base64
from contextlib import closing
import hashlib
import importlib.util
import json
import os
import shutil
import sqlite3
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
from unittest.mock import patch
import uuid

import psycopg
from psycopg import sql
from psycopg.types.json import Jsonb
from cryptography.fernet import Fernet

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location("legacy_import",ROOT/"scripts/legacy-import.py")
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
PG=Path(os.environ.get("MONICA_PG_BIN","/opt/homebrew/opt/postgresql@16/bin"))
SERVER=Path(os.environ.get("MONICA_TEST_SERVER",str(ROOT/"target/debug/monica-server")))
OLD="synthetic-old-installation-secret-at-least32"
NEW="synthetic-new-installation-secret-at-least32"
U=lambda value:str(uuid.uuid5(uuid.NAMESPACE_URL,"synthetic-legacy:"+value))

CRM_SCHEMA="""
 CREATE TABLE companies(id uuid PRIMARY KEY,name text,normalized_name text);
 CREATE TABLE schools(LIKE companies INCLUDING ALL);
 CREATE TABLE content_objects(id uuid PRIMARY KEY,content_kind text,sha256 text,byte_length bigint,payload_json jsonb);
 CREATE TABLE app_users(id uuid PRIMARY KEY,email text,password_hash text,totp_secret text);
 CREATE TABLE profiles(id uuid PRIMARY KEY,profile_type text,display_name text,summary text,avatar_media_id uuid,created_at timestamptz DEFAULT now(),archived_at timestamptz);
 CREATE TABLE tags(id uuid PRIMARY KEY,name text);
 CREATE TABLE source_accounts(id uuid PRIMARY KEY,provider text,external_id text,object_kind text,display_name text);
 CREATE TABLE source_account_links(account_id uuid PRIMARY KEY,profile_id uuid,linked_at timestamptz DEFAULT now());
 CREATE TABLE platform_link_decisions(source_account_id uuid,target_profile_id uuid,status text,score smallint,pair text,evidence jsonb,reviewed_by uuid,reviewed_at timestamptz DEFAULT now(),PRIMARY KEY(source_account_id,target_profile_id));
 CREATE TABLE profile_field_revisions(id uuid PRIMARY KEY,revision_seq bigint GENERATED ALWAYS AS IDENTITY,profile_id uuid,source_account_id uuid,field_key text,content_object_id uuid,content_hash text,source_type text,operation text,observed_at timestamptz DEFAULT now(),previous_revision_id uuid,is_conflict boolean DEFAULT false);
 CREATE TABLE profile_field_current(profile_id uuid,field_key text,current_revision_id uuid,source_account_id uuid,PRIMARY KEY(profile_id,field_key));
 CREATE TABLE source_account_fields(account_id uuid,field_key text,revision_id uuid,PRIMARY KEY(account_id,field_key));
 CREATE TABLE conversations(id uuid PRIMARY KEY,conversation_type text,profile_id uuid,external_id text,source_account_id uuid);
 CREATE TABLE message_events(id uuid,conversation_id uuid,sender_profile_id uuid,sender_account_id uuid,occurred_at timestamptz,message_type text,content_object_id uuid,raw_object_id uuid,source_record_id text,PRIMARY KEY(id,occurred_at));
 CREATE TABLE message_search(message_id uuid,occurred_at timestamptz,conversation_id uuid,body text,PRIMARY KEY(message_id,occurred_at));
"""
PROVIDER_SCHEMA="""
 CREATE TABLE accounts(id uuid PRIMARY KEY,external_id text,object_kind text,display_name text);
 CREATE TABLE payloads(sha256 text PRIMARY KEY,value jsonb,crm_content_kind text);
 CREATE TABLE objects(id uuid PRIMARY KEY,account_id uuid,object_kind text,external_id text,revision_number bigint,occurred_at timestamptz);
 CREATE TABLE revisions(object_id uuid,revision_number bigint,content_hash text,observed_at timestamptz,source_event text,PRIMARY KEY(object_id,revision_number));
 CREATE TABLE observations(object_id uuid,source_event text,content_hash text,observed_at timestamptz,PRIMARY KEY(object_id,source_event));
"""


class MigrationContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Register cleanup immediately: unittest skips tearDownClass if setup fails.
        cls.temp=tempfile.TemporaryDirectory(prefix="monica-legacy-",dir=os.environ.get("MONICA_TEST_TMP_ROOT","/tmp"))
        cls.addClassCleanup(cls.temp.cleanup)
        cls.folder=Path(cls.temp.name)
        subprocess.run([str(PG/"initdb"),"-D",str(cls.folder/"pg"),"-A","trust","--no-locale","-E","UTF8"],check=True,stdout=subprocess.DEVNULL)
        subprocess.run([str(PG/"pg_ctl"),"-D",str(cls.folder/"pg"),"-l",str(cls.folder/"pg.log"),"-o",f"-h '' -k {cls.folder} -c max_connections=30","-w","start"],check=True,stdout=subprocess.DEVNULL)
        cls.addClassCleanup(subprocess.run,[str(PG/"pg_ctl"),"-D",str(cls.folder/"pg"),"-m","immediate","-w","stop"],check=True,stdout=subprocess.DEVNULL)
        cls.admin=psycopg.connect(f"dbname=postgres host={cls.folder}",autocommit=True)
        cls.addClassCleanup(cls.admin.close)

    def setUp(self):
        self.suffix=uuid.uuid4().hex[:8];self.writers={};self.sources={}
        for store in m.STORES:
            name="legacy_"+store+"_"+self.suffix
            self.admin.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
            url=f"dbname={name} host={self.folder}"
            writer=psycopg.connect(url,autocommit=True)
            writer.execute(CRM_SCHEMA if store=="crm" else PROVIDER_SCHEMA)
            self.writers[store]=writer
            self.addCleanup(writer.close)
            os.environ[f"MONICA_LEGACY_{store.upper()}_URL"]=url
        os.environ.pop("MONICA_LEGACY_WECHAT_SELF_ID",None)
        os.environ["MONICA_LEGACY_SESSION_SECRET"]=OLD
        os.environ["MONICA_SESSION_SECRET"]=NEW
        os.environ["MONICA_LEGACY_FINALIZER"]=str(SERVER.resolve(strict=True))
        self.target_name="monica_rust_"+self.suffix
        self.admin.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(self.target_name)))
        url=f"postgresql:///{self.target_name}?host={self.folder}"
        subprocess.run([str(SERVER),"--database-url",url,"migrate"],check=True,stdout=subprocess.DEVNULL)
        self.target=psycopg.connect(url,autocommit=True)
        self.addCleanup(self.target.close)
        c=self.writers["crm"]
        cipher=Fernet(base64.urlsafe_b64encode(hashlib.sha256(OLD.encode()).digest()))
        c.execute("INSERT INTO app_users VALUES(%s,'synthetic@example.test','preserved-password-hash',%s)",(U("user"),cipher.encrypt(b"synthetic-totp-value").decode()))
        c.execute("INSERT INTO profiles(id,profile_type,display_name) VALUES(%s,'person','Synthetic Person')",(U("person"),))
        value={"text":"Original value", "number":123456789012345678901234567890}
        h=m.digest(value)
        c.execute("INSERT INTO content_objects VALUES(%s,'field_value',%s,100,%s)",(U("content"),h,Jsonb(value)))
        c.execute("INSERT INTO profile_field_revisions(id,profile_id,field_key,content_object_id,content_hash,source_type,operation) VALUES(%s,%s,'monica.note',%s,%s,'manual','manual_edit')",(U("revision"),U("person"),U("content"),h))
        c.execute("INSERT INTO profile_field_current VALUES(%s,'monica.note',%s,NULL)",(U("person"),U("revision")))
        c.execute("INSERT INTO source_accounts VALUES(%s,'wechat','synthetic-account','person','Synthetic account')",(U("account"),))
        c.execute("INSERT INTO platform_link_decisions VALUES(%s,%s,'accepted',90,'synthetic','{}',%s,now())",(U("account"),U("person"),U("user")))
        c.execute("INSERT INTO conversations VALUES(%s,'direct',%s,'direct:synthetic-account',%s)",(U("conversation"),U("person"),U("account")))
        c.execute("INSERT INTO message_events VALUES(%s,%s,%s,%s,'2020-01-01T00:00:00Z','text',%s,NULL,'server:7')",(U("message"),U("conversation"),U("person"),U("account"),U("content")))
        c.execute("INSERT INTO message_search VALUES(%s,'2020-01-01T00:00:00Z',%s,'Searchable original text')",(U("message"),U("conversation")))
        p=self.writers["wechat"]
        p.execute("INSERT INTO accounts VALUES(%s,'synthetic-account','person','Synthetic account')",(U("account"),))
        p.execute("INSERT INTO payloads VALUES(%s,NULL,'field_value')",(h,))
        changed={"text":"Changed value"};changed_hash=m.digest(changed)
        p.execute("INSERT INTO payloads VALUES(%s,%s,NULL)",(changed_hash,Jsonb(changed)))
        p.execute("INSERT INTO objects VALUES(%s,%s,'profile','synthetic-object',3,'2020-01-01Z')",(U("object"),U("account")))
        for version in (1,2,3):
            current_hash=changed_hash if version==2 else h
            p.execute("INSERT INTO revisions VALUES(%s,%s,%s,'2020-01-01T00:00:00Z',%s)",(U("object"),version,current_hash,"event:"+str(version)))
            p.execute("INSERT INTO observations VALUES(%s,%s,%s,'2020-01-01T00:00:00Z')",(U("object"),"event:"+str(version),current_hash))
        for store in m.STORES:
            self.sources[store]=m.connect_source(store)
            self.addCleanup(self.sources[store].close)

    def tearDown(self):
        for c in [*self.sources.values(),*self.writers.values(),self.target]:c.close()

    def test_preserves_data_versions_and_resumes_a_committed_page(self):
        with self.writers["crm"].transaction():
            self.writers["crm"].cursor().executemany("INSERT INTO tags VALUES(%s,%s)",[(U("tag:"+str(i)),"Synthetic "+str(i)) for i in range(501)])
        manifest=m.inspect_sources(self.sources)
        importer=m.Importer(self.sources,self.target,manifest)
        original=importer.insert
        batches=0
        def interrupt(table,rows,schema="crm"):
            nonlocal batches
            if table=="tags":
                batches+=1
                if batches==2:raise RuntimeError("synthetic interrupted second page")
            return original(table,rows,schema)
        importer.insert=interrupt
        with self.assertRaisesRegex(RuntimeError,"interrupted"):
            importer.apply()
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.tags").fetchone()[0],500)
        importer=m.Importer(self.sources,self.target,manifest);importer.apply()
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.tags").fetchone()[0],501)
        self.assertEqual(self.target.execute("SELECT password_hash FROM crm.app_users").fetchone()[0],"preserved-password-hash")
        self.assertEqual(self.target.execute("SELECT message_id,body,sender_profile_id,sender_account_id FROM crm.message_search").fetchone(),(uuid.UUID(U("message")),"Searchable original text",None,uuid.UUID(U("account"))))
        encrypted=self.target.execute("SELECT totp_secret FROM crm.app_users").fetchone()[0]
        cipher=Fernet(base64.urlsafe_b64encode(hashlib.sha256(NEW.encode()).digest()))
        self.assertEqual(cipher.decrypt(encrypted.encode()),b"synthetic-totp-value")
        self.assertEqual(self.target.execute("SELECT event_id,version FROM wechat.observations ORDER BY event_id").fetchall(),[("event:1",1),("event:2",2),("event:3",3)])
        self.assertEqual(m.inspect_sources(self.sources),manifest)
        subprocess.run([str(SERVER),"--database-url",f"postgresql:///{self.target_name}?host={self.folder}","legacy-finalize",str(importer.id)],check=True,stdout=subprocess.DEVNULL)
        self.assertEqual(self.target.execute("SELECT message_count FROM crm.conversations").fetchone()[0],1)
        self.assertEqual(self.target.execute("SELECT message_count FROM crm.conversation_summaries").fetchone()[0],1)
        self.assertEqual(self.target.execute("SELECT status FROM crm.platform_link_decisions").fetchone()[0],"confirmed")
        importer.verifying=True;importer.space_path=self.folder
        self.target.execute('SET default_transaction_read_only=on')
        try:
            with patch.object(m.shutil,'disk_usage',return_value=type('Disk',(),{'free':0})()):importer.apply()
        finally:self.target.execute('SET default_transaction_read_only=off')
        self.target.execute("UPDATE crm.field_revisions SET value='\"changed\"' WHERE id=%s",(U("revision"),))
        with self.assertRaisesRegex(RuntimeError,"content mismatch"):
            importer.apply()

    def test_rejects_unknown_tables_and_an_existing_business_target(self):
        self.writers["crm"].execute("CREATE TABLE unexpected_business(id integer PRIMARY KEY)")
        with self.assertRaisesRegex(RuntimeError,"unclassified"):
            m.inspect_sources(self.sources)
        self.writers["crm"].execute("DROP TABLE unexpected_business")
        manifest=m.inspect_sources(self.sources)
        self.target.execute("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES(%s,'person','Existing')",(U("existing"),))
        with self.assertRaisesRegex(RuntimeError,"must be empty"):
            m.Importer(self.sources,self.target,manifest).apply()
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.legacy_migrations").fetchone()[0],0)
        self.target.execute("CREATE TABLE public.companies(id uuid,name text,normalized_name text);CREATE TABLE public.schools(LIKE public.companies)")
        with psycopg.connect(self.target.info.dsn,autocommit=True,options="-c default_transaction_read_only=on") as same:
            with self.assertRaisesRegex(RuntimeError,"identical"):
                m.Importer({**self.sources,"crm":same},self.target,manifest).apply()

    def test_chatlog_copy_reconciles_messages_preserves_originals_and_verifies_replay(self):
        spec=importlib.util.spec_from_file_location("chatlog_import",ROOT/"scripts/legacy-chatlog-import.py")
        chatlog=importlib.util.module_from_spec(spec);spec.loader.exec_module(chatlog)
        os.environ["MONICA_LEGACY_WECHAT_SELF_ID"]="synthetic-self"
        self.target.execute("SET TIME ZONE 'America/Chicago'")
        self.writers["crm"].execute("UPDATE content_objects SET payload_json=%s",(Jsonb('Original value'),))
        self.writers["crm"].execute("UPDATE message_search SET body='Original value'")
        manifest=m.inspect_sources(self.sources);original=m.Importer(self.sources,self.target,manifest);original.apply()
        subprocess.run([str(SERVER),"--database-url",f"postgresql:///{self.target_name}?host={self.folder}","legacy-finalize",str(original.id)],check=True,stdout=subprocess.DEVNULL)
        snapshots=self.folder/("chatlog-"+self.suffix);(snapshots/"contact").mkdir(parents=True);(snapshots/"message").mkdir();(snapshots/"sns").mkdir()
        with closing(sqlite3.connect(snapshots/"contact/contact.db")) as db,db:
            db.execute("CREATE TABLE contact(username TEXT,remark TEXT,nick_name TEXT,local_type INTEGER)")
            db.execute("INSERT INTO contact VALUES('synthetic-account','Synthetic Contact','Synthetic',0)")
            db.execute("CREATE TABLE contact_label(id INTEGER PRIMARY KEY,value TEXT,original BLOB)")
            db.executemany("INSERT INTO contact_label VALUES(?,?,?)",[(i,'synthetic '+str(i),None) for i in range(1,206)])
            db.execute("UPDATE contact_label SET value=?,original=? WHERE id=1",('Text with NUL\0多',b'\xff\x00'))
            db.execute("UPDATE contact_label SET original=? WHERE id=2",(float('inf'),))
        table="Msg_"+hashlib.md5(b"synthetic-account").hexdigest()
        with closing(sqlite3.connect(snapshots/"message/message_0.db")) as db,db:
            db.execute("CREATE TABLE Name2Id(user_name TEXT)");db.execute("INSERT INTO Name2Id VALUES('synthetic-account')")
            db.execute(f"CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,create_time INTEGER,local_type TEXT,real_sender_id INTEGER,message_content TEXT,compress_content BLOB)")
            db.executemany(f"INSERT INTO {table} VALUES(?,?,?,'text',1,?,NULL)",[(1,7,1577836800,"Original value"),(2,8,1577836801,"Additional snapshot message")])
            from datetime import datetime
            fold=int(datetime.fromisoformat('2026-11-01T06:06:24+00:00').timestamp())
            db.execute(f"INSERT INTO {table} VALUES(60,120,?,'text',1,'DST fold message',NULL)",(fold,))
        with closing(sqlite3.connect(snapshots/"sns/sns.db")) as db,db:
            db.execute("CREATE TABLE SnsTimeLine(tid INTEGER PRIMARY KEY,user_name TEXT,content TEXT)")
            xml='<SnsObject><TimelineObject><id>synthetic-canonical-post</id><username>synthetic-account</username><createTime>1577836800</createTime><contentDesc>Original snapshot post</contentDesc></TimelineObject><LocalExtraInfo><comment_user_list><user_comment><username>synthetic-account</username><nickname>Synthetic author</nickname><comment_64id>99</comment_64id><content>Original comment</content><create_time>1577836801</create_time></user_comment></comment_user_list></LocalExtraInfo></SnsObject>'
            db.execute("INSERT INTO SnsTimeLine VALUES(12,'synthetic-account',?)",(xml,))
            db.execute("ALTER TABLE SnsTimeLine ADD COLUMN pack_info_buf TEXT")
            db.execute("UPDATE SnsTimeLine SET pack_info_buf=CAST(x'fffe' AS TEXT)")
        # The fixture writes only kilobytes. Hosted CI runners need not have
        # the 20 GiB reserve required for real personal-history migration.
        space_floor=mock.patch.object(chatlog,"FLOOR",1024**2)
        space_floor.start();self.addCleanup(space_floor.stop)
        def run(source,state,verify=False,reconcile=False):
            state.mkdir(exist_ok=True)
            current=chatlog.inspect(source,"synthetic-self")
            target=chatlog.Importer(self.target,current,verify,reconcile)
            if not verify:chatlog.bind_reader_target(self.target,state,"synthetic-self",target.id)
            scan=chatlog.reader.Reader(state,source,"synthetic-self",chatlog.atomic,lambda path:json.loads(Path(path).read_text()),own_id="synthetic-self",allow_unknown_source_sender=True,immutable_scan=verify or reconcile)
            frames=[]
            def emit(frame):
                target.page(frame);target.page(frame) if not verify else None
                frames.append(frame)
                chatlog.atomic(state/frame["ack_path"],{"page_sha256":frame["page_sha256"],"receipt_id":str(target.id)})
            try:
                while scan.step(emit):pass
                target.archive_tables(source,scan)
                target.finish(SERVER)
            finally:scan.close()
            return current,frames
        current,frames=run(snapshots,self.folder/("chatlog-state-"+self.suffix))
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.message_search").fetchone()[0],3)
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.timeline_events").fetchone()[0],1)
        self.assertEqual(self.target.execute("SELECT external_id FROM crm.timeline_events").fetchone()[0],'synthetic-canonical-post')
        self.assertEqual(self.target.execute("SELECT content->>'text' FROM crm.social_interactions").fetchone()[0],'Original comment')
        self.assertEqual(self.target.execute("SELECT sum(records) FROM crm.legacy_chatlog_pages").fetchone()[0],5)
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.legacy_chatlog_history WHERE table_name='contact_label'").fetchone()[0],205)
        archived=self.target.execute("SELECT payload FROM crm.legacy_chatlog_history WHERE table_name='contact_label' AND row_key=1").fetchone()[0]
        self.assertEqual(base64.b64decode(archived['value']['sqlite_text_utf8_base64']),b'Text with NUL\x00'+ '多'.encode())
        self.assertEqual(base64.b64decode(archived['original']['sqlite_blob_base64']),b'\xff\x00')
        self.assertEqual(self.target.execute("SELECT payload->'original'->>'sqlite_real_hex' FROM crm.legacy_chatlog_history WHERE table_name='contact_label' AND row_key=2").fetchone()[0],'inf')
        self.target.execute("UPDATE crm.legacy_chatlog_history SET payload='{}' WHERE table_name='contact_label' AND row_key=1")
        with self.assertRaisesRegex(ValueError,'historical content differs'):
            run(snapshots,self.folder/("chatlog-archive-tamper-"+self.suffix),verify=True)
        self.target.execute("UPDATE crm.legacy_chatlog_history SET payload=%s WHERE table_name='contact_label' AND row_key=1",(Jsonb(archived),))
        later=self.folder/("chatlog-later-"+self.suffix);shutil.copytree(snapshots,later)
        with closing(sqlite3.connect(later/"message/message_0.db")) as db,db:
            db.execute("INSERT INTO Name2Id VALUES('')")
            db.execute("INSERT INTO Name2Id VALUES('synthetic-other-sender')")
            db.execute(f"UPDATE {table} SET message_content='Corrected snapshot message' WHERE server_id=7")
            db.execute(f"UPDATE {table} SET real_sender_id=2 WHERE server_id=7")
            db.execute(f"UPDATE {table} SET real_sender_id=3 WHERE server_id=8")
            db.execute(f"INSERT INTO {table} VALUES(3,9,1577836802,'10000',1,?,NULL)",('<sysmsg type="revokemsg"><revokemsg><newmsgid>8</newmsgid><replacemsg>Synthetic revoke</replacemsg></revokemsg></sysmsg>',))
            db.execute(f"INSERT INTO {table} VALUES(4,10,1577836803,'10000',1,?,NULL)",('<sysmsg type="revokemsg"><revokemsg><content>Synthetic notice with no target</content><revoketime>1577836803</revoketime></revokemsg></sysmsg>',))
            db.execute(f"INSERT INTO {table} VALUES(5,11,1577836804,'43',0,'Video with source sender absent',NULL)")
            db.execute(f"INSERT INTO {table} VALUES(6,12,1577836802,'10000',1,?,NULL)",('<sysmsg><revokemsg><newmsgid>99</newmsgid></revokemsg></sysmsg>',))
        with closing(sqlite3.connect(later/"sns/sns.db")) as db,db:db.execute("UPDATE SnsTimeLine SET content=replace(replace(content,'Original snapshot post','Updated snapshot post'),'Original comment','Updated comment')")
        latest,frames=run(later,self.folder/("chatlog-later-state-"+self.suffix))
        self.assertEqual(self.target.execute("SELECT body FROM crm.message_search WHERE message_id=%s",(U("message"),)).fetchone()[0],"Corrected snapshot message")
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.message_search").fetchone()[0],7)
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.message_search WHERE tombstone").fetchone()[0],1)
        self.assertEqual(self.target.execute("SELECT sender FROM crm.message_search WHERE body='Additional snapshot message'").fetchone()[0],'synthetic-other-sender')
        self.assertEqual(self.target.execute("SELECT message_count,last_message_preview FROM crm.conversations").fetchone(),(6,'DST fold message'))
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.legacy_chatlog_issues WHERE status='source_missing'").fetchone()[0],4)
        self.assertEqual(self.target.execute("SELECT direction,sender_account_id FROM crm.message_search WHERE body='Video with source sender absent'").fetchone(),('unknown',None))
        self.assertEqual(self.target.execute("SELECT direction,sender_account_id FROM crm.message_search WHERE message_id=%s",(U('message'),)).fetchone(),('received',uuid.UUID(U('account'))))
        self.assertEqual(self.target.execute("SELECT content->>'text' FROM crm.social_interactions").fetchone()[0],'Updated comment')
        self.assertEqual(self.target.execute("SELECT original_row->>'body',revision FROM crm.legacy_chatlog_projection WHERE entity_type='message' AND entity_id=%s",(U("message"),)).fetchone(),("Original value",2))
        self.assertEqual(self.target.execute("SELECT c.payload_json FROM crm.legacy_message_evidence e JOIN crm.content_objects c ON c.id=e.content_object_id WHERE e.message_id=%s",(U("message"),)).fetchone()[0],"Original value")
        original.verifying=True;original.apply()
        # A successful old receipt must not prevent an explicit correction of
        # an older projection. Preserve counts and immutable original evidence.
        counts=self.target.execute('SELECT records,inserted FROM crm.legacy_chatlog_snapshots WHERE id=%s',(chatlog.Importer(self.target,latest).id,)).fetchone()
        # A different target, including a recreated database with the same
        # name, cannot inherit another database's already acknowledged cursors.
        other_name='monica_rust_other_'+self.suffix
        self.admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(other_name)))
        other_url=f'postgresql:///{other_name}?host={self.folder}'
        subprocess.run([str(SERVER),'--database-url',other_url,'migrate'],check=True,stdout=subprocess.DEVNULL)
        state_a=self.folder/('chatlog-later-state-'+self.suffix)
        chatlog.bind_state(self.target,state_a,chatlog.Importer(self.target,latest).id)
        state_b=self.folder/('other-state-'+self.suffix);state_b.mkdir()
        reconcile_only=self.folder/('old-reconcile-only-'+self.suffix);reconcile_only.mkdir()
        shutil.copytree(state_a/'raw',reconcile_only/'reconcile/raw')
        shutil.copyfile(state_a/'target.json',reconcile_only/'target.json')
        with psycopg.connect(other_url,autocommit=True) as other:
            with self.assertRaisesRegex(ValueError,'another target'):
                chatlog.bind_reader_target(other,reconcile_only,'synthetic-self',chatlog.Importer(self.target,latest).id)
            self.assertFalse((reconcile_only/'chatlog-target.json').exists())
            with self.assertRaisesRegex(ValueError,'another target'):
                chatlog.bind_state(other,state_a,chatlog.Importer(self.target,latest).id)
            legacy_state=self.folder/('unbound-state-'+self.suffix)
            shutil.copytree(state_a,legacy_state);(legacy_state/'target.json').unlink()
            with self.assertRaisesRegex(ValueError,'matching target receipts'):
                chatlog.bind_state(other,legacy_state,chatlog.Importer(self.target,latest).id)
            chatlog.bind_state(other,state_b,chatlog.Importer(self.target,latest).id)
        chatlog.bind_reader_target(self.target,reconcile_only,'synthetic-self',chatlog.Importer(self.target,latest).id)
        self.assertTrue((reconcile_only/'chatlog-target.json').is_file())
        self.admin.execute(sql.SQL('DROP DATABASE {}').format(sql.Identifier(other_name)))
        self.admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(other_name)))
        with psycopg.connect(other_url,autocommit=True) as other:
            with self.assertRaisesRegex(ValueError,'another target'):
                chatlog.bind_state(other,state_b,chatlog.Importer(self.target,latest).id)
        self.target.execute("UPDATE crm.message_search SET sender='synthetic-old-attribution' WHERE body='Additional snapshot message'")
        run(later,self.folder/('replay-old-receipt-'+self.suffix))
        self.assertEqual(self.target.execute("SELECT sender FROM crm.message_search WHERE body='Additional snapshot message'").fetchone()[0],'synthetic-old-attribution')
        run(later,self.folder/('reconcile-old-receipt-'+self.suffix),reconcile=True)
        self.assertEqual(self.target.execute("SELECT sender FROM crm.message_search WHERE body='Additional snapshot message'").fetchone()[0],'synthetic-other-sender')
        self.assertEqual(self.target.execute('SELECT records,inserted FROM crm.legacy_chatlog_snapshots WHERE id=%s',(chatlog.Importer(self.target,latest).id,)).fetchone(),counts)
        self.target.execute("SET default_transaction_read_only=on")
        try:run(later,self.folder/("chatlog-verify-state-"+self.suffix),verify=True)
        finally:self.target.execute("SET default_transaction_read_only=off")
        # A later authoritative sender-only correction must be rejected by
        # read-only verify, versioned by apply, and retain the original owner.
        corrected=self.folder/("chatlog-corrected-"+self.suffix);shutil.copytree(later,corrected)
        with closing(sqlite3.connect(corrected/"message/message_0.db")) as db,db:
            db.execute("INSERT INTO Name2Id VALUES('synthetic-corrected-sender')")
            db.execute(f"UPDATE {table} SET real_sender_id=4 WHERE server_id=7")
        corrected_manifest=chatlog.inspect(corrected,"synthetic-self")
        new_importer=chatlog.Importer(self.target,corrected_manifest)
        new_importer.verifying=True
        value={"thread_id":"synthetic-account","external_id":"message:synthetic-account:server:7","participants":[{"role":"sender","id":"synthetic-corrected-sender"}],"occurred_at":"2020-01-01T00:00:00+00:00","body":"Corrected snapshot message","message_type":"text","direction":"received"}
        # The new account must exist for read-only identity comparison.
        new_importer.verifying=False;new_importer.account('synthetic-corrected-sender');new_importer.verifying=True
        with self.assertRaisesRegex(ValueError,'attribution differs'):new_importer.messages([value])
        run(corrected,self.folder/("chatlog-corrected-state-"+self.suffix))
        self.assertEqual(self.target.execute("SELECT sender FROM crm.message_search WHERE message_id=%s",(U('message'),)).fetchone()[0],'synthetic-corrected-sender')
        self.assertEqual(self.target.execute("SELECT original_row->>'sender' FROM crm.legacy_chatlog_projection WHERE entity_type='message' AND entity_id=%s",(U('message'),)).fetchone()[0],'synthetic-account')
        self.target.execute("SET default_transaction_read_only=on")
        try:run(corrected,self.folder/("chatlog-corrected-verify-"+self.suffix),verify=True)
        finally:self.target.execute("SET default_transaction_read_only=off")
        # Verify also catches stale independent counters, not just body changes.
        self.target.execute("UPDATE crm.conversations SET message_count=message_count+1")
        with self.assertRaisesRegex(ValueError,'summaries are not settled'):
            run(corrected,self.folder/("chatlog-stale-summary-"+self.suffix),verify=True)
        new_importer.verifying=False;new_importer.finish(SERVER)
        # Target identities fence persisted ACKs, even for copies with the same
        # immutable migration manifest. Only local state is modified here.
        policy_path=self.folder/("chatlog-corrected-state-"+self.suffix)/"chatlog-target.json"
        policy=json.loads(policy_path.read_text());changed_policy={**policy,"target":["different-cluster",*policy['target'][1:]]}
        policy_path.write_text(json.dumps(changed_policy))
        with self.assertRaisesRegex(ValueError,'another target'):
            chatlog.bind_reader_target(self.target,policy_path.parent,'synthetic-self')
        policy_path.write_text(json.dumps(policy))
        # Reuse the Reader state against a later snapshot containing fewer
        # records. Missing local rows become evidence, never invented deletion.
        absent=self.folder/("chatlog-absent-"+self.suffix);shutil.copytree(corrected,absent)
        with closing(sqlite3.connect(absent/"contact/contact.db")) as db,db:db.execute("DELETE FROM contact")
        with closing(sqlite3.connect(absent/"sns/sns.db")) as db,db:db.execute("DELETE FROM SnsTimeLine")
        with closing(sqlite3.connect(absent/"message/message_0.db")) as db,db:db.execute(f"DELETE FROM {table} WHERE server_id=11")
        run(absent,self.folder/("chatlog-corrected-state-"+self.suffix))
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.legacy_chatlog_issues WHERE kind='snapshot_record_absent'").fetchone()[0],3)
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.message_search WHERE body='Video with source sender absent' AND NOT tombstone").fetchone()[0],1)
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.timeline_events").fetchone()[0],1)
        self.target.execute("SET default_transaction_read_only=on")
        try:run(absent,self.folder/("chatlog-absent-verify-"+self.suffix),verify=True)
        finally:self.target.execute("SET default_transaction_read_only=off")
        # A previous missing revoke target can arrive later without the
        # unchanged notice being emitted again by the persisted Reader.
        restored=self.folder/("chatlog-restored-"+self.suffix);shutil.copytree(absent,restored)
        with closing(sqlite3.connect(restored/"message/message_0.db")) as db,db:
            db.execute(f"INSERT INTO {table} VALUES(7,99,1577836799,'text',1,'Recovered withdrawn original',NULL)")
        run(restored,self.folder/("chatlog-corrected-state-"+self.suffix))
        self.assertTrue(self.target.execute("SELECT tombstone FROM crm.message_search WHERE body='Recovered withdrawn original'").fetchone()[0])
        self.assertEqual(self.target.execute("SELECT count(*) FROM crm.legacy_chatlog_revocations WHERE target_record_id='server:99' AND status<>'applied'").fetchone()[0],0)
        self.target.execute("SET default_transaction_read_only=on")
        try:run(restored,self.folder/("chatlog-restored-verify-"+self.suffix),verify=True)
        finally:self.target.execute("SET default_transaction_read_only=off")
        self.target.execute("UPDATE crm.message_search SET body='tampered copy' WHERE message_id=%s",(U("message"),))
        with self.assertRaisesRegex(ValueError,"differs"):
            run(restored,self.folder/("chatlog-tamper-state-"+self.suffix),verify=True)


if __name__=="__main__":
    unittest.main()

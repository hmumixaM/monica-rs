#!/usr/bin/env python3
"""Import an explicitly copied, standalone Chatlog snapshot into the Rust copy.

Never connects to a live Chatlog service, reads global decryption keys or starts
a collector. Uses the existing bounded Chatlog reader with atomic SQL receipts.
"""
import argparse
from contextlib import closing
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import uuid
from urllib.parse import quote, urlencode

import psycopg
from psycopg import sql
from psycopg.types.json import Jsonb

spec=importlib.util.spec_from_file_location("chatlog_reader",Path(__file__).with_name("collector-chatlog.py"))
reader=importlib.util.module_from_spec(spec);spec.loader.exec_module(reader)
schema_spec=importlib.util.spec_from_file_location('chatlog_schema',Path(__file__).with_name('legacy-chatlog-schema.py'))
schema=importlib.util.module_from_spec(schema_spec);schema_spec.loader.exec_module(schema)
NS=uuid.UUID("534b6268-97a1-4d99-a086-f8c9e3955815")
FLOOR=20*1024**3


def atomic(path,value):
    path=Path(path);path.parent.mkdir(parents=True,exist_ok=True)
    partial=path.with_suffix(".partial");partial.write_text(reader.encode(value));partial.replace(path)


def bind_state(db,state,snapshot):
    """Never let another database inherit completed scan cursors or ACKs."""
    system,name,oid=db.execute("SELECT system_identifier::text,current_database(),d.oid::text FROM pg_control_system() CROSS JOIN pg_database d WHERE d.datname=current_database()").fetchone()
    identity={'format':1,'system_identifier':system,'database':name,'database_oid':oid,'snapshot_id':str(snapshot)}
    marker=state/'target.json'
    if marker.is_symlink():raise ValueError('Chatlog target marker is a symlink')
    if marker.exists():
        if json.loads(marker.read_text())!=identity:raise ValueError('Chatlog state belongs to another target database or snapshot')
        return
    # Older versions did not write a target marker. Upgrade only if every ACK
    # is backed by an atomic page receipt in this actual database. Otherwise
    # require a new state directory, which safely replays the frozen source.
    existing=[*state.glob('raw/wechat/*/reader.sqlite3'),*(state/'reconcile').glob('raw/wechat/*/reader.sqlite3')]
    if len(existing)>16:raise ValueError('Chatlog state inventory exceeds its account budget')
    for saved in existing:
        receipts=[];count=0
        def confirm():
            found={r[0] for r in db.execute('SELECT digest FROM crm.legacy_chatlog_pages WHERE snapshot_id=%s AND digest=ANY(%s)',(snapshot,receipts))}
            if found!=set(receipts):raise ValueError('unbound Chatlog state has no matching target receipts; use a new state directory')
            receipts.clear()
        for ack in (saved.parent/'acks').glob('*.ack'):
            if ack.is_symlink() or ack.stat().st_size>4096:raise ValueError('invalid Chatlog state receipt')
            value=json.loads(ack.read_text())
            if value.get('page_sha256')!=ack.stem or value.get('receipt_id')!=str(snapshot):raise ValueError('Chatlog state receipt belongs to another snapshot')
            receipts.append(ack.stem);count+=1
            if len(receipts)==500:confirm()
        if receipts:confirm()
        if not count:raise ValueError('cannot establish the target of an old Chatlog state; use a new state directory')
    atomic(marker,identity);marker.chmod(0o600)


def bind_reader_target(db,state,own_id,snapshot=None):
    # ACK/known rows are receipts for one physical target, not transferable
    # evidence that another restored database has consumed these pages.
    identity=db.execute("SELECT system_identifier::text,current_database(),(SELECT oid::text FROM pg_database WHERE datname=current_database()) FROM pg_control_system()").fetchone()
    policy={"format":1,"target":[*identity,str(Path(db.info.host).resolve()),str(db.info.port)],"own_id":own_id}
    path=state/"chatlog-target.json"
    if path.is_symlink() or path.with_suffix('.partial').is_symlink():raise ValueError("Chatlog target policy is a symlink")
    if path.exists():
        if json.loads(path.read_text())!=policy:raise ValueError("Chatlog state belongs to another target; use a new state directory")
    elif (state/"raw").exists():
        if snapshot is None:raise ValueError("Existing Chatlog state has no target receipt; use a new state directory")
        bind_state(db,state,snapshot)
        atomic(path,policy);path.chmod(0o600)
    else:
        atomic(path,policy);path.chmod(0o600)

def inspect(root,own_id):
    root=root.resolve(strict=True)
    files=[]
    for path in sorted(root.rglob("*.db")):
        reader.safe_file(root,path)
        h=hashlib.sha256()
        with path.open("rb") as stream:
            for data in iter(lambda:stream.read(1024*1024),b""):h.update(data)
        with closing(reader.standalone(path)) as db:
            if [tuple(row) for row in db.execute("PRAGMA quick_check")] != [("ok",)]:raise ValueError("Chatlog snapshot integrity check failed")
            tables=[r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
            # Validate even empty databases and tables before accepting a source
            # manifest. A preserved .db file alone is not business coverage.
            schema.classify(path.relative_to(root).as_posix(),'sqlite_sequence')
            for table in tables:schema.classify(path.relative_to(root).as_posix(),table)
        files.append({"name":path.relative_to(root).as_posix(),"bytes":path.stat().st_size,"sha256":h.hexdigest(),"tables":tables})
    if not files or not (root/"contact/contact.db").is_file():raise ValueError("complete explicit Chatlog snapshot required")
    return {"format":1,"own_id":own_id,"files":files}


class Importer:
    def __init__(self,db,manifest,verifying=False,reconciling=False):
        if not db.info.dbname.startswith("monica_rust") or not db.info.host.startswith("/"):
            raise ValueError("dedicated offline Rust database on a local Unix socket required")
        if not db.execute("SELECT EXISTS(SELECT 1 FROM crm.legacy_migrations WHERE status='database_ready')").fetchone()[0]:
            raise ValueError("verified legacy database must be finalized before adding Chatlog snapshots")
        self.db=db;self.manifest=manifest;self.own=manifest["own_id"];self.verifying=verifying
        self.reconciling=reconciling
        self.id=uuid.uuid5(NS,"chatlog-snapshot:"+hashlib.sha256(reader.encode(manifest).encode()).hexdigest())
        self.accounts={r[0]:(r[1],r[2]) for r in db.execute("SELECT a.external_id,a.id,l.profile_id FROM crm.source_accounts a LEFT JOIN crm.source_account_links l ON l.account_id=a.id WHERE a.provider='wechat'")}
        self.conversations={}
        if not verifying:db.execute("INSERT INTO crm.legacy_chatlog_snapshots(id,manifest,own_id) VALUES(%s,%s,%s) ON CONFLICT DO NOTHING",(self.id,Jsonb(manifest),self.own))
        elif not db.execute("SELECT 1 FROM crm.legacy_chatlog_snapshots WHERE id=%s AND status IN ('converted','verified')",(self.id,)).fetchone():raise ValueError("Chatlog snapshot has not been fully converted")

    def recorded_source_gap(self,key):
        return self.db.execute("SELECT 1 FROM crm.legacy_chatlog_issues i JOIN crm.legacy_chatlog_snapshots s ON s.id=i.snapshot_id WHERE s.own_id=%s AND i.issue_key=%s AND i.status='source_missing' LIMIT 1",(self.own,key)).fetchone() is not None

    def snapshot_absence(self,value):
        # Reader tombstones describe disappearance from a later local snapshot,
        # not proof that the upstream user deleted the original. Keep historical
        # facts and archive this bounded observation as an explicit source gap.
        co,_=self.content("chatlog_snapshot_absence",value)
        key="snapshot-absence:"+hashlib.sha256(reader.encode(value).encode()).hexdigest()
        if self.verifying:
            if not self.recorded_source_gap(key):raise ValueError("Chatlog snapshot absence was not recorded")
        else:
            details={"observation":value,"content_object_id":str(co),"interpretation":"Snapshot absence is not proof of upstream deletion; historical facts retained"}
            self.db.execute("INSERT INTO crm.legacy_chatlog_issues(snapshot_id,issue_key,kind,status,details) VALUES(%s,%s,'snapshot_record_absent','source_missing',%s) ON CONFLICT DO NOTHING",(self.id,key,Jsonb(details)))

    def verify_conversations(self):
        if self.db.execute("SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_pending_records)").fetchone()[0]:raise ValueError("Chatlog deterministic Memory inputs are not settled")
        # Verify the independent read projections, including new conversations
        # and corrections that change the last message or its visibility.
        bad=self.db.execute("""SELECT EXISTS(
 SELECT 1 FROM crm.conversations c
 LEFT JOIN LATERAL(SELECT count(*) n,min(occurred_at) first_at,max(occurred_at) last_at FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone) t ON true
 LEFT JOIN LATERAL(SELECT message_id,external_id,left(body,80) preview FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1) m ON true
 LEFT JOIN crm.conversation_state s ON(s.provider,s.account,s.conversation)=(c.provider,c.account,c.external_id)
 LEFT JOIN crm.conversation_summaries v ON(v.provider,v.account,v.conversation)=(c.provider,c.account,c.external_id)
 WHERE c.provider='wechat' AND c.account=%s AND (
 (c.message_count,c.first_message_at,c.last_message_at,c.last_message_id,c.last_message_preview) IS DISTINCT FROM(t.n,t.first_at,t.last_at,m.message_id,m.preview)
 OR (s.message_count,s.last_at_ms,s.last_id,s.preview) IS DISTINCT FROM(t.n,coalesce((extract(epoch FROM t.last_at)*1000)::bigint,0),coalesce(m.external_id,''),coalesce(m.preview,''))
 OR (v.message_count,v.last_at_ms,v.last_id,v.preview) IS DISTINCT FROM(s.message_count,s.last_at_ms,s.last_id,s.preview)))""",(self.own,)).fetchone()[0]
        if bad:raise ValueError("Chatlog conversation summaries are not settled")
        for table,timezone in [("message_months","America/Chicago"),("message_months_utc","UTC")]:
            query=sql.SQL("""WITH expected AS(
 SELECT m.conversation_id,to_char(m.occurred_at AT TIME ZONE %s,'YYYY-MM') AS month,count(*) FILTER(WHERE m.sender=m.account) sent,count(*) FILTER(WHERE m.sender<>m.account) received,min(m.occurred_at) first_message_at,max(m.occurred_at) last_message_at
 FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.provider='wechat' AND c.account=%s AND NOT m.tombstone GROUP BY 1,2),actual AS(
 SELECT t.* FROM crm.{} t JOIN crm.conversations c ON c.id=t.conversation_id WHERE c.provider='wechat' AND c.account=%s)
 SELECT EXISTS((SELECT * FROM expected EXCEPT SELECT * FROM actual) UNION ALL(SELECT * FROM actual EXCEPT SELECT * FROM expected))""").format(sql.Identifier(table))
            if self.db.execute(query,(timezone,self.own,self.own)).fetchone()[0]:raise ValueError("Chatlog monthly summaries are not settled")

    def finish(self, server_binary):
        if self.verifying:
            self.verify_revocations()
            self.verify_conversations()
            return
        self.settle_revocations()
        self.db.execute("UPDATE crm.legacy_chatlog_snapshots SET status='settling' WHERE id=%s",(self.id,))
        migration=self.db.execute("SELECT id FROM crm.legacy_migrations WHERE status='database_ready'").fetchone()[0]
        parameters=psycopg.conninfo.conninfo_to_dict(self.db.info.dsn)
        connection={key:value for key,value in parameters.items() if key in {"host","port","user","password","sslmode","sslrootcert","sslcert","sslkey"}}
        database_url="postgresql:///"+quote(self.db.info.dbname,safe="")+"?"+urlencode(connection)
        environment=dict(os.environ,MONICA_DATABASE_URL=database_url)
        subprocess.run([str(server_binary),"legacy-finalize",str(migration)],env=environment,check=True)
        self.verify_revocations()
        self.verify_conversations()
        self.db.execute("UPDATE crm.legacy_chatlog_snapshots SET status='converted',completed_at=now() WHERE id=%s",(self.id,))

    def account(self,external,name=None):
        if external not in self.accounts:
            if self.verifying:raise ValueError("Chatlog account is missing from the Rust copy")
            aid=uuid.uuid5(NS,"chatlog-account:"+external)
            self.db.execute("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES(%s,'wechat',%s,%s,%s)",(aid,external,"group" if reader.is_group_identity(external) else "person",name or external))
            self.db.execute("INSERT INTO wechat.accounts(id,external_id,object_kind,display_name) VALUES(%s,%s,%s,%s) ON CONFLICT DO NOTHING",(aid,external,"group" if reader.is_group_identity(external) else "person",name or external))
            self.accounts[external]=(aid,None)
        return self.accounts[external]

    def archive_tables(self,root,scan):
        import base64
        import math
        def value(v):
            if isinstance(v,bytes):return {'sqlite_blob_base64':base64.b64encode(v).decode()}
            if isinstance(v,str) and '\0' in v:return {'sqlite_text_utf8_base64':base64.b64encode(v.encode('utf-8')).decode()}
            if isinstance(v,float) and not math.isfinite(v):return {'sqlite_real_hex':v.hex()}
            return v
        for file in self.manifest['files']:
            with closing(reader.standalone(root/file['name'])) as source:
                for table in file['tables']:
                    mode,reason=schema.classify(file['name'],table)
                    quoted='"'+table.replace('"','""')+'"'
                    expected=source.execute('SELECT count(*) FROM '+quoted).fetchone()[0]
                    key=(self.id,file['name'],table)
                    if not self.verifying:self.db.execute('INSERT INTO crm.legacy_chatlog_tables(snapshot_id,file_name,table_name,mode,expected,reason) VALUES(%s,%s,%s,%s,%s,%s) ON CONFLICT DO NOTHING',(*key,mode,expected,reason))
                    entry=self.db.execute('SELECT mode,expected,processed,after_row,status FROM crm.legacy_chatlog_tables WHERE snapshot_id=%s AND file_name=%s AND table_name=%s',key).fetchone()
                    if not entry or entry[:2]!=(mode,expected):raise ValueError('Chatlog table catalog differs from the frozen source')
                    if mode=='business':
                        known=scan.count_records(file['name'],table)
                        if known!=expected:raise ValueError('Chatlog business table is not fully reconciled')
                    if mode=='history':
                        columns=[r[1] for r in source.execute('PRAGMA table_info('+quoted+')')]
                        lengths='+'.join('coalesce(length(CAST("'+c.replace('"','""')+'" AS BLOB)),0)' for c in columns)
                        after=None;processed=0
                        if not self.verifying:after=entry[3];processed=entry[2]
                        while True:
                            metadata=source.execute('SELECT rowid,'+lengths+' FROM '+quoted+' WHERE (? IS NULL OR rowid>?) ORDER BY rowid LIMIT 200',(after,after)).fetchall()
                            if not metadata:break
                            selected=[];budget=0
                            for row in metadata:
                                if row[1]>reader.MAX_PAGE:raise ValueError('Chatlog historical row exceeds the bounded archive budget')
                                if budget+row[1]>reader.MAX_PAGE:break
                                budget+=row[1];selected.append(row[0])
                            rows=source.execute('SELECT rowid AS _migration_rowid,* FROM '+quoted+' WHERE rowid IN ('+','.join('?' for _ in selected)+') ORDER BY rowid',selected).fetchall()
                            payloads=[(r['_migration_rowid'],{k:value(r[k]) for k in r.keys() if k!='_migration_rowid'}) for r in rows]
                            with self.db.transaction():
                                if self.verifying:
                                    actual=dict(self.db.execute('SELECT row_key,payload FROM crm.legacy_chatlog_history WHERE snapshot_id=%s AND file_name=%s AND table_name=%s AND row_key=ANY(%s)',(*key,selected)))
                                    if any(actual.get(k)!=v for k,v in payloads):raise ValueError('Chatlog historical content differs')
                                else:
                                    for k,v in payloads:self.db.execute('INSERT INTO crm.legacy_chatlog_history VALUES(%s,%s,%s,%s,%s) ON CONFLICT DO NOTHING',(*key,k,Jsonb(v)))
                                processed+=len(rows);after=rows[-1]['_migration_rowid']
                                if not self.verifying:self.db.execute('UPDATE crm.legacy_chatlog_tables SET processed=%s,after_row=%s WHERE snapshot_id=%s AND file_name=%s AND table_name=%s',(processed,after,*key))
                            if shutil.disk_usage(root).free<FLOOR:raise ValueError('Mac free-space floor reached while archiving Chatlog history')
                        if processed!=expected or self.db.execute('SELECT count(*) FROM crm.legacy_chatlog_history WHERE snapshot_id=%s AND file_name=%s AND table_name=%s',key).fetchone()[0]!=expected:raise ValueError('Chatlog history table row count differs')
                    if self.verifying:
                        if entry[2]!=expected or entry[4]!='complete':raise ValueError('Chatlog table has not completed its declared disposition')
                    else:self.db.execute("UPDATE crm.legacy_chatlog_tables SET processed=expected,status='complete' WHERE snapshot_id=%s AND file_name=%s AND table_name=%s",key)
                    print(json.dumps({'phase':'chatlog_table_verified' if self.verifying else 'chatlog_table_committed','mode':mode,'rows':expected}),flush=True)

    def content(self,kind,value):
        encoded=reader.encode(value).encode();h=hashlib.sha256(encoded).hexdigest()
        cid=uuid.uuid5(NS,"chatlog-content:"+kind+":"+h)
        if not self.verifying:self.db.execute("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json) VALUES(%s,%s,%s,%s,%s) ON CONFLICT(content_kind,sha256) DO NOTHING",(cid,kind,h,len(encoded),Jsonb(value)))
        saved=self.db.execute("SELECT id,sha256,payload_json FROM crm.content_objects WHERE content_kind=%s AND sha256=%s",(kind,h)).fetchone()
        if not saved or saved[2]!=value:raise ValueError("Chatlog content checksum or body mismatch")
        return saved[:2]

    def queue(self,kind,ids):
        if self.verifying or not ids:return
        self.db.execute("""WITH ids AS(SELECT DISTINCT unnest(%s::uuid[]) entity_id),owners AS(
 SELECT entity_id,profile_id FROM crm.memory_legacy_owners WHERE kind=%s AND entity_id=ANY(%s::uuid[])
 UNION SELECT entity_id,profile_id FROM crm.memory_legacy_selected(%s,%s::uuid[]))
 INSERT INTO crm.memory_legacy_pending_records(kind,entity_id,profiles)
 SELECT %s,i.entity_id,coalesce(array_agg(o.profile_id ORDER BY o.profile_id) FILTER(WHERE o.profile_id IS NOT NULL),'{}'::uuid[])
 FROM ids i LEFT JOIN owners o USING(entity_id) GROUP BY i.entity_id
 ON CONFLICT(kind,entity_id) DO UPDATE SET profiles=ARRAY(SELECT DISTINCT unnest(crm.memory_legacy_pending_records.profiles||excluded.profiles)),requested_at=clock_timestamp()""",(ids,kind,ids,kind,ids,kind))

    def revoke_notice(self,value,mid,cid,co):
        import re
        import xml.etree.ElementTree as ET
        body=value.get("body","")
        if "<revokemsg>" not in body:return
        body=re.sub(r'^[^<\n]{1,100}:\n(?=<)','',body).strip()
        try:root=ET.fromstring(body)
        except ET.ParseError:raise ValueError("malformed Chatlog revoke notice")
        node=root.find('.//revokemsg') if root.tag!='revokemsg' else root
        if node is None:return
        target=node.findtext('newmsgid')
        if not target or target=='0':
            key='revoke-reference-absent:'+str(mid)
            if self.verifying:
                if not self.recorded_source_gap(key):raise ValueError("Chatlog notice source gap was not recorded")
            else:
                self.db.execute("INSERT INTO crm.legacy_chatlog_issues(snapshot_id,issue_key,kind,entity_id,status,details) VALUES(%s,%s,'revoke_notice_without_target',%s,'source_missing','{\"reason\":\"upstream notice contains no stable target server ID; original notice is preserved\"}') ON CONFLICT DO NOTHING",(self.id,key,mid))
            return
        if not target.isdecimal():raise ValueError("unsupported Chatlog revoke target identifier")
        if self.verifying:
            row=self.db.execute("SELECT status,target_record_id FROM crm.legacy_chatlog_revocations WHERE notice_message_id=%s AND conversation_id=%s AND target_record_id=%s AND status<>'pending' ORDER BY (snapshot_id=%s) DESC LIMIT 1",(mid,cid,'server:'+target,self.id)).fetchone()
            if not row or row[0]=='pending' or row[1]!='server:'+target:raise ValueError("Chatlog revoke notice is missing or unresolved")
            return
        self.db.execute("INSERT INTO crm.legacy_chatlog_revocations(notice_message_id,conversation_id,target_record_id,content_object_id,snapshot_id) VALUES(%s,%s,%s,%s,%s) ON CONFLICT DO NOTHING",(mid,cid,"server:"+target,co,self.id))

    def settle_revocations(self):
        after=(uuid.UUID(int=0),uuid.UUID(int=0))
        while True:
            rows=self.db.execute("SELECT r.notice_message_id,r.conversation_id,r.target_record_id,r.content_object_id,r.snapshot_id FROM crm.legacy_chatlog_revocations r JOIN crm.legacy_chatlog_snapshots s ON s.id=r.snapshot_id WHERE s.own_id=%s AND r.status IN ('pending','source_missing') AND (r.notice_message_id,r.snapshot_id)>(%s,%s) ORDER BY r.notice_message_id,r.snapshot_id LIMIT 100",(self.own,*after)).fetchall()
            if not rows:break
            with self.db.transaction():
                for notice,cid,sid,co,snapshot_id in rows:
                    targets=self.db.execute("SELECT m.message_id,to_jsonb(m),coalesce(v.content_object_id,e.content_object_id) FROM crm.legacy_message_evidence e JOIN crm.message_search m USING(message_id) LEFT JOIN crm.legacy_chatlog_projection p ON p.entity_type='message' AND p.entity_id=m.message_id LEFT JOIN crm.legacy_chatlog_versions v ON v.entity_type=p.entity_type AND v.entity_id=p.entity_id AND v.revision=p.revision WHERE e.source_record_id=%s AND m.conversation_id=%s",(sid,cid)).fetchall()
                    for mid,current,body_co in targets:
                        new={"body":current["body"],"message_type":current["message_type"],"direction":current["direction"],"tombstone":True}
                        if self.correct("message",mid,"message_search","message_id",current,new,body_co,raw_id=co):
                            self.queue("message",[mid])
                            self.db.execute("UPDATE crm.legacy_chatlog_snapshots SET changed=changed+1 WHERE id=%s",(self.id,))
                    status='applied' if targets else 'source_missing'
                    if targets:self.db.execute("UPDATE crm.legacy_chatlog_issues SET status='resolved' WHERE issue_key=%s AND kind='revoke_target_absent' AND snapshot_id IN(SELECT id FROM crm.legacy_chatlog_snapshots WHERE own_id=%s)",('revoke:'+str(notice),self.own))
                    if not targets:self.db.execute("INSERT INTO crm.legacy_chatlog_issues(snapshot_id,issue_key,kind,entity_id,status,details) VALUES(%s,%s,'revoke_target_absent',%s,'source_missing','{\"scope\":\"absent from the complete frozen CRM and Chatlog copies\"}') ON CONFLICT DO NOTHING",(self.id,'revoke:'+str(notice),notice))
                    self.db.execute("UPDATE crm.legacy_chatlog_revocations SET status=%s WHERE notice_message_id=%s AND snapshot_id=%s",(status,notice,snapshot_id))
            after=(rows[-1][0],rows[-1][4])
            print(json.dumps({"phase":"chatlog_revocations","processed":len(rows)}),flush=True)

    def verify_revocations(self):
        bad=self.db.execute("SELECT EXISTS(SELECT 1 FROM crm.legacy_chatlog_revocations r JOIN crm.legacy_chatlog_snapshots s ON s.id=r.snapshot_id WHERE s.own_id=%s AND (r.status='pending' OR (r.status='applied' AND EXISTS(SELECT 1 FROM crm.legacy_message_evidence e JOIN crm.message_search m USING(message_id) WHERE e.source_record_id=r.target_record_id AND m.conversation_id=r.conversation_id AND NOT m.tombstone)) OR (r.status='source_missing' AND EXISTS(SELECT 1 FROM crm.legacy_message_evidence e JOIN crm.message_search m USING(message_id) WHERE e.source_record_id=r.target_record_id AND m.conversation_id=r.conversation_id))))",(self.own,)).fetchone()[0]
        if bad:raise ValueError("Chatlog revoke projection is incomplete or differs")
        if self.db.execute("SELECT EXISTS(SELECT 1 FROM crm.legacy_chatlog_issues WHERE snapshot_id=%s AND status='pending')",(self.id,)).fetchone()[0]:raise ValueError("Chatlog migration has unresolved issues")



    def conversation(self,talker):
        if talker not in self.conversations:
            kind="group" if reader.is_group_identity(talker) else "direct"
            aid,pid=self.account(talker)
            found=self.db.execute("SELECT id FROM crm.conversations WHERE provider='wechat' AND account=%s AND external_id=%s",(self.own,kind+":"+talker)).fetchone()
            cid=found[0] if found else uuid.uuid5(NS,"chatlog-conversation:"+self.own+":"+talker)
            if not found:
                if self.verifying:raise ValueError("Chatlog conversation is missing from the Rust copy")
                self.db.execute("INSERT INTO crm.conversations(id,provider,account,external_id,source_account_id,profile_id,conversation_type) VALUES(%s,'wechat',%s,%s,%s,%s,%s)",(cid,self.own,kind+":"+talker,aid,pid,kind))
            self.conversations[talker]=cid
        return self.conversations[talker]

    def correct(self,kind,eid,table,key,current,new,content_id,initial_revision=1,force=False,raw_id=None):
        if not force and all(current[k]==v for k,v in new.items()):return False
        old=self.db.execute("SELECT original_row,current_row,revision FROM crm.legacy_chatlog_projection WHERE entity_type=%s AND entity_id=%s FOR UPDATE",(kind,eid)).fetchone()
        original={**({} if not old else old[0]),**{k:current[k] for k in new if not old or k not in old[0]}}
        revision=old[2]+1 if old else initial_revision+1
        new={**new,"version":revision} if kind=="post" else new
        projection={**({} if not old else old[1]),**new}
        self.db.execute("INSERT INTO crm.legacy_chatlog_projection VALUES(%s,%s,%s,%s,%s) ON CONFLICT(entity_type,entity_id) DO UPDATE SET original_row=excluded.original_row,current_row=excluded.current_row,revision=excluded.revision",(kind,eid,Jsonb(original),Jsonb(projection),revision))
        saved={**current,**new,**({"raw_object_id":str(raw_id)} if raw_id else {})}
        self.db.execute("INSERT INTO crm.legacy_chatlog_versions(entity_type,entity_id,revision,content_object_id,saved_row,snapshot_id) VALUES(%s,%s,%s,%s,%s,%s)",(kind,eid,revision,content_id,Jsonb(saved),self.id))
        self.db.execute(sql.SQL("UPDATE crm.{} SET {} WHERE {}=%s").format(sql.Identifier(table),sql.SQL(',').join(sql.SQL('{}=%s').format(sql.Identifier(k)) for k in new),sql.Identifier(key)),[*[Jsonb(v) if isinstance(v,(dict,list)) else v for v in new.values()],eid])
        return True

    def messages(self,records):
        from datetime import datetime, timezone
        inserted=changed=unchanged=0
        affected=[]
        cid=self.conversation(records[0]["thread_id"])
        # One indexed reconciliation lookup per bounded source page, not a
        # source-history scan for every individual message.
        source_ids=[]
        for value in records:
            sid=value["external_id"][len("message:"+value["thread_id"]+":"):]
            source_ids.append(sid.removeprefix("message/"))
        existing={}
        for mid,sid,at,row,payload,saved_co in self.db.execute("SELECT m.message_id,e.source_record_id,m.occurred_at,to_jsonb(m),c.payload_json,c.id FROM crm.legacy_message_evidence e JOIN crm.message_search m ON m.message_id=e.message_id LEFT JOIN crm.legacy_chatlog_projection p ON p.entity_type='message' AND p.entity_id=m.message_id LEFT JOIN crm.legacy_chatlog_versions v ON v.entity_type=p.entity_type AND v.entity_id=p.entity_id AND v.revision=p.revision JOIN crm.content_objects c ON c.id=coalesce(v.content_object_id,e.content_object_id) WHERE e.source_record_id=ANY(%s) AND m.conversation_id=%s",(source_ids,cid)):
            # ZoneInfo's inter-zone equality is deliberately false during a
            # DST fold, even for the same instant. Reconcile absolute UTC keys.
            existing[(sid,at.astimezone(timezone.utc))]=(mid,row,payload,saved_co)
        for value,sid in zip(records,source_ids):
            sender=next((p["id"] for p in value["participants"] if p["role"]=="sender"),None)
            if sender is None and value.get('source_gap')!='sender_reference_absent':raise ValueError('Chatlog sender is unresolved without source evidence')
            aid,pid=self.account(sender) if sender else (None,None)
            at=datetime.fromisoformat(value["occurred_at"]).astimezone(timezone.utc)
            found=existing.get((sid,at))
            body=value.get("body","")
            # Match the pre-existing CRM normalization for XML media messages.
            visible=search_text(body)
            new={"body":visible,"message_type":value["message_type"],"tombstone":False,"direction":value["direction"]}
            if found:
                mid,current,payload,saved_co=found
                # Legacy CRM accepts both a JSON string and structured content.
                # Normalize only the comparison view; the original object stays
                # byte-for-byte in the immutable content store.
                if not isinstance(payload,dict):payload={'text':payload if isinstance(payload,str) else reader.encode(payload)}
                if sender is None:new['direction']=current['direction']
                else:new.update(sender=sender,sender_account_id=str(aid))
                self.sender_gap(value,mid,bool(current['sender_account_id'] or current['sender_profile_id']))
                raw_equal=payload.get("text")==body and payload.get("binary_base64",payload.get("chatlog_original",{}).get("binary_base64"))==value.get("binary_base64")
                revoked=current["tombstone"] and self.db.execute("SELECT EXISTS(SELECT 1 FROM crm.legacy_chatlog_revocations WHERE conversation_id=%s AND target_record_id=%s AND status='applied')",(cid,sid)).fetchone()[0]
                if raw_equal and (all(current[k]==v for k,v in new.items()) or revoked and all(current[k]==v for k,v in new.items() if k!="tombstone")):
                    if value["message_type"]=="10000":self.revoke_notice(value,mid,cid,saved_co)
                    unchanged+=1;continue
                if self.verifying:raise ValueError("Chatlog message content or attribution differs")
                co,h=self.content("message",{"text":body,"sender_external_id":sender,"message_type":value["message_type"],"chatlog_original":value})
                if current["tombstone"]:new["tombstone"]=True
                if self.correct("message",mid,"message_search","message_id",current,new,co,force=not raw_equal):changed+=1;affected.append(mid)
                else:unchanged+=1
            else:
                if self.verifying:raise ValueError("Chatlog message is missing from the Rust copy")
                mid=uuid.uuid5(cid,sid+":"+at.isoformat())
                co,h=self.content("message",{"text":body,"sender_external_id":sender,"message_type":value["message_type"],"chatlog_original":value})
                self.db.execute("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_account_id,sender_profile_id,occurred_at,body,message_type,tombstone,direction) VALUES(%s,%s,'wechat',%s,%s,%s,%s,NULL,%s,%s,%s,false,%s)",(mid,cid,self.own,str(mid),sender or 'unknown',aid,at,visible,value["message_type"],value["direction"]))
                self.db.execute("INSERT INTO crm.legacy_message_evidence(message_id,source_record_id,content_object_id,original) VALUES(%s,%s,%s,%s)",(mid,sid,co,Jsonb(value["raw_message"])))
                self.sender_gap(value,mid,False)
                inserted+=1
                affected.append(mid)
            if value["message_type"]=="10000":self.revoke_notice(value,mid,cid,co)
        self.queue("message",affected)
        return inserted,unchanged,changed

    def sender_gap(self,value,mid,retained):
        if value.get('source_gap')!='sender_reference_absent':return
        key='sender-reference-absent:'+str(mid)
        if self.verifying:
            if not self.recorded_source_gap(key):raise ValueError('Chatlog sender source gap was not recorded')
        else:
            details={'reason':'source Name2Id reference is the empty or zero sentinel','retained_existing_attribution':retained,'source_sender_id':value['raw_message']['real_sender_id']}
            self.db.execute("INSERT INTO crm.legacy_chatlog_issues(snapshot_id,issue_key,kind,entity_id,status,details) VALUES(%s,%s,'sender_reference_absent',%s,'source_missing',%s) ON CONFLICT DO NOTHING",(self.id,key,mid,Jsonb(details)))

    def post(self,value):
        author=next(p["id"] for p in value["participants"] if p["role"]=="author")
        root,_=reader.moments.parse_moments_xml(value["raw_content"])
        obj=root if root.tag=='TimelineObject' else root.find('TimelineObject')
        if obj is None:raise ValueError("Chatlog Moments stable object is missing")
        # Legacy Monica keys Moments by the XML object ID. SQLite tid is a
        # separate local identity and remains in source_tid and the raw snapshot.
        external=obj.findtext('id') or str(value["source_tid"])
        aid,pid=self.account(author)
        found=self.db.execute("SELECT id,to_jsonb(t) FROM crm.timeline_events t WHERE source_account_id=%s AND event_type='post' AND external_id=%s",(aid,external)).fetchone()
        if found:self.interactions(found[0],value.get("interactions",[]))
        if found and found[1]["content"].get("raw_xml_sha256")==value.get("raw_xml_sha256"):return 0,1,0
        if self.verifying:raise ValueError("Chatlog Moments post is missing or differs")
        # Keep the rendered payload compatible with existing Moments posts.
        content={k:v for k,v in value.items() if k not in {"raw_content","external_id","kind","body","participants","coordinate_format"}}
        co,h=self.content("social_post",content)
        raw_co,_=self.content("chatlog_post_original",value)
        if found:
            eid,current=found
            changed=self.correct("post",eid,"timeline_events","id",current,{"content":content,"content_hash":h,"version":current["version"]},co,current["version"],raw_id=raw_co)
            if changed:self.queue("social",[eid])
            return 0,int(not changed),int(changed)
        eid=uuid.uuid5(aid,"chatlog-post:"+external);oid=uuid.uuid5(aid,"post:"+external)
        self.db.execute("INSERT INTO crm.timeline_events(id,provider,source_account_id,profile_id,object_id,external_id,event_type,occurred_at,content,content_hash,status,version) VALUES(%s,'wechat',%s,%s,%s,%s,'post',%s,%s,%s,'active',1)",(eid,aid,pid,oid,external,value["occurred_at"],Jsonb(content),h))
        self.db.execute("INSERT INTO crm.legacy_social_versions(id,event_id,content_object_id,content_hash,observed_at,original) VALUES(%s,%s,%s,%s,now(),%s)",(uuid.uuid5(eid,h),eid,co,h,Jsonb({**value,"raw_object_id":str(raw_co)})))
        self.interactions(eid,value.get("interactions",[]))
        self.queue("social",[eid])
        return 1,0,0

    def interactions(self,post,items):
        from datetime import datetime
        existing={(row[0],row[1]):(row[2],row[3]) for row in self.db.execute("SELECT external_id,interaction_type,id,to_jsonb(i) FROM crm.social_interactions i WHERE post_id=%s",(post,))}
        for item in items:
            who=item.get('author_external_id');aid,pid=self.account(who) if who else (None,None)
            data={"author_account_id":str(aid) if aid else None,"author_profile_id":str(pid) if pid else None,"author_external_id":who,"author_name":item.get('author_name'),"occurred_at":item.get('occurred_at'),"content":{"text":item.get('text') or ''},"is_deleted":bool(item.get('is_deleted'))}
            key=(item['external_id'],item['interaction_type']);old=existing.get(key)
            if old:
                iid,current=old
                if current['author_account_id'] is None and current['author_profile_id'] is not None:
                    data['author_account_id']=None;data['author_profile_id']=current['author_profile_id']
                same=True
                for field,value in data.items():
                    actual=current[field]
                    if field=='occurred_at' and value and actual:
                        same=same and datetime.fromisoformat(value)==datetime.fromisoformat(actual)
                    elif field=='content':same=same and (actual.get('text') or '')==value['text']
                    else:same=same and actual==value
                if same:continue
                if self.verifying:raise ValueError("Chatlog interaction differs from its snapshot")
                co,_=self.content('social_interaction',item)
                data['metadata']={**current['metadata'],**item}
                self.correct('interaction',iid,'social_interactions','id',current,data,co)
            else:
                if self.verifying:raise ValueError("Chatlog interaction is missing")
                iid=uuid.uuid5(post,item['interaction_type']+':'+item['external_id'])
                co,_=self.content('social_interaction',item)
                self.db.execute("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_account_id,author_profile_id,author_external_id,author_name,occurred_at,content,metadata,is_deleted) VALUES(%s,%s,'wechat',%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)",(iid,post,*key,aid,pid,who,item.get('author_name'),item.get('occurred_at'),Jsonb(data['content']),Jsonb(item),data['is_deleted']))

    def page(self,frame):
        digest=frame["page_sha256"]
        with self.db.transaction():
            seen=not self.verifying and bool(self.db.execute("SELECT 1 FROM crm.legacy_chatlog_pages WHERE snapshot_id=%s AND digest=%s",(self.id,digest)).fetchone())
            if seen and not self.reconciling:return
            records=[r["content"] for r in frame["batch"]["records"]]
            values=[]
            for value in records:
                if value.get("deleted") and not value.get("participants"):self.snapshot_absence(value)
                else:values.append(value)
            counts=[0,len(records)-len(values),0]
            if values and values[0]["kind"]=="message":counts=[a+b for a,b in zip(counts,self.messages(values))]
            else:
                for value in values:
                    if value["kind"]=="post":result=self.post(value)
                    elif value["kind"]=="contact":
                        person=value["participants"][0];aid,pid=self.account(person["id"],value["display_name"])
                        co,h=self.content("chatlog_contact",value)
                        exists=self.db.execute("SELECT 1 FROM crm.legacy_chatlog_versions WHERE entity_type='contact' AND entity_id=%s AND content_object_id=%s",(aid,co)).fetchone()
                        if self.verifying and not exists:raise ValueError("Chatlog contact version is missing")
                        if not exists:
                            self.db.execute("INSERT INTO crm.legacy_chatlog_versions(entity_type,entity_id,revision,content_object_id,saved_row,snapshot_id) SELECT 'contact',%s,coalesce(max(revision),0)+1,%s,%s,%s FROM crm.legacy_chatlog_versions WHERE entity_type='contact' AND entity_id=%s",(aid,co,Jsonb(value),self.id,aid))
                        result=(0,1,0)
                    else:raise ValueError("unmapped Chatlog record kind")
                    counts=[a+b for a,b in zip(counts,result)]
            if not self.verifying:
                self.db.execute("INSERT INTO crm.legacy_chatlog_pages VALUES(%s,%s,%s) ON CONFLICT DO NOTHING",(self.id,digest,len(records)))
                self.db.execute("UPDATE crm.legacy_chatlog_snapshots SET records=records+%s,inserted=inserted+%s,unchanged=unchanged+%s,changed=changed+%s WHERE id=%s",(0 if seen else len(records),counts[0],0 if seen else counts[1],counts[2],self.id))
        print(json.dumps({"phase":"chatlog_verify" if self.verifying else "chatlog_convert","records":len(records),"inserted":counts[0],"unchanged":counts[1],"changed":counts[2]}),flush=True)


def search_text(value):
    import re
    import xml.etree.ElementTree as ET
    candidate=re.sub(r'^[^<\n]{1,100}:\n(?=<)','',value).strip()
    if not candidate.startswith('<'):return value
    try:root=ET.fromstring(candidate)
    except ET.ParseError:return value
    labels={'title','des','description','content','text','displayname','nickname','pay_memo','memo','appname','recorddesc'}
    return '\n'.join(dict.fromkeys(n.text.strip() for n in root.iter() if n.tag.lower() in labels and n.text and n.text.strip()))


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("action",choices=["inspect","apply","reconcile","verify"])
    p.add_argument("--snapshots",type=Path,required=True)
    p.add_argument("--state",type=Path,required=True)
    p.add_argument("--manifest",type=Path,required=True)
    p.add_argument("--server-binary",type=Path,default=Path(os.environ.get("MONICA_SERVER_BINARY",os.environ.get("MONICA_LEGACY_FINALIZER") or shutil.which("monica-server") or str(Path(__file__).resolve().parents[1]/"target/debug/monica-server"))))
    a=p.parse_args();source=a.snapshots.resolve(strict=True);state=a.state.resolve()
    if source==state or source in state.parents or state in source.parents:p.error("snapshot and writable state roots must be disjoint")
    own=os.environ.get("MONICA_LEGACY_WECHAT_SELF_ID")
    if not own:p.error("explicit verified WeChat self identity required")
    state.mkdir(mode=0o700,parents=True,exist_ok=True)
    if shutil.disk_usage(state).free<FLOOR:raise ValueError("Mac must retain at least 20 GiB free")
    manifest=inspect(source,own)
    if a.action=="inspect":
        if a.manifest.exists():raise ValueError("snapshot manifest already exists")
        atomic(a.manifest,manifest);a.manifest.chmod(0o600)
        print(json.dumps({"phase":"chatlog_inspected","files":len(manifest["files"]),"bytes":sum(f["bytes"] for f in manifest["files"])}));return
    if json.loads(a.manifest.read_text())!=manifest:raise ValueError("Chatlog source snapshot changed")
    with psycopg.connect(os.environ["MONICA_DATABASE_URL"],autocommit=True) as db:
        verifying=a.action=="verify"
        if verifying:db.execute("SET default_transaction_read_only=on")
        if not db.execute("SELECT pg_try_advisory_lock(hashtextextended('monica-offline-chatlog',0))").fetchone()[0]:raise ValueError('another offline Chatlog operation is active on this target')
        reconciling=a.action=='reconcile'
        importer=Importer(db,manifest,verifying,reconciling)
        if not verifying:bind_reader_target(db,state,own,importer.id)
        import tempfile
        temporary=tempfile.TemporaryDirectory(prefix="verify-",dir=state) if verifying else None
        reader_state=Path(temporary.name) if temporary else state
        if reconciling:
            reader_state=state/'reconcile'
            completed=reader_state/'completed.json'
            if completed.exists():
                if json.loads(completed.read_text())!={'snapshot_id':str(importer.id)}:raise ValueError('reconcile state belongs to another snapshot')
                shutil.rmtree(reader_state)  # Only this operation's recomputable scan state, never source or business data.
            reader_state.mkdir(mode=0o700,exist_ok=True)
        scan=reader.Reader(reader_state,source,own,atomic,lambda path:json.loads(Path(path).read_text()),own_id=own,allow_unknown_source_sender=True,immutable_scan=verifying or reconciling)
        def emit(frame):
            if shutil.disk_usage(state).free<FLOOR:raise ValueError("Mac free-space floor reached")
            importer.page(frame)
            atomic(reader_state/frame["ack_path"],{"page_sha256":frame["page_sha256"],"receipt_id":str(importer.id)})
        try:
            while scan.step(emit):pass
            if scan.db.execute("SELECT count(*) FROM files WHERE done=0").fetchone()[0]:raise ValueError("Chatlog source scan incomplete")
            importer.archive_tables(source,scan)
            importer.finish(a.server_binary)
        finally:
            scan.close()
            if temporary:temporary.cleanup()
        if not verifying:db.execute("UPDATE crm.legacy_chatlog_snapshots SET status='converted',completed_at=now() WHERE id=%s",(importer.id,))
        if reconciling:atomic(reader_state/'completed.json',{'snapshot_id':str(importer.id)})
        print(json.dumps({"phase":"chatlog_verified" if verifying else "chatlog_converted","snapshot_id":str(importer.id)}))


if __name__=="__main__":main()

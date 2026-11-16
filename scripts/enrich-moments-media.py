"""One-time source-backed Moments media semantics repair.

Only type, shared title/link and cover annotations change. Existing media
references and captions stay authoritative. Raw source XML stays in memory.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import time
import uuid

import psycopg
from psycopg.types.json import Jsonb

spec=importlib.util.spec_from_file_location('moments_semantics',Path(__file__).with_name('chatlog_moments.py'))
moments=importlib.util.module_from_spec(spec);spec.loader.exec_module(moments)
NS=uuid.UUID('534b6268-97a1-4d99-a086-f8c9e3955815')


def enriched(current, obj):
    kind=obj.findtext('ContentObject/type')
    if kind not in('4','5','54'):return current
    result=json.loads(json.dumps(current))
    link=moments.shared_link(obj)
    if link:
        result['shared_link']={**link,**(result.get('shared_link')or{}),'kind':link['kind']}
    parsed=moments.moments_media(obj)
    for media in result.get('media',[]):
        if kind not in('5','54'):continue
        matches=[p for p in parsed if p.get('source_media_id')==media.get('source_media_id')]
        if len(matches)!=1:
            matches=[p for p in parsed if p.get('source_url')==media.get('source_url')and p.get('thumbnail_url')==media.get('thumbnail_url')]
        if len(matches)!=1:continue
        for field in ['source_kind','source_media_type']:
            if matches[0].get(field):media[field]=matches[0][field]
    return result


def apply(db, event_id, external, obj, raw_hash):
    with db.transaction():
        old=db.execute("SELECT to_jsonb(e)FROM crm.timeline_events e WHERE id=%s AND status='active'FOR UPDATE",(event_id,)).fetchone()
        if not old:return False
        old=old[0];assert old['external_id']==external and old['content'].get('username')==obj.findtext('username')
        content=enriched(old['content'],obj)
        if content==old['content']:return False
        encoded=json.dumps(content,sort_keys=True,ensure_ascii=False,separators=(',',':')).encode();digest=hashlib.sha256(encoded).hexdigest();co=uuid.uuid5(NS,'moments-semantics:'+digest)
        db.execute("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json)VALUES(%s,'social_post',%s,%s,%s)ON CONFLICT(content_kind,sha256)DO NOTHING",(co,digest,len(encoded),Jsonb(content)))
        co=db.execute("SELECT id FROM crm.content_objects WHERE content_kind='social_post'AND sha256=%s",(digest,)).fetchone()[0]
        prior=db.execute("SELECT original_row,current_row,revision FROM crm.legacy_chatlog_projection WHERE entity_type='post'AND entity_id=%s FOR UPDATE",(event_id,)).fetchone()
        revision=max(old['version'],prior[2]if prior else 0)+1
        original={**{k:old[k]for k in ['content','content_hash','version']},**(prior[0]if prior else{})}
        current={**(prior[1]if prior else{}),**dict(content=content,content_hash=digest,version=old['version'])}
        db.execute("""INSERT INTO crm.legacy_chatlog_projection VALUES('post',%s,%s,%s,%s)
 ON CONFLICT(entity_type,entity_id)DO UPDATE SET original_row=excluded.original_row,current_row=excluded.current_row,revision=excluded.revision""",(event_id,Jsonb(original),Jsonb(current),revision))
        proof=dict(reason='moments_media_semantics',source_xml_sha256=raw_hash,source_post_type=obj.findtext('ContentObject/type'),source_external_id=external)
        db.execute("INSERT INTO crm.legacy_social_versions(id,event_id,content_object_id,content_hash,observed_at,original)VALUES(%s,%s,%s,%s,now(),%s)ON CONFLICT DO NOTHING",(uuid.uuid5(uuid.UUID(str(event_id)),'moments-semantics:'+digest),event_id,co,digest,Jsonb(proof)))
        # Presentation annotations have their own history; they do not advance
        # the source object's version and suppress a later source revision.
        db.execute("UPDATE crm.timeline_events SET content=%s,content_hash=%s WHERE id=%s",(Jsonb(content),digest,event_id))
        db.execute("INSERT INTO crm.audit_events(action,entity_type,entity_id,metadata)VALUES('timeline.source_media_semantics_restored','social_post',%s,%s)",(event_id,Jsonb(proof)))
    return True


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--source',type=Path,required=True);parser.add_argument('--state',type=Path,required=True);parser.add_argument('--expected-system-id',required=True);args=parser.parse_args()
    assert args.source.resolve(strict=True)==args.source
    args.state.mkdir(parents=True,exist_ok=True);scanned=changed=missing=invalid=0;after='00000000-0000-0000-0000-000000000000'
    with psycopg.connect(os.environ['MONICA_DATABASE_URL'],autocommit=True)as db,sqlite3.connect('file:'+str(args.source)+'?mode=ro',uri=True)as source:
        target=db.execute("SELECT system_identifier::text,current_database(),(SELECT oid::text FROM pg_database WHERE datname=current_database()),(SELECT max(version)FROM _sqlx_migrations WHERE success)FROM pg_control_system()").fetchone()
        assert target==(args.expected_system_id,'monica_rust','16384',88)
        source.execute('PRAGMA query_only=ON');source.execute('BEGIN');db.execute("SET statement_timeout='30s'")
        while True:
            page=db.execute("SELECT id,external_id,content->>'source_tid',content->>'username'FROM crm.timeline_events WHERE provider='wechat'AND event_type='post'AND status='active'AND content ? 'raw_xml_sha256'AND external_id~'^[0-9]+$'AND id>%s ORDER BY id LIMIT 100",(after,)).fetchall()
            if not page:break
            for event_id,external,tid,author in page:
                scanned+=1;number=int(external);keys=[external,number-2**64 if number>=2**63 else number]
                if tid is not None:keys.append(int(tid))
                row=source.execute('SELECT content FROM SnsTimeLine WHERE tid IN('+','.join('?'for _ in keys)+')',keys).fetchone()
                if not row:missing+=1;continue
                raw=row[0]
                try:
                    root,_=moments.parse_moments_xml(raw);obj=root if root.tag=='TimelineObject'else root.find('TimelineObject')
                    if obj is None or obj.findtext('id')!=external or obj.findtext('username')!=author:invalid+=1;continue
                except Exception:invalid+=1;continue
                if obj.findtext('ContentObject/type')not in('4','5','54'):continue
                changed+=apply(db,event_id,external,obj,hashlib.sha256(raw.encode()if isinstance(raw,str)else raw).hexdigest())
            after=page[-1][0];(args.state/'progress.json').write_text(json.dumps(dict(scanned=scanned,changed=changed,source_lookup_unmatched=missing,invalid=invalid,at=time.time())))
        result=dict(complete=True,scanned=scanned,changed=changed,source_lookup_unmatched=missing,invalid=invalid,media_references_overwritten=0,files_deleted=0,at=time.time())
        (args.state/'result.json').write_text(json.dumps(result));print(json.dumps(result))


if __name__=='__main__':
    main()

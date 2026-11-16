"""One-time consolidation of source duplicates into verified Monica Next IDs.

Source observations, prior rows, media files and archived attachment links are
retained. The redundant projection becomes an archived identity after its current references move.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time
import uuid

import psycopg
from psycopg.types.json import Jsonb


def rows(db, table, column, identity):
    return [r[0] for r in db.execute(f"SELECT to_jsonb(t) FROM crm.{table} t WHERE {column}=%s", (identity,))]


def copy_new_roles(db, entity, source, target):
    db.execute("""INSERT INTO crm.media_links(entity_type,entity_id,media_id,role,source_account_id)
 SELECT l.entity_type,%s,l.media_id,l.role,l.source_account_id FROM crm.media_links l
 WHERE l.entity_type=%s AND l.entity_id=%s
 AND NOT EXISTS(SELECT 1 FROM crm.media_links k WHERE k.entity_type=l.entity_type AND k.entity_id=%s AND k.role=l.role)
 ON CONFLICT DO NOTHING""", (target,entity,source,target))
    db.execute("""INSERT INTO crm.media_slots(entity_type,entity_id,role,media_id)
 SELECT l.entity_type,l.entity_id,l.role,l.media_id FROM crm.media_links l
 WHERE l.entity_type=%s AND l.entity_id=%s
 AND NOT EXISTS(SELECT 1 FROM crm.media_links other WHERE other.entity_type=l.entity_type
  AND other.entity_id=l.entity_id AND other.role=l.role AND other.media_id<>l.media_id)
 ON CONFLICT DO NOTHING""", (entity,target))


def merge_pair(db, keep_id, duplicate_id):
    with db.transaction():
        alias=db.execute("SELECT event_id FROM crm.timeline_event_aliases WHERE alias_id=%s",(duplicate_id,)).fetchone()
        if alias:
            assert str(alias[0])==str(keep_id)
            return False
        locked=db.execute("SELECT id,to_jsonb(e) FROM crm.timeline_events e WHERE id=ANY(%s::uuid[]) ORDER BY id FOR UPDATE",([str(keep_id),str(duplicate_id)],)).fetchall()
        saved={str(r[0]):r[1]for r in locked};assert len(saved)==2
        keep,duplicate=saved[str(keep_id)],saved[str(duplicate_id)]
        assert keep['provider']==duplicate['provider']=='wechat' and keep['event_type']==duplicate['event_type']=='post'
        assert keep['external_id']==duplicate['external_id'] and keep['connection_id'] is None and duplicate['connection_id'] is None
        assert duplicate['source_account_id'] is not None and keep['source_account_id'] in(None,duplicate['source_account_id'])
        author=db.execute("SELECT external_id FROM crm.source_accounts WHERE id=%s AND provider='wechat'",(duplicate['source_account_id'],)).fetchone()
        assert author and keep['content'].get('username')==duplicate['content'].get('username')==author[0]
        preserved={
            'canonical_before':keep,
            'attachments':rows(db,'media_links','entity_id',duplicate_id),
            'slots':rows(db,'media_slots','entity_id',duplicate_id),
            'interactions':rows(db,'social_interactions','post_id',duplicate_id),
            'readings':rows(db,'media_readings','entity_id',duplicate_id),
            'source_versions':[{k:v for k,v in r.items()if k!='original'}for r in rows(db,'legacy_social_versions','event_id',duplicate_id)],
        }
        db.execute("INSERT INTO crm.timeline_event_aliases(alias_id,event_id,original_row,preserved_data)VALUES(%s,%s,%s,%s)",(duplicate_id,keep_id,Jsonb(duplicate),Jsonb(preserved)))
        copy_new_roles(db,'social_post',duplicate_id,keep_id)
        existing={(r[1]['external_id'],r[1]['interaction_type']):r for r in db.execute("SELECT id,to_jsonb(i)FROM crm.social_interactions i WHERE post_id=%s FOR UPDATE",(keep_id,))}
        for item in preserved['interactions']:
            target=existing.get((item['external_id'],item['interaction_type']))
            if target:
                copy_new_roles(db,'social_interaction',item['id'],target[0])
                db.execute("INSERT INTO crm.social_interaction_aliases(alias_id,interaction_id,original_row)VALUES(%s,%s,%s)",(item['id'],target[0],Jsonb(item)))
                db.execute("DELETE FROM crm.social_interactions WHERE id=%s",(item['id'],))
            else:
                db.execute("UPDATE crm.social_interactions SET post_id=%s WHERE id=%s",(keep_id,item['id']))
        # Immutable source revisions keep their original event addresses.
        # The alias relation makes them part of the surviving post's history.
        db.execute("""INSERT INTO crm.media_readings
 SELECT (jsonb_populate_record(NULL::crm.media_readings,to_jsonb(r)||jsonb_build_object('entity_id',%s::uuid))).*
 FROM crm.media_readings r WHERE r.entity_id=%s ON CONFLICT(entity_id,entity_type)DO NOTHING""",(keep_id,duplicate_id))
        db.execute("""INSERT INTO crm.ai_social_observed(event_id,first_observed_at,checked_at)
 SELECT %s,first_observed_at,checked_at FROM crm.ai_social_observed WHERE event_id=%s
 ON CONFLICT(event_id)DO UPDATE SET first_observed_at=least(crm.ai_social_observed.first_observed_at,excluded.first_observed_at)""",(keep_id,duplicate_id))
        # Rebuild just these two small counters after overlapping interactions
        # have been joined, rather than adding two copies of the same likes.
        db.execute("DELETE FROM crm.interaction_months WHERE post_id=ANY(%s::uuid[])",([str(keep_id),str(duplicate_id)],))
        db.execute("""INSERT INTO crm.interaction_months(profile_id,post_id,month,count)
 SELECT author_profile_id,post_id,to_char(occurred_at AT TIME ZONE 'America/Chicago','YYYY-MM'),count(*)
 FROM crm.social_interactions WHERE post_id=%s AND author_profile_id IS NOT NULL
 AND occurred_at IS NOT NULL AND NOT is_deleted AND interaction_type IN('like','comment','reply')
 GROUP BY 1,2,3""",(keep_id,))
        db.execute("""UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,
 error='Duplicate source post merged into Monica Next identity',completed_at=now()
 WHERE subject=%s AND kind IN('ai.post_media_one','ai.story_media_one')AND status IN('queued','running')""",(str(duplicate_id),))
        # The attachment links on the archived alias are kept as references.
        # They keep every real file retained even when Next's slot wins.
        db.execute("UPDATE crm.timeline_events SET status='merged',source_account_id=NULL,profile_id=NULL,object_id=id WHERE id=%s",(duplicate_id,))
        # New source observations can add comments while Next's caption and
        # attachment references remain authoritative.
        content=dict(keep['content'])
        for field in ['interactions','raw_xml_sha256','like_count','comment_count','interaction_count']:
            if field in duplicate['content']:
                content[field]=duplicate['content'][field]
        original={k:keep[k]for k in['source_account_id','object_id']}
        current={k:duplicate[k]for k in['source_account_id','object_id']}
        if content!=keep['content']:
            encoded=json.dumps(content,sort_keys=True,ensure_ascii=False,separators=(',',':')).encode();digest=hashlib.sha256(encoded).hexdigest()
            co=uuid.uuid5(uuid.UUID(str(keep_id)),'timeline-identity-content:'+digest)
            db.execute("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json)VALUES(%s,'social_post',%s,%s,%s)ON CONFLICT(content_kind,sha256)DO NOTHING",(co,digest,len(encoded),Jsonb(content)))
            co=db.execute("SELECT id FROM crm.content_objects WHERE content_kind='social_post'AND sha256=%s",(digest,)).fetchone()[0]
            db.execute("INSERT INTO crm.legacy_social_versions(id,event_id,content_object_id,content_hash,observed_at,original)VALUES(%s,%s,%s,%s,now(),%s)ON CONFLICT DO NOTHING",(uuid.uuid5(uuid.UUID(str(keep_id)),'timeline-identity-version:'+digest),keep_id,co,digest,Jsonb(dict(reason='source_identity_merge',source_event_id=str(duplicate_id)))))
            original.update({k:keep[k]for k in['content','content_hash','version']})
            current.update(content=content,content_hash=digest,version=max(keep['version'],duplicate['version']))
            db.execute("UPDATE crm.timeline_events SET content=%s,content_hash=%s,version=%s WHERE id=%s",(Jsonb(content),digest,current['version'],keep_id))
        db.execute("UPDATE crm.timeline_events SET source_account_id=%s,object_id=%s,last_seen_at=greatest(last_seen_at,%s::timestamptz)WHERE id=%s",(duplicate['source_account_id'],duplicate['object_id'],duplicate['last_seen_at'],keep_id))
        db.execute("""INSERT INTO crm.legacy_chatlog_projection VALUES('post',%s,%s,%s,%s)
 ON CONFLICT(entity_type,entity_id)DO UPDATE
 SET original_row=excluded.original_row||crm.legacy_chatlog_projection.original_row,
 current_row=crm.legacy_chatlog_projection.current_row||excluded.current_row,
 revision=greatest(crm.legacy_chatlog_projection.revision,excluded.revision)""",(keep_id,Jsonb(original),Jsonb(current),keep['version']))
        owners=list({v for v in[keep['profile_id'],duplicate['profile_id']]if v})
        db.execute("""INSERT INTO crm.memory_legacy_pending_records(kind,entity_id,profiles)
 SELECT 'social',unnest(%s::uuid[]),%s::uuid[]
 ON CONFLICT(kind,entity_id)DO UPDATE SET profiles=ARRAY(SELECT DISTINCT unnest(crm.memory_legacy_pending_records.profiles||excluded.profiles)),requested_at=clock_timestamp()""",([str(keep_id),str(duplicate_id)],owners))
        db.execute("INSERT INTO crm.ai_social_observed_pending(event_id)VALUES(%s)ON CONFLICT(event_id)DO UPDATE SET version=gen_random_uuid()",(keep_id,))
        db.execute("INSERT INTO crm.audit_events(action,entity_type,entity_id,metadata)VALUES('timeline.next_identity_reconciled','social_post',%s,%s)",(keep_id,Jsonb(dict(alias_id=str(duplicate_id),files_deleted=0,canonical_media_preferred=True,source_rows_preserved=True))))
        assert db.execute("SELECT count(*)FROM crm.timeline_events WHERE provider='wechat'AND event_type='post'AND source_account_id=%s AND external_id=%s",(duplicate['source_account_id'],duplicate['external_id'])).fetchone()[0]==1
    return True


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest',type=Path,required=True);parser.add_argument('--manifest-sha256',required=True)
    parser.add_argument('--state',type=Path,required=True);parser.add_argument('--expected-system-id',required=True)
    args=parser.parse_args()
    assert hashlib.sha256(args.manifest.read_bytes()).hexdigest()==args.manifest_sha256
    data=json.loads(args.manifest.read_text());assert data['source_database']=='snapshot_crm'
    pairs=data['pairs'];assert len({p['duplicate_id']for p in pairs})==len(pairs)
    args.state.mkdir(parents=True,exist_ok=True);complete=0
    with psycopg.connect(os.environ['MONICA_DATABASE_URL'],autocommit=True)as db:
        identity=db.execute("SELECT system_identifier::text,current_database(),(SELECT oid::text FROM pg_database WHERE datname=current_database()),(SELECT max(version)FROM _sqlx_migrations WHERE success)FROM pg_control_system()").fetchone()
        assert identity==(args.expected_system_id,'monica_rust','16384',88)
        db.execute("SET statement_timeout='30s'");db.execute("SET lock_timeout='5s'")
        for pair in pairs:
            merge_pair(db,pair['next_id'],pair['duplicate_id']);complete+=1
            if complete%25==0:(args.state/'progress.json').write_text(json.dumps(dict(completed=complete,total=len(pairs),at=time.time())))
    result=dict(complete=True,pairs=complete,files_deleted=0,source_observations_deleted=0,at=time.time())
    (args.state/'result.json').write_text(json.dumps(result));print(json.dumps(result))


if __name__=='__main__':
    main()

"""Archive incorrect image summaries generated with no image evidence.

Original posts, image OCR, media links, files and model-job inputs are retained.
Only incorrect derived current readings/outputs are removed after auditing them.
"""
import argparse,json,os
from pathlib import Path
import psycopg


def repair(db):
    with db.transaction():
        # Empty evidence in the actual source job proves that these are text
        # summaries, independently of the current post's attachment state.
        archived=db.execute("""INSERT INTO crm.audit_events(action,entity_type,entity_id,metadata)
SELECT 'ai.empty_image_evidence_corrected',r.entity_type,r.entity_id,
 jsonb_build_object('reason','post media job had no image evidence','reading',to_jsonb(r),'job',to_jsonb(j),
 'output',(SELECT to_jsonb(o)FROM crm.ai_outputs o WHERE o.kind='ai.post_media'AND o.subject=r.entity_id::text AND o.job_id=j.id))
FROM crm.media_readings r JOIN crm.ai_jobs j ON j.id=r.job_id
WHERE j.kind='ai.post_media'AND jsonb_array_length(coalesce(j.input->'evidence','[]'))=0""").rowcount
        removed=db.execute("DELETE FROM crm.media_readings r USING crm.ai_jobs j WHERE r.job_id=j.id AND j.kind='ai.post_media'AND jsonb_array_length(coalesce(j.input->'evidence','[]'))=0").rowcount
        assert removed==archived
        outputs=db.execute("DELETE FROM crm.ai_outputs o USING crm.ai_jobs j WHERE o.job_id=j.id AND j.kind='ai.post_media'AND jsonb_array_length(coalesce(j.input->'evidence','[]'))=0 AND EXISTS(SELECT 1 FROM crm.audit_events a WHERE a.action='ai.empty_image_evidence_corrected'AND a.entity_id::text=o.subject AND a.metadata->'job'->>'id'=j.id::text)").rowcount
        jobs=db.execute("UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,error='No image evidence: incorrect media summary cancelled' WHERE kind='ai.post_media'AND jsonb_array_length(coalesce(input->'evidence','[]'))=0 AND status<>'cancelled'").rowcount
        db.execute("DELETE FROM crm.ai_subject_heads h USING crm.ai_jobs j WHERE h.job_id=j.id AND j.kind='ai.post_media'AND jsonb_array_length(coalesce(j.input->'evidence','[]'))=0")
    return dict(complete=True,archived_incorrect_readings=archived,removed_current_readings=removed,removed_current_outputs=outputs,cancelled_empty_evidence_jobs=jobs,post_changes=0,media_link_changes=0,files_deleted=0)


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--expected-system-id',required=True);p.add_argument('--expected-schema',type=int,required=True);p.add_argument('--result',type=Path,required=True);a=p.parse_args()
    with psycopg.connect(os.environ['MONICA_DATABASE_URL'],autocommit=True)as db:
        actual=db.execute("SELECT system_identifier::text,current_database(),(SELECT max(version)FROM _sqlx_migrations WHERE success)FROM pg_control_system()").fetchone()
        assert actual==(a.expected_system_id,'monica_rust',a.expected_schema)
        result=repair(db)
    a.result.write_text(json.dumps(result));print(json.dumps(result))


if __name__=='__main__':main()

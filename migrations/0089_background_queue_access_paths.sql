-- Background queues read their next candidate through an index. Before this,
-- every poll scanned or sorted the whole backlog.

-- Post reading claims newest posts first; the date used to live only in the job JSON.
ALTER TABLE crm.ai_jobs ADD COLUMN priority_at timestamptz;
UPDATE crm.ai_jobs SET priority_at=(input#>>'{post,occurred_at}')::timestamptz
 WHERE kind='ai.post_media' AND status IN('queued','running');
CREATE INDEX ai_jobs_post_queue ON crm.ai_jobs(priority_at DESC NULLS LAST,created_at,id)
 WHERE kind='ai.post_media' AND status IN('queued','running');
CREATE INDEX ai_jobs_analysis_queue ON crm.ai_jobs(created_at,id)
 WHERE kind NOT IN('ai.post_media','ai.image_ocr') AND status IN('queued','running');
CREATE INDEX ai_jobs_image_queue ON crm.ai_jobs(created_at,id)
 WHERE kind='ai.image_ocr' AND status IN('queued','running');
CREATE INDEX ai_subject_heads_job ON crm.ai_subject_heads(job_id);
CREATE INDEX ai_subject_heads_subject ON crm.ai_subject_heads(subject text_pattern_ops);
CREATE INDEX image_ocr_readings_job ON crm.image_ocr_readings(job_id);

CREATE INDEX media_publications_pending ON crm.media_publications(updated_at) WHERE status='pending';
CREATE INDEX import_batches_due ON crm.import_batches(retry_after,received_at)
 WHERE status IN('received','running');
CREATE INDEX media_import_progress_open ON crm.media_import_progress(updated_at,batch_id)
 WHERE NOT complete;
CREATE INDEX objects_posts ON wechat.objects(id) WHERE object_kind='post';

-- An hourly sweep only tops up its backlog, so older queued sweeps of the same
-- kind repeat the newest one. New periods now wait for the live sweep.
UPDATE crm.ai_dispatches d SET status='complete',completed_at=now(),token=NULL,lease_until=NULL
 WHERE d.kind IN('ai.post_media','ai.image_ocr') AND d.status='queued'
 AND EXISTS(SELECT 1 FROM crm.ai_dispatches n WHERE n.kind=d.kind AND n.status IN('queued','running') AND n.period>d.period);

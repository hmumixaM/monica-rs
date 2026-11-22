-- Analysis about a person used to run in the order jobs were created, which
-- for a sweep is no order at all: of 3,700 people, the ones talked to this
-- week waited behind people never talked to. It now runs for the most recent
-- contact first. Mail and community jobs carry their creation time.
UPDATE crm.ai_jobs j SET priority_at=CASE WHEN j.kind IN('ai.profile','ai.reconnect_one')
  THEN (SELECT max(c.last_message_at) FROM crm.conversations c
        WHERE c.profile_id=split_part(j.subject,':',1)::uuid AND c.conversation_type='direct')
  ELSE j.created_at END
 WHERE j.kind NOT IN('ai.post_media','ai.image_ocr') AND j.status IN('queued','running');
DROP INDEX crm.ai_jobs_analysis_queue;
CREATE INDEX ai_jobs_analysis_queue ON crm.ai_jobs(priority_at DESC NULLS LAST,created_at,id)
 WHERE kind NOT IN('ai.post_media','ai.image_ocr') AND status IN('queued','running');

-- A reason to get back in touch was requested daily for every person, but the
-- list shows only people last talked to more than 90 days ago. The daily
-- sweep now asks for those; work queued for everyone else is dropped.
UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,error='not on the reconnect list',completed_at=now()
 WHERE kind='ai.reconnect_one' AND status IN('queued','running');

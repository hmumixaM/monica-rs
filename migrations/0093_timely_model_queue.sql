-- The weekly review and weekly notes are a handful of jobs that matter this
-- week. They used to wait in creation order behind the profile and reconnect
-- sweeps, which can hold thousands of jobs for days.
CREATE INDEX ai_jobs_timely_queue ON crm.ai_jobs(created_at,id)
 WHERE kind IN('ai.digest','ai.person_week') AND status IN('queued','running');

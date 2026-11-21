-- Profile, reconnect and community sweeps enqueue the same subjects whatever
-- their period, so an older queued sweep only repeats the newest one.
UPDATE crm.ai_dispatches d SET status='complete',completed_at=now(),token=NULL,lease_until=NULL
 WHERE d.kind IN('ai.profiles','ai.reconnect','ai.community_names') AND d.status='queued'
 AND EXISTS(SELECT 1 FROM crm.ai_dispatches n WHERE n.kind=d.kind AND n.status IN('queued','running') AND n.period>d.period);

ALTER TABLE crm.import_activity ADD COLUMN phase text;
ALTER TABLE crm.import_activity ADD COLUMN heartbeat_at timestamptz;
ALTER TABLE crm.import_activity ADD COLUMN next_run_at timestamptz;
CREATE INDEX import_activity_pending ON crm.import_activity(connection_id) WHERE kind IN ('page','batch') AND state='saved';
CREATE TABLE crm.import_activity_totals (
 provider text NOT NULL,account text NOT NULL,records bigint NOT NULL,processed bigint NOT NULL,
 first_recorded_at timestamptz NOT NULL,last_progress_at timestamptz NOT NULL, PRIMARY KEY(provider,account)
);
INSERT INTO crm.import_activity_totals SELECT provider,coalesce(connection_id::text,'legacy'),sum(records),sum(processed),min(received_at),max(updated_at) FROM crm.import_activity GROUP BY provider,coalesce(connection_id::text,'legacy');
CREATE FUNCTION crm.import_activity_total() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO crm.import_activity_totals VALUES(NEW.provider,coalesce(NEW.connection_id::text,'legacy'),NEW.records-CASE WHEN TG_OP='UPDATE' THEN OLD.records ELSE 0 END,NEW.processed-CASE WHEN TG_OP='UPDATE' THEN OLD.processed ELSE 0 END,NEW.received_at,NEW.updated_at)
 ON CONFLICT(provider,account) DO UPDATE SET records=crm.import_activity_totals.records+excluded.records,processed=crm.import_activity_totals.processed+excluded.processed,first_recorded_at=least(crm.import_activity_totals.first_recorded_at,excluded.first_recorded_at),last_progress_at=greatest(crm.import_activity_totals.last_progress_at,excluded.last_progress_at);
 RETURN NULL;
END $$;
CREATE TRIGGER activity_total AFTER INSERT OR UPDATE ON crm.import_activity FOR EACH ROW EXECUTE FUNCTION crm.import_activity_total();

CREATE INDEX import_activity_collection ON crm.import_activity(provider,received_at DESC,id DESC) WHERE kind='collection';
CREATE FUNCTION crm.track_connection_run() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE run_token uuid; run_state text;
BEGIN
 run_token:=coalesce(NEW.lease_token,OLD.lease_token);
 IF run_token IS NULL THEN RETURN NULL; END IF;
 run_state:=CASE WHEN NEW.lease_token IS NOT NULL THEN 'collecting' WHEN NOT NEW.enabled THEN 'paused' WHEN NEW.error IS NOT NULL THEN 'failed' ELSE 'ready' END;
 INSERT INTO crm.import_activity(id,provider,connection_id,stream,kind,state,phase,received_at,heartbeat_at,completed_at,next_run_at,failures)
 VALUES('connection:'||NEW.id||':'||run_token,NEW.provider,NEW.id,'sync','collection',run_state,'sync',now(),now(),CASE WHEN NEW.lease_token IS NULL THEN now() END,NEW.next_sync_at,CASE WHEN run_state='failed' THEN 1 ELSE 0 END)
 ON CONFLICT(id) DO UPDATE SET state=excluded.state,updated_at=now(),heartbeat_at=now(),completed_at=excluded.completed_at,next_run_at=excluded.next_run_at,failures=greatest(crm.import_activity.failures,excluded.failures);
 IF NEW.lease_token IS DISTINCT FROM OLD.lease_token THEN
 INSERT INTO crm.import_run_events(run_id,provider,connection_id,state,phase) VALUES('connection:'||NEW.id||':'||run_token,NEW.provider,NEW.id,run_state,'sync');
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER connection_run AFTER UPDATE OF lease_token,lease_until ON crm.source_connections FOR EACH ROW EXECUTE FUNCTION crm.track_connection_run();

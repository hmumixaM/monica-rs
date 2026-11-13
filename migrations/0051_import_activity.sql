-- Compact receipt counters survive detail retention and are never recomputed
-- from full source/message history. One write per committed page/batch.
CREATE TABLE crm.import_activity (
 id text PRIMARY KEY, provider text NOT NULL, connection_id uuid,
 stream text NOT NULL, kind text NOT NULL, batch_id text,
 state text NOT NULL, received_at timestamptz NOT NULL,
 updated_at timestamptz NOT NULL DEFAULT now(), completed_at timestamptz,
 records bigint NOT NULL DEFAULT 0, processed bigint NOT NULL DEFAULT 0,
 inserted bigint NOT NULL DEFAULT 0, changed bigint NOT NULL DEFAULT 0,
 unchanged bigint NOT NULL DEFAULT 0, failures bigint NOT NULL DEFAULT 0
);
CREATE INDEX import_activity_recent ON crm.import_activity(received_at DESC,id DESC);
CREATE INDEX import_activity_connection ON crm.import_activity(connection_id,received_at DESC,id DESC);
CREATE INDEX import_activity_provider ON crm.import_activity(provider,received_at DESC,id DESC);
CREATE TABLE crm.import_activity_daily (
 bucket timestamptz NOT NULL,provider text NOT NULL,account text NOT NULL,stream text NOT NULL,
 records bigint NOT NULL,processed bigint NOT NULL,inserted bigint NOT NULL,
 changed bigint NOT NULL,unchanged bigint NOT NULL,failures bigint NOT NULL,
 PRIMARY KEY(bucket,provider,account,stream)
);
CREATE TABLE crm.import_run_events (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 run_id text NOT NULL,provider text NOT NULL,connection_id uuid,
 state text NOT NULL,phase text NOT NULL,created_at timestamptz NOT NULL DEFAULT now(),
 details jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX import_run_events_recent ON crm.import_run_events(run_id,id DESC);
CREATE INDEX import_run_events_expiry ON crm.import_run_events(created_at);
CREATE FUNCTION crm.import_activity_rollup() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO crm.import_activity_daily VALUES(
  date_bin('15 minutes',NEW.received_at,'2000-01-01'::timestamptz),NEW.provider,coalesce(NEW.connection_id::text,'legacy'),NEW.stream,
  NEW.records-CASE WHEN TG_OP='UPDATE' THEN OLD.records ELSE 0 END,
  NEW.processed-CASE WHEN TG_OP='UPDATE' THEN OLD.processed ELSE 0 END,
  NEW.inserted-CASE WHEN TG_OP='UPDATE' THEN OLD.inserted ELSE 0 END,
  NEW.changed-CASE WHEN TG_OP='UPDATE' THEN OLD.changed ELSE 0 END,
  NEW.unchanged-CASE WHEN TG_OP='UPDATE' THEN OLD.unchanged ELSE 0 END,
  NEW.failures-CASE WHEN TG_OP='UPDATE' THEN OLD.failures ELSE 0 END)
 ON CONFLICT(bucket,provider,account,stream) DO UPDATE SET
 records=crm.import_activity_daily.records+excluded.records,
 processed=crm.import_activity_daily.processed+excluded.processed,
 inserted=crm.import_activity_daily.inserted+excluded.inserted,
 changed=crm.import_activity_daily.changed+excluded.changed,
 unchanged=crm.import_activity_daily.unchanged+excluded.unchanged,
 failures=crm.import_activity_daily.failures+excluded.failures;
 RETURN NULL;
END $$;
CREATE TRIGGER activity_rollup AFTER INSERT OR UPDATE ON crm.import_activity FOR EACH ROW EXECUTE FUNCTION crm.import_activity_rollup();
CREATE FUNCTION crm.track_import_batch() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE pending boolean:=false; visible_state text; visible_count bigint;
BEGIN
 IF NEW.connection_id IS NOT NULL THEN
  EXECUTE format('SELECT EXISTS(SELECT 1 FROM %I.connection_outbox WHERE connection_id=$1 AND delivered_at IS NULL)',NEW.source) INTO pending USING NEW.connection_id;
 END IF;
 visible_state:=CASE WHEN NEW.status='complete' AND pending THEN 'saved' ELSE NEW.status END;
 visible_count:=CASE WHEN pending THEN 0 ELSE NEW.processed_count END;
 INSERT INTO crm.import_activity(id,provider,connection_id,stream,kind,batch_id,state,received_at,completed_at,records,processed,inserted,changed,unchanged,failures)
 VALUES('batch:'||NEW.id,NEW.source,NEW.connection_id,NEW.stream,'batch',NEW.batch_id,visible_state,NEW.received_at,CASE WHEN NOT pending THEN NEW.completed_at END,NEW.record_count,visible_count,NEW.inserted_count,NEW.changed_count,NEW.unchanged_count,CASE WHEN NEW.status='failed' THEN 1 ELSE 0 END)
 ON CONFLICT(id) DO UPDATE SET state=excluded.state,completed_at=excluded.completed_at,records=excluded.records,processed=excluded.processed,inserted=excluded.inserted,changed=excluded.changed,unchanged=excluded.unchanged,failures=greatest(crm.import_activity.failures,excluded.failures),updated_at=now();
 RETURN NULL;
END $$;
CREATE TRIGGER track_batch AFTER INSERT OR UPDATE OF status,processed_count,inserted_count,changed_count,unchanged_count ON crm.import_batches FOR EACH ROW EXECUTE FUNCTION crm.track_import_batch();
-- Preserve observable old receipts; absence of records is not a successful day.
INSERT INTO crm.import_activity(id,provider,connection_id,stream,kind,batch_id,state,received_at,completed_at,records,processed,inserted,changed,unchanged,failures)
 SELECT 'batch:'||id,source,connection_id,stream,'batch',batch_id,status,received_at,completed_at,record_count,processed_count,inserted_count,changed_count,unchanged_count,CASE WHEN status='failed' THEN 1 ELSE 0 END FROM crm.import_batches;
DO $$DECLARE p text;BEGIN
 FOREACH p IN ARRAY ARRAY['wechat','instagram','linkedin','whatsapp','apple_messages','apple_contacts','mail'] LOOP
  EXECUTE format('UPDATE crm.import_activity a SET state=''saved'',processed=0,completed_at=NULL WHERE provider=%L AND kind=''batch'' AND state=''complete'' AND connection_id IS NOT NULL AND EXISTS(SELECT 1 FROM %I.connection_outbox o WHERE o.connection_id=a.connection_id AND delivered_at IS NULL)',p,p);
 END LOOP;
END $$;

CREATE FUNCTION crm.track_activity_state() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.kind IN ('page','batch') AND (TG_OP='INSERT' OR NEW.state IS DISTINCT FROM OLD.state) THEN
 INSERT INTO crm.import_run_events(run_id,provider,connection_id,state,phase) VALUES(NEW.id,NEW.provider,NEW.connection_id,NEW.state,'import');
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER activity_state AFTER INSERT OR UPDATE OF state ON crm.import_activity FOR EACH ROW EXECUTE FUNCTION crm.track_activity_state();

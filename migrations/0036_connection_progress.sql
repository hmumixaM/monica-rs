CREATE INDEX connection_evidence_coverage ON crm.connection_evidence(connection_id,occurred_at DESC,id) WHERE included AND NOT deleted;
CREATE TABLE crm.connection_progress(
 connection_id uuid PRIMARY KEY REFERENCES crm.source_connections(id),records bigint NOT NULL CHECK(records>=0),
 earliest_at timestamptz,latest_at timestamptz
);
INSERT INTO crm.connection_progress SELECT connection_id,count(*),min(occurred_at),max(occurred_at) FROM crm.connection_evidence WHERE included AND NOT deleted GROUP BY connection_id;
CREATE FUNCTION crm.refresh_connection_progress() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE changes text;r record;
BEGIN
 IF TG_OP='INSERT' THEN changes:='SELECT connection_id,1::bigint delta FROM added WHERE included AND NOT deleted';
 ELSIF TG_OP='DELETE' THEN changes:='SELECT connection_id,-1::bigint delta FROM removed WHERE included AND NOT deleted';
 ELSE changes:='SELECT n.connection_id,1::bigint delta FROM added n JOIN removed o USING(id) WHERE n.included AND NOT n.deleted AND (n.connection_id,n.included,n.deleted,n.occurred_at) IS DISTINCT FROM (o.connection_id,o.included,o.deleted,o.occurred_at) UNION ALL SELECT o.connection_id,-1::bigint delta FROM removed o JOIN added n USING(id) WHERE o.included AND NOT o.deleted AND (n.connection_id,n.included,n.deleted,n.occurred_at) IS DISTINCT FROM (o.connection_id,o.included,o.deleted,o.occurred_at)';
 END IF;
 FOR r IN EXECUTE 'SELECT connection_id,sum(delta)::bigint delta FROM ('||changes||') x GROUP BY connection_id ORDER BY connection_id' LOOP
  -- Lock sorted compact counters; extrema use the connection/time index.
  INSERT INTO crm.connection_progress(connection_id,records) VALUES(r.connection_id,0) ON CONFLICT DO NOTHING;
  UPDATE crm.connection_progress SET records=records+r.delta,
   earliest_at=(SELECT occurred_at FROM crm.connection_evidence WHERE connection_id=r.connection_id AND included AND NOT deleted AND occurred_at IS NOT NULL ORDER BY occurred_at ASC,id ASC LIMIT 1),
   latest_at=(SELECT occurred_at FROM crm.connection_evidence WHERE connection_id=r.connection_id AND included AND NOT deleted AND occurred_at IS NOT NULL ORDER BY occurred_at DESC,id DESC LIMIT 1)
   WHERE connection_id=r.connection_id;
 END LOOP;
 RETURN NULL;
END $$;
CREATE TRIGGER connection_progress_i AFTER INSERT ON crm.connection_evidence REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.refresh_connection_progress();
CREATE TRIGGER connection_progress_u AFTER UPDATE ON crm.connection_evidence REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.refresh_connection_progress();
CREATE TRIGGER connection_progress_d AFTER DELETE ON crm.connection_evidence REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.refresh_connection_progress();

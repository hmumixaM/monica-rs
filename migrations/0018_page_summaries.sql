-- Committed call/money deltas: pages read counters, never scan event history.
CREATE TABLE crm.profile_contact_counters (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),kind text NOT NULL,
 metric text NOT NULL,dimension text NOT NULL DEFAULT '',value numeric NOT NULL,
 PRIMARY KEY(profile_id,kind,metric,dimension)
);
CREATE FUNCTION crm.contact_deltas(kind text,body jsonb)
RETURNS TABLE(metric text,dimension text,value numeric) LANGUAGE sql IMMUTABLE AS $$
 WITH input AS (SELECT
  CASE WHEN jsonb_typeof(body->'duration_seconds')='number' AND body->>'duration_seconds' ~ '^-?[0-9]+$' THEN (body->>'duration_seconds')::numeric END AS duration,
  CASE WHEN jsonb_typeof(body->'amount_minor')='number' AND body->>'amount_minor' ~ '^-?[0-9]+$' THEN (body->>'amount_minor')::numeric END AS amount)
 SELECT * FROM (
  SELECT 'total','',1::numeric WHERE kind IN ('call','money')
  UNION ALL SELECT body->>'media_type','',1 WHERE kind='call' AND body->>'media_type' IN ('voice','video')
  UNION ALL SELECT 'completed','',1 WHERE kind='call' AND body->>'status'='completed'
  UNION ALL SELECT CASE body->>'direction' WHEN 'in' THEN 'incoming' ELSE 'outgoing' END,'',1 WHERE kind='call' AND body->>'direction' IN ('in','out')
  UNION ALL SELECT 'known_duration','',1 FROM input WHERE kind='call' AND duration IS NOT NULL
  UNION ALL SELECT 'duration_seconds','',duration FROM input WHERE kind='call' AND duration IS NOT NULL
  UNION ALL SELECT 'inferred_type','',1 WHERE kind='call' AND body->>'media_type_confidence' IS DISTINCT FROM 'high'
  UNION ALL SELECT body->>'kind','',1 WHERE kind='money' AND body->>'kind' IN ('transfer','red_packet')
  UNION ALL SELECT 'unknown_amount','',1 FROM input WHERE kind='money' AND amount IS NULL
  UNION ALL SELECT 'unknown_direction','',1 WHERE kind='money' AND coalesce(body->>'direction','') NOT IN ('in','out')
  UNION ALL SELECT 'currency_'||(body->>'direction'),body->>'currency',amount FROM input
   WHERE kind='money' AND amount IS NOT NULL AND jsonb_typeof(body->'currency')='string' AND body->>'direction' IN ('in','out')
  UNION ALL SELECT 'currency_count_'||(body->>'direction'),body->>'currency',1 FROM input
   WHERE kind='money' AND amount IS NOT NULL AND jsonb_typeof(body->'currency')='string' AND body->>'direction' IN ('in','out')
 ) deltas WHERE body->>'is_deleted' IS DISTINCT FROM 'true'
$$;
CREATE FUNCTION crm.update_contact_counters() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_profile uuid;new_profile uuid;old_kind text;new_kind text;old_content jsonb;new_content jsonb;delta record;
BEGIN
 IF TG_OP<>'INSERT' THEN old_profile=OLD.profile_id;old_kind=OLD.kind;old_content=OLD.content; END IF;
 IF TG_OP<>'DELETE' THEN new_profile=NEW.profile_id;new_kind=NEW.kind;new_content=NEW.content; END IF;
 FOR delta IN
  SELECT profile_id,kind,metric,dimension,sum(value) AS value FROM (
   SELECT old_profile AS profile_id,old_kind AS kind,d.metric,d.dimension,-d.value AS value FROM crm.contact_deltas(old_kind,old_content) d WHERE old_profile IS NOT NULL
   UNION ALL SELECT new_profile,new_kind,d.metric,d.dimension,d.value FROM crm.contact_deltas(new_kind,new_content) d WHERE new_profile IS NOT NULL
  ) changes GROUP BY profile_id,kind,metric,dimension HAVING sum(value)<>0 ORDER BY profile_id,kind,metric,dimension
 LOOP
  INSERT INTO crm.profile_contact_counters VALUES(delta.profile_id,delta.kind,delta.metric,delta.dimension,delta.value)
  ON CONFLICT(profile_id,kind,metric,dimension) DO UPDATE SET value=profile_contact_counters.value+excluded.value;
 END LOOP;
 RETURN NULL;
END $$;
INSERT INTO crm.profile_contact_counters
 SELECT e.profile_id,e.kind,d.metric,d.dimension,sum(d.value) FROM crm.contact_events e
 CROSS JOIN LATERAL crm.contact_deltas(e.kind,e.content) d WHERE e.profile_id IS NOT NULL
 GROUP BY e.profile_id,e.kind,d.metric,d.dimension;
CREATE TRIGGER contact_counters AFTER INSERT OR UPDATE OR DELETE ON crm.contact_events
 FOR EACH ROW EXECUTE FUNCTION crm.update_contact_counters();

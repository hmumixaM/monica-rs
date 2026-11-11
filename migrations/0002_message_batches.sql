-- Message append bypasses mutable-object revisions and per-record jobs.
DO $$ DECLARE s text; BEGIN
 FOREACH s IN ARRAY ARRAY['wechat','instagram','linkedin'] LOOP
  EXECUTE format('CREATE TABLE %I.messages (
   account text NOT NULL, conversation text NOT NULL, external_id text NOT NULL,
   sent_at_ms bigint NOT NULL, sender text NOT NULL, body text NOT NULL,
   PRIMARY KEY(account,conversation,external_id))',s);
  EXECUTE format('CREATE INDEX ON %I.messages(account,conversation,sent_at_ms DESC,external_id DESC)',s);
 END LOOP;
END $$;

-- One small cumulative state row per conversation; no source-history scan on reads.
CREATE TABLE crm.conversation_state (
 provider text NOT NULL, account text NOT NULL, conversation text NOT NULL,
 message_count bigint NOT NULL, last_at_ms bigint NOT NULL, last_id text NOT NULL,
 preview text NOT NULL, generation bigint NOT NULL,
 PRIMARY KEY(provider,account,conversation)
);
CREATE TABLE crm.conversation_work (
 provider text NOT NULL, account text NOT NULL, conversation text NOT NULL,
 requested bigint NOT NULL, token uuid, lease_until timestamptz,
 PRIMARY KEY(provider,account,conversation)
) WITH (fillfactor=80);
CREATE TABLE crm.conversation_summaries (
 provider text NOT NULL, account text NOT NULL, conversation text NOT NULL,
 message_count bigint NOT NULL, last_at_ms bigint NOT NULL, last_id text NOT NULL,
 preview text NOT NULL, generation bigint NOT NULL,
 PRIMARY KEY(provider,account,conversation)
);
CREATE INDEX conversation_sidebar ON crm.conversation_summaries(provider,account,last_at_ms DESC,conversation);

CREATE TABLE crm.job_history (LIKE crm.jobs INCLUDING DEFAULTS);
INSERT INTO crm.job_history SELECT * FROM crm.jobs WHERE state='complete';
DELETE FROM crm.jobs WHERE state='complete';
-- Per-object committed projection versions replace a misleading global watermark.
DROP SEQUENCE crm.projection_generation;

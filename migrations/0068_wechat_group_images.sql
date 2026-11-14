-- Opt-in, offline group-image recovery. Installing this migration starts no work.
CREATE TABLE crm.wechat_image_control (
 singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton),
 enabled boolean NOT NULL DEFAULT false, paused boolean NOT NULL DEFAULT true,
 updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO crm.wechat_image_control DEFAULT VALUES;
CREATE TABLE crm.wechat_image_runs (
 id uuid PRIMARY KEY, account text NOT NULL, status text NOT NULL DEFAULT 'indexing',
 discovery_complete boolean NOT NULL DEFAULT false,
 counts jsonb NOT NULL DEFAULT '{}', indexed_files bigint NOT NULL DEFAULT 0,
 copied_bytes bigint NOT NULL DEFAULT 0, reused_files bigint NOT NULL DEFAULT 0,
 heartbeat_at timestamptz NOT NULL DEFAULT now(), created_at timestamptz NOT NULL DEFAULT now(),
 completed_at timestamptz, error_code text
);
CREATE INDEX wechat_image_runs_recent ON crm.wechat_image_runs(created_at DESC,id DESC);
CREATE TABLE crm.wechat_group_images (
 run_id uuid NOT NULL REFERENCES crm.wechat_image_runs(id), message_id uuid NOT NULL,
 conversation_id uuid NOT NULL, day date NOT NULL,
 -- Retain historical diagnostics without preventing message deletion/retraction.
 state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','available','no_reference','source_missing','decode_failed','source_changed')),
 quality text NOT NULL DEFAULT 'none' CHECK(quality IN ('none','original','high','standard','thumbnail','unknown')),
 media_id uuid REFERENCES crm.media_assets(id), attempts integer NOT NULL DEFAULT 0,
 updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(run_id,message_id)
);
CREATE INDEX wechat_group_images_pending ON crm.wechat_group_images(run_id,message_id) WHERE state='pending';
CREATE TABLE crm.wechat_image_daily (
 run_id uuid NOT NULL REFERENCES crm.wechat_image_runs(id), conversation_id uuid NOT NULL, day date NOT NULL,
 counts jsonb NOT NULL DEFAULT '{}', updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(run_id,day,conversation_id)
);
-- Exactly-once counters share the item transaction. Replays/resumes do not inflate coverage.
CREATE FUNCTION crm.wechat_image_count(value jsonb, state text, quality text, delta integer)
RETURNS jsonb LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
 value=jsonb_set(value,ARRAY['total'],to_jsonb(coalesce((value->>'total')::bigint,0)+delta));
 value=jsonb_set(value,ARRAY[state],to_jsonb(coalesce((value->>state)::bigint,0)+delta));
 IF state='available' THEN
  value=jsonb_set(value,ARRAY['quality_'||quality],to_jsonb(coalesce((value->>('quality_'||quality))::bigint,0)+delta));
 END IF;
 RETURN value;
END $$;
CREATE FUNCTION crm.wechat_image_counters() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='UPDATE' AND NEW.state=OLD.state AND NEW.quality=OLD.quality
  AND NEW.conversation_id=OLD.conversation_id AND NEW.day=OLD.day THEN RETURN NEW; END IF;
 -- Serialize summaries in a fixed order: run first, then its group/day.
 UPDATE crm.wechat_image_runs SET counts=crm.wechat_image_count(
  CASE WHEN TG_OP='UPDATE' THEN crm.wechat_image_count(counts,OLD.state,OLD.quality,-1) ELSE counts END,
  NEW.state,NEW.quality,1) WHERE id=NEW.run_id;
 IF TG_OP='UPDATE' THEN
  UPDATE crm.wechat_image_daily SET counts=crm.wechat_image_count(counts,OLD.state,OLD.quality,-1),updated_at=now()
  WHERE run_id=OLD.run_id AND conversation_id=OLD.conversation_id AND day=OLD.day;
 END IF;
 INSERT INTO crm.wechat_image_daily(run_id,conversation_id,day) VALUES(NEW.run_id,NEW.conversation_id,NEW.day) ON CONFLICT DO NOTHING;
 UPDATE crm.wechat_image_daily SET counts=crm.wechat_image_count(counts,NEW.state,NEW.quality,1),updated_at=now()
 WHERE run_id=NEW.run_id AND conversation_id=NEW.conversation_id AND day=NEW.day;
 IF TG_OP='UPDATE' THEN
  DELETE FROM crm.wechat_image_daily WHERE run_id=OLD.run_id AND conversation_id=OLD.conversation_id
   AND day=OLD.day AND (counts->>'total')::bigint=0;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wechat_image_item_counters AFTER INSERT OR UPDATE OF state,quality,conversation_id,day ON crm.wechat_group_images
 FOR EACH ROW EXECUTE FUNCTION crm.wechat_image_counters();

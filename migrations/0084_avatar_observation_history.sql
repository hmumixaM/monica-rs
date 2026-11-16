-- An observation time is not the unknown time at which a person changed photos.
CREATE TABLE crm.avatar_observations (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(),sequence bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
 account_id uuid NOT NULL REFERENCES crm.source_accounts(id),role text NOT NULL,
 previous_media_id uuid REFERENCES crm.media_assets(id),media_id uuid NOT NULL REFERENCES crm.media_assets(id),
 observed_at timestamptz,recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 batch_id uuid REFERENCES crm.import_batches(id),revision_id uuid REFERENCES crm.field_revisions(id),
 evidence_key text NOT NULL UNIQUE,CHECK(previous_media_id IS DISTINCT FROM media_id)
);
CREATE INDEX avatar_observations_account ON crm.avatar_observations(account_id,observed_at DESC,id);
CREATE TRIGGER append_only_facts BEFORE UPDATE OR DELETE ON crm.avatar_observations FOR EACH STATEMENT EXECUTE FUNCTION crm.reject_fact_rewrite();
CREATE FUNCTION crm.record_avatar_observation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE previous uuid; owner uuid;
BEGIN
 IF NEW.entity_type<>'source_account' OR NEW.role NOT IN ('wechat_avatar_small','wechat_avatar','wechat_avatar_big','instagram_avatar','linkedin_avatar') THEN RETURN NULL; END IF;
 IF TG_OP='UPDATE' THEN previous=OLD.media_id; END IF;
 IF previous IS NOT DISTINCT FROM NEW.media_id THEN RETURN NULL; END IF;
 -- Equal content on a rotated source URL is the same photo.
 IF previous IS NOT NULL AND EXISTS(SELECT 1 FROM crm.media_assets a JOIN crm.media_assets b ON a.sha256=b.sha256 WHERE a.id=previous AND b.id=NEW.media_id) THEN RETURN NULL; END IF;
 INSERT INTO crm.avatar_observations(account_id,role,previous_media_id,media_id,observed_at,batch_id,evidence_key)
 VALUES(NEW.entity_id,NEW.role,previous,NEW.media_id,NEW.source_observed_at,NEW.source_batch_id,
 'slot:'||NEW.entity_id::text||':'||NEW.role||':'||NEW.generation::text||':'||coalesce(NEW.source_batch_id::text,'')||':'||coalesce(NEW.source_observed_at::text,'')||':'||NEW.media_id::text)
 ON CONFLICT DO NOTHING;
 SELECT profile_id INTO owner FROM crm.source_account_links WHERE account_id=NEW.entity_id;
 IF owner IS NOT NULL THEN INSERT INTO crm.memory_profile_refresh VALUES(owner) ON CONFLICT DO NOTHING; END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER avatar_observation_i AFTER INSERT ON crm.media_slots FOR EACH ROW EXECUTE FUNCTION crm.record_avatar_observation();
CREATE TRIGGER avatar_observation_u AFTER UPDATE ON crm.media_slots FOR EACH ROW EXECUTE FUNCTION crm.record_avatar_observation();

-- Exact field URL-to-asset matching plus account-owned media proves a historic
-- observation. Ambiguous reused URLs cannot prove which image was observed.
CREATE FUNCTION crm.backfill_avatar_observations() RETURNS void LANGUAGE sql AS $$
WITH addresses AS (
 SELECT id media_id,source_type provider,source_record_id url FROM crm.media_assets WHERE source_record_id IS NOT NULL
 UNION
 SELECT media_id,source,source_url FROM crm.media_provenance WHERE source_url IS NOT NULL
 UNION
 SELECT media_id,source,source_record_id FROM crm.media_provenance
), matches AS (
 SELECT r.id,r.revision_seq,r.source_account_id account_id,r.observed_at,
 CASE WHEN r.field_key='linkedin.avatar_url' THEN 'linkedin_avatar' WHEN r.field_key LIKE 'instagram.%' THEN 'instagram_avatar' ELSE 'wechat_avatar' END AS role,
 min(a.media_id::text)::uuid media_id,count(DISTINCT a.media_id) n
 FROM crm.field_revisions r JOIN addresses a ON a.url=r.value#>>'{}' AND a.provider=r.source_type
 WHERE r.source_account_id IS NOT NULL AND r.field_key IN ('linkedin.avatar_url','instagram.profile_pic_url','instagram.avatar_url','wechat.avatar_url')
 AND EXISTS(SELECT 1 FROM crm.media_links m WHERE m.media_id=a.media_id AND ((m.entity_type='source_account' AND m.entity_id=r.source_account_id) OR m.source_account_id=r.source_account_id))
 GROUP BY r.id,r.revision_seq,r.source_account_id,r.observed_at,r.field_key
), ordered AS (
 SELECT *,lag(media_id) OVER(PARTITION BY account_id,role ORDER BY observed_at,revision_seq) previous FROM matches WHERE n=1
)
INSERT INTO crm.avatar_observations(account_id,role,previous_media_id,media_id,observed_at,revision_id,evidence_key)
 SELECT account_id,role,previous,media_id,observed_at,id,'revision:'||id::text||':'||media_id::text FROM ordered
 WHERE previous IS DISTINCT FROM media_id ON CONFLICT(evidence_key) DO NOTHING;
$$;
SELECT crm.backfill_avatar_observations();
-- A baseline invents no historical change and does not duplicate proven history.
INSERT INTO crm.avatar_observations(account_id,role,media_id,observed_at,batch_id,evidence_key)
 SELECT entity_id,role,media_id,source_observed_at,source_batch_id,'baseline:'||entity_id::text||':'||role
 FROM crm.media_slots s WHERE entity_type='source_account' AND role IN ('wechat_avatar_small','wechat_avatar','wechat_avatar_big','instagram_avatar','linkedin_avatar')
 AND NOT EXISTS(SELECT 1 FROM crm.avatar_observations o WHERE o.account_id=s.entity_id AND o.role=s.role AND o.media_id=s.media_id);

-- A current slot outranks legacy links even when their role aliases differ.
CREATE VIEW crm.profile_source_media AS
 SELECT l.profile_id,CASE WHEN s.role IN ('wechat_avatar_small','wechat_avatar_big') THEN 'wechat_avatar' ELSE s.role END AS role,
 s.media_id,l.account_id AS source_account_id,coalesce(s.source_observed_at,s.updated_at) AS observed_at
 FROM crm.source_account_links l JOIN crm.media_slots s ON s.entity_type='source_account' AND s.entity_id=l.account_id
 WHERE s.role IN ('cover','linkedin_cover','linkedin_avatar','wechat_avatar','wechat_avatar_small','wechat_avatar_big','instagram_avatar')
 UNION ALL
 SELECT m.entity_id,m.role,m.media_id,m.source_account_id,m.created_at
 FROM crm.media_links m WHERE m.entity_type='profile' AND (
  m.source_account_id IS NULL OR EXISTS(SELECT 1 FROM crm.source_account_links l WHERE l.account_id=m.source_account_id AND l.profile_id=m.entity_id))
 AND NOT EXISTS(SELECT 1 FROM crm.source_account_links al JOIN crm.media_slots s ON s.entity_type='source_account' AND s.entity_id=al.account_id
 WHERE al.profile_id=m.entity_id AND (m.source_account_id IS NULL OR al.account_id=m.source_account_id)
 AND (CASE WHEN s.role IN ('wechat_avatar_small','wechat_avatar_big') THEN 'wechat_avatar' ELSE s.role END)=(CASE WHEN m.role IN ('wechat_avatar_small','wechat_avatar_big') THEN 'wechat_avatar' ELSE m.role END));
INSERT INTO crm.memory_profile_refresh SELECT DISTINCT l.profile_id FROM crm.source_account_links l JOIN crm.avatar_observations a ON a.account_id=l.account_id ON CONFLICT DO NOTHING;

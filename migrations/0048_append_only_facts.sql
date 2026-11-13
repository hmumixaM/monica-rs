-- Facts are immutable; current heads, links, queues and read models are not.
-- Keep this separate from the original DDL so existing installations gain the
-- same invariant without rebuilding or copying their source history.
CREATE FUNCTION crm.reject_fact_rewrite() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 RAISE EXCEPTION '% is append-only; append a revision instead of %',
   TG_TABLE_SCHEMA||'.'||TG_TABLE_NAME, TG_OP USING ERRCODE='55000';
END $$;

DO $$ DECLARE s text; t text; BEGIN
 FOREACH s IN ARRAY ARRAY['wechat','instagram','linkedin','whatsapp','apple_messages','apple_contacts','mail'] LOOP
  FOREACH t IN ARRAY ARRAY['payloads','revisions','observations','revision_metadata','messages','message_corrections','events','connection_payloads','connection_revision_refs'] LOOP
   EXECUTE format('CREATE TRIGGER append_only_facts BEFORE UPDATE OR DELETE ON %I.%I FOR EACH STATEMENT EXECUTE FUNCTION crm.reject_fact_rewrite()',s,t);
  END LOOP;
 END LOOP;
 FOREACH t IN ARRAY ARRAY['content_objects','notebook_revisions','notebook_social_revisions','connection_social_revisions','source_structured_revisions','import_batch_inputs','source_blob_refs'] LOOP
  EXECUTE format('CREATE TRIGGER append_only_facts BEFORE UPDATE OR DELETE ON crm.%I FOR EACH STATEMENT EXECUTE FUNCTION crm.reject_fact_rewrite()',t);
 END LOOP;
END $$;

-- Content hashes alone miss timestamps, profile hints and other record metadata.
-- Existing heads are sealed lazily on the next import; no history rewrite.
ALTER TABLE crm.notebook_objects ADD COLUMN record_hash text;
COMMENT ON COLUMN crm.notebook_objects.record_hash IS
 'Complete import-record fingerprint for unchanged post/story replay; not a content revision or observation timestamp';

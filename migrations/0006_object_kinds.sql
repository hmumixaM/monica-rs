-- Stable platform object identities include kind. An external ID may identify
-- both a post and a story under the same source account.
DO $$ DECLARE s text; BEGIN
 FOREACH s IN ARRAY ARRAY['wechat','instagram','linkedin'] LOOP
  EXECUTE format('ALTER TABLE %I.objects ADD COLUMN object_kind text NOT NULL DEFAULT ''record''', s);
  EXECUTE format('ALTER TABLE %I.objects DROP CONSTRAINT objects_account_id_external_id_key', s);
  EXECUTE format('ALTER TABLE %I.objects ADD UNIQUE(account_id,object_kind,external_id)', s);
  EXECUTE format('ALTER TABLE %I.objects ADD CHECK(length(object_kind)>0)', s);
 END LOOP;
END $$;

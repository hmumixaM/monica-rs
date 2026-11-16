CREATE TABLE crm.profile_merges (
 operation_id uuid PRIMARY KEY,source_id uuid NOT NULL REFERENCES crm.profiles(id),target_id uuid NOT NULL REFERENCES crm.profiles(id),
 actor_user_id uuid REFERENCES crm.app_users(id),created_at timestamptz NOT NULL DEFAULT now(),result jsonb NOT NULL,
 CHECK(source_id<>target_id)
);
CREATE TRIGGER append_only_facts BEFORE UPDATE OR DELETE ON crm.profile_merges FOR EACH STATEMENT EXECUTE FUNCTION crm.reject_fact_rewrite();
CREATE FUNCTION crm.canonical_profile(requested uuid) RETURNS uuid LANGUAGE sql STABLE AS $$
 WITH RECURSIVE chain(id,seen) AS(SELECT requested,ARRAY[requested] UNION ALL
 SELECT r.new_profile_id,c.seen||r.new_profile_id FROM chain c JOIN crm.profile_redirects r ON r.old_profile_id=c.id
 WHERE cardinality(c.seen)<32 AND NOT r.new_profile_id=ANY(c.seen))
 SELECT id FROM chain WHERE NOT EXISTS(SELECT 1 FROM crm.profile_redirects WHERE old_profile_id=chain.id)
$$;
-- Only mutable current references are normalized. Historical facts keep their IDs.
CREATE FUNCTION crm.normalize_profile_reference() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE value jsonb;key text;id uuid;canonical uuid;
BEGIN
 IF TG_TABLE_NAME='message_search' THEN IF NEW.sender_profile_id IS NULL THEN RETURN NEW; END IF; END IF;
 value=to_jsonb(NEW);
 FOREACH key IN ARRAY TG_ARGV LOOP
  id=(value->>key)::uuid;
  IF id IS NOT NULL THEN
   canonical=crm.canonical_profile(id);
   IF canonical IS NULL THEN RAISE EXCEPTION 'profile redirect cycle'; END IF;
   value=jsonb_set(value,ARRAY[key],to_jsonb(canonical));
  END IF;
 END LOOP;
 NEW=jsonb_populate_record(NEW,value);RETURN NEW;
END $$;
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.faces FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id','suggested_profile_id','rejected_profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.message_search FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('sender_profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.connection_people FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('resolved_profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.connection_identifiers FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.connection_identity_links FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.connection_evidence_people FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
-- Enumerate aliases once, then use each profile's existing history index.
CREATE FUNCTION crm.profile_aliases(requested uuid) RETURNS SETOF uuid LANGUAGE sql STABLE AS $$
 WITH RECURSIVE aliases(id,seen) AS(SELECT crm.canonical_profile(requested),ARRAY[crm.canonical_profile(requested)] UNION ALL
 SELECT r.old_profile_id,a.seen||r.old_profile_id FROM aliases a JOIN crm.profile_redirects r ON r.new_profile_id=a.id
 WHERE cardinality(a.seen)<32 AND NOT r.old_profile_id=ANY(a.seen)) SELECT id FROM aliases
$$;
CREATE INDEX profile_redirect_target ON crm.profile_redirects(new_profile_id,old_profile_id);

CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.field_current FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.notebook_structured_current FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.notebook_objects FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.timeline_events FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.conversations FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.contact_events FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.identities FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id');
CREATE TRIGGER canonical_person BEFORE INSERT OR UPDATE ON crm.connection_import_bindings FOR EACH ROW EXECUTE FUNCTION crm.normalize_profile_reference('profile_id','sender_profile_id');

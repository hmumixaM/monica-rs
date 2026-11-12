CREATE TABLE crm.notebook_objects (
 id uuid PRIMARY KEY,stream text NOT NULL,external_id text NOT NULL,version bigint NOT NULL,content_hash text NOT NULL,
 profile_id uuid REFERENCES crm.profiles(id),UNIQUE(stream,external_id)
);
CREATE TABLE crm.notebook_revisions (
 object_id uuid NOT NULL REFERENCES crm.notebook_objects(id),version bigint NOT NULL,
 content_object_id uuid NOT NULL REFERENCES crm.content_objects(id),observed_at timestamptz NOT NULL,
 PRIMARY KEY(object_id,version)
);
ALTER TABLE crm.source_structured_revisions ALTER COLUMN source_account_id DROP NOT NULL;
ALTER TABLE crm.source_structured_revisions ADD COLUMN notebook_profile_id uuid REFERENCES crm.profiles(id);
CREATE UNIQUE INDEX notebook_structured_revision_identity ON crm.source_structured_revisions(notebook_profile_id,fact_type,fact_key,object_id,source_version) WHERE notebook_profile_id IS NOT NULL;
CREATE TABLE crm.notebook_structured_current (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),fact_type text NOT NULL,fact_key text NOT NULL,
 revision_id uuid NOT NULL REFERENCES crm.source_structured_revisions(id),PRIMARY KEY(profile_id,fact_type,fact_key)
);
CREATE OR REPLACE VIEW crm.profile_structured_current AS
 SELECT l.profile_id,r.fact_key AS field_key,r.fact_key,r.id AS revision_id,r.id AS current_revision_id,
 r.fact_type,r.value,r.position,r.content_hash AS current_content_hash,r.source_type AS current_source_type,
 r.observed_at AS updated_at,r.source_account_id,r.content_object_id,
 CASE WHEN r.fact_type='employment' THEN e.name END AS company_name,
 CASE WHEN r.fact_type='education' THEN e.name END AS school_name,
 CASE WHEN r.fact_type='employment' THEN e.id END AS company_id,
 CASE WHEN r.fact_type='education' THEN e.id END AS school_id
 FROM (SELECT l.profile_id,c.revision_id FROM crm.source_structured_current c JOIN crm.source_account_links l ON l.account_id=c.source_account_id UNION ALL SELECT profile_id,revision_id FROM crm.notebook_structured_current) l
 JOIN crm.source_structured_revisions r ON r.id=l.revision_id LEFT JOIN crm.organizations e ON e.id=r.entity_id;
CREATE OR REPLACE VIEW crm.structured_revisions AS
 SELECT r.id,r.revision_seq,r.source_account_id,r.fact_type,r.fact_key,r.entity_id,r.content_object_id,r.content_hash,r.value,r.source_type,r.source_record_id,r.object_id,r.source_version,r.observed_at,r.previous_revision_id,r.is_conflict,r.position,
 coalesce(r.notebook_profile_id,l.profile_id) AS profile_id,e.name AS entity_name
 FROM crm.source_structured_revisions r LEFT JOIN crm.source_account_links l ON l.account_id=r.source_account_id LEFT JOIN crm.organizations e ON e.id=r.entity_id;
CREATE TRIGGER notebook_structured_cache_i AFTER INSERT ON crm.notebook_structured_current REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,profile_id subject FROM changed');
CREATE TRIGGER notebook_structured_cache_u AFTER UPDATE ON crm.notebook_structured_current REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,profile_id subject FROM changed');
ALTER TABLE crm.timeline_events ALTER COLUMN source_account_id DROP NOT NULL;
CREATE TABLE crm.notebook_social_revisions (
 id uuid PRIMARY KEY,event_id uuid NOT NULL REFERENCES crm.timeline_events(id),revision_seq bigint GENERATED ALWAYS AS IDENTITY,
 content_object_id uuid NOT NULL REFERENCES crm.content_objects(id),content_hash text NOT NULL,observed_at timestamptz NOT NULL,source_record_id text NOT NULL
);
CREATE INDEX ON crm.notebook_social_revisions(event_id,revision_seq DESC);

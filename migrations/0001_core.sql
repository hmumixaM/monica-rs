CREATE SCHEMA crm;
CREATE SCHEMA wechat;
CREATE SCHEMA instagram;
CREATE SCHEMA linkedin;

-- Source schemas deliberately contain no CRM profile references.
DO $$ DECLARE s text; BEGIN
 FOREACH s IN ARRAY ARRAY['wechat','instagram','linkedin'] LOOP
  EXECUTE format('CREATE TABLE %I.accounts (
    id uuid PRIMARY KEY, external_id text NOT NULL UNIQUE)', s);
  EXECUTE format('CREATE TABLE %I.objects (
    id uuid PRIMARY KEY, account_id uuid NOT NULL REFERENCES %I.accounts(id),
    external_id text NOT NULL, revision bigint NOT NULL DEFAULT 0,
    UNIQUE(account_id, external_id))', s, s);
  EXECUTE format('CREATE TABLE %I.revisions (
    object_id uuid NOT NULL REFERENCES %I.objects(id), version bigint NOT NULL,
    payload jsonb NOT NULL, hash text NOT NULL,
    PRIMARY KEY(object_id, version))', s, s);
  EXECUTE format('CREATE TABLE %I.observations (
    object_id uuid NOT NULL REFERENCES %I.objects(id), event_id text NOT NULL,
    hash text NOT NULL, version bigint NOT NULL,
    PRIMARY KEY(object_id,event_id),
    FOREIGN KEY(object_id,version) REFERENCES %I.revisions(object_id,version))', s,s,s);
 END LOOP;
END $$;

CREATE TABLE crm.jobs (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 provider text NOT NULL CHECK(provider IN ('wechat','instagram','linkedin')),
 object_id uuid NOT NULL, version bigint NOT NULL,
 state text NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','running','complete')),
 token uuid, lease_until timestamptz, attempts integer NOT NULL DEFAULT 0,
 UNIQUE(provider,object_id,version)
);
CREATE INDEX jobs_claim ON crm.jobs(state,id) WHERE state <> 'complete';
CREATE TABLE crm.projections (
 provider text NOT NULL, object_id uuid NOT NULL, version bigint NOT NULL,
 payload jsonb NOT NULL, PRIMARY KEY(provider,object_id)
);
-- Sequences avoid a shared row lock between unrelated projection commits.
-- Gaps on rollback are harmless: this is an invalidation generation, not a count.
CREATE SEQUENCE crm.projection_generation;

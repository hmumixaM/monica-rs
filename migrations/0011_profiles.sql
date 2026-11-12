-- Current notebook facts, independent source memberships and manual overrides.
CREATE VIEW crm.profile_structured_current AS
 SELECT l.profile_id,r.fact_key AS field_key,r.fact_key,r.id AS revision_id,r.id AS current_revision_id,
 r.fact_type,r.value,r.position,r.content_hash AS current_content_hash,r.source_type AS current_source_type,
 r.observed_at AS updated_at,r.source_account_id,r.content_object_id,
 CASE WHEN r.fact_type='employment' THEN e.name END AS company_name,
 CASE WHEN r.fact_type='education' THEN e.name END AS school_name,
 CASE WHEN r.fact_type='employment' THEN e.id END AS company_id,
 CASE WHEN r.fact_type='education' THEN e.id END AS school_id
 FROM crm.source_structured_current c JOIN crm.source_structured_revisions r ON r.id=c.revision_id
 JOIN crm.source_account_links l ON l.account_id=c.source_account_id
 LEFT JOIN crm.organizations e ON e.id=r.entity_id;

CREATE VIEW crm.structured_revisions AS
 SELECT r.*,l.profile_id,e.name AS entity_name
 FROM crm.source_structured_revisions r LEFT JOIN crm.source_account_links l ON l.account_id=r.source_account_id
 LEFT JOIN crm.organizations e ON e.id=r.entity_id;

CREATE TABLE crm.group_memberships (
 group_profile_id uuid NOT NULL REFERENCES crm.profiles(id),person_profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 role text,joined_at timestamptz,left_at timestamptz,observed_at timestamptz NOT NULL DEFAULT now(),
 source_type text NOT NULL DEFAULT 'manual',source_record_id uuid,
 PRIMARY KEY(group_profile_id,person_profile_id),CHECK(group_profile_id<>person_profile_id)
);
CREATE INDEX ON crm.group_memberships(person_profile_id) WHERE left_at IS NULL;
CREATE VIEW crm.current_group_memberships AS
 SELECT * FROM crm.group_memberships
 UNION ALL
 SELECT gl.profile_id,pl.profile_id,s.role,s.joined_at,s.left_at,
   greatest(gl.linked_at,pl.linked_at),'wechat',s.group_account_id
 FROM crm.source_group_memberships s
 JOIN crm.source_account_links gl ON gl.account_id=s.group_account_id
 JOIN crm.source_account_links pl ON pl.account_id=s.person_account_id
 WHERE gl.profile_id<>pl.profile_id AND NOT EXISTS(
   SELECT 1 FROM crm.group_memberships m WHERE m.group_profile_id=gl.profile_id AND m.person_profile_id=pl.profile_id);

CREATE TABLE crm.relationship_edges (
 id uuid PRIMARY KEY,from_profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 to_profile_id uuid NOT NULL REFERENCES crm.profiles(id),relationship_type text NOT NULL CHECK(length(relationship_type) BETWEEN 1 AND 120),
 note text,valid_from timestamptz,valid_to timestamptz,source_type text NOT NULL DEFAULT 'manual',confidence double precision,
 created_at timestamptz NOT NULL DEFAULT now(),updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(from_profile_id,to_profile_id,relationship_type),CHECK(from_profile_id<>to_profile_id)
);
CREATE INDEX ON crm.relationship_edges(to_profile_id);
CREATE TABLE crm.relationship_edge_revisions (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,edge_id uuid NOT NULL REFERENCES crm.relationship_edges(id),
 operation text NOT NULL,payload jsonb NOT NULL,actor_user_id uuid REFERENCES crm.app_users(id),created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE crm.profile_dates (
 id uuid PRIMARY KEY,profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 kind text NOT NULL CHECK(kind IN ('birthday','anniversary','other')),label text CHECK(length(label)<=60),
 date_precision text NOT NULL CHECK(date_precision IN ('month_day','approximate','exact')),
 year integer CHECK(year BETWEEN 1800 AND 2200),month integer CHECK(month BETWEEN 1 AND 12),day integer CHECK(day BETWEEN 1 AND 31),
 updated_by uuid REFERENCES crm.app_users(id),created_at timestamptz NOT NULL DEFAULT now(),updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ON crm.profile_dates(profile_id) WHERE kind='birthday';
CREATE UNIQUE INDEX profile_dates_same ON crm.profile_dates(profile_id,kind,coalesce(label,''),date_precision,coalesce(year,0),coalesce(month,0),coalesce(day,0));
CREATE VIEW crm.profile_dates_presented AS
 WITH parts AS (SELECT d.*, (now() AT TIME ZONE 'UTC')::date AS today FROM crm.profile_dates d),
 candidates AS (SELECT parts.*,
  CASE WHEN month IS NOT NULL AND day IS NOT NULL THEN make_date(extract(year from today)::int,month,1)+day-1 END AS candidate,
  CASE WHEN year IS NOT NULL AND kind<>'other' THEN extract(year from today)::int-year
   - CASE WHEN month IS NOT NULL AND (extract(month from today)::int,CASE WHEN day IS NULL THEN 1 ELSE extract(day from today)::int END)<(month,coalesce(day,1)) THEN 1 ELSE 0 END END AS years
  FROM parts),
 next_date AS (SELECT candidates.*,CASE WHEN candidate>=today THEN candidate WHEN candidate IS NOT NULL THEN make_date(extract(year from today)::int+1,month,1)+day-1 END AS next FROM candidates)
 SELECT id,profile_id,created_at,kind,month,day,jsonb_build_object('id',id,'kind',kind,'label',label,'precision',date_precision,'year',year,'month',month,'day',day,
  'years',CASE WHEN years>=0 THEN years END,'years_exact',date_precision='exact' AND coalesce(years>=0,false),
  'next',next,'days_until',next-today) AS value FROM next_date;

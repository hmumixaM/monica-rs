CREATE TABLE crm.activities (
 id uuid PRIMARY KEY,activity_type text NOT NULL CHECK(length(activity_type) BETWEEN 1 AND 120),
 occurred_at timestamptz NOT NULL,title text NOT NULL CHECK(length(title) BETWEEN 1 AND 255),body text,
 source_type text NOT NULL,source_record_id text,created_by uuid REFERENCES crm.app_users(id),
 state text NOT NULL DEFAULT 'open' CHECK(state IN ('open','done')),due_at timestamptz,
 content_hash text NOT NULL,created_at timestamptz NOT NULL DEFAULT now(),updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON crm.activities(occurred_at DESC,id DESC);
CREATE INDEX ON crm.activities(due_at,id) WHERE state='open' AND activity_type IN ('task','reminder');
CREATE TABLE crm.activity_participants (
 activity_id uuid NOT NULL REFERENCES crm.activities(id),profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 PRIMARY KEY(activity_id,profile_id)
);
CREATE INDEX ON crm.activity_participants(profile_id,activity_id);
CREATE TABLE crm.activity_revisions (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,activity_id uuid NOT NULL REFERENCES crm.activities(id),
 operation text NOT NULL,payload jsonb NOT NULL,content_hash text NOT NULL,
 actor_user_id uuid REFERENCES crm.app_users(id),created_at timestamptz NOT NULL DEFAULT now()
);
CREATE OR REPLACE VIEW crm.timeline_feed AS
 SELECT id,provider,external_id,profile_id,event_type,occurred_at,content,content_hash,status FROM crm.timeline_events
 UNION ALL
 SELECT a.id,'manual',a.id::text,NULL::uuid,'activity',a.occurred_at,
   to_jsonb(a)-'content_hash'||jsonb_build_object('text',a.body),a.content_hash,'active'
 FROM crm.activities a WHERE activity_type NOT IN ('wechat_direct_summary','wechat_group_summary');

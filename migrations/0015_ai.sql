-- AI is disabled until explicitly configured. No credentials live in job inputs.
CREATE TABLE crm.ai_settings (
 id boolean PRIMARY KEY DEFAULT true CHECK(id),enabled boolean NOT NULL DEFAULT false,
 provider text NOT NULL DEFAULT 'fake' CHECK(provider IN ('fake','openai')),
 model text NOT NULL DEFAULT 'gemini-3.8-flash',base_url text,
 include_private_notes boolean NOT NULL DEFAULT false,include_messages boolean NOT NULL DEFAULT false,
 updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO crm.ai_settings(id) VALUES(true);
CREATE TABLE crm.ai_jobs (
 id uuid PRIMARY KEY,kind text NOT NULL,subject text NOT NULL,model text NOT NULL,input_hash text NOT NULL,
 input jsonb NOT NULL,source_versions jsonb NOT NULL,privacy jsonb NOT NULL,
 status text NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','complete','failed','cancelled')),
 token uuid,lease_until timestamptz,attempts integer NOT NULL DEFAULT 0,error text,
 created_at timestamptz NOT NULL DEFAULT now(),completed_at timestamptz,
 UNIQUE(kind,subject,model,input_hash)
);
CREATE INDEX ON crm.ai_jobs(status,created_at) WHERE status IN ('queued','running');
CREATE TABLE crm.ai_outputs (
 kind text NOT NULL,subject text NOT NULL,output jsonb NOT NULL,model text NOT NULL,
 input_hash text NOT NULL,source_versions jsonb NOT NULL,job_id uuid REFERENCES crm.ai_jobs(id),
 generated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(kind,subject)
);
CREATE TABLE crm.image_ocr_settings (
 id boolean PRIMARY KEY DEFAULT true CHECK(id),enabled boolean NOT NULL DEFAULT false,
 include_chat boolean NOT NULL DEFAULT false,model text NOT NULL DEFAULT 'gemini-3.8-flash',
 daily_limit integer NOT NULL DEFAULT 100 CHECK(daily_limit>0),updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO crm.image_ocr_settings(id) VALUES(true);
CREATE TABLE crm.image_ocr_readings (
 media_id uuid PRIMARY KEY REFERENCES crm.media_assets(id),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','running','complete','failed')),
 summary text,description text,category text,text text,model text,generated_at timestamptz,error text,
 input_hash text,source_at timestamptz,job_id uuid REFERENCES crm.ai_jobs(id)
);
CREATE TABLE crm.media_readings (
 entity_id uuid NOT NULL REFERENCES crm.timeline_events(id),entity_type text NOT NULL CHECK(entity_type IN ('social_post','social_story')),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','running','complete','failed')),
 summary text,items jsonb NOT NULL DEFAULT '[]',model text,generated_at timestamptz,error text,
 input_hash text,job_id uuid REFERENCES crm.ai_jobs(id),PRIMARY KEY(entity_id,entity_type)
);
CREATE VIEW crm.post_media_readings AS SELECT entity_id AS post_id,status,summary,items,model,generated_at,error,input_hash,job_id FROM crm.media_readings WHERE entity_type='social_post';
CREATE VIEW crm.story_media_readings AS SELECT entity_id AS story_id,status,summary,items,model,generated_at,error,input_hash,job_id FROM crm.media_readings WHERE entity_type='social_story';
CREATE TABLE crm.profile_suggestions (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),fingerprint text NOT NULL,kind text NOT NULL,
 value text NOT NULL,date date,evidence jsonb NOT NULL,quote text NOT NULL,confidence text NOT NULL CHECK(confidence IN ('high','medium')),
 input_hash text NOT NULL,generated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(profile_id,fingerprint)
);
CREATE TABLE crm.profile_suggestion_decisions (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),fingerprint text NOT NULL,kind text NOT NULL,
 decision text NOT NULL CHECK(decision IN ('accepted','ignored')),applied jsonb,
 created_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(profile_id,fingerprint)
);
CREATE TABLE crm.person_weekly_updates (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),week_start date NOT NULL,output jsonb NOT NULL,
 generated_at timestamptz NOT NULL DEFAULT now(),input_hash text NOT NULL,PRIMARY KEY(profile_id,week_start)
);
CREATE TABLE crm.weekly_digests (
 week_start date PRIMARY KEY,output jsonb NOT NULL,generated_at timestamptz NOT NULL DEFAULT now(),input_hash text NOT NULL
);
-- Manual community labels live in crm.community_names (0013_pages.sql).
-- A subject head fences an older request even if a newer request finishes first.
CREATE TABLE crm.ai_subject_heads (
 kind text NOT NULL,subject text NOT NULL,job_id uuid NOT NULL REFERENCES crm.ai_jobs(id),
 PRIMARY KEY(kind,subject)
);
CREATE TABLE crm.ai_reconnect_decisions (
 profile_id uuid PRIMARY KEY REFERENCES crm.profiles(id),action text NOT NULL CHECK(action IN ('dismiss','snooze','contacted')),
 until_at timestamptz,created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.ai_call_reservations (
 job_id uuid PRIMARY KEY REFERENCES crm.ai_jobs(id),day date NOT NULL,created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON crm.ai_call_reservations(day);
CREATE VIEW crm.ai_visible_outputs AS
 SELECT o.* FROM crm.ai_outputs o JOIN crm.ai_jobs j ON j.id=o.job_id
 JOIN crm.ai_settings s ON s.id AND s.enabled
 WHERE j.privacy->'policy'=jsonb_build_object('include_private_notes',s.include_private_notes,'include_messages',s.include_messages);
CREATE TABLE crm.ai_community_inputs (
 community_id text PRIMARY KEY,input jsonb NOT NULL,version_hash text NOT NULL,
 updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.ai_dispatches (
 id uuid PRIMARY KEY,kind text NOT NULL,period text NOT NULL,status text NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','complete')),
 cursor text,processed_count bigint NOT NULL DEFAULT 0,error_count bigint NOT NULL DEFAULT 0,
 token uuid,lease_until timestamptz,created_at timestamptz NOT NULL DEFAULT now(),completed_at timestamptz,
 UNIQUE(kind,period)
);

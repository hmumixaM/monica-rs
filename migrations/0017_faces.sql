CREATE TABLE crm.face_scans (
 media_id uuid PRIMARY KEY REFERENCES crm.media_assets(id),model text NOT NULL,faces integer NOT NULL DEFAULT 0,
 status text NOT NULL CHECK(status IN ('success','retryable_error','permanent_error')),error text,
 next_attempt_at timestamptz,scanned_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.faces (
 id uuid PRIMARY KEY,media_id uuid NOT NULL REFERENCES crm.media_assets(id),box real[] NOT NULL CHECK(array_length(box,1)=4),
 quality real NOT NULL CHECK(quality BETWEEN 0 AND 1),in_post boolean NOT NULL DEFAULT false,
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','confirmed','ignored')),
 profile_id uuid REFERENCES crm.profiles(id),suggested_profile_id uuid REFERENCES crm.profiles(id),
 rejected_profile_id uuid REFERENCES crm.profiles(id),suggestion_score real,cluster_key text,decided_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX faces_media ON crm.faces(media_id,id);
CREATE INDEX faces_review ON crm.faces(suggested_profile_id,cluster_key) WHERE status='pending' AND in_post AND quality>=0.6;
CREATE INDEX faces_confirmed ON crm.faces(profile_id,media_id) WHERE status='confirmed';
CREATE TABLE crm.face_device_runtime (
 singleton boolean PRIMARY KEY CHECK(singleton),state text NOT NULL DEFAULT 'needs_models',
 model text,error text,updated_at timestamptz NOT NULL DEFAULT now(),last_batch_at timestamptz
);
INSERT INTO crm.face_device_runtime(singleton) VALUES(true);

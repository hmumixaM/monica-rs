CREATE TABLE crm.app_users (
 id uuid PRIMARY KEY, singleton boolean NOT NULL DEFAULT true UNIQUE CHECK(singleton),
 email text NOT NULL UNIQUE, password_hash text NOT NULL,
 totp_secret text, pending_totp_secret text, last_login_at timestamptz
);
CREATE TABLE crm.sessions (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES crm.app_users(id),
 token_hash text NOT NULL UNIQUE, created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL, revoked_at timestamptz,
 user_agent text, ip_address text
);
CREATE TABLE crm.auth_recovery_codes (
 user_id uuid NOT NULL REFERENCES crm.app_users(id), code_hash text NOT NULL,
 consumed_at timestamptz, PRIMARY KEY(user_id,code_hash)
);
CREATE TABLE crm.auth_login_attempts (
 attempt_key text PRIMARY KEY, attempts integer NOT NULL, window_started_at timestamptz NOT NULL
);
CREATE TABLE crm.audit_events (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 user_id uuid REFERENCES crm.app_users(id), action text NOT NULL,
 entity_type text, entity_id uuid, metadata jsonb NOT NULL DEFAULT '{}',
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON crm.audit_events(user_id,created_at DESC,id DESC);

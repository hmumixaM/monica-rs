CREATE TABLE crm.mcp_settings (
 singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton),
 enabled boolean NOT NULL DEFAULT false,
 public_url text
);
INSERT INTO crm.mcp_settings DEFAULT VALUES;
CREATE TABLE crm.mcp_keys (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES crm.app_users(id),
 name text NOT NULL, token_hash text NOT NULL UNIQUE,
 created_at timestamptz NOT NULL DEFAULT now(), last_used_at timestamptz, revoked_at timestamptz
);

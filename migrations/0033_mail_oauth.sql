CREATE TABLE crm.mail_oauth_requests (
 state_hash text PRIMARY KEY, session_hash text NOT NULL, provider text NOT NULL CHECK(provider IN ('gmail','outlook')),
 credential_ciphertext text NOT NULL, expires_at timestamptz NOT NULL
);
CREATE INDEX ON crm.mail_oauth_requests(expires_at);

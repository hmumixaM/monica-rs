-- Shared local-byte cache; no network fetch is enabled by this migration.
CREATE TABLE crm.wechat_emoji_cache (
 md5 text PRIMARY KEY CHECK(md5 ~ '^[0-9a-f]{32}$'),media_id uuid REFERENCES crm.media_assets(id),
 state text NOT NULL CHECK(state IN ('cached','not_in_local_archive')),
 checked_at timestamptz NOT NULL DEFAULT now(),retry_after timestamptz NOT NULL DEFAULT now()+interval '1 day',
 CHECK((state='cached')=(media_id IS NOT NULL))
);
CREATE TABLE crm.wechat_emoji_messages (
 message_id uuid PRIMARY KEY REFERENCES crm.message_search(message_id),reference_fingerprint text NOT NULL,
 md5 text REFERENCES crm.wechat_emoji_cache(md5),state text NOT NULL CHECK(state IN ('cached','not_in_local_archive','no_reference')),
 checked_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX wechat_emoji_messages_scan ON crm.message_search(message_id) WHERE provider='wechat' AND message_type='47' AND NOT tombstone;

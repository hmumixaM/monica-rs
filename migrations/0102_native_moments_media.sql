-- Only the native Chatlog connection uses this queue. NAS archive imports
-- retain their existing worker and durable receipts.
CREATE TABLE crm.native_moments_media (
    event_id uuid PRIMARY KEY REFERENCES crm.timeline_events(id) ON DELETE CASCADE,
    connection_id uuid NOT NULL REFERENCES crm.source_connections(id) ON DELETE CASCADE,
    version bigint NOT NULL,
    attempts integer NOT NULL DEFAULT 0,
    retry_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX native_moments_media_due ON crm.native_moments_media(connection_id, retry_at, event_id);

CREATE FUNCTION crm.queue_native_moments_media() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.provider <> 'wechat' OR NEW.event_type <> 'post' OR NEW.connection_id IS NULL THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE' AND (OLD.version, OLD.status) IS NOT DISTINCT FROM (NEW.version, NEW.status) THEN
        RETURN NEW;
    END IF;
    IF NEW.status <> 'active' THEN
        DELETE FROM crm.native_moments_media WHERE event_id = NEW.id;
    ELSIF NEW.content->>'coordinate_format' = 'chatlog_sns_xml' AND (
        NEW.content->'media' <> '[]'::jsonb OR
        jsonb_path_exists(NEW.content, '$.interactions[*].media[*]')
    ) AND EXISTS (
        SELECT FROM crm.source_connections s WHERE s.id = NEW.connection_id
          AND s.external_account LIKE 'native-chatlog-%'
    ) THEN
        INSERT INTO crm.native_moments_media(event_id, connection_id, version)
        VALUES (NEW.id, NEW.connection_id, NEW.version)
        ON CONFLICT (event_id) DO UPDATE SET version = excluded.version,
            attempts = 0, retry_at = now()
        WHERE native_moments_media.version <> excluded.version;
    ELSE
        DELETE FROM crm.native_moments_media WHERE event_id = NEW.id;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER queue_native_moments_media AFTER INSERT OR UPDATE OF version, status
    ON crm.timeline_events FOR EACH ROW EXECUTE FUNCTION crm.queue_native_moments_media();

INSERT INTO crm.native_moments_media(event_id, connection_id, version)
SELECT e.id, e.connection_id, e.version FROM crm.timeline_events e
JOIN crm.source_connections s ON s.id = e.connection_id
WHERE e.provider = 'wechat' AND e.event_type = 'post' AND e.status = 'active'
  AND s.external_account LIKE 'native-chatlog-%'
  AND e.content->>'coordinate_format' = 'chatlog_sns_xml'
  AND (e.content->'media' <> '[]'::jsonb OR jsonb_path_exists(e.content, '$.interactions[*].media[*]'));

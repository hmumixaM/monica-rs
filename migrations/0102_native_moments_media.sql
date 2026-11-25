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
    IF NOT EXISTS (
        SELECT FROM crm.source_connections s WHERE s.id = NEW.connection_id
          AND s.external_account LIKE 'native-chatlog-%'
    ) THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE' THEN
        -- Retire only links produced by the native worker. Immutable blobs and
        -- their provenance stay available to audit earlier source revisions.
        WITH previous_media(entity_type, entity_id, items) AS (
            SELECT 'social_post', NEW.id, OLD.content->'media'
            UNION ALL
            SELECT 'social_interaction', i.id, item->'media'
            FROM crm.social_interactions i
            JOIN LATERAL jsonb_array_elements(coalesce(OLD.content->'interactions','[]')) item
                ON item->>'external_id' = i.external_id
            WHERE i.post_id = NEW.id
        ), current_media(entity_type, entity_id, items) AS (
            SELECT 'social_post', NEW.id, NEW.content->'media'
            UNION ALL
            SELECT 'social_interaction', i.id, item->'media'
            FROM crm.social_interactions i
            JOIN LATERAL jsonb_array_elements(coalesce(NEW.content->'interactions','[]')) item
                ON item->>'external_id' = i.external_id
            WHERE i.post_id = NEW.id AND item->>'is_deleted' IS DISTINCT FROM 'true'
        ), retired AS (
            DELETE FROM crm.media_links l USING previous_media original
            WHERE l.entity_type = original.entity_type AND l.entity_id = original.entity_id
              AND EXISTS (
                  SELECT FROM crm.media_provenance p WHERE p.media_id = l.media_id
                    AND p.source = 'wechat'
                    AND starts_with(p.source_record_id, 'sns-import:' || l.entity_id::text || ':')
              )
              AND (NEW.status <> 'active' OR NOT EXISTS (
                  SELECT FROM current_media cur,
                      jsonb_array_elements(coalesce(original.items,'[]')) old_slot,
                      jsonb_array_elements(coalesce(cur.items,'[]')) new_slot
                  WHERE cur.entity_type = original.entity_type AND cur.entity_id = original.entity_id
                    AND old_slot->>'role' = regexp_replace(regexp_replace(l.role, '^live-', ''), '-poster$', '')
                    AND CASE WHEN l.role LIKE 'live-%' THEN old_slot = new_slot
                             ELSE old_slot - 'live_motion_url' - 'live_motion_md5' = new_slot - 'live_motion_url' - 'live_motion_md5' END
              ))
            RETURNING l.entity_type, l.entity_id, l.media_id, l.role
        )
        DELETE FROM crm.media_slots s USING retired r
        WHERE (s.entity_type, s.entity_id, s.media_id, s.role) = (r.entity_type, r.entity_id, r.media_id, r.role);
    END IF;
    IF NEW.status = 'active' AND NEW.content->>'coordinate_format' = 'chatlog_sns_xml' AND (
        NEW.content->'media' <> '[]'::jsonb OR
        jsonb_path_exists(NEW.content, '$.interactions[*].media[*]')
    ) THEN
        INSERT INTO crm.native_moments_media(event_id, connection_id, version)
        VALUES (NEW.id, NEW.connection_id, NEW.version)
        ON CONFLICT (event_id) DO UPDATE SET version = excluded.version,
            attempts = 0, retry_at = now()
        WHERE native_moments_media.version <> excluded.version;
    ELSE
        -- An interrupted previous turn can own staged files. Give the worker
        -- a cleanup turn for revisions without any remaining media.
        UPDATE crm.native_moments_media SET version = NEW.version, attempts = 0, retry_at = now()
        WHERE event_id = NEW.id;
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

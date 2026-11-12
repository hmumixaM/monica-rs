-- Keep presentation separate from account-scoped internal conversation keys.
ALTER TABLE crm.conversations ADD COLUMN source_display_name text;
ALTER TABLE crm.conversations ADD COLUMN source_display_at timestamptz;

CREATE FUNCTION crm.conversation_source_name(kind text, payload jsonb) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT COALESCE(
    CASE WHEN kind = 'group' THEN NULLIF(btrim(payload->>'title'), '') END,
    (SELECT string_agg(label, ', ' ORDER BY position) FROM (
      SELECT position, COALESCE(NULLIF(btrim(person->>'name'), ''),
        NULLIF(person->'phones'->>0, ''), NULLIF(person->'emails'->>0, ''),
        CASE WHEN person->>'id' ~ '^\+?[0-9 ()-]{3,}$'
               OR person->>'id' ~ '^[^@: ]+@[^: ]+\.[^: ]+$'
          THEN person->>'id' END) AS label
      FROM jsonb_array_elements(COALESCE(payload->'participants', '[]'::jsonb))
        WITH ORDINALITY AS p(person, position)
      WHERE COALESCE(person->>'is_self', 'false') <> 'true'
    ) names WHERE label IS NOT NULL)
  )
$$;

WITH names AS (
  SELECT e.conversation_id, e.occurred_at, e.id,
    crm.conversation_source_name(c.conversation_type, e.metadata) AS label
  FROM crm.connection_evidence e JOIN crm.conversations c ON c.id=e.conversation_id
  WHERE e.included AND NOT e.deleted
), latest AS (
  SELECT DISTINCT ON (conversation_id) conversation_id, occurred_at, label
  FROM names WHERE label IS NOT NULL
  ORDER BY conversation_id, occurred_at DESC NULLS LAST, id DESC
)
UPDATE crm.conversations c SET source_display_name=l.label, source_display_at=l.occurred_at
FROM latest l WHERE c.id=l.conversation_id;

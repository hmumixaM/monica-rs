-- LinkedIn messages name their participants by member URN only. The inbox
-- title of a two-person conversation is the other person's name, so use it:
-- for the conversation label, and for the person shown in identity review.

-- A participant "name" equal to its id is a placeholder, not a name.
CREATE OR REPLACE FUNCTION crm.conversation_source_name(kind text, payload jsonb) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT COALESCE(
    CASE WHEN kind = 'group' THEN NULLIF(btrim(payload->>'title'), '') END,
    (SELECT string_agg(label, ', ' ORDER BY position) FROM (
      SELECT position, COALESCE(NULLIF(NULLIF(btrim(person->>'name'), ''), person->>'id'),
        NULLIF(person->'phones'->>0, ''), NULLIF(person->'emails'->>0, ''),
        CASE WHEN person->>'id' ~ '^\+?[0-9 ()-]{3,}$'
               OR person->>'id' ~ '^[^@: ]+@[^: ]+\.[^: ]+$'
          THEN person->>'id' END) AS label
      FROM jsonb_array_elements(COALESCE(payload->'participants', '[]'::jsonb))
        WITH ORDINALITY AS p(person, position)
      WHERE COALESCE(person->>'is_self', 'false') <> 'true'
    ) names WHERE label IS NOT NULL),
    NULLIF(btrim(payload->>'conversation_title'), '')
  )
$$;

WITH titled AS (
  SELECT DISTINCT ON (e.connection_id, person->>'id') e.connection_id, person->>'id' AS external_id,
    e.conversation_id, btrim(e.metadata->>'conversation_title') AS title, e.occurred_at
  FROM crm.connection_evidence e JOIN crm.source_connections c ON c.id=e.connection_id AND c.provider='linkedin',
    jsonb_array_elements(e.metadata->'participants') person
  WHERE e.kind='message' AND e.metadata->>'is_group'='false' AND COALESCE(person->>'is_self','false')<>'true'
    AND btrim(COALESCE(e.metadata->>'conversation_title',''))<>''
  ORDER BY e.connection_id, person->>'id', e.occurred_at DESC
), people AS (
  UPDATE crm.connection_people p SET display_name=t.title FROM titled t
  WHERE p.connection_id=t.connection_id AND p.external_id=t.external_id AND p.display_name=p.external_id
)
UPDATE crm.conversations c SET source_display_name=t.title,source_display_at=t.occurred_at FROM titled t
WHERE c.id=t.conversation_id AND c.conversation_type='direct';

-- Conversations whose only label is still a member URN have no name to show.
UPDATE crm.conversations SET source_display_name=NULL WHERE provider='linkedin' AND source_display_name LIKE 'urn:li:%';

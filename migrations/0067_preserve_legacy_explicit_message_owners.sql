-- Repair the one known settlement shape without replacing a chosen person or
-- a recorded person revision (an explicit JSON null is also a revision).
-- Original immutable CRM evidence and source observation versions stay intact.
WITH candidates AS MATERIALIZED (
 SELECT m.message_id,p.id AS profile_id
 FROM crm.legacy_chatlog_projection c
 JOIN crm.message_search m ON c.entity_type='message' AND c.entity_id=m.message_id
 JOIN crm.legacy_message_evidence e USING(message_id)
 JOIN crm.profiles p ON p.id::text=e.original->>'sender_profile_id'
 WHERE m.sender_profile_id IS NULL AND m.sender_account_id IS NOT NULL
   AND e.original->>'sender_account_id' IS NULL
   AND c.original_row->>'sender_account_id' IS NULL
   AND c.current_row->>'sender_account_id'=m.sender_account_id::text
   AND NOT(c.original_row?'sender_profile_id')
   AND NOT(c.current_row?'sender_profile_id')
), repaired AS (
 UPDATE crm.message_search m SET sender_profile_id=c.profile_id
 FROM candidates c WHERE m.message_id=c.message_id
 RETURNING m.message_id,m.sender_profile_id
), projections AS (
 UPDATE crm.legacy_chatlog_projection c
 SET original_row=c.original_row||jsonb_build_object('sender_profile_id',r.sender_profile_id),
     current_row=c.current_row||jsonb_build_object('sender_profile_id',r.sender_profile_id)
 FROM repaired r WHERE c.entity_type='message' AND c.entity_id=r.message_id
 RETURNING c.entity_id
)
INSERT INTO crm.memory_legacy_pending_records(kind,entity_id,profiles)
SELECT 'message',r.message_id,
 ARRAY(SELECT DISTINCT profile_id FROM (
  SELECT r.sender_profile_id AS profile_id
  UNION ALL SELECT o.profile_id FROM crm.memory_legacy_owners o
   WHERE o.kind='message' AND o.entity_id=r.message_id
 ) owners ORDER BY profile_id)
FROM repaired r
ON CONFLICT(kind,entity_id) DO UPDATE SET
 profiles=ARRAY(SELECT DISTINCT unnest(crm.memory_legacy_pending_records.profiles||excluded.profiles)),
 requested_at=clock_timestamp();

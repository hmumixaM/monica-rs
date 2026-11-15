-- Person-author streams must be filtered before pagination, independently of
-- other members' newer group messages. Preserve explicit native attribution.
CREATE INDEX message_search_person_speech_page
 ON crm.message_search(sender_profile_id,occurred_at DESC,message_id DESC)
 WHERE NOT tombstone AND sender_profile_id IS NOT NULL;
CREATE INDEX message_search_account_speech_page
 ON crm.message_search(sender_account_id,occurred_at DESC,message_id DESC)
 WHERE NOT tombstone AND sender_profile_id IS NULL AND sender_account_id IS NOT NULL;

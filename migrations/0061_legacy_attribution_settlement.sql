-- Offline settlement removes the old person projection in bounded commits.
-- Keep its remaining-work lookup bounded, including the final empty batch.
CREATE INDEX legacy_message_attribution_pending ON crm.message_search(message_id)
 WHERE sender_account_id IS NOT NULL AND sender_profile_id IS NOT NULL;

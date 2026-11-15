-- History is append-only. Index the account scope before the paging key;
-- histories across linked accounts must not scan unrelated people's versions.
CREATE INDEX field_revisions_account_sequence
    ON crm.field_revisions(source_account_id, revision_seq DESC);
CREATE INDEX field_revisions_manual_profile_sequence
    ON crm.field_revisions(profile_id, revision_seq DESC) WHERE source_account_id IS NULL;
CREATE INDEX source_structured_revisions_account_history
    ON crm.source_structured_revisions(source_account_id, observed_at DESC, id DESC);

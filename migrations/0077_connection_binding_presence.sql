-- An omitted owner is different from an explicitly unassigned owner. Existing
-- signed bindings stay authoritative; do not guess what historical NULL meant.
ALTER TABLE crm.connection_import_bindings
    ADD COLUMN infer_profile boolean NOT NULL DEFAULT false,
    ADD COLUMN infer_sender boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT binding_inferred_profile_has_no_override CHECK (NOT infer_profile OR profile_id IS NULL),
    ADD CONSTRAINT binding_inferred_sender_has_no_override CHECK (NOT infer_sender OR sender_profile_id IS NULL);

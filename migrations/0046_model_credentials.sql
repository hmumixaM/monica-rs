-- Installation-scoped authenticated encryption; never return this value in API reads.
ALTER TABLE crm.ai_settings ADD COLUMN credential_ciphertext text;

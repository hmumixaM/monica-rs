-- Models the user may pick between in Ask Monica, besides the default one.
ALTER TABLE crm.ai_settings ADD COLUMN qa_models text[] NOT NULL DEFAULT '{}';

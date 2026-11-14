ALTER TABLE crm.legacy_chatlog_versions DROP CONSTRAINT legacy_chatlog_versions_entity_type_check;
ALTER TABLE crm.legacy_chatlog_versions ADD CONSTRAINT legacy_chatlog_versions_entity_type_check
 CHECK(entity_type IN ('message','post','contact','interaction'));

-- Graph reads also resolve mentions and unmapped authors directly from facts.
-- Those writes need not change interaction_months. Queue committed tokens using
-- the existing transaction-coalesced invalidation path; do not lock source rows.
-- No historical scan is needed: the initial empty token is a valid generation.
CREATE TRIGGER graph_cache_i AFTER INSERT ON crm.social_interactions REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,post_id subject FROM changed');
CREATE TRIGGER graph_cache_u AFTER UPDATE ON crm.social_interactions REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,post_id subject FROM changed');
CREATE TRIGGER graph_cache_d AFTER DELETE ON crm.social_interactions REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,post_id subject FROM changed');
CREATE TRIGGER graph_cache_i AFTER INSERT ON crm.timeline_events REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,id subject FROM changed');
CREATE TRIGGER graph_cache_u AFTER UPDATE ON crm.timeline_events REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,id subject FROM changed');
CREATE TRIGGER graph_cache_d AFTER DELETE ON crm.timeline_events REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,id subject FROM changed');
CREATE TRIGGER graph_cache_i AFTER INSERT ON crm.source_relationship_edges REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,id subject FROM changed');
CREATE TRIGGER graph_cache_u AFTER UPDATE ON crm.source_relationship_edges REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,id subject FROM changed');
CREATE TRIGGER graph_cache_d AFTER DELETE ON crm.source_relationship_edges REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,id subject FROM changed');
CREATE TRIGGER graph_cache_i AFTER INSERT ON crm.legacy_relationship_owners REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,edge_id subject FROM changed');
CREATE TRIGGER graph_cache_u AFTER UPDATE ON crm.legacy_relationship_owners REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,edge_id subject FROM changed');
CREATE TRIGGER graph_cache_d AFTER DELETE ON crm.legacy_relationship_owners REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''graph'' domain,edge_id subject FROM changed');

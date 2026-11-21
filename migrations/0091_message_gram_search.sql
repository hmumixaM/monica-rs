-- Substring search over message text through an index.
--
-- The word index this replaces was never read: the 'simple' parser keeps a run
-- of Chinese as a single word, so every search fell back to scanning bodies.
-- Any body that contains a search term also contains each two-character piece
-- of its Chinese runs and each three-character piece of its letter and digit
-- runs. Indexing those pieces narrows a search to candidate rows; the exact
-- substring test still decides the match.

-- Overlapping pieces come from matching the same text at each starting offset:
-- once as is, and again after dropping the first one (or two) characters of
-- every run. Only the first 20,000 characters of a body are indexed.
CREATE FUNCTION crm.search_grams(body text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
  WITH s AS (SELECT lower(left(body, 20000)) t),
  shifted AS (SELECT t,
    regexp_replace(t, '(^|[^\u3400-\u9fff\u3040-\u30ff\uac00-\ud7af])[\u3400-\u9fff\u3040-\u30ff\uac00-\ud7af]', '\1 ', 'g') c1,
    regexp_replace(t, '(^|[^a-z0-9])[a-z0-9]', '\1 ', 'g') l1 FROM s)
  SELECT array_to_tsvector(coalesce(array_agg(DISTINCT m[1]), '{}')) FROM shifted, LATERAL (
    SELECT regexp_matches(t, '[\u3400-\u9fff\u3040-\u30ff\uac00-\ud7af]{2}|[a-z0-9]{3}', 'g') m
    UNION ALL SELECT regexp_matches(c1, '[\u3400-\u9fff\u3040-\u30ff\uac00-\ud7af]{2}', 'g')
    UNION ALL SELECT regexp_matches(l1, '[a-z0-9]{3}', 'g')
    UNION ALL SELECT regexp_matches(regexp_replace(l1, '(^|[^a-z0-9])[a-z0-9]', '\1 ', 'g'), '[a-z0-9]{3}', 'g')) pieces
$$;

-- Every piece of the term must be present. NULL when the term is too short to
-- have any piece (one Chinese character, or fewer than three letters).
CREATE FUNCTION crm.search_gram_query(term text) RETURNS tsquery
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
  SELECT CASE WHEN cardinality(pieces) > 0
    THEN array_to_string(ARRAY(SELECT quote_literal(piece) FROM unnest(pieces) piece), ' & ')::tsquery END
  FROM (SELECT tsvector_to_array(crm.search_grams(term)) pieces) s
$$;

DROP INDEX crm.message_search_to_tsvector_idx;
CREATE INDEX message_search_grams ON crm.message_search USING gin(crm.search_grams(body));

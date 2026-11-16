-- Expensive message aggregates are durable weekly snapshots, never GET work.
-- Names, visibility, ownership and reminder dates remain current reads.
CREATE TABLE crm.weekly_message_statistics (
 id boolean PRIMARY KEY DEFAULT true CHECK(id),
 week_start date NOT NULL,
 refreshed_at timestamptz NOT NULL,
 contact_counts jsonb NOT NULL,
 month_counts jsonb NOT NULL,
 recent_conversations jsonb NOT NULL
);
CREATE TRIGGER cache_i AFTER INSERT ON crm.weekly_message_statistics REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''weekly-statistics'' domain,''00000000-0000-0000-0000-000000000000''::uuid subject FROM changed');
CREATE TRIGGER cache_u AFTER UPDATE ON crm.weekly_message_statistics REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''weekly-statistics'' domain,''00000000-0000-0000-0000-000000000000''::uuid subject FROM changed');

CREATE FUNCTION crm.refresh_weekly_message_statistics() RETURNS boolean LANGUAGE plpgsql AS $$
DECLARE this_week date=date_trunc('week',now() AT TIME ZONE 'America/New_York')::date;
BEGIN
 IF EXISTS(SELECT 1 FROM crm.weekly_message_statistics WHERE id AND week_start=this_week)
 OR NOT pg_try_advisory_xact_lock(hashtextextended('crm.weekly-message-statistics',0)) THEN RETURN false; END IF;
 INSERT INTO crm.weekly_message_statistics(id,week_start,refreshed_at,contact_counts,month_counts,recent_conversations)
 WITH cutoff AS(SELECT now()-interval '30 days' t),recent AS(
  SELECT d.conversation_id,sum(d.sent+d.received)::bigint n
  FROM crm.message_stat_counts d,cutoff
  WHERE d.scope='day:America/Chicago' AND d.period>((cutoff.t AT TIME ZONE 'America/Chicago')::date)::text
  GROUP BY d.conversation_id
  UNION ALL
  SELECT m.conversation_id,count(*) FROM (
   -- Fence the time range before classifying messages; avoid the full-table
   -- scan selected for a classification predicate on the outer cutoff join.
   SELECT m.conversation_id,m.direction,m.sender,m.account,m.message_type
   FROM crm.message_search m,cutoff WHERE NOT m.tombstone AND m.occurred_at>=cutoff.t
   AND m.occurred_at<((cutoff.t AT TIME ZONE 'America/Chicago')::date+1)::timestamp AT TIME ZONE 'America/Chicago'
   OFFSET 0
  ) m WHERE crm.message_stat_kind(m.direction,m.sender,m.account,m.message_type) IN('sent','received')
  GROUP BY m.conversation_id
 ),totals AS(SELECT conversation_id,sum(n)::bigint n FROM recent GROUP BY conversation_id)
 SELECT true,this_week,now(),
  coalesce((SELECT jsonb_agg(jsonb_build_object('profile_id',profile_id,'direct_message_count',direct_message_count,'group_message_count',group_message_count)) FROM crm.page_contact_stats),'[]'::jsonb),
  coalesce((SELECT jsonb_agg(to_jsonb(m)) FROM crm.page_profile_months m WHERE month>=to_char(date_trunc('month',now() AT TIME ZONE 'America/Chicago')-interval '12 months','YYYY-MM')),'[]'::jsonb),
  coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM totals t),'[]'::jsonb)
 ON CONFLICT(id) DO UPDATE SET week_start=excluded.week_start,refreshed_at=excluded.refreshed_at,
 contact_counts=excluded.contact_counts,month_counts=excluded.month_counts,recent_conversations=excluded.recent_conversations;
 RETURN true;
END $$;

CREATE VIEW crm.weekly_contact_stats AS
 WITH counts AS(SELECT v.* FROM crm.weekly_message_statistics w CROSS JOIN LATERAL
  jsonb_to_recordset(w.contact_counts) v(profile_id uuid,direct_message_count bigint,group_message_count bigint)),
 current_contacts AS(SELECT c.profile_id,
  min(c.first_message_at) FILTER(WHERE c.conversation_type='direct') first_contact_at,
  max(c.last_message_at) FILTER(WHERE c.conversation_type='direct') last_contact_at,
  max(c.last_message_at) FILTER(WHERE c.conversation_type='group') last_group_at
  FROM crm.conversations c WHERE c.profile_id IS NOT NULL GROUP BY c.profile_id)
 SELECT c.*,coalesce(s.direct_message_count,0)::bigint direct_message_count,
 coalesce(s.group_message_count,0)::bigint group_message_count
 FROM current_contacts c LEFT JOIN counts s ON s.profile_id=c.profile_id;
CREATE VIEW crm.weekly_profile_months AS
 SELECT v.* FROM crm.weekly_message_statistics w CROSS JOIN LATERAL
 jsonb_to_recordset(w.month_counts) v(profile_id uuid,month text,sent bigint,received bigint);

-- Prime before the first web read, including after a restart/deployment.
SELECT crm.refresh_weekly_message_statistics();

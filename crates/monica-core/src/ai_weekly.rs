//! Weekly counts and dates are database facts, never values invented by a model.
use anyhow::Result;
use chrono::NaiveDate;
use serde_json::Value;
use sqlx::{Postgres, Transaction};

pub async fn facts(tx: &mut Transaction<'_, Postgres>, week: NaiveDate) -> Result<Value> {
    Ok(sqlx::query_scalar(r#"
WITH bounds AS (SELECT $1::date first_day,($1::date+7) next_day),
counts AS (SELECT c.profile_id,sum(d.sent+d.received)::bigint messages
 FROM crm.conversation_days d JOIN crm.conversations c ON c.id=d.conversation_id,bounds w
 WHERE d.day>=w.first_day AND d.day<w.next_day AND c.conversation_type='direct' AND c.profile_id IS NOT NULL GROUP BY c.profile_id),
calls AS (SELECT d.profile_id,sum(d.count)::bigint calls FROM crm.contact_days d,bounds w
 WHERE d.kind='call' AND d.day>=w.first_day AND d.day<w.next_day GROUP BY d.profile_id),
top_contacts AS (SELECT p.id profile_id,p.display_name name,p.avatar_media_id,coalesce(c.messages,0) messages,coalesce(k.calls,0) calls
 FROM counts c FULL JOIN calls k USING(profile_id) JOIN crm.profiles p ON p.id=coalesce(c.profile_id,k.profile_id)
 WHERE p.profile_type='person' AND p.archived_at IS NULL ORDER BY coalesce(c.messages,0)+10*coalesce(k.calls,0) DESC,p.id LIMIT 8),
new_people AS (SELECT p.id profile_id,p.display_name name FROM crm.profiles p,bounds w
 WHERE p.profile_type='person' AND p.archived_at IS NULL AND p.created_at>=w.first_day::timestamp AT TIME ZONE 'America/Chicago' AND p.created_at<w.next_day::timestamp AT TIME ZONE 'America/Chicago' ORDER BY p.created_at DESC,p.id LIMIT 12),
new_links AS (SELECT p.id profile_id,p.display_name name,a.provider FROM crm.source_account_links l JOIN crm.source_accounts a ON a.id=l.account_id JOIN crm.profiles p ON p.id=l.profile_id,bounds w
 WHERE p.archived_at IS NULL AND l.linked_at>=w.first_day::timestamp AT TIME ZONE 'America/Chicago' AND l.linked_at<w.next_day::timestamp AT TIME ZONE 'America/Chicago' ORDER BY l.linked_at DESC,a.id LIMIT 12),
date_candidates AS (SELECT d.*,p.display_name name,p.avatar_media_id,w.next_day,
 make_date(y.candidate_year,d.month,1)+d.day-1 occurrence
 FROM crm.profile_dates d JOIN crm.profiles p ON p.id=d.profile_id,bounds w,
 LATERAL (SELECT extract(year FROM w.next_day)::int AS candidate_year UNION ALL SELECT extract(year FROM w.next_day)::int+1) y
 WHERE d.month IS NOT NULL AND d.day IS NOT NULL AND p.archived_at IS NULL),
upcoming_dates AS (SELECT jsonb_strip_nulls(jsonb_build_object('profile_id',profile_id,'name',name,'avatar_media_id',avatar_media_id,'kind',kind,'label',label,'date',occurrence,'days_until',occurrence-next_day,'years',CASE WHEN date_precision='exact' AND kind<>'other' AND extract(year FROM occurrence)::int>year THEN extract(year FROM occurrence)::int-year END)) value
 FROM date_candidates WHERE occurrence>=next_day AND occurrence<=next_day+6 ORDER BY occurrence,name,profile_id),
newly_dormant AS (SELECT p.id profile_id,p.display_name name,s.last_contact_at,s.direct_message_count messages FROM crm.page_contact_stats s JOIN crm.profiles p ON p.id=s.profile_id,bounds w
 WHERE p.profile_type='person' AND p.archived_at IS NULL AND s.direct_message_count>=100
 AND s.last_contact_at>=(w.first_day-90)::timestamp AT TIME ZONE 'America/Chicago' AND s.last_contact_at<(w.next_day-90)::timestamp AT TIME ZONE 'America/Chicago' ORDER BY s.direct_message_count DESC,p.id LIMIT 12)
SELECT jsonb_build_object('week_start',$1::date,'totals',jsonb_build_object(
 'messages',coalesce((SELECT sum(d.sent+d.received) FROM crm.conversation_days d,bounds w WHERE d.day>=w.first_day AND d.day<w.next_day),0),
 'people',(SELECT count(*) FROM counts),
 'posts',(SELECT count(*) FROM crm.page_posts p,bounds w WHERE p.kind='post' AND p.occurred_at>=w.first_day::timestamp AT TIME ZONE 'America/Chicago' AND p.occurred_at<w.next_day::timestamp AT TIME ZONE 'America/Chicago')),
 'top_contacts',coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM top_contacts t),'[]'),
 'new_people',coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM new_people t),'[]'),
 'new_links',coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM new_links t),'[]'),
 'upcoming_dates',coalesce((SELECT jsonb_agg(value) FROM upcoming_dates),'[]'),
 'newly_dormant',coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM newly_dormant t),'[]'))
"#).bind(week).fetch_one(&mut **tx).await?)
}

use super::*;
use crate::auth::user;
use axum::{
    Json,
    extract::{Path, Query, State},
    routing::{get, put},
};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/home", get(home))
        .route("/api/v1/home/dates", get(home_dates))
        .route("/api/v1/dates/birthdays", get(birthdays))
        .route("/api/v1/insights", get(insights))
        .route("/api/v1/insights/calendar", get(calendar))
        .route("/api/v1/insights/calendar/day", get(day))
        .route("/api/v1/profiles/{id}/ribbon", get(ribbon))
        .route("/api/v1/insights/orbit", get(orbit))
        .route("/api/v1/communities/{id}/name", put(rename).delete(reset))
}
async fn home(State(s): State<AppState>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    // Reads use the durable weekly snapshot; message history is worker-only.
    let value:Value=sqlx::query_scalar(r#"WITH recent AS(
 SELECT v.* FROM crm.weekly_message_statistics w CROSS JOIN LATERAL
 jsonb_to_recordset(w.recent_conversations) v(conversation_id uuid,n bigint))
 SELECT jsonb_build_object('statistics_updated_at',(SELECT refreshed_at FROM crm.weekly_message_statistics WHERE id),'key_numbers',jsonb_build_object('contacts',(SELECT count(*) FROM crm.directory_profiles WHERE archived_at IS NULL AND profile_type='person'),'groups',(SELECT count(*) FROM crm.directory_profiles WHERE archived_at IS NULL AND profile_type='group'),'messages_30d',coalesce((SELECT sum(n) FROM recent),0),'active_contacts_30d',(SELECT count(DISTINCT c.profile_id) FROM recent r JOIN crm.conversations c ON c.id=r.conversation_id WHERE r.n>0 AND c.conversation_type='direct')))"#).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(value))
}
async fn upcoming_dates(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Value>> {
    // Restrict owners before hydrating their directory rows.
    Ok(sqlx::query_scalar(r#"WITH upcoming AS MATERIALIZED (SELECT * FROM crm.profile_dates_presented WHERE (value->>'days_until')::int BETWEEN 0 AND 60)
 SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'kind',d.kind,'label',d.value->'label','date',d.value->'next','days_until',d.value->'days_until','years',CASE WHEN (d.value->>'years_exact')::boolean THEN extract(year from (d.value->>'next')::date)::int-(d.value->>'year')::int END) FROM upcoming d JOIN crm.directory_profiles p ON p.id=d.profile_id AND p.archived_at IS NULL WHERE p.id=ANY(ARRAY(SELECT profile_id FROM upcoming)) ORDER BY (d.value->>'days_until')::int,p.display_name"#).fetch_all(&mut **tx).await?)
}
async fn home_dates(State(s): State<AppState>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let dates = upcoming_dates(&mut tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"upcoming_dates":dates})))
}

#[derive(Deserialize)]
struct BirthdayPage {
    month: Option<i32>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn birthdays(
    State(s): State<AppState>,
    Query(q): Query<BirthdayPage>,
) -> Result<Json<Value>> {
    if q.month.is_some_and(|month| !(1..=12).contains(&month)) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid birthday month",
        ));
    }
    let mut tx = snapshot(&s).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 100);
    let offset = q.offset.unwrap_or(0).max(0);
    let rows: Vec<Value> = sqlx::query_scalar(
        r#"SELECT jsonb_build_object(
        'profile_id',p.id,'name',p.display_name,'year',d.value->'year','month',d.month,'day',d.day,
        'precision',d.value->'precision','next',d.value->'next','days_until',d.value->'days_until',
        'link','/profiles/'||p.id::text)
        FROM crm.profile_dates_presented d JOIN crm.profiles p ON p.id=d.profile_id
        WHERE d.kind='birthday' AND p.archived_at IS NULL AND p.profile_type='person'
        AND ($1::int IS NULL OR d.month=$1)
        ORDER BY d.month NULLS LAST,d.day NULLS LAST,p.display_name,p.id LIMIT $2 OFFSET $3"#,
    )
    .bind(q.month)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    let total: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM crm.profile_dates d
        JOIN crm.profiles p ON p.id=d.profile_id WHERE d.kind='birthday'
        AND p.archived_at IS NULL AND p.profile_type='person' AND ($1::int IS NULL OR d.month=$1)"#,
    )
    .bind(q.month)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let end = offset.saturating_add(limit);
    Ok(Json(
        json!({"rows":rows,"total":total,"next_offset":(end<total).then_some(end)}),
    ))
}
#[derive(Deserialize)]
struct Insight {
    dormant_days: Option<i32>,
}
async fn insights(State(s): State<AppState>, Query(q): Query<Insight>) -> Result<Json<Value>> {
    let threshold = q.dormant_days.unwrap_or(90);
    if ![90, 180, 365].contains(&threshold) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid dormant_days",
        ));
    }
    let mut tx = snapshot(&s).await?;
    // Expand each weekly snapshot once for all selected contacts.
    let people:Vec<Value>=sqlx::query_scalar(r#"WITH cached_stats AS MATERIALIZED(SELECT * FROM crm.weekly_contact_stats),cached_months AS MATERIALIZED(SELECT * FROM crm.weekly_profile_months) SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'last_contact_at',s.last_contact_at,'last_group_at',s.last_group_at,'direct_message_count',s.direct_message_count,'group_message_count',s.group_message_count,'calls',coalesce((SELECT sum(count) FROM crm.contact_days d WHERE d.profile_id=p.id AND d.kind='call'),0),'dormant',s.last_contact_at<now()-make_interval(days=>$1),'months',coalesce((SELECT jsonb_agg(jsonb_build_object('month',months.month,'sent',coalesce(m.sent,0),'received',coalesce(m.received,0)) ORDER BY months.month) FROM(SELECT to_char(month,'YYYY-MM') AS month FROM generate_series(date_trunc('month',now() AT TIME ZONE 'America/Chicago')-interval '11 months',date_trunc('month',now() AT TIME ZONE 'America/Chicago'),interval '1 month') month) months LEFT JOIN cached_months m ON m.profile_id=p.id AND m.month=months.month),'[]'))
 FROM cached_stats s JOIN crm.directory_profiles p ON p.id=s.profile_id AND p.archived_at IS NULL AND p.profile_type='person'
 WHERE p.id IN (SELECT profile_id FROM cached_stats ORDER BY last_contact_at DESC NULLS LAST LIMIT 30)
 OR p.id IN(SELECT s.profile_id FROM cached_stats s WHERE s.last_contact_at<now()-make_interval(days=>$1) ORDER BY s.direct_message_count+10*coalesce((SELECT sum(count) FROM crm.contact_days d WHERE d.profile_id=s.profile_id AND d.kind='call'),0) DESC,s.last_contact_at DESC LIMIT 60)"#).bind(threshold).fetch_all(&mut *tx).await?;
    let mut recent = people.clone();
    recent.sort_by(|a, b| {
        b["last_contact_at"]
            .as_str()
            .cmp(&a["last_contact_at"].as_str())
    });
    recent.truncate(30);
    let mut dormant: Vec<_> = people
        .into_iter()
        .filter(|p| p["dormant"] == true)
        .collect();
    dormant.sort_by_key(|p| {
        std::cmp::Reverse(
            p["direct_message_count"].as_i64().unwrap_or(0) + 10 * p["calls"].as_i64().unwrap_or(0),
        )
    });
    dormant.truncate(60);
    for p in recent.iter_mut().chain(dormant.iter_mut()) {
        p.as_object_mut().unwrap().remove("dormant");
    }
    let upcoming = upcoming_dates(&mut tx).await?;
    let dates:Vec<Value>=sqlx::query_scalar("WITH d AS(SELECT d.*,make_date(extract(year from ((now() AT TIME ZONE 'America/Chicago')::date-7))::int,d.month,1)+d.day-1 AS date FROM crm.profile_dates d WHERE d.month IS NOT NULL AND d.day IS NOT NULL) SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'kind',d.kind,'label',d.label,'date',d.date,'days_ago',(now() AT TIME ZONE 'America/Chicago')::date-d.date) FROM d JOIN crm.directory_profiles p ON p.id=d.profile_id AND p.archived_at IS NULL WHERE d.date BETWEEN (now() AT TIME ZONE 'America/Chicago')::date-7 AND (now() AT TIME ZONE 'America/Chicago')::date-1 ORDER BY d.date DESC,p.display_name").fetch_all(&mut *tx).await?;
    let posters:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'posts',count(*) FILTER(WHERE e.kind='post'),'stories',count(*) FILTER(WHERE e.kind='story'),'latest_at',max(e.occurred_at),'providers',jsonb_agg(DISTINCT e.provider)) FROM crm.page_posts e JOIN crm.directory_profiles p ON p.id=e.profile_id AND p.archived_at IS NULL WHERE e.occurred_at>=now()-interval '14 days' GROUP BY p.id,p.display_name,p.avatar_media_id ORDER BY max(e.occurred_at) DESC LIMIT 60").fetch_all(&mut *tx).await?;
    let companies = organizations(&mut tx, "company").await?;
    let schools = organizations(&mut tx, "school").await?;
    let statistics_updated_at = message_snapshot_at(&mut tx).await?;
    tx.commit().await?;
    Ok(Json(monica_core::pages::compact(
        json!({"statistics_updated_at":statistics_updated_at,"dormant_days":threshold,"dormant_contacts":dormant,"recent_contacts":recent,"upcoming_dates":upcoming,"recent_dates":dates,"recent_posters":posters,"top_companies":companies,"top_schools":schools}),
    )))
}
async fn organizations(tx: &mut Transaction<'_, Postgres>, kind: &str) -> Result<Vec<Value>> {
    let rows:Vec<(Uuid,String,i64)>=sqlx::query_as("SELECT CASE WHEN $1='company' THEN f.company_id ELSE f.school_id END id,CASE WHEN $1='company' THEN f.company_name ELSE f.school_name END name,count(DISTINCT f.profile_id) FROM crm.profile_structured_current f JOIN crm.directory_profiles p ON p.id=f.profile_id AND p.archived_at IS NULL AND p.profile_type='person' WHERE CASE WHEN $1='company' THEN f.company_id ELSE f.school_id END IS NOT NULL GROUP BY 1,2 ORDER BY count(DISTINCT f.profile_id) DESC,2 LIMIT 10").bind(kind).fetch_all(&mut **tx).await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, people)| json!({"id":id,"name":name,"people":people}))
        .collect())
}
#[derive(Deserialize)]
struct Calendar {
    year: Option<i32>,
}
async fn message_snapshot_at(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    Ok(
        sqlx::query_scalar("SELECT refreshed_at FROM crm.weekly_message_statistics WHERE id")
            .fetch_optional(&mut **tx)
            .await?,
    )
}
async fn calendar(State(s): State<AppState>, Query(q): Query<Calendar>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let current: i32 =
        sqlx::query_scalar("SELECT extract(year from now() AT TIME ZONE 'America/Chicago')::int")
            .fetch_one(&mut *tx)
            .await?;
    let year = q.year.unwrap_or(current);
    if !(2000..=current).contains(&year) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "year is out of range",
        ));
    }
    let days:Vec<Value>=sqlx::query_scalar("WITH contacts AS(SELECT d.day,c.profile_id FROM crm.weekly_message_days d JOIN crm.conversations c ON c.id=d.conversation_id WHERE c.conversation_type='direct' AND d.sent+d.received>0 AND d.day>=make_date($1,1,1) AND d.day<make_date($1+1,1,1) UNION SELECT day,profile_id FROM crm.contact_days WHERE kind='call' AND count>0 AND day>=make_date($1,1,1) AND day<make_date($1+1,1,1)) SELECT jsonb_build_object('date',c.day,'people',count(DISTINCT c.profile_id)) FROM contacts c JOIN crm.directory_profiles p ON p.id=c.profile_id AND p.archived_at IS NULL AND p.profile_type='person' GROUP BY c.day ORDER BY c.day").bind(year).fetch_all(&mut *tx).await?;
    let first:Option<i32>=sqlx::query_scalar("SELECT extract(year from min(day))::int FROM(SELECT min(day) AS day FROM crm.weekly_message_days WHERE sent+received>0 UNION ALL SELECT min(day) FROM crm.contact_days WHERE kind='call' AND count>0) days").fetch_one(&mut *tx).await?;
    let message_snapshot_at = message_snapshot_at(&mut tx).await?;
    tx.commit().await?;
    let max = days
        .iter()
        .filter_map(|d| d["people"].as_i64())
        .max()
        .unwrap_or(0);
    Ok(Json(
        json!({"year":year,"years":(first.unwrap_or(year).min(year)..=current).collect::<Vec<_>>(),"days":days,"max":max,"message_snapshot_at":message_snapshot_at}),
    ))
}
#[derive(Deserialize)]
struct Day {
    day: chrono::NaiveDate,
}
async fn day(State(s): State<AppState>, Query(q): Query<Day>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let people:Vec<Value>=sqlx::query_scalar("WITH contacts AS(SELECT c.profile_id,d.sent+d.received messages,0::bigint calls FROM crm.weekly_message_days d JOIN crm.conversations c ON c.id=d.conversation_id WHERE c.conversation_type='direct' AND d.day=$1::text::date AND d.sent+d.received>0 UNION ALL SELECT profile_id,0,count FROM crm.contact_days WHERE day=$1::text::date AND kind='call' AND count>0) SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'messages',sum(c.messages),'calls',sum(c.calls)) FROM contacts c JOIN crm.directory_profiles p ON p.id=c.profile_id AND p.archived_at IS NULL AND p.profile_type='person' GROUP BY p.id,p.display_name,p.avatar_media_id ORDER BY sum(c.calls) DESC,sum(c.messages) DESC,p.display_name").bind(q.day.to_string()).fetch_all(&mut *tx).await?;
    let message_snapshot_at = message_snapshot_at(&mut tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"date":q.day,"people":people,"message_snapshot_at":message_snapshot_at}),
    ))
}
pub(super) async fn ribbon_value(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value> {
    let zone = "America/Chicago";
    let messages = crate::messages::stats::rows(tx, id, zone).await?;
    let months:Vec<Value>=sqlx::query_scalar(r#"WITH messages AS(SELECT month,sent,received FROM jsonb_to_recordset($3) AS x(month text,sent bigint,received bigint)),m AS(
 SELECT month,sent,received,0::bigint calls,0::bigint interactions FROM messages
 UNION ALL SELECT to_char(occurred_at AT TIME ZONE $2,'YYYY-MM'),0,0,count(*),0 FROM crm.contact_events WHERE profile_id=$1 AND kind='call' AND occurred_at IS NOT NULL GROUP BY 1
 UNION ALL SELECT to_char(i.occurred_at AT TIME ZONE $2,'YYYY-MM'),0,0,0,count(*) FROM crm.social_interactions i JOIN crm.page_posts p ON p.id=i.post_id WHERE i.author_profile_id=$1 AND p.profile_id IS DISTINCT FROM i.author_profile_id AND i.interaction_type IN('like','comment','reply') AND i.occurred_at IS NOT NULL GROUP BY 1)
 SELECT jsonb_build_object('month',month,'sent',sum(sent),'received',sum(received),'calls',sum(calls),'interactions',sum(interactions)) FROM m GROUP BY month ORDER BY month"#).bind(id).bind(zone).bind(json!(messages)).fetch_all(&mut **tx).await?;
    Ok(json!({"timezone":zone,"months":months}))
}
async fn ribbon(State(s): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let id = canonical(&mut tx, id).await?;
    let value = ribbon_value(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(value))
}

async fn orbit(State(s): State<AppState>) -> Result<Json<Value>> {
    let initial_version = super::cache::version(&s, "/api/v1/insights/orbit").await?;
    let mut tx = snapshot(&s).await?;
    let mut people:Vec<Value>=sqlx::query_scalar(r#"WITH m AS(SELECT profile_id,sum(sent+received) FILTER(WHERE month>=to_char(date_trunc('month',now() AT TIME ZONE 'America/Chicago')-interval '2 months','YYYY-MM')) short,sum(sent+received) long FROM crm.weekly_profile_months WHERE month>=to_char(date_trunc('month',now() AT TIME ZONE 'America/Chicago')-interval '11 months','YYYY-MM') GROUP BY profile_id), c AS(SELECT profile_id,sum(count) FILTER(WHERE day>=(date_trunc('month',now() AT TIME ZONE 'America/Chicago')-interval '2 months')::date) short,sum(count) long FROM crm.contact_days WHERE kind='call' AND day>=(date_trunc('month',now() AT TIME ZONE 'America/Chicago')-interval '11 months')::date GROUP BY profile_id)
 SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'last_contact_at',s.last_contact_at,'calls',coalesce(c.long,0),'messages',jsonb_build_object('3',coalesce(m.short,0),'12',coalesce(m.long,0)),'score_short',coalesce(m.short,0)+10*coalesce(c.short,0),'score_long',coalesce(m.long,0)+10*coalesce(c.long,0),'ring',jsonb_build_object('3',null,'12',null)) FROM crm.directory_profiles p LEFT JOIN m ON m.profile_id=p.id LEFT JOIN c ON c.profile_id=p.id LEFT JOIN crm.weekly_contact_stats s ON s.profile_id=p.id WHERE p.profile_type='person' AND p.archived_at IS NULL AND(coalesce(m.long,0)+coalesce(c.long,0)>0)"#).fetch_all(&mut *tx).await?;
    for (key, window) in [("score_short", "3"), ("score_long", "12")] {
        let mut ranking: Vec<usize> = (0..people.len())
            .filter(|i| people[*i][key].as_i64().unwrap_or(0) > 0)
            .collect();
        ranking.sort_by(|a, b| {
            people[*b][key]
                .as_i64()
                .cmp(&people[*a][key].as_i64())
                .then_with(|| {
                    people[*a]["name"]
                        .as_str()
                        .cmp(&people[*b]["name"].as_str())
                })
                .then_with(|| {
                    people[*a]["profile_id"]
                        .as_str()
                        .cmp(&people[*b]["profile_id"].as_str())
                })
        });
        for (rank, i) in ranking.into_iter().take(150).enumerate() {
            people[i]["ring"][window] =
                json!([5, 15, 50, 150].iter().position(|n| rank < *n).unwrap());
        }
    }
    people.retain(|p| !p["ring"]["3"].is_null() || !p["ring"]["12"].is_null());
    people.sort_by(|a, b| {
        b["score_long"]
            .as_i64()
            .cmp(&a["score_long"].as_i64())
            .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });
    let ids: Vec<Uuid> = people
        .iter()
        .map(|p| Uuid::parse_str(p["profile_id"].as_str().unwrap()).unwrap())
        .collect();
    let index: BTreeMap<Uuid, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let memberships:Vec<(Uuid,String,i64,Uuid)>=sqlx::query_as("WITH small AS(SELECT group_profile_id,count(DISTINCT person_profile_id) size FROM crm.current_group_memberships WHERE left_at IS NULL GROUP BY group_profile_id HAVING count(DISTINCT person_profile_id)<=80) SELECT DISTINCT g.id,g.display_name,small.size,m.person_profile_id FROM crm.current_group_memberships m JOIN small USING(group_profile_id) JOIN crm.directory_profiles g ON g.id=m.group_profile_id AND g.archived_at IS NULL AND g.profile_type='group' WHERE m.left_at IS NULL AND m.person_profile_id=ANY($1)").bind(&ids).fetch_all(&mut *tx).await?;
    let mut groups = BTreeMap::<Uuid, (String, i64, BTreeSet<usize>)>::new();
    for (g, name, size, p) in memberships {
        groups
            .entry(g)
            .or_insert((name, size, BTreeSet::new()))
            .2
            .insert(index[&p]);
    }
    let mut graph = vec![vec![0.0; ids.len()]; ids.len()];
    for (_, size, members) in groups.values() {
        for &a in members {
            for &b in members {
                if a != b {
                    graph[a][b] += 1.0 / (*size as f64);
                }
            }
        }
    }
    let edges:Vec<(Uuid,Uuid)>=sqlx::query_as("SELECT from_profile_id,to_profile_id FROM crm.presented_relationship_edges WHERE from_profile_id=ANY($1) AND to_profile_id=ANY($1) AND (valid_to IS NULL OR valid_to>now())").bind(&ids).fetch_all(&mut *tx).await?;
    for (a, b) in edges {
        graph[index[&a]][index[&b]] += 2.;
        graph[index[&b]][index[&a]] += 2.;
    }
    let labels: BTreeMap<String, String> =
        sqlx::query_as::<_, (String, String)>("SELECT community_id,label FROM crm.community_names")
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
    tx.commit().await?;
    let mut found = monica_core::pages::communities(&graph);
    found.sort_by_key(|g| (std::cmp::Reverse(g.len()), g.iter().map(|i| ids[*i]).min()));
    let mut communities = Vec::new();
    let mut inputs = Vec::new();
    let mut other = 0;
    let mut used = BTreeMap::<String, usize>::new();
    for members in found {
        if members.len() == 1 {
            people[members[0]]["community"] = json!("other");
            other += 1;
            continue;
        }
        let top = groups
            .iter()
            .map(|(id, (name, _, m))| (*id, name, members.iter().filter(|i| m.contains(i)).count()))
            .filter(|(_, _, n)| *n > 1)
            .max_by(|a, b| a.2.cmp(&b.2).then_with(|| b.0.cmp(&a.0)));
        let base = top
            .map(|(id, _, _)| format!("g:{id}"))
            .unwrap_or_else(|| format!("c:{}", members.iter().map(|i| ids[*i]).min().unwrap()));
        let count = used.entry(base.clone()).or_default();
        *count += 1;
        let key = if *count > 1 {
            format!("{base}:{count}")
        } else {
            base
        };
        let label = labels.get(&key).cloned().unwrap_or_else(|| {
            top.map(|(_, name, _)| name.clone())
                .unwrap_or_else(|| "相互认识的一群人".into())
        });
        let mut community = json!({"id":key,"label":label,"size":members.len()});
        if labels.contains_key(&key) {
            community["label_source"] = json!("manual");
        }
        let mut member_inputs: Vec<Value> = members
            .iter()
            .map(|i| json!({"id":people[*i]["profile_id"],"name":people[*i]["name"]}))
            .collect();
        member_inputs.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let input = json!({"community_id":key,"members":member_inputs,"shared_group":top.map(|(_,name,_)|name),"evidence":[]});
        let hash =
            monica_core::identity::bytes_hash(&serde_json::to_vec(&input).expect("community JSON"));
        inputs.push(json!({"community_id":key,"input":input,"version_hash":hash}));
        communities.push(community);
        for i in members {
            people[i]["community"] = json!(key);
        }
    }
    if other > 0 {
        let mut c = json!({"id":"other","label":labels.get("other").map(String::as_str).unwrap_or("其他"),"size":other});
        if labels.contains_key("other") {
            c["label_source"] = json!("manual");
        }
        communities.push(c);
    }
    // A stale graph computation cannot replace a newer snapshot. Use a brief
    // publication lock only after the read transaction and CPU work have ended.
    let mut publish = s.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(846028474212)")
        .execute(&mut *publish)
        .await?;
    if super::cache::version(&s, "/api/v1/insights/orbit").await? == initial_version {
        sqlx::query("INSERT INTO crm.ai_community_inputs(community_id,input,version_hash) SELECT community_id,input,version_hash FROM jsonb_to_recordset($1) AS r(community_id text,input jsonb,version_hash text) ON CONFLICT(community_id) DO UPDATE SET input=excluded.input,version_hash=excluded.version_hash,updated_at=now() WHERE ai_community_inputs.version_hash IS DISTINCT FROM excluded.version_hash")
            .bind(json!(inputs)).execute(&mut *publish).await?;
        let ids: Vec<&str> = inputs
            .iter()
            .filter_map(|i| i["community_id"].as_str())
            .collect();
        sqlx::query("DELETE FROM crm.ai_community_inputs WHERE NOT(community_id=ANY($1))")
            .bind(&ids)
            .execute(&mut *publish)
            .await?;
    }
    let ai_labels:Vec<(String,Value,Value)>=sqlx::query_as("SELECT subject,output,source_versions FROM crm.ai_visible_outputs WHERE kind='ai.community'").fetch_all(&mut *publish).await?;
    publish.commit().await?;
    for community in &mut communities {
        if community.get("label_source").is_some() {
            continue;
        }
        if let Some(input) = inputs.iter().find(|i| i["community_id"] == community["id"])
            && let Some((_, output, _)) = ai_labels.iter().find(|(id, _, versions)| {
                id == community["id"].as_str().unwrap_or("")
                    && versions["community"] == input["version_hash"]
            })
            && let Some(label) = output["label"].as_str().filter(|l| !l.trim().is_empty())
        {
            community["label"] = json!(label);
            community["label_source"] = json!("ai");
        }
    }
    for p in &mut people {
        p.as_object_mut().unwrap().remove("score_short");
        p.as_object_mut().unwrap().remove("score_long");
    }
    Ok(Json(
        json!({"rings":[5,15,50,150],"windows":[3,12],"communities":communities,"people":people}),
    ))
}
#[derive(Deserialize)]
struct Name {
    label: String,
}
async fn rename(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(n): Json<Name>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let label = n.label.trim();
    if label.is_empty() || label.chars().count() > 40 || id.is_empty() || id.len() > 160 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid community name",
        ));
    }
    sqlx::query("INSERT INTO crm.community_names(community_id,label) VALUES($1,$2) ON CONFLICT(community_id) DO UPDATE SET label=excluded.label,updated_at=now()").bind(&id).bind(label).execute(&s.pool).await?;
    Ok(Json(
        json!({"key":id,"label":label,"label_source":"manual"}),
    ))
}
async fn reset(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    sqlx::query("DELETE FROM crm.community_names WHERE community_id=$1")
        .bind(&id)
        .execute(&s.pool)
        .await?;
    Ok(Json(json!({"key":id})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    async fn read(app: &Router) -> Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/insights/orbit")
                    .header("host", "127.0.0.1:17494")
                    .header("cookie", "crm_session=synthetic-community")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn community_real_inputs_manual_priority_privacy_and_stale_labels(pool: sqlx::PgPool) {
        let root = tempfile::tempdir().unwrap();
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'community@example.invalid','unused')").bind(user).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(user).bind(monica_core::identity::bytes_hash(b"synthetic-community")).execute(&pool).await.unwrap();
        let group = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'group','Synthetic school')").bind(group).execute(&pool).await.unwrap();
        let mut people = vec![];
        for name in ["Synthetic One", "Synthetic Two"] {
            let person = Uuid::new_v4();
            people.push(person);
            sqlx::query(
                "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2)",
            )
            .bind(person)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO crm.group_memberships(group_profile_id,person_profile_id) VALUES($1,$2)").bind(group).bind(person).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO crm.contact_events(id,provider,profile_id,kind,external_id,occurred_at,content) VALUES($1,'monica',$2,'call',$3,now(),'{}')").bind(Uuid::new_v4()).bind(person).bind(person.to_string()).execute(&pool).await.unwrap();
        }
        let state = AppState::new(
            pool.clone(),
            "synthetic-signing-key-at-least-32-bytes",
            "http://127.0.0.1:17494".into(),
            root.path().into(),
        )
        .await
        .unwrap();
        let app = crate::auth::routes(state);
        let initial = read(&app).await;
        assert_eq!(initial["communities"].as_array().unwrap().len(), 1);
        let key = initial["communities"][0]["id"].as_str().unwrap();
        let input: Value =
            sqlx::query_scalar("SELECT input FROM crm.ai_community_inputs WHERE community_id=$1")
                .bind(key)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(input["members"].as_array().unwrap().len(), 2);
        sqlx::query("UPDATE crm.ai_settings SET enabled=true")
            .execute(&pool)
            .await
            .unwrap();
        monica_core::ai::enqueue(&pool, "ai.community", key)
            .await
            .unwrap();
        assert!(
            monica_core::ai::run_one(
                &pool,
                &monica_core::ai::provider::FakeProvider {
                    output: json!({"label":"School friends","reason":"shared school"})
                }
            )
            .await
            .unwrap()
        );
        let named = read(&app).await;
        assert_eq!(named["communities"][0]["label"], "School friends");
        assert_eq!(named["communities"][0]["label_source"], "ai");
        sqlx::query("INSERT INTO crm.community_names(community_id,label) VALUES($1,'My friends')")
            .bind(key)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(read(&app).await["communities"][0]["label_source"], "manual");
        sqlx::query("DELETE FROM crm.community_names WHERE community_id=$1")
            .bind(key)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE crm.ai_settings SET enabled=false")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            read(&app).await["communities"][0]["label"],
            "Synthetic school"
        );
        sqlx::query("UPDATE crm.ai_settings SET enabled=true")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE crm.profiles SET display_name='Changed member' WHERE id=$1")
            .bind(people[0])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            read(&app).await["communities"][0]["label"],
            "Synthetic school"
        );
    }
}

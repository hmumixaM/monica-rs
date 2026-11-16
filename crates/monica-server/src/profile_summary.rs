//! First-paint summaries use committed current projections and indexed lookups.
use super::{Result, read};
use crate::auth::{AppState, user};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    routing::get,
};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/profiles/{id}/summary", get(summary))
}

pub(crate) async fn contact_events(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value> {
    let rows:Vec<(String,String,String,Value)>=sqlx::query_as("SELECT kind,metric,dimension,to_jsonb(value) FROM crm.profile_contact_counters WHERE profile_id=$1 AND value<>0 ORDER BY kind,metric,dimension")
        .bind(id).fetch_all(&mut **tx).await?;
    let mut calls = json!({"total":0,"voice":0,"video":0,"completed":0,"incoming":0,"outgoing":0,"duration_seconds":0,"known_duration":0,"inferred_type":0});
    let mut money = json!({"total":0,"transfer":0,"red_packet":0,"unknown_amount":0,"unknown_direction":0,"currencies":{}});
    for (kind, metric, dimension, value) in rows {
        if kind == "call" {
            calls[metric] = value;
        } else if let Some(direction) = metric.strip_prefix("currency_") {
            if money["currencies"].get(&dimension).is_none() {
                money["currencies"][&dimension] = json!({"in":0,"out":0});
            }
            if !direction.starts_with("count_") {
                money["currencies"][&dimension][direction] = value;
            }
        } else {
            money[metric] = value;
        }
    }
    let mut result = json!({});
    if calls["total"] != 0 {
        result["calls"] = calls;
    }
    if money["total"] != 0 {
        result["money"] = money;
    }
    Ok(result)
}

async fn summary(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(requested): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mut profile = read::header(&mut tx, requested).await?;
    let id =
        Uuid::parse_str(profile["id"].as_str().expect("database UUID")).expect("database UUID");
    let identities:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(i)-'profile_id'-'source_account_id' FROM crm.identities i WHERE profile_id=$1 AND status NOT IN ('invalid','merged') ORDER BY provider,id").bind(id).fetch_all(&mut *tx).await?;
    let dates:Vec<Value>=sqlx::query_scalar("SELECT value FROM crm.profile_dates_presented WHERE profile_id=$1 ORDER BY kind='birthday' DESC,month NULLS LAST,day NULLS LAST,created_at").bind(id).fetch_all(&mut *tx).await?;
    let groups:i64=sqlx::query_scalar("SELECT count(*) FROM crm.current_group_memberships m JOIN crm.profiles g ON g.id=m.group_profile_id AND g.archived_at IS NULL WHERE m.person_profile_id=$1 AND m.left_at IS NULL").bind(id).fetch_one(&mut *tx).await?;
    profile["identities"] = json!(identities);
    profile["linked_group_count"] = json!(groups);
    profile["birthday"] = dates
        .iter()
        .find(|d| d["kind"] == "birthday")
        .cloned()
        .unwrap_or(Value::Null);
    profile["significant_dates"] = json!(dates);
    let has_posts:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.timeline_events WHERE profile_id=$1 AND status<>'deleted')").bind(id).fetch_one(&mut *tx).await?;
    let mut overview = json!({"contact_events":contact_events(&mut tx,id).await?});
    if profile["profile_type"] == "person" {
        let mut stats = crate::messages::stats::summary(&mut tx, id).await?;
        if stats["record_total"] != 0 || stats["group_total"] != 0 {
            let months:Vec<Value>=sqlx::query_scalar("WITH months AS (SELECT to_char(month,'YYYY-MM') AS month FROM generate_series(date_trunc('month',now() AT TIME ZONE 'America/Chicago')-interval '23 months',date_trunc('month',now() AT TIME ZONE 'America/Chicago'),interval '1 month') month),counts AS (SELECT month,sent,received FROM crm.page_profile_months WHERE profile_id=$1) SELECT jsonb_build_object('month',months.month,'sent',coalesce(counts.sent,0),'received',coalesce(counts.received,0)) FROM months LEFT JOIN counts USING(month) ORDER BY months.month")
                .bind(id).fetch_all(&mut *tx).await?;
            stats["months"] = json!(months);
            overview["messages"] = stats;
        } else {
            overview["messages"] = Value::Null;
        }
    } else {
        overview["messages"] = Value::Null;
    }
    tx.commit().await?;
    Ok(Json(
        json!({"profile":profile,"has_posts":has_posts,"overview":overview}),
    ))
}

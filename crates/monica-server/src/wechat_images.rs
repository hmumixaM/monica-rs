//! Offline group-photo coverage and explicit controls; no request starts a collector.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
#[path = "wechat_images_tests.rs"]
mod tests;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/imports/wechat-images", get(status).post(control))
        .route("/api/v1/imports/wechat-images/daily", get(daily))
        .route("/api/v1/imports/wechat-images/items", get(items))
}
async fn status(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let control: Value =
        sqlx::query_scalar("SELECT to_jsonb(c)-'singleton' FROM crm.wechat_image_control c")
            .fetch_one(&s.pool)
            .await?;
    let runs: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(r)||jsonb_build_object('heartbeat_stale',status IN ('indexing','running') AND heartbeat_at<now()-interval '2 minutes') FROM crm.wechat_image_runs r ORDER BY created_at DESC,id DESC LIMIT 20")
        .fetch_all(&s.pool).await?;
    Ok(Json(json!({"control":control,"runs":runs})))
}
#[derive(Deserialize)]
struct Action {
    action: String,
}
async fn control(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Action>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let (enabled, paused) = match p.action.as_str() {
        "enable" => (true, false),
        "pause" => (true, true),
        "disable" => (false, true),
        _ => {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid image recovery action",
            ));
        }
    };
    let mut tx = s.pool.begin().await?;
    let result: Value = sqlx::query_scalar("UPDATE crm.wechat_image_control SET enabled=$1,paused=$2,updated_at=now() RETURNING to_jsonb(wechat_image_control)-'singleton'")
        .bind(enabled).bind(paused).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type) VALUES($1,$2,'wechat_image_control')")
        .bind(actor).bind(format!("wechat.images.{}",p.action)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(result))
}
#[derive(Deserialize)]
struct Daily {
    run_id: Uuid,
    conversation_id: Option<Uuid>,
    after_day: Option<chrono::NaiveDate>,
    after_group: Option<Uuid>,
    limit: Option<i64>,
}
async fn daily(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(p): Query<Daily>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if p.after_day.is_some() != p.after_group.is_some() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "complete daily cursor required",
        ));
    }
    let limit = p.limit.unwrap_or(50).clamp(1, 100);
    let mut rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(d)||jsonb_build_object('group_name',coalesce(nullif(c.source_display_name,''),c.external_id,d.conversation_id::text)) FROM crm.wechat_image_daily d LEFT JOIN crm.conversations c ON c.id=d.conversation_id WHERE d.run_id=$1 AND ($2::uuid IS NULL OR d.conversation_id=$2) AND ($3::date IS NULL OR (d.day,d.conversation_id)<($3,$4)) ORDER BY d.day DESC,d.conversation_id DESC LIMIT $5")
        .bind(p.run_id).bind(p.conversation_id).bind(p.after_day).bind(p.after_group).bind(limit+1).fetch_all(&s.pool).await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next = if more {
        rows.last()
            .map(|r| json!({"after_day":r["day"],"after_group":r["conversation_id"]}))
    } else {
        None
    };
    Ok(Json(
        json!({"records":rows,"next":next,"day_timezone":"UTC"}),
    ))
}
#[derive(Deserialize)]
struct Items {
    run_id: Uuid,
    conversation_id: Option<Uuid>,
    day: Option<chrono::NaiveDate>,
    after: Option<Uuid>,
    limit: Option<i64>,
}
async fn items(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(p): Query<Items>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let limit = p.limit.unwrap_or(50).clamp(1, 100);
    let mut rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(i) FROM crm.wechat_group_images i WHERE run_id=$1 AND ($2::uuid IS NULL OR conversation_id=$2) AND ($3::date IS NULL OR day=$3) AND ($4::uuid IS NULL OR message_id>$4) ORDER BY message_id LIMIT $5")
        .bind(p.run_id).bind(p.conversation_id).bind(p.day).bind(p.after).bind(limit+1).fetch_all(&s.pool).await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next = if more {
        rows.last().map(|r| r["message_id"].clone())
    } else {
        None
    };
    Ok(Json(json!({"records":rows,"next":next})))
}

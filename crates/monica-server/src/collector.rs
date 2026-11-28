//! Managed adapter controls and source-compatible collection endpoints.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch, post},
};
use monica_core::{adapter, collector};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use subtle::ConstantTimeEq;
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn invalid(message: &'static str) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message)
}
fn missing(message: &'static str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, message)
}
fn operation(error: anyhow::Error) -> ApiError {
    tracing::warn!(%error,"adapter operation rejected");
    ApiError(
        StatusCode::CONFLICT,
        "Adapter disabled, limited, or ownership changed",
    )
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/profiles/{id}/linkedin-targets",
            get(linkedin).put(save_linkedin),
        )
        .route(
            "/api/v1/profiles/{id}/linkedin-targets/{aid}",
            axum::routing::delete(remove_linkedin),
        )
        .route("/api/v1/linkedin/collection-targets", get(targets))
        .route("/api/v1/profiles/{id}/group-speech", get(speech))
        .route("/api/v1/adapters", get(adapters))
        .route("/api/v1/adapters/{provider}", patch(controls))
        .route("/api/v1/adapters/{provider}/runs", post(start))
        .route("/api/v1/adapters/runs/{id}", get(run).delete(cancel))
}
async fn person(s: &AppState, id: Uuid) -> Result<()> {
    let kind: Option<String> = sqlx::query_scalar(
        "SELECT profile_type FROM crm.profiles WHERE id=$1 AND archived_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&s.pool)
    .await?;
    match kind.as_deref() {
        None => Err(missing("profile not found")),
        Some("person") => Ok(()),
        _ => Err(invalid("LinkedIn 账号只能添加到人物资料")),
    }
}
async fn linkedin(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    person(&s, id).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('account_id',a.id,'external_id',a.external_id,'profile_url',a.profile_url) FROM crm.linkedin_collection_targets t JOIN crm.source_accounts a ON a.id=t.account_id JOIN crm.source_account_links l ON l.account_id=a.id WHERE l.profile_id=$1 ORDER BY t.created_at,a.id").bind(id).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
#[derive(Deserialize)]
struct LinkedInSave {
    account: String,
}
async fn save_linkedin(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<LinkedInSave>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    person(&s, id).await?;
    collector::canonical_linkedin(&body.account)
        .map_err(|_| invalid("请输入 LinkedIn 个人主页链接或 /in/ 后的账号"))?;
    let mut tx = s.pool.begin().await?;
    let saved = collector::save_linkedin(&mut tx, id, &body.account, actor)
        .await
        .map_err(linkedin_error)?;
    tx.commit().await?;
    Ok(Json(saved))
}

pub(crate) fn linkedin_error(error: anyhow::Error) -> ApiError {
    if error.to_string().contains("belongs to another person") {
        ApiError(
            StatusCode::CONFLICT,
            "此 LinkedIn 账号已关联其他人物，请先在来源账号中核对归属",
        )
    } else {
        operation(error)
    }
}
async fn remove_linkedin(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, aid)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let (actor, _) = user(&s, &h).await?;
    person(&s, id).await?;
    let mut tx = s.pool.begin().await?;
    sqlx::query("SELECT id FROM crm.source_accounts WHERE id=$1 FOR UPDATE")
        .bind(aid)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(missing("collection target not found"))?;
    let removed=sqlx::query("DELETE FROM crm.linkedin_collection_targets t USING crm.source_account_links l WHERE t.account_id=$1 AND l.account_id=t.account_id AND l.profile_id=$2").bind(aid).bind(id).execute(&mut *tx).await?.rows_affected();
    if removed == 0 {
        return Err(missing("collection target not found"));
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'linkedin.collection.disabled','profile',$2,$3)").bind(actor).bind(id).bind(json!({"account_id":aid})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn targets(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    let expected = std::env::var("MONICA_LINKEDIN_COLLECTION_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let actual = h
        .get("x-linkedin-targets-token")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if !expected.is_some_and(|e| bool::from(e.as_bytes().ct_eq(actual.as_bytes()))) {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "collector token required",
        ));
    }
    Ok(Json(collector::targets(&s.pool).await.map_err(operation)?))
}
async fn adapters(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(c)||jsonb_build_object('run',(SELECT to_jsonb(r)-'token' FROM crm.adapter_runs r WHERE r.provider=c.provider ORDER BY created_at DESC LIMIT 1)) FROM crm.adapter_controls c ORDER BY provider").fetch_all(&s.pool).await?;
    Ok(Json(json!({"adapters":rows})))
}
#[derive(Deserialize)]
struct Controls {
    enabled: Option<bool>,
    paused: Option<bool>,
    minimum_interval_seconds: Option<i32>,
}
async fn controls(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(provider): Path<String>,
    Json(body): Json<Controls>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    if body
        .minimum_interval_seconds
        .is_some_and(|n| !(1..=86400).contains(&n))
    {
        return Err(invalid("invalid platform interval"));
    }
    let mut tx = s.pool.begin().await?;
    let row:Value=sqlx::query_scalar("UPDATE crm.adapter_controls c SET enabled=coalesce($2,enabled),paused=coalesce($3,paused),minimum_interval_seconds=coalesce($4,minimum_interval_seconds),updated_at=now() WHERE provider=$1 RETURNING to_jsonb(c)").bind(&provider).bind(body.enabled).bind(body.paused).bind(body.minimum_interval_seconds).fetch_optional(&mut *tx).await?.ok_or(missing("unknown adapter"))?;
    if body.enabled == Some(false) || body.paused == Some(true) {
        sqlx::query("UPDATE crm.adapter_runs SET cancel_requested=true,status=CASE WHEN status='queued' THEN 'cancelled' ELSE status END WHERE provider=$1 AND status IN ('queued','running')").bind(&provider).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,metadata) VALUES($1,'adapter.controls','adapter',$2)").bind(actor).bind(json!({"provider":provider,"enabled":body.enabled,"paused":body.paused})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn start(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(provider): Path<String>,
) -> Result<(StatusCode, Json<Value>)> {
    user(&s, &h).await?;
    let id = adapter::enqueue(&s.pool, &provider)
        .await
        .map_err(operation)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run_id":id,"provider":provider,"queued":true})),
    ))
}
async fn run(State(s): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let row: Value =
        sqlx::query_scalar("SELECT to_jsonb(r)-'token' FROM crm.adapter_runs r WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.pool)
            .await?
            .ok_or(missing("adapter run not found"))?;
    Ok(Json(row))
}
async fn cancel(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let changed = adapter::cancel(&s.pool, id).await.map_err(operation)?;
    if !changed {
        return Err(missing("no active adapter run"));
    }
    Ok(Json(json!({"run_id":id,"cancel_requested":true})))
}
#[derive(Deserialize)]
struct SpeechQuery {
    group_id: Option<Uuid>,
    conversation_id: Option<Uuid>,
    q: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<i64>,
    group_offset: Option<i64>,
    before: Option<String>,
    before_id: Option<Uuid>,
}
async fn speech(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<SpeechQuery>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    tokio::time::timeout(Duration::from_secs(5), local_speech(&s, id, q))
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::GATEWAY_TIMEOUT,
                "Local group speech query timed out; retry",
            )
        })?
}
// Old Chatlog filters accepted calendar days in the installation's Chicago
// timezone. Retain that input while storing/returning timestamps in UTC.
async fn speech_boundary(s: &AppState, value: Option<String>, end: bool) -> Result<Option<String>> {
    let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    let value = value.trim();
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(Some(at.to_rfc3339()));
    }
    let local = if let Ok(day) = value.parse::<chrono::NaiveDate>() {
        let day = if end {
            day.succ_opt()
                .ok_or_else(|| invalid("Invalid speech date"))?
        } else {
            day
        };
        day.and_hms_opt(0, 0, 0).unwrap()
    } else {
        chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
            .map_err(|_| invalid("Invalid speech date"))?
    };
    let at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT $1::timestamp AT TIME ZONE 'America/Chicago'")
            .bind(local)
            .fetch_one(&s.pool)
            .await?;
    Ok(Some(at.to_rfc3339()))
}
async fn local_speech(s: &AppState, id: Uuid, q: SpeechQuery) -> Result<Json<Value>> {
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.profiles WHERE id=$1 AND profile_type='person' AND archived_at IS NULL)")
        .bind(id).fetch_one(&s.pool).await?;
    if !exists {
        return Err(missing("人物资料不可用"));
    }
    if q.before_id.is_some() != q.before.is_some() {
        return Err(invalid("Speech cursor requires time and ID"));
    }
    if let Some(before) = &q.before {
        chrono::DateTime::parse_from_rfc3339(before)
            .map_err(|_| invalid("Invalid speech cursor"))?;
    }
    let since = speech_boundary(s, q.since, false).await?;
    let until = speech_boundary(s, q.until, true).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 99) as usize;
    let offset = q.group_offset.unwrap_or(0).max(0);
    let _memory = s
        .resources
        .acquire(monica_core::resources::TaskResources {
            memory_bytes: 32 * 1024 * 1024,
            ..Default::default()
        })
        .await
        .map_err(operation)?;
    let filter = "NOT m.tombstone AND ($2::text IS NULL OR position(lower($2) in lower(m.body))>0) AND ($3::timestamptz IS NULL OR m.occurred_at>=$3::timestamptz) AND ($4::timestamptz IS NULL OR m.occurred_at<$4::timestamptz)";
    let query = format!(
        "WITH owned_rooms AS (SELECT DISTINCT m.conversation_id FROM crm.message_search m WHERE m.sender_profile_id=$1 AND {filter} UNION SELECT DISTINCT m.conversation_id FROM crm.message_search m JOIN crm.source_account_links l ON l.account_id=m.sender_account_id AND l.profile_id=$1 WHERE m.sender_profile_id IS NULL AND {filter}) SELECT jsonb_build_object('conversation_id',c.id,'group_id',c.profile_id,'provider',c.provider,'name',coalesce(p.display_name,c.source_display_name,a.display_name,c.external_id),'chat',c.external_id) FROM owned_rooms own JOIN crm.conversations c ON c.id=own.conversation_id LEFT JOIN crm.profiles p ON p.id=c.profile_id LEFT JOIN crm.source_accounts a ON a.id=c.source_account_id WHERE c.conversation_type='group' AND ($5::uuid IS NULL OR c.profile_id=$5) AND ($6::uuid IS NULL OR c.id=$6) ORDER BY coalesce(p.display_name,c.source_display_name,a.display_name,c.external_id),c.id OFFSET $7 LIMIT 11"
    );
    let mut rooms: Vec<Value> = sqlx::query_scalar(&query)
        .bind(id)
        .bind(&q.q)
        .bind(&since)
        .bind(&until)
        .bind(q.group_id)
        .bind(q.conversation_id)
        .bind(offset)
        .fetch_all(&s.pool)
        .await?;
    let next = if rooms.len() > 10 {
        rooms.truncate(10);
        Some(offset + 10)
    } else {
        None
    };
    for room in &mut rooms {
        let cid = room["conversation_id"].as_str().unwrap().parse().unwrap();
        let Json(rows) = crate::messages::local_group_speech(
            s,
            crate::messages::LocalSpeechPage {
                profile_id: id,
                conversation_id: cid,
                query: q.q.clone(),
                before: q.before.clone(),
                before_id: q.before_id,
                since: since.clone(),
                until: until.clone(),
                limit: (limit + 1) as i64,
            },
        )
        .await?;
        let mut rows = rows.as_array().cloned().unwrap_or_default();
        let more = rows.len() > limit;
        rows.truncate(limit);
        room["next"] = if more {
            let last = rows.last().unwrap();
            json!({"before":last["occurred_at"],"before_id":last["id"]})
        } else {
            Value::Null
        };
        for row in &mut rows {
            row["time"] = row["occurred_at"].clone();
            row["sender"] = row["sender_name"].clone();
        }
        room["messages"] = json!(rows);
        room["partial"] = json!(more);
    }
    Ok(Json(
        json!({"groups":rooms,"next_group_offset":next,"storage":"local","scope":"imported_records","note":"显示已导入的群聊发言；导入完整性与媒体进度请查看导入中心。"}),
    ))
}

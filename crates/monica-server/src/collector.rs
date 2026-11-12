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
use std::{collections::BTreeSet, sync::Arc, time::Duration};
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
        .map_err(|e| {
            if e.to_string().contains("belongs to another person") {
                ApiError(
                    StatusCode::CONFLICT,
                    "此 LinkedIn 账号已关联其他人物，请先在来源账号中核对归属",
                )
            } else {
                operation(e)
            }
        })?;
    tx.commit().await?;
    Ok(Json(saved))
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
    q: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<i64>,
    group_offset: Option<i64>,
    window_index: Option<usize>,
}
async fn speech(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<SpeechQuery>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let base = std::env::var("MONICA_CHATLOG_ALPHA_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Chatlog Alpha is not configured",
        ))?;
    let client = Arc::new(collector::Chatlog::new(&base).map_err(operation)?);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let limit = q.limit.unwrap_or(50).clamp(1, 200) as usize;
    let offset = q.group_offset.unwrap_or(0).max(0);
    let window = q.window_index.unwrap_or(0).min(8);
    let _memory = s
        .resources
        .acquire(monica_core::resources::TaskResources {
            memory_bytes: 32 * 1024 * 1024,
            ..Default::default()
        })
        .await
        .map_err(operation)?;
    let mut tx = s.pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='5000'")
        .execute(&mut *tx)
        .await?;
    let senders:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('external_id',a.external_id,'username',coalesce(a.username,'')) FROM crm.source_account_links l JOIN crm.source_accounts a ON a.id=l.account_id WHERE l.profile_id=$1 AND a.provider='wechat' AND a.object_kind='person' AND a.external_id NOT LIKE '%@chatroom' AND a.external_id NOT LIKE 'gh_%' AND a.external_id NOT LIKE '%@openim' ORDER BY a.external_id").bind(id).fetch_all(&mut *tx).await?;
    if senders.is_empty() {
        return Err(missing("这个人没有微信号"));
    }
    let keys = Arc::new(
        senders
            .iter()
            .flat_map(|s| [s["external_id"].as_str(), s["username"].as_str()])
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .map(|s| collector::casefold(s.trim()))
            .collect::<BTreeSet<_>>(),
    );
    let mut groups:Vec<Value>=sqlx::query_scalar("WITH rooms AS (SELECT DISTINCT gp.id AS group_id,g.display_name AS name,g.external_id AS chat FROM crm.source_account_links l JOIN crm.source_group_memberships m ON m.person_account_id=l.account_id JOIN crm.source_accounts g ON g.id=m.group_account_id LEFT JOIN crm.source_account_links gl ON gl.account_id=g.id LEFT JOIN crm.profiles gp ON gp.id=gl.profile_id AND gp.profile_type='group' WHERE l.profile_id=$1 AND m.left_at IS NULL AND g.object_kind='group' UNION SELECT DISTINCT c.profile_id,p.display_name,c.external_id FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id LEFT JOIN crm.profiles p ON p.id=c.profile_id WHERE c.provider='wechat' AND c.conversation_type='group' AND NOT m.tombstone AND (m.sender_profile_id=$1 OR EXISTS(SELECT 1 FROM crm.source_account_links own WHERE own.account_id=m.sender_account_id AND own.profile_id=$1))) SELECT jsonb_build_object('group_id',group_id,'name',name,'chat',chat) FROM rooms WHERE ($2::uuid IS NULL OR group_id=$2) ORDER BY name NULLS LAST,chat OFFSET $3 LIMIT 11").bind(id).bind(q.group_id).bind(offset).fetch_all(&mut *tx).await?;
    if q.group_id.is_some() && groups.is_empty() {
        return Err(missing("这个人不在该群里"));
    }
    let next = if groups.len() > 10 {
        groups.truncate(10);
        Some(offset + 10)
    } else {
        None
    };
    let query = q.q.unwrap_or_default();
    let since = q.since.unwrap_or_default().trim().to_owned();
    let until = q.until.unwrap_or_default().trim().to_owned();
    let mut rooms = Vec::new();
    let mut stored = Vec::new();
    for group in &groups {
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis()
            .min(5000);
        if remaining == 0 {
            return Err(ApiError(
                StatusCode::GATEWAY_TIMEOUT,
                "Group speech query budget exhausted",
            ));
        }
        sqlx::query("SELECT set_config('statement_timeout',$1,true)")
            .bind(remaining.to_string())
            .execute(&mut *tx)
            .await?;
        let chat = group["chat"].as_str().unwrap_or_default();
        let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('time',to_char(m.occurred_at AT TIME ZONE 'America/Chicago','YYYY-MM-DD HH24:MI:SS'),'timestamp',extract(epoch from m.occurred_at),'sender',m.sender,'content',left(m.body,4000),'type',m.message_type) FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.provider='wechat' AND c.external_id=$2 AND c.conversation_type='group' AND NOT m.tombstone AND (m.sender_profile_id=$1 OR EXISTS(SELECT 1 FROM crm.source_account_links own WHERE own.account_id=m.sender_account_id AND own.profile_id=$1)) ORDER BY m.occurred_at DESC,m.message_id DESC LIMIT 200").bind(id).bind(chat).fetch_all(&mut *tx).await?;
        let saved = collector::imported_rows(&rows, &query, limit);
        let mut room = group.clone();
        room["name"] = group["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map_or(json!(chat), |s| json!(s));
        room["messages"] = json!(saved);
        room["partial"] = json!(true);
        room["error"] = json!("读取尚未完成");
        room["next_window"] = json!(window);
        rooms.push(room);
        stored.push(rows);
    }
    tx.commit().await?; // Release the DB connection before external reads.
    let mut tasks = tokio::task::JoinSet::new();
    let capacity = Arc::new(tokio::sync::Semaphore::new(2));
    for (index, group) in groups.into_iter().enumerate() {
        let client = client.clone();
        let keys = keys.clone();
        let query = query.clone();
        let since = since.clone();
        let until = until.clone();
        let capacity = capacity.clone();
        let resources = s.resources.clone();
        let saved = stored[index].clone();
        let mut room = rooms[index].clone();
        tasks.spawn(async move {
            let chat = group["chat"].as_str().unwrap_or_default();
            if !chat.ends_with("@chatroom") {
                room["partial"] = json!(false);
                room["error"] = json!("这个群没有 Chatlog 房间号");
                return (index, room);
            }
            let mut windows = vec![String::new()];
            if since.is_empty() && until.is_empty() {
                let days = saved
                    .iter()
                    .filter_map(|m| m["time"].as_str())
                    .filter_map(|t| t.get(..10))
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                windows.extend(
                    days.into_iter()
                        .rev()
                        .take(8)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev(),
                );
            }
            let mut collected = Vec::new();
            for (position, when) in windows.iter().enumerate().skip(window) {
                let _capacity = match capacity.acquire().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let _network = match resources
                    .acquire(monica_core::resources::TaskResources {
                        network: 1,
                        ..Default::default()
                    })
                    .await
                {
                    Ok(p) => p,
                    Err(_) => break,
                };
                match client.history(chat, &since, &until, when).await {
                    Ok(rows) => collected.extend(rows),
                    Err(_) => {
                        room["error"] = json!("Chatlog 读取失败；可继续读取");
                        return (index, room);
                    }
                }
                let mut messages = collector::imported_rows(&saved, &query, limit);
                messages.extend(collector::filter_messages(&collected, &keys, &query, limit));
                let mut seen = BTreeSet::new();
                messages.retain(|m| {
                    seen.insert((
                        m["time"]
                            .as_str()
                            .unwrap_or_default()
                            .chars()
                            .take(10)
                            .collect::<String>(),
                        m["text"].as_str().unwrap_or_default().trim().to_owned(),
                    ))
                });
                messages.sort_by(|a, b| a["time"].as_str().cmp(&b["time"].as_str()));
                messages.truncate(limit);
                room["messages"] = json!(messages);
                room["next_window"] = if position + 1 < windows.len() {
                    json!(position + 1)
                } else {
                    Value::Null
                };
            }
            room["partial"] = json!(false);
            room["error"] = Value::Null;
            (index, room)
        });
    }
    while !tasks.is_empty() {
        match tokio::time::timeout_at(deadline, tasks.join_next()).await {
            Ok(Some(Ok((index, room)))) => rooms[index] = room,
            Ok(None) => break,
            Ok(Some(Err(_))) => {}
            Err(_) => {
                tasks.abort_all();
                break;
            }
        }
    }
    for room in &mut rooms {
        if room["partial"] == true && room["error"] == "读取尚未完成" {
            room["error"] = json!("已达到读取时限；可选择这个群继续读取");
        }
    }
    let partial = rooms.iter().any(|r| r["partial"] == true);
    Ok(Json(
        json!({"senders":keys.iter().collect::<Vec<_>>(),"groups":rooms,"partial":partial,"next_group_offset":next}),
    ))
}

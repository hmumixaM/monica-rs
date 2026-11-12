#[path = "phone_export.rs"]
mod phone_export;
// Authenticated connection management and versioned source evidence.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use monica_core::{Provider, connections as core, mail_oauth};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn invalid(message: &'static str) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message)
}
fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found")
}
fn operation(_: anyhow::Error) -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "Connection operation failed or ownership changed",
    )
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/whatsapp/phone-export",
            post(phone_export::import).layer(axum::extract::DefaultBodyLimit::max(
                monica_core::media::MAX_FILE as usize + 65536,
            )),
        )
        .route("/api/v1/sources", get(catalogue))
        .route("/api/v1/source-connections", get(list).post(create))
        .route("/api/v1/source-connections/progress", get(progress))
        .route("/api/v1/whatsapp/connect", post(whatsapp_connect))
        .route(
            "/api/v1/source-connections/{id}/whatsapp/status",
            get(whatsapp_status),
        )
        .route("/api/v1/source-connections/{id}/action", post(action))
        .route("/api/v1/source-connections/{id}/people", get(people))
        .route("/api/v1/source-connections/{id}/identity", post(identity))
        .route(
            "/api/v1/source-connections/{id}/mail-rules",
            post(mail_rule).get(mail_rules),
        )
        .route("/api/v1/profiles/{id}/identifiers", post(identifier))
        .route("/api/v1/profiles/{id}/source-coverage", get(coverage))
        .route("/api/v1/evidence", get(evidence))
        .route("/api/v1/evidence/{id}", get(detail))
        .route("/api/v1/evidence/{id}/files/{digest}", get(file))
        .route("/api/v1/mail-oauth/config", get(oauth_config))
        .route("/api/v1/mail-oauth/start", post(oauth_start))
        .route(mail_oauth::CALLBACK, get(oauth_callback))
}
async fn catalogue(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut catalogue = core::catalogue();
    for source in catalogue.as_array_mut().expect("source catalogue") {
        let native = matches!(
            source["id"].as_str(),
            Some("apple_contacts" | "apple_messages")
        );
        let available = !native || s.native_sources_available;
        source["collection_available"] = json!(available);
        source["unavailable_reason"] = if available {
            Value::Null
        } else {
            json!("此部署没有 macOS 本机读取组件。已有资料与证据仍可浏览。")
        };
    }
    Ok(Json(catalogue))
}
async fn list(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'provider',provider,'external_account',external_account,'display_name',display_name,'connection_method',connection_method,'state',state,'enabled',enabled,'settings',settings,'last_success_at',last_success_at,'error',error,'next_sync_at',next_sync_at) FROM crm.source_connections ORDER BY created_at,id").fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
#[derive(Deserialize)]
struct Create {
    provider: String,
    external_account: String,
    display_name: String,
    #[serde(rename = "connection_method", alias = "method")]
    method: String,
    host: Option<String>,
    #[serde(default = "port")]
    port: u16,
    #[serde(default = "folders")]
    folders: Vec<String>,
    password: Option<String>,
}
fn port() -> u16 {
    993
}
fn folders() -> Vec<String> {
    vec!["INBOX".into()]
}
async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(v): Json<Create>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if v.provider != "mail" || v.method != "imap" {
        return Err(invalid("此连接方式需要由对应的授权流程建立"));
    }
    let host = v
        .host
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or(invalid("IMAP host and password required"))?;
    let password = v
        .password
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or(invalid("IMAP host and password required"))?;
    if v.external_account.is_empty()
        || v.external_account.chars().count() > 320
        || v.display_name.is_empty()
        || v.display_name.chars().count() > 120
        || v.folders.len() > 50
        || v.port == 0
    {
        return Err(invalid("invalid connection configuration"));
    }
    let account =
        core::normalize_identifier("email", &v.external_account).ok_or(invalid("invalid email"))?;
    let id = core::connection_id(Provider::Mail, &account);
    let credentials = s
        .credentials
        .encrypt(&json!({"password":password}))
        .map_err(operation)?;
    let settings = json!({"host":host,"port":v.port,"folders":v.folders});
    let mut tx = s.pool.begin().await?;
    core::lock_connection(&mut tx, id)
        .await
        .map_err(operation)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("identifier:email:{account}"))
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,settings,credential_ciphertext,enabled,state) VALUES($1,'mail',$2,$3,'imap',$4,$5,true,'connecting') ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,connection_method='imap',settings=excluded.settings,credential_ciphertext=excluded.credential_ciphertext,enabled=true,state='connecting',error=NULL,next_sync_at=now(),updated_at=now(),generation=crm.source_connections.generation+1,lease_owner=NULL,lease_token=NULL,lease_until=NULL")
 .bind(id).bind(&account).bind(v.display_name).bind(settings).bind(credentials).execute(&mut *tx).await?;
    core::queue_identifier(&mut tx, "email", &account)
        .await
        .map_err(operation)?;
    tx.commit().await?;
    Ok(Json(json!({"id":id})))
}
#[derive(Deserialize)]
struct Action {
    action: String,
}
async fn action(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(v): Json<Action>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let (enabled, state) = match v.action.as_str() {
        "pause" => (false, "paused"),
        "resume" | "refresh" => (true, "connecting"),
        "disconnect" => (false, "disconnected"),
        _ => return Err(invalid("invalid action")),
    };
    let mut tx = s.pool.begin().await?;
    core::lock_connection(&mut tx, id)
        .await
        .map_err(operation)?;
    let provider: Option<String> =
        sqlx::query_scalar("SELECT provider FROM crm.source_connections WHERE id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if enabled
        && !s.native_sources_available
        && matches!(
            provider.as_deref(),
            Some("apple_contacts" | "apple_messages")
        )
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "此部署不支持 macOS 本机采集；已有证据仍可浏览",
        ));
    }
    let n=sqlx::query("UPDATE crm.source_connections SET enabled=$2,state=$3,credential_ciphertext=CASE WHEN $3='disconnected' THEN NULL ELSE credential_ciphertext END,next_sync_at=now(),updated_at=now(),generation=generation+1,lease_owner=NULL,lease_token=NULL,lease_until=NULL WHERE id=$1").bind(id).bind(enabled).bind(state).execute(&mut *tx).await?.rows_affected();
    if n == 0 {
        return Err(missing());
    }
    tx.commit().await?;
    Ok(Json(json!({"id":id})))
}
#[derive(Deserialize)]
struct People {
    #[serde(default)]
    q: String,
    #[serde(default)]
    after: String,
    #[serde(default = "limit")]
    limit: i64,
}
fn limit() -> i64 {
    40
}
async fn people(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<People>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(p)||jsonb_build_object('profile_id',CASE WHEN l.blocked THEN NULL WHEN l.confirmed THEN l.profile_id ELSE p.resolved_profile_id END,'profile_name',r.display_name,'blocked',l.blocked,'candidates',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',c.profile_id,'name',cp.display_name,'reason',c.reason) ORDER BY cp.display_name,c.profile_id) FROM crm.connection_identity_candidates c JOIN crm.profiles cp ON cp.id=c.profile_id WHERE c.connection_id=p.connection_id AND c.external_id=p.external_id),'[]'::jsonb)) FROM crm.connection_people p LEFT JOIN crm.connection_identity_links l USING(connection_id,external_id) LEFT JOIN crm.profiles r ON r.id=CASE WHEN l.blocked THEN NULL WHEN l.confirmed THEN l.profile_id ELSE p.resolved_profile_id END WHERE p.connection_id=$1 AND p.external_id>$2 AND ($3='' OR p.display_name ILIKE '%'||$3||'%' OR p.external_id ILIKE '%'||$3||'%') ORDER BY p.external_id LIMIT $4")
 .bind(id).bind(q.after).bind(q.q).bind(q.limit.clamp(1,100)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
#[derive(Deserialize)]
struct Identity {
    external_id: String,
    action: String,
    profile_id: Option<Uuid>,
}
async fn identity(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(v): Json<Identity>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if v.external_id.is_empty()
        || v.external_id.len() > 1024
        || !["link", "unlink", "automatic"].contains(&v.action.as_str())
    {
        return Err(invalid("invalid identity action"));
    }
    let mut tx = s.pool.begin().await?;
    // Same namespace/order as journal projection: serialize one connection's map edits.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("connection:{id}"))
        .execute(&mut *tx)
        .await?;
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.connection_people WHERE connection_id=$1 AND external_id=$2)").bind(id).bind(&v.external_id).fetch_one(&mut *tx).await?;
    if !exists {
        return Err(missing());
    }
    let profile = if v.action == "link" {
        let p = v.profile_id.ok_or(invalid("profile_id required"))?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM crm.profiles WHERE id=$1 AND archived_at IS NULL)",
        )
        .bind(p)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(missing());
        }
        Some(p)
    } else {
        None
    };
    if v.action == "automatic" {
        sqlx::query(
            "DELETE FROM crm.connection_identity_links WHERE connection_id=$1 AND external_id=$2",
        )
        .bind(id)
        .bind(&v.external_id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query("INSERT INTO crm.connection_identity_links VALUES($1,$2,$3,true,$4) ON CONFLICT(connection_id,external_id) DO UPDATE SET profile_id=excluded.profile_id,confirmed=true,blocked=excluded.blocked").bind(id).bind(&v.external_id).bind(profile).bind(v.action=="unlink").execute(&mut *tx).await?;
    }
    let identifiers:Vec<(String,String)>=sqlx::query_as("UPDATE crm.connection_identifiers SET active=false WHERE connection_id=$1 AND external_id=$2 RETURNING kind,value").bind(id).bind(&v.external_id).fetch_all(&mut *tx).await?;
    sqlx::query("UPDATE crm.connection_people SET resolved_profile_id=NULL WHERE connection_id=$1 AND external_id=$2").bind(id).bind(&v.external_id).execute(&mut *tx).await?;
    sqlx::query(
        "DELETE FROM crm.connection_identity_candidates WHERE connection_id=$1 AND external_id=$2",
    )
    .bind(id)
    .bind(&v.external_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO crm.connection_relink(evidence_id) SELECT id FROM crm.connection_evidence WHERE connection_id=$1 AND metadata @> $2 ON CONFLICT(evidence_id) DO UPDATE SET requested_at=now()").bind(id).bind(json!({"participants":[{"id":v.external_id}]})).execute(&mut *tx).await?;
    for (kind, value) in identifiers {
        core::queue_identifier(&mut tx, &kind, &value)
            .await
            .map_err(operation)?;
    }
    sqlx::query(
        "UPDATE crm.source_connections SET sync_pending=true,next_sync_at=now() WHERE id=$1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"action":v.action,"profile_id":profile})))
}
#[derive(Deserialize)]
struct Identifier {
    kind: String,
    value: String,
    #[serde(default)]
    is_self: bool,
}
async fn identifier(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(v): Json<Identifier>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !["email", "phone", "source"].contains(&v.kind.as_str())
        || v.value.is_empty()
        || v.value.len() > 512
    {
        return Err(invalid("invalid identifier"));
    }
    let value = core::normalize_identifier(&v.kind, &v.value)
        .ok_or(invalid("phone requires international country code"))?;
    let mut tx = s.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("identifier:{}:{value}", v.kind))
        .execute(&mut *tx)
        .await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM crm.profiles WHERE id=$1 AND archived_at IS NULL)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(missing());
    }
    sqlx::query("INSERT INTO crm.person_identifiers VALUES($1,$2,$3,true,$4) ON CONFLICT(profile_id,kind,value) DO UPDATE SET confirmed=true,is_self=excluded.is_self").bind(id).bind(&v.kind).bind(&value).bind(v.is_self).execute(&mut *tx).await?;
    core::queue_identifier(&mut tx, &v.kind, &value)
        .await
        .map_err(operation)?;
    tx.commit().await?;
    Ok(Json(json!({"kind":v.kind,"value":value})))
}
#[derive(Deserialize)]
struct Rule {
    scope: String,
    target: String,
    category: String,
}
async fn mail_rule(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(v): Json<Rule>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !["message", "thread", "sender"].contains(&v.scope.as_str())
        || !["personal", "transactional", "marketing", "pending"].contains(&v.category.as_str())
        || v.target.is_empty()
        || v.target.len() > 1024
    {
        return Err(invalid("invalid mail rule"));
    }
    let mut tx = s.pool.begin().await?;
    core::lock_connection(&mut tx, id)
        .await
        .map_err(operation)?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM crm.source_connections WHERE id=$1 AND provider='mail')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(missing());
    }
    sqlx::query("INSERT INTO crm.mail_rules VALUES($1,$2,$3,$4) ON CONFLICT(connection_id,scope,target) DO UPDATE SET category=excluded.category").bind(id).bind(&v.scope).bind(&v.target).bind(v.category).execute(&mut *tx).await?;
    let n=sqlx::query("INSERT INTO crm.connection_reprocess(connection_id,external_id) SELECT connection_id,external_id FROM crm.connection_evidence WHERE connection_id=$1 AND (($2='message' AND external_id=$3) OR ($2='thread' AND metadata->>'thread_id'=$3) OR ($2='sender' AND metadata @> jsonb_build_object('participants',jsonb_build_array(jsonb_build_object('role','sender','id',$3::text))))) ON CONFLICT(connection_id,external_id) DO UPDATE SET requested_at=now()")
 .bind(id).bind(v.scope).bind(v.target).execute(&mut *tx).await?.rows_affected();
    sqlx::query(
        "UPDATE crm.source_connections SET sync_pending=true,next_sync_at=now(),generation=generation+1,lease_owner=NULL,lease_token=NULL,lease_until=NULL WHERE id=$1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"queued":n})))
}
async fn coverage(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let sources:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('connection_id',c.id,'provider',c.provider,'display_name',c.display_name,'state',c.state,'enabled',c.enabled,'last_success_at',c.last_success_at,'error',c.error,'records',count(*),'earliest_at',min(e.occurred_at),'latest_at',max(e.occurred_at)) FROM crm.source_connections c JOIN crm.connection_evidence e ON e.connection_id=c.id WHERE e.included AND NOT e.deleted AND EXISTS(SELECT 1 FROM crm.connection_evidence_people p WHERE p.evidence_id=e.id AND p.profile_id=$1) GROUP BY c.id ORDER BY max(e.occurred_at) DESC NULLS LAST").bind(id).fetch_all(&s.pool).await?;
    let identifiers:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('kind',kind,'value',value,'is_self',is_self) FROM crm.person_identifiers WHERE profile_id=$1 ORDER BY kind,value").bind(id).fetch_all(&s.pool).await?;
    Ok(Json(
        json!({"sources":sources,"identifiers":identifiers,"coverage":"已收录内容的时间范围；未完成同步的历史不计为完整覆盖"}),
    ))
}
#[derive(Deserialize)]
struct EvidenceQuery {
    profile_id: Option<Uuid>,
    connection_id: Option<Uuid>,
    #[serde(default)]
    q: String,
    category: Option<String>,
    provider: Option<String>,
    before: Option<chrono::DateTime<chrono::Utc>>,
    before_id: Option<Uuid>,
    #[serde(default = "limit")]
    limit: i64,
    #[serde(default)]
    include_filtered: bool,
}
async fn evidence(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<EvidenceQuery>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("WITH matches AS MATERIALIZED(SELECT e.*,c.provider,c.display_name account_name,c.external_account FROM crm.connection_evidence e JOIN crm.source_connections c ON c.id=e.connection_id WHERE NOT e.deleted AND ($1 OR e.included) AND ($2::uuid IS NULL OR EXISTS(SELECT 1 FROM crm.connection_evidence_people p WHERE p.evidence_id=e.id AND p.profile_id=$2)) AND ($3::uuid IS NULL OR e.connection_id=$3) AND ($4='' OR to_tsvector('simple',e.title||' '||e.body) @@ websearch_to_tsquery('simple',$4)) AND ($5::text IS NULL OR e.category=$5) AND ($6::text IS NULL OR c.provider=$6)), ranked AS(SELECT m.*,count(*) OVER(PARTITION BY COALESCE(dedup_key,id::text)) copy_count,row_number() OVER(PARTITION BY COALESCE(dedup_key,id::text) ORDER BY occurred_at DESC NULLS LAST,id DESC) copy_rank FROM matches m) SELECT to_jsonb(r)-'copy_rank'||jsonb_build_object('citation','/evidence/'||id||'?revision='||revision) FROM ranked r WHERE copy_rank=1 AND (($7::timestamptz IS NULL AND ($8::uuid IS NULL OR (occurred_at IS NULL AND id<$8))) OR ($7::timestamptz IS NOT NULL AND (occurred_at IS NULL OR occurred_at<$7 OR (occurred_at=$7 AND $8::uuid IS NOT NULL AND id<$8)))) ORDER BY occurred_at DESC NULLS LAST,id DESC LIMIT $9")
 .bind(q.include_filtered).bind(q.profile_id).bind(q.connection_id).bind(q.q).bind(q.category).bind(q.provider).bind(q.before).bind(q.before_id).bind(q.limit.clamp(1,100)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
#[derive(Default, Deserialize)]
struct DetailQuery {
    revision: Option<i64>,
    #[serde(default)]
    body_offset: usize,
    body_limit: Option<usize>,
    #[serde(default)]
    original: bool,
}
async fn detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<DetailQuery>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let (mut value, payload) = version(&s, id, q.revision).await?;
    for (key, default) in [
        ("body", json!("")),
        (
            "raw_body",
            payload.get("body").cloned().unwrap_or(json!("")),
        ),
        ("body_type", json!("text/plain")),
        ("title", json!("")),
        ("occurred_at", Value::Null),
        ("category", Value::Null),
        ("deleted", json!(false)),
        ("included", json!(true)),
    ] {
        value[key] = payload.get(key).cloned().unwrap_or(default);
    }
    let mut metadata = payload.clone();
    if let Some(object) = metadata.as_object_mut() {
        for key in ["body", "raw_body", "raw_message", "raw_revoke"] {
            object.remove(key);
        }
    }
    // The current revision's resolved participants/direction reflect later relinking.
    if value["revision"] == value["current_revision"] {
        for key in ["participants", "direction"] {
            metadata[key] = value["metadata"].get(key).cloned().unwrap_or_else(|| {
                if key == "participants" {
                    json!([])
                } else {
                    Value::Null
                }
            });
        }
    }
    value["metadata"] = metadata;
    value.as_object_mut().unwrap().remove("current_revision");
    let copies:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',e.id,'revision',e.revision,'account_name',c.display_name,'external_account',c.external_account) FROM crm.connection_evidence e JOIN crm.source_connections c ON c.id=e.connection_id WHERE e.id=$1 OR ($2::text IS NOT NULL AND e.dedup_key=$2 AND NOT e.deleted) ORDER BY c.display_name,e.id").bind(id).bind(payload["dedup_key"].as_str()).fetch_all(&s.pool).await?;
    value["copies"] = json!(copies);
    value["events"] = sqlx::query_scalar::<_, Value>(
        "SELECT x.events FROM crm.mail_event_extractions x JOIN crm.ai_visible_outputs o ON o.job_id=x.job_id WHERE x.evidence_id=$1 AND x.revision=$2",
    )
    .bind(id)
    .bind(value["revision"].as_i64())
    .fetch_optional(&s.pool)
    .await?
    .unwrap_or(json!([]));
    if let Some(limit) = q.body_limit {
        let body = if q.original {
            value["raw_body"].as_str().or(value["body"].as_str())
        } else {
            value["body"].as_str()
        }
        .unwrap_or("");
        let total = body.chars().count();
        let offset = q.body_offset;
        let end = offset.saturating_add(limit.clamp(1, 20000));
        let selected = body
            .chars()
            .skip(offset)
            .take(end.saturating_sub(offset))
            .collect::<String>();
        value["body"] = json!(selected);
        value["raw_body"] = json!("");
        value["body_offset"] = json!(offset);
        value["next_body_offset"] = if end < total { json!(end) } else { Value::Null };
        value["total_characters"] = json!(total);
        value["original"] = json!(q.original);
    }
    value["citation"] = json!(format!("/evidence/{id}?revision={}", value["revision"]));
    Ok(Json(value))
}
async fn version(s: &AppState, id: Uuid, revision: Option<i64>) -> Result<(Value, Value)> {
    let mut value:Value=sqlx::query_scalar("SELECT to_jsonb(e)||jsonb_build_object('provider',c.provider,'account_name',c.display_name,'external_account',c.external_account,'current_revision',e.revision) FROM crm.connection_evidence e JOIN crm.source_connections c ON c.id=e.connection_id WHERE e.id=$1").bind(id).fetch_optional(&s.pool).await?.ok_or_else(missing)?;
    let provider = Provider::parse(value["provider"].as_str().unwrap_or("")).map_err(operation)?;
    let cid: Uuid =
        serde_json::from_value(value["connection_id"].clone()).map_err(|_| missing())?;
    let revision = revision.unwrap_or(value["revision"].as_i64().unwrap_or(0));
    let row=sqlx::query(&format!("SELECT payload,content_hash FROM {}.connection_revisions WHERE connection_id=$1 AND external_id=$2 AND revision=$3",provider.schema())).bind(cid).bind(value["external_id"].as_str()).bind(revision).fetch_optional(&s.pool).await?.ok_or_else(missing)?;
    value["revision"] = json!(revision);
    value["content_hash"] = json!(row.try_get::<String, _>("content_hash")?);
    Ok((value, row.try_get("payload")?))
}
fn session(h: &HeaderMap) -> Result<&str> {
    h.get("cookie")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            h.split(';')
                .find_map(|v| v.trim().strip_prefix("crm_session="))
        })
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "login required"))
}
fn redirect(s: &AppState) -> Result<String> {
    let authority = s
        .origin
        .strip_prefix("http://")
        .ok_or(invalid("请在 Mac 本机地址中连接邮箱。"))?;
    let host = authority.split(':').next().unwrap_or("");
    if !["127.0.0.1", "localhost"].contains(&host) {
        return Err(invalid("请在 Mac 本机地址中连接邮箱。"));
    }
    Ok(format!("{}{}", s.origin, mail_oauth::CALLBACK))
}
async fn oauth_config(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(json!({"redirect_uri":redirect(&s)?})))
}
#[derive(Deserialize)]
struct Authorization {
    provider: String,
    client_id: String,
    client_secret: Option<String>,
}
async fn oauth_start(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(v): Json<Authorization>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !["gmail", "outlook"].contains(&v.provider.as_str())
        || v.client_id.trim().is_empty()
        || v.client_id.len() > 256
    {
        return Err(invalid("invalid OAuth configuration"));
    }
    let url = mail_oauth::start(
        &s.pool,
        &s.credentials,
        session(&h)?,
        &v.provider,
        v.client_id.trim(),
        v.client_secret.as_deref(),
        &redirect(&s)?,
    )
    .await
    .map_err(operation)?;
    Ok(Json(json!({"authorization_url":url})))
}
#[derive(Default, Deserialize)]
struct Callback {
    #[serde(default)]
    state: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    error: String,
}
async fn oauth_callback(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(v): Query<Callback>,
) -> Result<Response> {
    user(&s, &h).await?;
    if !v.error.is_empty() || v.code.is_empty() {
        return Ok(Redirect::to("/sources?mail_authorization=cancelled").into_response());
    }
    let uri = redirect(&s)?;
    let client = mail_oauth::OAuthClient::new().map_err(operation)?;
    let result = client
        .complete(
            &s.pool,
            &s.credentials,
            session(&h)?,
            &v.state,
            &v.code,
            &uri,
        )
        .await;
    Ok(Redirect::to(if result.is_ok() {
        "/sources?mail_authorization=connected"
    } else {
        "/sources?mail_authorization=failed"
    })
    .into_response())
}

async fn file(
    State(s): State<AppState>,
    Path((id, digest)): Path<(Uuid, String)>,
    Query(q): Query<DetailQuery>,
    mut request: axum::extract::Request,
) -> Result<Response> {
    use axum::{body::Body, http::HeaderValue};
    use tower::ServiceExt;
    user(&s, request.headers()).await?;
    let (value, payload) = version(&s, id, q.revision).await?;
    let mut refs = payload["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(original) = payload.get("original") {
        refs.push(original.clone());
    }
    let attachment = refs
        .iter()
        .find(|r| r["blob"].as_str() == Some(digest.as_str()))
        .ok_or_else(missing)?;
    let cid: Uuid =
        serde_json::from_value(value["connection_id"].clone()).map_err(|_| missing())?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM crm.source_blob_refs WHERE connection_id=$1 AND sha256=$2)",
    )
    .bind(cid)
    .bind(&digest)
    .fetch_one(&s.pool)
    .await?;
    if !exists {
        return Err(missing());
    }
    let path = monica_core::source_blobs::path(&s.media_root, &digest).map_err(|_| missing())?;
    let stat = tokio::fs::metadata(&path).await.map_err(|_| missing())?;
    if !stat.is_file() {
        return Err(missing());
    }
    let etag = format!("\"{digest}\"");
    if request
        .headers()
        .get("if-none-match")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| {
            h.split(',').any(|tag| {
                tag.trim() == etag || tag.trim() == "*" || tag.trim() == format!("W/{etag}")
            })
        })
    {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [
                ("etag", etag),
                (
                    "cache-control",
                    "private, max-age=31536000, immutable".to_string(),
                ),
            ],
        )
            .into_response());
    }
    if let Some(condition) = request
        .headers()
        .get("if-range")
        .and_then(|h| h.to_str().ok())
    {
        let matches = condition == etag
            || chrono::DateTime::parse_from_rfc2822(condition)
                .ok()
                .zip(stat.modified().ok())
                .is_some_and(|(date, modified)| {
                    chrono::DateTime::<chrono::Utc>::from(modified).timestamp() <= date.timestamp()
                });
        if !matches {
            request.headers_mut().remove("range");
        }
        request.headers_mut().remove("if-range");
    }
    let name = attachment["filename"].as_str().unwrap_or("original");
    let name =
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string();
    let mut response = tower_http::services::ServeFile::new(path)
        .oneshot(request)
        .await
        .map_err(|_| missing())?
        .map(Body::new);
    response
        .headers_mut()
        .insert("etag", HeaderValue::from_str(&etag).map_err(|_| missing())?);
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        "content-disposition",
        HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{name}"))
            .map_err(|_| missing())?,
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok(response)
}
#[cfg(test)]
#[path = "connections_tests.rs"]
mod tests;

async fn progress(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let pending=[Provider::Wechat,Provider::Instagram,Provider::Linkedin,Provider::Whatsapp,Provider::AppleMessages,Provider::AppleContacts,Provider::Mail].iter().map(|p|format!("SELECT connection_id,count(*) pending_records FROM {}.connection_outbox WHERE delivered_at IS NULL GROUP BY connection_id",p.schema())).collect::<Vec<_>>().join(" UNION ALL ");
    let query = format!(
        "WITH pending AS({pending}) SELECT jsonb_build_object('id',c.id,'records',coalesce(p.records,0),'earliest_at',p.earliest_at,'latest_at',p.latest_at,'pending_records',coalesce(q.pending_records,0),'sync_pending',c.sync_pending,'state',c.state,'enabled',c.enabled,'last_success_at',c.last_success_at,'error',c.error,'coverage','已收录内容的时间范围；未完成同步的历史不计为完整覆盖') FROM crm.source_connections c LEFT JOIN crm.connection_progress p ON p.connection_id=c.id LEFT JOIN pending q ON q.connection_id=c.id ORDER BY c.created_at,c.id"
    );
    let rows: Vec<Value> = sqlx::query_scalar(&query).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn mail_rules(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('scope',scope,'target',target,'category',category) FROM crm.mail_rules WHERE connection_id=$1 ORDER BY scope,target").bind(id).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
#[derive(Deserialize)]
struct WhatsAppConnect {
    external_account: String,
    display_name: String,
}
async fn whatsapp_connect(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(v): Json<WhatsAppConnect>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let account = v.external_account.trim();
    let name = v.display_name.trim();
    if account.is_empty()
        || account.starts_with("phone-export:")
        || account.chars().count() > 320
        || name.is_empty()
        || name.chars().count() > 120
    {
        return Err(invalid("account and display name required"));
    }
    let id = core::connection_id(Provider::Whatsapp, account);
    let mut tx = s.pool.begin().await?;
    core::lock_connection(&mut tx, id)
        .await
        .map_err(operation)?;
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,'whatsapp',$2,$3,'qr',true,'connecting') ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,connection_method='qr',enabled=true,state='connecting',generation=crm.source_connections.generation+1,lease_owner=NULL,lease_token=NULL,lease_until=NULL,next_sync_at=now(),error=NULL").bind(id).bind(account).bind(name).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id})))
}
async fn whatsapp_status(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let row=sqlx::query("SELECT enabled,state,connection_method FROM crm.source_connections WHERE id=$1 AND provider='whatsapp'").bind(id).fetch_optional(&s.pool).await?.ok_or_else(missing)?;
    let enabled: bool = row.try_get("enabled")?;
    let state: String = row.try_get("state")?;
    if !enabled || row.try_get::<String, _>("connection_method")? != "qr" {
        return Ok(Json(json!({"state":state})));
    }
    let path = s
        .media_root
        .join("sources/whatsapp")
        .join(id.to_string())
        .join("status.json");
    if let Ok(meta) = tokio::fs::metadata(&path).await {
        if meta.len() > 256 * 1024 {
            return Err(invalid("invalid bridge status"));
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| operation(e.into()))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| operation(e.into()))?;
        let mut public = json!({"state":value["state"],"updated_at":value["updated_at"],"coverage":value["coverage"],"error":value["error"]});
        let fresh = value["updated_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|at| {
                (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds() < 60
            });
        if let Some(qr) = value["qr_data"]
            .as_str()
            .filter(|q| fresh && q.starts_with("data:image/png;base64,"))
        {
            public["qr_data"] = json!(qr);
        }
        return Ok(Json(public));
    }
    Ok(Json(
        json!({"state":if std::env::var_os("MONICA_WHATSAPP_BINARY").is_some(){"connecting"}else{"needs_bridge"}}),
    ))
}

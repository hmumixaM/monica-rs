//! Authenticated legacy migration evidence and deterministic offline settlement.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use axum::{
    body::Body,
    extract::Request,
    http::HeaderValue,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
#[cfg(test)]
#[path = "legacy_tests.rs"]
mod tests;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/imports/migrations", get(list))
        .route("/api/v1/imports/migrations/{id}/history", get(history))
        .route("/api/v1/imports/chatlog/{id}/issues", get(chatlog_issues))
        .route("/api/v1/imports/chatlog/{id}/tables", get(chatlog_tables))
        .route("/api/v1/imports/chatlog/{id}/history", get(chatlog_history))
        .route("/api/v1/imports/legacy/messages/{id}", get(message))
        .route("/api/v1/imports/legacy/content/{id}", get(content))
        .route(
            "/api/v1/imports/legacy/social/{id}/versions",
            get(social_versions),
        )
}

#[derive(Deserialize)]
struct SocialPage {
    before: Option<chrono::DateTime<chrono::Utc>>,
    before_id: Option<String>,
    limit: Option<i64>,
}
async fn social_versions(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<SocialPage>,
) -> Result<Json<Value>, ApiError> {
    user(&s, &h).await?;
    if q.before.is_some() != q.before_id.is_some() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Both version cursor fields are required",
        ));
    }
    let records:Vec<Value>=sqlx::query_scalar("WITH versions AS(SELECT 'legacy:'||v.id::text key,v.content_hash,v.observed_at,coalesce(v.original->>'raw_object_id',v.content_object_id::text) content_object_id,'legacy' kind FROM crm.legacy_social_versions v WHERE v.event_id=$1 UNION ALL SELECT 'chatlog:'||v.entity_id::text||':'||lpad(v.revision::text,20,'0'),c.sha256,v.created_at,coalesce(v.saved_row->>'raw_object_id',v.content_object_id::text),'chatlog' FROM crm.legacy_chatlog_versions v JOIN crm.content_objects c ON c.id=v.content_object_id WHERE v.entity_type='post' AND v.entity_id=$1) SELECT to_jsonb(v) FROM versions v WHERE $2::timestamptz IS NULL OR (v.observed_at,v.key)<($2,$3) ORDER BY v.observed_at DESC,v.key DESC LIMIT $4")
        .bind(id).bind(q.before).bind(q.before_id).bind(q.limit.unwrap_or(20).clamp(1,50)).fetch_all(&s.pool).await?;
    let next = records
        .last()
        .map(|r| json!({"before":r["observed_at"],"before_id":r["key"]}));
    Ok(Json(json!({"records":records,"next":next})))
}
async fn list(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',m.id,'status',m.status,'created_at',m.created_at,'completed_at',m.completed_at,'manifest_hash',m.manifest_hash,'tables',(SELECT jsonb_agg((to_jsonb(t)-'after_key'-'digest')||jsonb_build_object('has_history',EXISTS(SELECT 1 FROM crm.legacy_history h WHERE h.migration_id=t.migration_id AND h.store=t.store AND h.table_name=t.table_name)) ORDER BY store,table_name) FROM crm.legacy_migration_tables t WHERE t.migration_id=m.id)) FROM crm.legacy_migrations m ORDER BY created_at DESC LIMIT 20").fetch_all(&s.pool).await?;
    let chatlog:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',s.id,'status',s.status,'records',records,'inserted',inserted,'unchanged',unchanged,'changed',changed,'created_at',created_at,'completed_at',completed_at,'files',jsonb_array_length(manifest->'files'),'issues',(SELECT coalesce(jsonb_object_agg(status,n),'{}') FROM(SELECT status,count(*) n FROM crm.legacy_chatlog_issues WHERE snapshot_id=s.id GROUP BY status)i)) FROM crm.legacy_chatlog_snapshots s ORDER BY created_at DESC LIMIT 20").fetch_all(&s.pool).await?;
    let mut copies = std::collections::BTreeMap::new();
    if let Some(root) = &s.legacy_migration_root {
        for source in [
            "chatlog-db",
            "media",
            "raw",
            "staging",
            "exports",
            "backups",
            "sns-cache",
            "",
        ] {
            let name = if source.is_empty() {
                "copy-status.json".into()
            } else {
                format!("copy-status-{source}.json")
            };
            let path = root.join(name);
            if tokio::fs::metadata(&path)
                .await
                .is_ok_and(|m| m.is_file() && m.len() <= 128 * 1024)
                && let Ok(data) = tokio::fs::read(&path).await
                && let Ok(value) = serde_json::from_slice::<Value>(&data)
            {
                let label = value["source"]
                    .as_str()
                    .unwrap_or(if value["phase"] == "sqlite_snapshot" {
                        "chatlog-db"
                    } else {
                        "unknown"
                    })
                    .to_owned();
                copies.insert(label, value);
            }
        }
    }
    Ok(Json(
        json!({"records":rows,"chatlog":chatlog,"copies":copies.into_values().collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize)]
struct CatalogPage {
    after_file: Option<String>,
    after_table: Option<String>,
    limit: Option<i64>,
}
async fn chatlog_tables(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<CatalogPage>,
) -> Result<Json<Value>, ApiError> {
    user(&s, &h).await?;
    if q.after_file.is_some() != q.after_table.is_some() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Both table cursor fields are required",
        ));
    }
    let records:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(t)-'snapshot_id'-'after_row' FROM crm.legacy_chatlog_tables t WHERE snapshot_id=$1 AND ($2::text IS NULL OR (file_name,table_name)>($2,$3)) ORDER BY file_name,table_name LIMIT $4")
        .bind(id).bind(q.after_file).bind(q.after_table).bind(q.limit.unwrap_or(100).clamp(1,100)).fetch_all(&s.pool).await?;
    let next = records
        .last()
        .map(|r| json!({"after_file":r["file_name"],"after_table":r["table_name"]}));
    Ok(Json(json!({"records":records,"next":next})))
}
#[derive(Deserialize)]
struct ChatlogHistoryPage {
    file: String,
    table: String,
    after: Option<i64>,
    limit: Option<i64>,
}
async fn chatlog_history(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<ChatlogHistoryPage>,
) -> Result<Json<Value>, ApiError> {
    user(&s, &h).await?;
    let records:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('key',row_key::text,'original',payload) FROM crm.legacy_chatlog_history WHERE snapshot_id=$1 AND file_name=$2 AND table_name=$3 AND ($4::bigint IS NULL OR row_key>$4) ORDER BY row_key LIMIT $5")
        .bind(id).bind(q.file).bind(q.table).bind(q.after).bind(q.limit.unwrap_or(25).clamp(1,100)).fetch_all(&s.pool).await?;
    let next = records.last().map(|r| r["key"].clone());
    Ok(Json(json!({"records":records,"next":next})))
}

#[derive(Deserialize)]
struct IssuePage {
    after: Option<String>,
    limit: Option<i64>,
}
async fn chatlog_issues(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<IssuePage>,
) -> Result<Json<Value>, ApiError> {
    user(&s, &h).await?;
    let records:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('key',issue_key,'kind',kind,'entity_id',entity_id,'status',status,'details',details) FROM crm.legacy_chatlog_issues WHERE snapshot_id=$1 AND ($2::text IS NULL OR issue_key>$2) ORDER BY issue_key LIMIT $3")
        .bind(id).bind(q.after).bind(q.limit.unwrap_or(25).clamp(1,100)).fetch_all(&s.pool).await?;
    let next = records.last().map(|r| r["key"].clone());
    Ok(Json(json!({"records":records,"next":next})))
}
#[derive(Deserialize)]
struct Page {
    store: String,
    table: String,
    after: Option<String>,
    limit: Option<i64>,
}
async fn history(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    user(&s, &h).await?;
    let records:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('key',record_key,'original',payload) FROM crm.legacy_history WHERE migration_id=$1 AND store=$2 AND table_name=$3 AND ($4::text IS NULL OR record_key>$4) ORDER BY record_key LIMIT $5")
        .bind(id).bind(q.store).bind(q.table).bind(q.after).bind(q.limit.unwrap_or(25).clamp(1,50)).fetch_all(&s.pool).await?;
    let next = records.last().map(|r| r["key"].clone());
    Ok(Json(json!({"records":records,"next":next})))
}
async fn message(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    user(&s, &h).await?;
    let value:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(e)||jsonb_build_object('content',c.payload_json,'raw',r.payload_json) FROM crm.legacy_message_evidence e LEFT JOIN crm.content_objects c ON c.id=e.content_object_id LEFT JOIN crm.content_objects r ON r.id=e.raw_object_id WHERE message_id=$1").bind(id).fetch_optional(&s.pool).await?;
    value
        .map(Json)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "Legacy message not found"))
}

/// Reuse the existing paginated evidence UI and MCP read tool for the original
/// immutable message. Revision one always retains the original CRM content;
/// newer Chatlog corrections have their own immutable revisions.
pub async fn evidence_version(
    pool: &PgPool,
    id: Uuid,
    revision: Option<i64>,
) -> Result<Option<(Value, Value)>, ApiError> {
    type LegacyEvidenceRow = (Value, Value, Value, String, Option<Value>, Option<i64>);
    let row:Option<LegacyEvidenceRow>=sqlx::query_as("SELECT to_jsonb(m),to_jsonb(e),c.payload_json,c.sha256,p.original_row,p.revision FROM crm.message_search m JOIN crm.legacy_message_evidence e USING(message_id) JOIN crm.content_objects c ON c.id=e.content_object_id LEFT JOIN crm.legacy_chatlog_projection p ON p.entity_type='message' AND p.entity_id=m.message_id WHERE m.message_id=$1")
        .bind(id).fetch_optional(pool).await?;
    let Some((message, evidence, mut original, mut hash, baseline, current)) = row else {
        return Ok(None);
    };
    let current = current.unwrap_or(1);
    let selected = revision.unwrap_or(current);
    if selected < 1 || selected > current {
        return Ok(None);
    }
    let mut presentation = baseline.unwrap_or_else(|| message.clone());
    let mut content_id = evidence["content_object_id"].clone();
    if selected > 1 {
        let saved:Option<(Value,Value,String,Uuid)>=sqlx::query_as("SELECT v.saved_row,c.payload_json,c.sha256,c.id FROM crm.legacy_chatlog_versions v JOIN crm.content_objects c ON c.id=v.content_object_id WHERE v.entity_type='message' AND v.entity_id=$1 AND v.revision=$2")
            .bind(id).bind(selected).fetch_optional(pool).await?;
        let Some((row, payload, digest, cid)) = saved else {
            return Ok(None);
        };
        presentation = row;
        original = payload;
        hash = digest;
        content_id = json!(cid);
    }
    // Historical attribution belongs to the selected immutable observation.
    // Only the current revision follows today's confirmed person association.
    let attribution = if selected == current {
        &message
    } else {
        &presentation
    };
    let external = original.get("sender_external_id").and_then(Value::as_str);
    let sender = attribution
        .get("sender_account_id")
        .or_else(|| {
            external
                .is_none()
                .then(|| evidence["original"].get("sender_account_id"))
                .flatten()
        })
        .and_then(Value::as_str);
    let profile = attribution
        .get("sender_profile_id")
        .or_else(|| {
            external
                .is_none()
                .then(|| evidence["original"].get("sender_profile_id"))
                .flatten()
        })
        .and_then(Value::as_str);
    let mut participants:Value=sqlx::query_scalar("SELECT coalesce(jsonb_agg(jsonb_build_object('role','sender','id',a.external_id,'profile_id',CASE WHEN $3 THEN coalesce($4::uuid,l.profile_id) ELSE $4::uuid END,'name',a.display_name)),'[]'::jsonb) FROM crm.source_accounts a LEFT JOIN crm.source_account_links l ON l.account_id=a.id WHERE a.provider=$5 AND (a.id=$1::uuid OR ($1::uuid IS NULL AND a.external_id=$2))")
        .bind(sender).bind(external).bind(selected==current).bind(profile).bind(message["provider"].as_str()).fetch_one(pool).await?;
    if participants.as_array().is_some_and(Vec::is_empty)
        && let Some(profile) = profile
    {
        let person: Option<Value> = sqlx::query_scalar(
            "SELECT jsonb_build_object('role','sender','id',id,'profile_id',id,'name',display_name) FROM crm.profiles WHERE id=$1::uuid",
        )
        .bind(profile)
        .fetch_optional(pool)
        .await?;
        if let Some(person) = person {
            participants = json!([person]);
        }
    }
    let versions:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('revision',revision,'recorded_at',created_at) FROM crm.legacy_chatlog_versions WHERE entity_type='message' AND entity_id=$1 ORDER BY revision DESC LIMIT 50")
        .bind(id).fetch_all(pool).await?;
    let body = presentation
        .get("body")
        .cloned()
        .unwrap_or_else(|| message["body"].clone());
    let raw = original
        .get("text")
        .or_else(|| original.get("body"))
        .cloned()
        .or_else(|| original.as_str().map(|s| json!(s)))
        .unwrap_or_else(|| body.clone());
    let direction = presentation
        .get("direction")
        .unwrap_or(&message["direction"])
        .clone();
    Ok(Some((
        json!({"id":id,"provider":message["provider"],"account_name":message["account"],"external_account":message["account"],"external_id":evidence["source_record_id"],"revision":selected,"current_revision":current,"content_hash":hash,"versions":versions,"metadata":{"participants":participants,"direction":direction}}),
        json!({"body":body,"raw_body":raw,"body_type":"text/plain","title":"历史聊天记录","occurred_at":message["occurred_at"],"included":true,"deleted":presentation.get("tombstone").unwrap_or(&message["tombstone"]),"direction":direction,"participants":participants,"original_content_url":format!("/api/v1/imports/legacy/content/{}",content_id.as_str().unwrap()),"legacy":true}),
    )))
}

async fn content(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Result<Response, ApiError> {
    user(&s, request.headers()).await?;
    let row:Option<(Value,Option<String>)>=sqlx::query_as("SELECT c.payload_json,l.storage_uri FROM crm.content_objects c LEFT JOIN crm.legacy_content_locations l ON l.content_object_id=c.id WHERE c.id=$1").bind(id).fetch_optional(&s.pool).await?;
    let (value, uri) = row.ok_or(ApiError(
        StatusCode::NOT_FOUND,
        "Original content not found",
    ))?;
    if let Some(uri) = uri {
        let relative = uri.strip_prefix("/data/").ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "Original storage location is not mapped",
        ))?;
        let valid_prefix = ["media/", "raw/", "staging/", "exports/", "sns-cache/"]
            .iter()
            .any(|prefix| relative.starts_with(prefix));
        if !valid_prefix
            || std::path::Path::new(relative)
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Original storage location is not mapped",
            ));
        }
        let root = s.legacy_archive_root.as_ref().ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "Original archive is not mounted",
        ))?;
        let root = tokio::fs::canonicalize(root)
            .await
            .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Original archive is unavailable"))?;
        let path = tokio::fs::canonicalize(root.join(relative))
            .await
            .map_err(|_| {
                ApiError(
                    StatusCode::NOT_FOUND,
                    "Original file is missing from the source snapshot",
                )
            })?;
        if !path.starts_with(&root) {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Original file is outside its archive",
            ));
        }
        let mut response = tower_http::services::ServeFile::new(path)
            .oneshot(request)
            .await
            .unwrap()
            .map(Body::new);
        response.headers_mut().insert(
            "content-disposition",
            HeaderValue::from_str(&format!("attachment; filename=\"original-{id}\"")).unwrap(),
        );
        return Ok(response);
    }
    Ok(Json(value).into_response())
}

pub async fn finalize(pool: &PgPool, id: Uuid) -> anyhow::Result<()> {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await?;
    anyhow::ensure!(
        name.starts_with("monica_rust"),
        "dedicated Rust database required"
    );
    let ready:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.legacy_migrations WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM crm.legacy_migration_tables WHERE migration_id=$1 AND (status<>'complete' OR processed<>expected))").bind(id).fetch_one(pool).await?;
    anyhow::ensure!(
        ready,
        "all migration tables must be committed before settlement"
    );
    sqlx::query("UPDATE crm.relationship_edges e SET legacy_source_owned=true FROM crm.legacy_relationship_owners o WHERE o.edge_id=e.id AND NOT e.legacy_source_owned")
        .execute(pool).await?;
    loop {
        let count=sqlx::query("WITH batch AS(SELECT m.message_id FROM crm.message_search m JOIN crm.legacy_message_evidence e USING(message_id) WHERE m.sender_account_id IS NOT NULL AND m.sender_profile_id IS NOT NULL ORDER BY m.message_id LIMIT 10000) UPDATE crm.message_search m SET sender_profile_id=NULL FROM batch b WHERE m.message_id=b.message_id")
            .execute(pool).await?.rows_affected();
        if count == 0 {
            break;
        }
        println!(
            "{}",
            json!({"phase":"message_attribution","corrected":count})
        );
    }
    // Imported explicit sequence values must precede ordinary application writes.
    let sequences:Vec<(String,String,String)>=sqlx::query_as("SELECT table_schema,table_name,column_name FROM information_schema.columns WHERE table_schema='crm' AND is_identity='YES'").fetch_all(pool).await?;
    for (schema, table, column) in sequences {
        anyhow::ensure!(
            [&schema, &table, &column]
                .iter()
                .all(|s| s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')),
            "unexpected sequence identifier"
        );
        let qualified = format!("{schema}.{table}");
        let query = format!(
            "SELECT setval(pg_get_serial_sequence($1,$2),coalesce(max({column}),1),count(*)>0) FROM {qualified}"
        );
        sqlx::query(&query)
            .bind(qualified)
            .bind(column)
            .execute(pool)
            .await?;
    }
    // Preserve the imported immutable heads as the starting point for a later
    // explicit source reconnection. Legacy work is never queued for replay.
    for provider in ["wechat", "instagram", "linkedin"] {
        sqlx::query(&format!("INSERT INTO crm.projections(provider,object_id,version) SELECT $1,id,revision FROM {provider}.objects ON CONFLICT DO NOTHING"))
            .bind(provider).execute(pool).await?;
    }
    let conversations: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM crm.conversations ORDER BY id")
            .fetch_all(pool)
            .await?;
    for conversation in conversations {
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE crm.conversations c SET message_count=(SELECT count(*) FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone),first_message_at=(SELECT min(occurred_at) FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone),last_message_at=(SELECT max(occurred_at) FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone),last_message_id=(SELECT message_id FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1),last_message_preview=(SELECT left(body,80) FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1) WHERE id=$1")
            .bind(conversation).execute(&mut *tx).await?;
        for (table, timezone) in [
            ("message_months", "America/Chicago"),
            ("message_months_utc", "UTC"),
        ] {
            sqlx::query(&format!("DELETE FROM crm.{table} WHERE conversation_id=$1"))
                .bind(conversation)
                .execute(&mut *tx)
                .await?;
            sqlx::query(&format!("INSERT INTO crm.{table}(conversation_id,month,sent,received,first_message_at,last_message_at) SELECT conversation_id,to_char(occurred_at AT TIME ZONE $2,'YYYY-MM'),count(*) FILTER(WHERE sender=account),count(*) FILTER(WHERE sender<>account),min(occurred_at),max(occurred_at) FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone GROUP BY conversation_id,2"))
                .bind(conversation).bind(timezone).execute(&mut *tx).await?;
        }
        // A later append must increment the imported baseline, and sidebar reads
        // must work without replaying legacy jobs through the live queue.
        sqlx::query("INSERT INTO crm.conversation_state(provider,account,conversation,message_count,last_at_ms,last_id,preview,generation) SELECT c.provider,c.account,c.external_id,c.message_count,coalesce((extract(epoch FROM c.last_message_at)*1000)::bigint,0),coalesce(m.external_id,''),coalesce(c.last_message_preview,''),1 FROM crm.conversations c LEFT JOIN crm.message_search m ON m.message_id=c.last_message_id WHERE c.id=$1 ON CONFLICT(provider,account,conversation) DO UPDATE SET message_count=excluded.message_count,last_at_ms=excluded.last_at_ms,last_id=excluded.last_id,preview=excluded.preview,generation=crm.conversation_state.generation+1")
            .bind(conversation).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO crm.conversation_summaries SELECT s.* FROM crm.conversation_state s JOIN crm.conversations c ON (c.provider,c.account,c.external_id)=(s.provider,s.account,s.conversation) WHERE c.id=$1 ON CONFLICT(provider,account,conversation) DO UPDATE SET message_count=excluded.message_count,last_at_ms=excluded.last_at_ms,last_id=excluded.last_id,preview=excluded.preview,generation=excluded.generation")
            .bind(conversation).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    let mut after = Uuid::nil();
    loop {
        let profiles: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM crm.profiles WHERE id>$1 ORDER BY id LIMIT 100")
                .bind(after)
                .fetch_all(pool)
                .await?;
        if profiles.is_empty() {
            break;
        }
        for profile in profiles {
            let mut tx = pool.begin().await?;
            monica_core::linking::rebuild_profile(&mut tx, profile).await?;
            monica_core::ai::memory::profile_facts(&mut tx, profile).await?;
            tx.commit().await?;
            after = profile;
        }
        println!("{}", json!({"phase":"profile_projection","after":after}));
    }
    while monica_core::ai::memory::legacy::drain(pool).await? > 0 {
        println!("{}", json!({"phase":"memory_inputs"}));
    }
    monica_core::pages::refresh_metrics(pool).await?;
    sqlx::query("UPDATE crm.legacy_migrations SET status='database_ready' WHERE id=$1")
        .bind(id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE crm.import_activity SET phase='database_ready',state='saved',updated_at=now(),heartbeat_at=now() WHERE id='migration:'||$1::text").bind(id).execute(pool).await?;
    println!("{}", json!({"phase":"database_ready","migration_id":id}));
    Ok(())
}

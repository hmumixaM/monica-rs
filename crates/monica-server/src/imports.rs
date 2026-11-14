use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn err(code: StatusCode, message: &'static str) -> ApiError {
    ApiError(code, message)
}
fn failed(e: anyhow::Error) -> ApiError {
    tracing::warn!(error=%e,"import rejected");
    err(
        StatusCode::CONFLICT,
        "Import conflicts with a saved identity or checkpoint",
    )
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/imports/batches", post(accept).get(batches))
        .route("/api/v1/imports/batches/{id}", get(detail))
        .route("/api/v1/imports/batches/{id}/replay", post(replay))
        .route("/api/v1/imports/tokens", get(tokens).post(create_token))
        .route("/api/v1/imports/tokens/{id}/revoke", post(revoke))
        .route("/api/v1/imports/cursors", get(cursors))
        .route("/api/v1/imports/jobs", get(jobs))
        .route("/api/v1/imports/events", get(events))
        .route("/api/v1/imports/reconciliation", get(reconciliation))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
}
fn header<'a>(h: &'a HeaderMap, name: &str) -> Result<&'a str> {
    h.get(name)
        .and_then(|x| x.to_str().ok())
        .ok_or(err(StatusCode::UNAUTHORIZED, "import signature required"))
}
async fn accept(
    State(s): State<AppState>,
    h: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>)> {
    let secret = s.import_hmac_secret.as_deref().ok_or(err(
        StatusCode::SERVICE_UNAVAILABLE,
        "Import authentication is not configured",
    ))?;
    let stamp = header(&h, "x-import-timestamp")?;
    let nonce = header(&h, "x-import-nonce")?;
    let signature = header(&h, "x-import-signature")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let ts = stamp
        .parse::<i64>()
        .map_err(|_| err(StatusCode::UNAUTHORIZED, "invalid import signature"))?;
    if now.abs_diff(ts) > 300
        || nonce.is_empty()
        || nonce.len() > 255
        || signature.len() != 64
        || !signature.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid import signature"));
    }
    let signature = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&signature[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| err(StatusCode::UNAUTHORIZED, "invalid import signature"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| {
        err(
            StatusCode::SERVICE_UNAVAILABLE,
            "Import authentication is not configured",
        )
    })?;
    mac.update(stamp.as_bytes());
    mac.update(b".");
    mac.update(nonce.as_bytes());
    mac.update(b".");
    mac.update(&body);
    mac.verify_slice(&signature)
        .map_err(|_| err(StatusCode::UNAUTHORIZED, "invalid import signature"))?;
    let batch: monica_core::imports::Batch = serde_json::from_slice(&body)
        .map_err(|_| err(StatusCode::UNPROCESSABLE_ENTITY, "invalid import batch"))?;
    batch
        .validate()
        .map_err(|_| err(StatusCode::UNPROCESSABLE_ENTITY, "invalid import batch"))?;
    let mut tx = s.pool.begin().await?;
    if let Some(cid) = batch.connection_id {
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM crm.source_connections WHERE id=$1 AND provider=$2)",
        )
        .bind(cid)
        .bind(batch.source.schema())
        .fetch_one(&mut *tx)
        .await?;
        if !valid {
            return Err(err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Source connection does not match import provider",
            ));
        }
    }
    // Once an installation issues a collector token, never fall back to
    // HMAC-only imports, including after every token has been revoked.
    let tokens_required: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.import_service_tokens)")
            .fetch_one(&mut *tx)
            .await?;
    if tokens_required {
        let token = header(&h, "x-import-token")?;
        let n=sqlx::query("UPDATE crm.import_service_tokens SET last_used_at=now() WHERE source=$1 AND token_hash=$2 AND revoked_at IS NULL").bind(batch.source.schema()).bind(format!("{:x}",Sha256::digest(token.as_bytes()))).execute(&mut *tx).await?.rows_affected();
        if n != 1 {
            return Err(err(
                StatusCode::UNAUTHORIZED,
                "invalid import service token",
            ));
        }
    }
    sqlx::query("DELETE FROM crm.import_request_nonces WHERE expires_at<now()")
        .execute(&mut *tx)
        .await?;
    let n=sqlx::query("INSERT INTO crm.import_request_nonces(nonce,expires_at) VALUES($1,now()+interval '5 minutes') ON CONFLICT DO NOTHING").bind(nonce).execute(&mut *tx).await?.rows_affected();
    if n != 1 {
        return Err(err(StatusCode::CONFLICT, "replayed import request"));
    }
    let id =
        monica_core::imports::receive(&mut tx, &batch, &format!("{:x}", Sha256::digest(&body)))
            .await
            .map_err(failed)?;
    tx.commit().await?;
    let value = monica_core::imports::receipt(&s.pool, id)
        .await
        .map_err(failed)?;
    Ok((StatusCode::CREATED, Json(value)))
}
async fn batches(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(b)-'body_hash'-(CASE WHEN connection_id IS NULL THEN 'connection_id' ELSE '' END) FROM crm.import_batches b ORDER BY received_at DESC LIMIT 100").fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn batch_id(s: &AppState, id: &str) -> Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM crm.import_batches WHERE id::text=$1 OR batch_id=$1 ORDER BY received_at DESC LIMIT 1").bind(id).fetch_optional(&s.pool).await?.ok_or(err(StatusCode::NOT_FOUND,"batch not found"))
}
async fn detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let id = batch_id(&s, &id).await?;
    let row:Value=sqlx::query_scalar("SELECT (to_jsonb(b)-'body_hash')||jsonb_build_object('raw_manifest',i.payload,'raw_manifest_uri',NULL) FROM crm.import_batches b JOIN crm.import_batch_inputs i ON i.batch_id=b.id WHERE b.id=$1").bind(id).fetch_one(&s.pool).await?;
    Ok(Json(row))
}
async fn replay(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let id = batch_id(&s, &id).await?;
    sqlx::query("UPDATE crm.import_batches SET status=CASE WHEN status='failed' THEN 'received' ELSE status END,failure_count=0,error_summary=NULL,retry_after=clock_timestamp() WHERE id=$1").bind(id).execute(&s.pool).await?;
    Ok(Json(
        monica_core::imports::receipt(&s.pool, id)
            .await
            .map_err(failed)?,
    ))
}
#[derive(Deserialize)]
struct Token {
    source: String,
    label: String,
}
async fn create_token(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Token>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    if !["wechat", "instagram", "linkedin", "monica", "faces"].contains(&p.source.as_str())
        || p.label.is_empty()
        || p.label.len() > 120
    {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid token source or label",
        ));
    }
    let raw = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let id = Uuid::new_v4();
    let mut tx = s.pool.begin().await?;
    let row:Value=sqlx::query_scalar("INSERT INTO crm.import_service_tokens(id,source,label,token_hash) VALUES($1,$2,$3,$4) RETURNING to_jsonb(import_service_tokens)-'token_hash'").bind(id).bind(p.source).bind(p.label).bind(format!("{:x}",Sha256::digest(raw.as_bytes()))).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) VALUES($1,'import.token.created','import_service_token',$2)").bind(actor).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    let mut row = row;
    row["token"] = json!(raw);
    Ok((StatusCode::CREATED, Json(row)))
}
async fn tokens(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t)-'token_hash' FROM crm.import_service_tokens t ORDER BY created_at DESC",
    )
    .fetch_all(&s.pool)
    .await?;
    Ok(Json(json!(rows)))
}
async fn revoke(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    if sqlx::query(
        "UPDATE crm.import_service_tokens SET revoked_at=now() WHERE id=$1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        != 1
    {
        return Err(err(StatusCode::NOT_FOUND, "import token not found"));
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) VALUES($1,'import.token.revoked','import_service_token',$2)").bind(actor).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"status":"revoked"})))
}
async fn cursors(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(c)||jsonb_build_object('cursor_value',cursor) FROM crm.source_cursors c ORDER BY source,stream").fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
#[derive(Deserialize)]
struct Page {
    limit: Option<i64>,
    status_filter: Option<String>,
}
async fn jobs(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(j)||jsonb_build_object('status',state,'job_type','source.project','payload',jsonb_build_object('provider',provider,'object_id',object_id,'version',version)) FROM (SELECT * FROM crm.jobs UNION ALL SELECT * FROM crm.job_history) j WHERE $1::text IS NULL OR state=$1 ORDER BY id DESC LIMIT $2").bind(q.status_filter).bind(q.limit.unwrap_or(100).clamp(1,200)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn events(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let sql=["wechat","instagram","linkedin","whatsapp","apple_messages","apple_contacts","mail"].iter().map(|p|format!("SELECT jsonb_build_object('id',o.object_id,'source','{p}','external_id',x.external_id,'content_hash',o.hash,'state',CASE WHEN o.version=1 THEN 'new' ELSE 'changed' END,'observed_at',m.observed_at,'source_event',o.event_id) AS value,m.observed_at FROM {p}.observations o JOIN {p}.objects x ON x.id=o.object_id LEFT JOIN {p}.revision_metadata m ON m.object_id=o.object_id AND m.version=o.version")).collect::<Vec<_>>().join(" UNION ALL ");
    let rows: Vec<Value> = sqlx::query_scalar(&format!(
        "SELECT value FROM ({sql}) e ORDER BY observed_at DESC NULLS LAST LIMIT $1"
    ))
    .bind(q.limit.unwrap_or(100).clamp(1, 200))
    .fetch_all(&s.pool)
    .await?;
    Ok(Json(json!(rows)))
}
async fn reconciliation(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut sources:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('source',source,'stream',stream,'source_digest',(array_agg(body_hash ORDER BY received_at DESC,id DESC))[1],'source_count',sum(record_count),'projected_count',sum(processed_count),'excluded_count',0,'missing_ids',coalesce(jsonb_agg(batch_id ORDER BY received_at) FILTER(WHERE processed_count<record_count),'[]'),'stage',CASE WHEN bool_and(processed_count=record_count) THEN 'durable' ELSE 'receiving' END,'details',jsonb_build_object('batches',count(*),'inserted',sum(inserted_count),'changed',sum(changed_count),'unchanged',sum(unchanged_count)),'checked_at',max(coalesce(completed_at,received_at))) FROM crm.import_batches GROUP BY source,stream ORDER BY source,stream").fetch_all(&s.pool).await?;
    let pages:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('source',provider,'stream',stream,'source_digest',NULL,'source_count',sum(records),'projected_count',sum(processed),'excluded_count',0,'missing_ids','[]'::jsonb,'stage',CASE WHEN bool_and(state='complete') THEN 'durable' ELSE 'receiving' END,'details',jsonb_build_object('pages',count(*),'inserted',sum(inserted),'changed',sum(changed),'unchanged',sum(unchanged)),'checked_at',max(updated_at)) FROM crm.import_activity WHERE kind='page' GROUP BY provider,stream ORDER BY provider,stream").fetch_all(&s.pool).await?;
    sources.extend(pages);
    let jobs:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('job_type',job_type,'status',status,'count',sum(n)) FROM (SELECT 'project_source'::text job_type,state status,count(*) n FROM crm.jobs GROUP BY state UNION ALL SELECT 'project_source','complete',count(*) FROM crm.job_history HAVING count(*)>0 UNION ALL SELECT 'conversation_summary','queued',count(*) FROM crm.conversation_work HAVING count(*)>0) x GROUP BY job_type,status ORDER BY job_type,status").fetch_all(&s.pool).await?;
    let media:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('state',state,'count',count(*)) FROM crm.media_dependencies GROUP BY state ORDER BY state").fetch_all(&s.pool).await?;
    let messages:Value=sqlx::query_scalar("SELECT jsonb_build_object('processed_tables',(SELECT count(*) FROM crm.conversation_state),'source_rows',(SELECT count(*) FROM wechat.messages)+(SELECT count(*) FROM instagram.messages)+(SELECT count(*) FROM linkedin.messages)+(SELECT count(*) FROM whatsapp.messages)+(SELECT count(*) FROM apple_messages.messages)+(SELECT count(*) FROM apple_contacts.messages)+(SELECT count(*) FROM mail.messages),'processed_rows',(SELECT count(*) FROM crm.message_search))").fetch_one(&s.pool).await?;
    Ok(Json(
        json!({"sources":sources,"media":media,"jobs":jobs,"messages":messages}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use sqlx::PgPool;
    use tower::ServiceExt;

    fn signed(body: &[u8], nonce: &str, secret: &str) -> Request<Body> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(stamp.as_bytes());
        mac.update(b".");
        mac.update(nonce.as_bytes());
        mac.update(b".");
        mac.update(body);
        let signature = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Request::post("/api/v1/imports/batches")
            .header("host", "localhost:17494")
            .header("content-type", "application/json")
            .header("x-import-timestamp", stamp)
            .header("x-import-nonce", nonce)
            .header("x-import-signature", signature)
            .body(Body::from(body.to_vec()))
            .unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn signed_http_import_nonce_token_idempotency_and_real_projection(pool: PgPool) {
        let dir = tempfile::tempdir().unwrap();
        let secret = "synthetic-import-signature-secret-32";
        let mut state = AppState::new(
            pool.clone(),
            "synthetic-session-secret-32-characters",
            "http://localhost:17494".into(),
            dir.path().into(),
        )
        .await
        .unwrap();
        state.import_hmac_secret = Some(secret.into());
        let router = crate::auth::routes(state);
        let body=serde_json::to_vec(&json!({"source":"instagram","stream":"posts","batch_id":"fixture","idempotency_key":"fixture","observed_at":"2026-01-01T00:00:00Z","records":[{"external_id":"post-one","entity_type":"post","source_account_external_id":"fixture-account","occurred_at":"2026-01-01T00:00:00Z","content":{"text":"Synthetic post"}}]})).unwrap();
        let response = router
            .clone()
            .oneshot(signed(&body, "nonce-one", secret))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 100000).await.unwrap()).unwrap();
        assert_eq!(value["inserted_count"], 0);
        assert_eq!(value["status"], "received");
        assert_eq!(
            router
                .clone()
                .oneshot(signed(&body, "nonce-one", secret))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            router
                .clone()
                .oneshot(signed(&body, "nonce-two", secret))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            router
                .clone()
                .oneshot(signed(&body, "nonce-bad", "wrong-key"))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        while monica_core::imports::resume_pending(&pool).await.unwrap() > 0 {}
        while let Some(lease) = monica_core::claim(&pool).await.unwrap() {
            monica_core::project(&pool, &lease).await.unwrap();
        }
        let text: String = sqlx::query_scalar("SELECT content->>'text' FROM crm.timeline_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(text, "Synthetic post");
        sqlx::query("INSERT INTO crm.import_service_tokens(id,source,label,token_hash) VALUES($1,'instagram','fixture',$2)").bind(Uuid::new_v4()).bind(format!("{:x}",Sha256::digest(b"collector-token"))).execute(&pool).await.unwrap();
        assert_eq!(
            router
                .clone()
                .oneshot(signed(&body, "nonce-token", secret))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let mut request = signed(&body, "nonce-token", secret);
        request
            .headers_mut()
            .insert("x-import-token", "collector-token".parse().unwrap());
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::CREATED
        );
        sqlx::query("UPDATE crm.import_service_tokens SET revoked_at=now()")
            .execute(&pool)
            .await
            .unwrap();
        // Revoking the last key never restores HMAC-only access, including
        // attempts to switch to a provider with no issued token.
        for (index, source) in ["instagram", "wechat"].into_iter().enumerate() {
            let mut other: Value = serde_json::from_slice(&body).unwrap();
            other["source"] = json!(source);
            let other = serde_json::to_vec(&other).unwrap();
            for with_token in [false, true] {
                let mut request = signed(&other, &format!("revoked-{index}-{with_token}"), secret);
                if with_token {
                    request
                        .headers_mut()
                        .insert("x-import-token", "collector-token".parse().unwrap());
                }
                assert_eq!(
                    router.clone().oneshot(request).await.unwrap().status(),
                    StatusCode::UNAUTHORIZED
                );
            }
        }
        sqlx::query("INSERT INTO crm.import_service_tokens(id,source,label,token_hash) VALUES($1,'instagram','replacement',$2)")
            .bind(Uuid::new_v4()).bind(format!("{:x}",Sha256::digest(b"replacement-token"))).execute(&pool).await.unwrap();
        let mut request = signed(&body, "replacement", secret);
        request
            .headers_mut()
            .insert("x-import-token", "replacement-token".parse().unwrap());
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::CREATED
        );
        let mut request = signed(&body, "nonce-unicode", secret);
        request.headers_mut().insert(
            "x-import-signature",
            format!("{}{}", "a".repeat(62), "é").parse().unwrap(),
        );
        assert_eq!(
            router.oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn signed_connection_batches_isolate_same_ids_and_preserve_explicit_profiles(
        pool: PgPool,
    ) {
        use monica_core::{Provider, connections, imports};
        let dir = tempfile::tempdir().unwrap();
        let secret = "synthetic-signed-connection-secret";
        let mut state = AppState::new(
            pool.clone(),
            "synthetic-session-secret-32-characters",
            "http://localhost:17494".into(),
            dir.path().into(),
        )
        .await
        .unwrap();
        state.import_hmac_secret = Some(secret.into());
        let router = routes().with_state(state);
        let mut profiles = vec![];
        let mut connections_ids = vec![];
        for name in ["first", "second"] {
            let profile = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2)",
            )
            .bind(profile)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
            profiles.push(profile);
            let cid = connections::connection_id(Provider::Linkedin, name);
            sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,state,enabled) VALUES($1,'linkedin',$2,$2,'browser','connected',true)").bind(cid).bind(name).execute(&pool).await.unwrap();
            connections_ids.push(cid);
        }
        for (i, cid) in connections_ids.iter().enumerate() {
            let value = json!({"source":"linkedin","connection_id":cid,"stream":"messages","batch_id":"same-batch","idempotency_key":format!("scope-{i}"),"observed_at":"2026-01-01T12:00:00Z","cursor_after":"1","records":[{"external_id":"same-message","conversation_external_id":"same-thread","conversation_type":"direct","profile_id":profiles[i],"sender_profile_id":profiles[i],"occurred_at":"2026-01-01T00:00:00Z","content":{"text":format!("hello-{i}")}}]});
            let body = serde_json::to_vec(&value).unwrap();
            let response = router
                .clone()
                .oneshot(signed(&body, &format!("cid-{i}"), secret))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
            let receipt: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                    .unwrap();
            let receipt_id: Uuid = receipt["id"].as_str().unwrap().parse().unwrap();
            imports::resume(&pool, receipt_id).await.unwrap();
            let visible: (String, i64) =
                sqlx::query_as("SELECT state,processed FROM crm.import_activity WHERE id=$1")
                    .bind(format!("batch:{receipt_id}"))
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                visible,
                ("saved".into(), 0),
                "journal completion is not query readiness"
            );
            connections::deliver(&pool, Provider::Linkedin, *cid, 100)
                .await
                .unwrap();
            assert_eq!(
                router
                    .clone()
                    .oneshot(signed(&body, &format!("cid-{i}"), secret))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::CONFLICT
            );
            let replay = router
                .clone()
                .oneshot(signed(&body, &format!("replay-{i}"), secret))
                .await
                .unwrap();
            assert_eq!(replay.status(), StatusCode::CREATED);
            let replay: Value =
                serde_json::from_slice(&to_bytes(replay.into_body(), 1024 * 1024).await.unwrap())
                    .unwrap();
            assert_eq!(replay["id"], receipt["id"]);
            let state:(Option<Uuid>,Option<Uuid>,String)=sqlx::query_as("SELECT c.profile_id,m.sender_profile_id,m.body FROM crm.conversations c JOIN crm.message_search m ON m.conversation_id=c.id WHERE c.connection_id=$1").bind(cid).fetch_one(&pool).await.unwrap();
            assert_eq!(
                state,
                (Some(profiles[i]), Some(profiles[i]), format!("hello-{i}"))
            );
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.conversations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
        let cid = connections_ids[0];
        let counted: i64 = sqlx::query_scalar(
            "SELECT sum(records)::bigint FROM crm.import_activity WHERE provider='linkedin'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let receipts: i64 = sqlx::query_scalar(
            "SELECT sum(record_count)::bigint FROM crm.import_batches WHERE source='linkedin'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            counted, receipts,
            "signed connection pages are tracked by receipts only"
        );
        let changed = json!({"source":"linkedin","connection_id":cid,"stream":"messages","batch_id":"edit","idempotency_key":"edit","observed_at":"2026-01-02T12:00:00Z","cursor_before":"1","cursor_after":"2","records":[{"external_id":"same-message","conversation_external_id":"same-thread","occurred_at":"2026-01-01T00:00:00Z","content":{"text":"edited"}}]});
        let body = serde_json::to_vec(&changed).unwrap();
        let response = router
            .clone()
            .oneshot(signed(&body, "edited", secret))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let receipt: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        imports::resume(&pool, receipt["id"].as_str().unwrap().parse().unwrap())
            .await
            .unwrap();
        connections::deliver(&pool, Provider::Linkedin, cid, 100)
            .await
            .unwrap();
        connections::relink_pending(&pool, 100).await.unwrap();
        let state:(Option<Uuid>,Option<Uuid>,String)=sqlx::query_as("SELECT c.profile_id,m.sender_profile_id,m.body FROM crm.conversations c JOIN crm.message_search m ON m.conversation_id=c.id WHERE c.connection_id=$1").bind(cid).fetch_one(&pool).await.unwrap();
        assert_eq!(state, (None, None, "edited".into()));
        let mut mismatch = changed;
        mismatch["source"] = json!("mail");
        assert_eq!(
            router
                .oneshot(signed(
                    &serde_json::to_vec(&mismatch).unwrap(),
                    "wrong-provider",
                    secret
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}

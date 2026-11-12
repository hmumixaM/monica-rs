//! Authenticated WAL audit and independent durable pipeline inspection.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
type Result<T> = std::result::Result<T, ApiError>;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cdc/events", get(events))
        .route("/api/v1/cdc/events/{event}", get(detail))
        .route("/api/v1/cdc/status", get(cdc_status))
        .route("/api/v1/pipeline/status", get(status))
        .route("/api/v1/pipeline/tasks/{event}/retry", post(retry))
}
fn enabled(s: &AppState) -> Result<()> {
    if s.cdc_enabled {
        Ok(())
    } else {
        Err(ApiError(StatusCode::NOT_FOUND, "CDC is not enabled"))
    }
}
#[derive(Deserialize)]
struct Filters {
    limit: Option<i64>,
    before: Option<i64>,
    provider: Option<String>,
}
async fn events(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filters>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    enabled(&s)?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'provider',provider,'commit_lsn',commit_lsn::text,'relation_name',relation_name,'operation',operation,'captured_at',captured_at,'committed_at',committed_at) FROM crm.cdc_events WHERE ($1::bigint IS NULL OR id<$1) AND ($2::text IS NULL OR provider=$2) ORDER BY id DESC LIMIT $3")
 .bind(q.before).bind(q.provider).bind(q.limit.unwrap_or(50).clamp(1,200)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn business_values(s: &AppState, row: &Value, side: &str) -> Result<Value> {
    let value = &row[side];
    if row["provider"] == "crm" && row["relation_name"] == "field_current" {
        if let Some(id) = value["revision_id"]
            .as_str()
            .and_then(|v| uuid::Uuid::parse_str(v).ok())
        {
            let field: Option<Value> = sqlx::query_scalar(
                "SELECT jsonb_build_object(field_key,value) FROM crm.field_revisions WHERE id=$1",
            )
            .bind(id)
            .fetch_optional(&s.pool)
            .await?;
            return Ok(field.unwrap_or_else(|| value.clone()));
        }
        return Ok(json!({}));
    }
    Ok(value.clone())
}
async fn detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    enabled(&s)?;
    let mut row:Value=sqlx::query_scalar("SELECT to_jsonb(e)||jsonb_build_object('commit_lsn',commit_lsn::text) FROM crm.cdc_events e WHERE id=$1").bind(id).fetch_optional(&s.pool).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"CDC event not found"))?;
    let old = business_values(&s, &row, "before_row").await?;
    let new = business_values(&s, &row, "after_row").await?;
    let keys: BTreeSet<&String> = old
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys())
        .chain(new.as_object().into_iter().flat_map(|m| m.keys()))
        .collect();
    row["changed_fields"]=json!(keys.into_iter().filter(|key| !["updated_at","last_seen_at"].contains(&key.as_str()) && old.get(*key)!=new.get(*key)).map(|key|json!({"field":key,"before":old.get(key),"after":new.get(key),"had_before":old.get(key).is_some(),"has_after":new.get(key).is_some()})).collect::<Vec<_>>());
    Ok(Json(row))
}
async fn cdc_status(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    enabled(&s)?;
    read_status(&s).await
}
async fn status(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    read_status(&s).await
}
async fn read_status(s: &AppState) -> Result<Json<Value>> {
    let consumers:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('provider',provider,'state',state,'retained_wal_bytes',retained_wal_bytes,'heartbeat_age_seconds',extract(epoch from now()-heartbeat_at)) FROM crm.cdc_consumers ORDER BY provider").fetch_all(&s.pool).await?;
    let projection:Value=sqlx::query_scalar("SELECT jsonb_build_object('count',count(*),'oldest_seconds',extract(epoch from now()-min(created_at))) FROM crm.jobs").fetch_one(&s.pool).await?;
    let media:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('reason',reason,'count',count(*)) FROM crm.media_dependencies WHERE state='waiting' GROUP BY reason ORDER BY reason").fetch_all(&s.pool).await?;
    let lanes:Vec<Value>=sqlx::query_scalar("WITH work AS (SELECT 'projection'::text lane,j.state status,j.created_at,coalesce(f.retry_at>now() OR f.quarantined,false) retry FROM crm.jobs j LEFT JOIN crm.scheduler_failures f ON f.queue='object' AND f.work_key=j.id::text UNION ALL SELECT 'ai',status,created_at,false FROM crm.ai_jobs WHERE status IN ('queued','running','failed') UNION ALL SELECT 'adapter:'||provider,status,created_at,false FROM crm.adapter_runs WHERE status IN ('queued','running','failed') UNION ALL SELECT 'import',status,received_at,false FROM crm.import_batches WHERE status NOT IN ('complete')) SELECT jsonb_build_object('lane',lane,'status',status,'count',count(*),'oldest_seconds',extract(epoch from now()-min(created_at)),'retry_waiting',count(*) FILTER(WHERE retry)) FROM work GROUP BY lane,status ORDER BY lane,status").fetch_all(&s.pool).await?;
    Ok(Json(
        json!({"consumers":consumers,"projection":projection,"media_waiting":media,"lanes":lanes}),
    ))
}
async fn retry(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    let state: Option<String> =
        sqlx::query_scalar("SELECT state FROM crm.jobs WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if state.is_none() {
        let complete: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.job_history WHERE id=$1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        return if complete {
            Ok(Json(json!({"state":"complete"})))
        } else {
            Err(ApiError(StatusCode::NOT_FOUND, "Projection task not found"))
        };
    }
    // Clearing the token fences an older worker before admitting a retry.
    sqlx::query("UPDATE crm.jobs SET state='queued',token=NULL,lease_until=NULL WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM crm.scheduler_failures WHERE queue='object' AND work_key=$1")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"state":"queued"})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    async fn call(app: &Router, method: &str, path: &str) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "127.0.0.1:17494")
                    .header("cookie", "crm_session=synthetic-cdc; crm_csrf=fixture")
                    .header("x-csrf-token", "fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1048576).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn audit_gates_fields_pagination_pipeline_and_fenced_retry(pool: sqlx::PgPool) {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'cdc@example.invalid','unused')").bind(id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(uuid::Uuid::new_v4()).bind(id).bind(monica_core::identity::bytes_hash(b"synthetic-cdc")).execute(&pool).await.unwrap();
        let mut state = AppState::new(
            pool.clone(),
            "synthetic-signing-key-at-least-32-bytes",
            "http://127.0.0.1:17494".into(),
            root.path().into(),
        )
        .await
        .unwrap();
        state.cdc_enabled = false;
        let app = crate::auth::routes(state.clone());
        assert_eq!(
            call(&app, "GET", "/api/v1/cdc/events").await.0,
            StatusCode::NOT_FOUND
        );
        let (status, body) = call(&app, "GET", "/api/v1/pipeline/status").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["projection"]["count"], 0);
        state.cdc_enabled = true;
        let app = crate::auth::routes(state);
        let event:i64=sqlx::query_scalar("INSERT INTO crm.cdc_events(provider,commit_lsn,ordinal,relation_name,operation,before_row,after_row,committed_at) VALUES('mail','0/123',0,'accounts','U','{\"display_name\":\"Before\",\"updated_at\":\"a\"}','{\"display_name\":\"After\",\"updated_at\":\"b\"}',now()) RETURNING id").fetch_one(&pool).await.unwrap();
        let (_, detail) = call(&app, "GET", &format!("/api/v1/cdc/events/{event}")).await;
        assert_eq!(
            detail["changed_fields"],
            json!([{"field":"display_name","before":"Before","after":"After","had_before":true,"has_after":true}])
        );
        let (_, rows) = call(&app, "GET", "/api/v1/cdc/events?provider=mail&limit=1").await;
        assert_eq!(rows.as_array().unwrap().len(), 1);
        let (_, rows) = call(&app, "GET", &format!("/api/v1/cdc/events?before={event}")).await;
        assert_eq!(rows, json!([]));
        let object = uuid::Uuid::new_v4();
        let old = uuid::Uuid::new_v4();
        let task:i64=sqlx::query_scalar("INSERT INTO crm.jobs(provider,object_id,version,state,token,lease_until) VALUES('wechat',$1,1,'running',$2,now()+interval '1 minute') RETURNING id").bind(object).bind(old).fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.scheduler_failures(queue,work_key,failures,retry_at,quarantined,error_code) VALUES('object',$1,9,now()+interval '1 hour',true,'test')").bind(task.to_string()).execute(&pool).await.unwrap();
        let (status, body) = call(&app, "GET", "/api/v1/pipeline/status").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["projection"]["count"], 1);
        assert_eq!(body["lanes"][0]["retry_waiting"], 1);
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("/api/v1/pipeline/tasks/{task}/retry")
            )
            .await
            .1,
            json!({"state":"queued"})
        );
        let token: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT token FROM crm.jobs WHERE id=$1")
                .bind(task)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(token.is_none());
        assert_eq!(
            call(&app, "POST", "/api/v1/pipeline/tasks/999999/retry")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
}

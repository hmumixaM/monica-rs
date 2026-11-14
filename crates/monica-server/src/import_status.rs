//! Indexed, paginated operational reads. No source payloads or credentials.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/imports/status", get(status))
        .route("/api/v1/imports/runs", get(runs))
        .route("/api/v1/imports/runs/{id}", get(detail))
        .route("/api/v1/imports/runs/{id}/diagnostics", get(detail))
        .route("/api/v1/imports/daily", get(daily))
}
#[derive(Default, Deserialize)]
struct Filter {
    provider: Option<String>,
    connection_id: Option<Uuid>,
    before: Option<String>,
    before_id: Option<String>,
    limit: Option<i64>,
    days: Option<i32>,
    timezone: Option<String>,
}
async fn status(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let connections:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',c.id,'provider',c.provider,'display_name',c.display_name,'state',c.state,'enabled',c.enabled,'last_success_at',c.last_success_at,'next_run_at',c.next_sync_at,'lease_until',c.lease_until,'error',CASE WHEN c.error IS NOT NULL THEN '来源同步失败，请查看运行记录或重新授权' END,'records',coalesce(p.records,0)) FROM crm.source_connections c LEFT JOIN crm.connection_progress p ON p.connection_id=c.id ORDER BY c.provider,c.created_at,c.id").fetch_all(&s.pool).await?;
    let adapters:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('provider',c.provider,'enabled',c.enabled,'paused',c.paused,'run',(SELECT jsonb_build_object('id',r.id,'status',r.status,'progress',r.progress,'heartbeat_at',r.heartbeat_at,'created_at',r.created_at,'completed_at',r.completed_at,'failed',r.error IS NOT NULL) FROM crm.adapter_runs r WHERE r.provider=c.provider ORDER BY created_at DESC LIMIT 1)) FROM crm.adapter_controls c ORDER BY c.provider").fetch_all(&s.pool).await?;
    let stages:Value=sqlx::query_scalar("SELECT jsonb_build_object('import_pending',(SELECT count(*) FROM crm.import_batches WHERE status<>'complete'),'projection_pending',(SELECT count(*) FROM crm.jobs),'connection_records_pending',(SELECT coalesce(sum(records-processed),0) FROM crm.import_activity_totals),'memory_records_pending',(SELECT count(*) FROM crm.memory_legacy_pending_records)+(SELECT count(*) FROM crm.memory_profile_refresh)+(SELECT count(*) FROM crm.memory_visual_pending),'memory_scopes_pending',(SELECT count(*) FROM crm.memory_legacy_scopes),'media_pending',(SELECT count(*) FROM crm.media_dependencies WHERE state IN ('pending','downloading')),'analysis_pending',(SELECT count(*) FROM crm.ai_jobs WHERE status IN ('queued','running')))").fetch_one(&s.pool).await?;
    let coverage: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('provider',provider,'records',sum(records),'processed',sum(processed),'first_recorded_at',min(first_recorded_at),'last_progress_at',max(last_progress_at)) FROM crm.import_activity_totals GROUP BY provider ORDER BY provider",
    )
    .fetch_all(&s.pool)
    .await?;
    let collection:Vec<Value>=sqlx::query_scalar("SELECT DISTINCT ON(provider) to_jsonb(a) FROM crm.import_activity a WHERE kind='collection' ORDER BY provider,received_at DESC,id DESC").fetch_all(&s.pool).await?;
    Ok(Json(
        json!({"collection":collection,"connections":connections,"adapters":adapters,"stages":stages,"coverage":coverage,"as_of":chrono::Utc::now()}),
    ))
}
async fn runs(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let before = parse_before(q.before.as_deref())?;
    let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT to_jsonb(a) FROM crm.import_activity a WHERE true",
    );
    if let Some(provider) = q.provider {
        query.push(" AND provider=").push_bind(provider);
    }
    if let Some(connection) = q.connection_id {
        query.push(" AND connection_id=").push_bind(connection);
    }
    if let Some(before) = before {
        query
            .push(" AND (received_at,id)<(")
            .push_bind(before)
            .push(",")
            .push_bind(q.before_id.unwrap_or_default())
            .push(")");
    }
    query
        .push(" ORDER BY received_at DESC,id DESC LIMIT ")
        .push_bind(q.limit.unwrap_or(40).clamp(1, 100));
    let rows: Vec<Value> = query.build_query_scalar().fetch_all(&s.pool).await?;
    let next = rows
        .last()
        .map(|r| json!({"before":r["received_at"],"before_id":r["id"]}));
    Ok(Json(json!({"records":rows,"next":next})))
}
fn parse_before(value: Option<&str>) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    value
        .map(|v| {
            v.parse()
                .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "Invalid before timestamp"))
        })
        .transpose()
}
async fn detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let run: Value =
        sqlx::query_scalar("SELECT to_jsonb(a) FROM crm.import_activity a WHERE id=$1")
            .bind(&id)
            .fetch_optional(&s.pool)
            .await?
            .ok_or(ApiError(StatusCode::NOT_FOUND, "Run not found"))?;
    let events:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'state',state,'phase',phase,'created_at',created_at,'details',details) FROM crm.import_run_events WHERE run_id=$1 ORDER BY id DESC LIMIT 200").bind(id).fetch_all(&s.pool).await?;
    Ok(Json(
        json!({"run":run,"events":events,"note":"诊断不包含密钥、Cookie、原始消息或采集器原始日志。详细事件保留 90 天。"}),
    ))
}
async fn daily(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let tz = q.timezone.unwrap_or_else(|| "America/Chicago".into());
    let valid: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)")
            .bind(&tz)
            .fetch_one(&s.pool)
            .await?;
    if !valid {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid IANA timezone",
        ));
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('date',(bucket AT TIME ZONE $1)::date,'provider',provider,'connection_id',nullif(account,'legacy'),'stream',stream,'records',sum(records),'processed',sum(processed),'inserted',sum(inserted),'changed',sum(changed),'unchanged',sum(unchanged),'failures',sum(failures)) FROM crm.import_activity_daily WHERE bucket>=((date_trunc('day',now() AT TIME ZONE $1) - make_interval(days=>$2-1)) AT TIME ZONE $1) AND ($3::text IS NULL OR provider=$3) AND ($4::uuid IS NULL OR account=$4::text) GROUP BY (bucket AT TIME ZONE $1)::date,provider,account,stream ORDER BY (bucket AT TIME ZONE $1)::date DESC,provider,account,stream")
 .bind(&tz).bind(q.days.unwrap_or(7).clamp(1,366)).bind(q.provider).bind(q.connection_id).fetch_all(&s.pool).await?;
    Ok(Json(
        json!({"timezone":tz,"records":rows,"missing_days":"无记录"}),
    ))
}

//! Authenticated reads from current rows and compact day/month page projections.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;
mod cache;
mod cities;
mod directory;
mod engagement;
mod maps;
mod metrics;
mod overview;
type Result<T> = std::result::Result<T, ApiError>;
pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .merge(directory::routes())
        .merge(engagement::routes())
        .merge(maps::routes())
        .merge(overview::routes())
        .merge(metrics::routes())
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            cache::middleware,
        ))
}
#[derive(Clone)]
struct BackgroundReadPool(sqlx::PgPool);
async fn snapshot(s: &AppState, h: &HeaderMap) -> Result<Transaction<'static, Postgres>> {
    snapshot_on(s, h, None).await
}
async fn snapshot_on(
    s: &AppState,
    h: &HeaderMap,
    background: Option<sqlx::PgPool>,
) -> Result<Transaction<'static, Postgres>> {
    user(s, h).await?;
    let mut tx = background.unwrap_or_else(|| s.pool.clone()).begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
async fn canonical(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Uuid> {
    sqlx::query_scalar("WITH RECURSIVE chain(id) AS(SELECT $1::uuid UNION SELECT r.new_profile_id FROM crm.profile_redirects r JOIN chain c ON c.id=r.old_profile_id) SELECT p.id FROM chain c JOIN crm.profiles p ON p.id=c.id WHERE p.archived_at IS NULL AND NOT EXISTS(SELECT 1 FROM crm.profile_redirects r WHERE r.old_profile_id=p.id)").bind(id).fetch_optional(&mut **tx).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"profile not found"))
}
// Conditional caching is applied by the route layer before any page SQL.
async fn cached(_s: &AppState, _h: &HeaderMap, _scope: String, value: Value) -> Result<Response> {
    use axum::response::IntoResponse;
    Ok(axum::Json(value).into_response())
}

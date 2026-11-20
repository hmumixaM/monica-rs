//! Authenticated reads from current rows and compact day/month page projections.
use crate::auth::{ApiError, AppState};
use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;
pub(crate) mod cache;
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
/// One consistent read-only snapshot. The route layer has authenticated the request.
async fn snapshot(s: &AppState) -> Result<Transaction<'static, Postgres>> {
    Ok(s.pool
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?)
}
pub(crate) async fn canonical(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Uuid> {
    sqlx::query_scalar("WITH RECURSIVE chain(id) AS(SELECT $1::uuid UNION SELECT r.new_profile_id FROM crm.profile_redirects r JOIN chain c ON c.id=r.old_profile_id) SELECT p.id FROM chain c JOIN crm.profiles p ON p.id=c.id WHERE p.archived_at IS NULL AND NOT EXISTS(SELECT 1 FROM crm.profile_redirects r WHERE r.old_profile_id=p.id)").bind(id).fetch_optional(&mut **tx).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"profile not found"))
}

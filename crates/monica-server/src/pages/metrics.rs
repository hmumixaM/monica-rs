use super::*;
use axum::{
    Json,
    extract::{Path, Query, State},
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/metrics/summary", get(summary))
        .route("/api/v1/metrics/people-overview", get(people))
        .route("/api/v1/metrics/snapshots", get(snapshots))
        .route("/api/v1/profiles/{id}/metrics", get(profile))
}
async fn summary_value(tx: &mut Transaction<'_, Postgres>) -> Result<Value> {
    Ok(sqlx::query_scalar(r#"SELECT jsonb_build_object(
 'people',(SELECT count(*) FROM crm.directory_profiles WHERE profile_type='person' AND archived_at IS NULL),
 'groups',(SELECT count(*) FROM crm.directory_profiles WHERE profile_type='group' AND archived_at IS NULL),
 'identities',(SELECT count(*) FROM crm.identities WHERE status='active'),
 'wechat_identities',(SELECT count(*) FROM crm.identities WHERE provider='wechat' AND status='active'),
 'instagram_identities',(SELECT count(*) FROM crm.identities WHERE provider='instagram' AND status='active'),
 'linkedin_identities',(SELECT count(*) FROM crm.identities WHERE provider='linkedin' AND status='active'),
 'wechat_moments',(SELECT coalesce(sum(c.value),0) FROM crm.page_entity_counts c JOIN wechat.objects o ON o.id=c.subject WHERE c.metric='wechat.observations' AND o.object_kind='post'),
 'group_memberships',(SELECT count(*) FROM crm.current_group_memberships),
 'media',(SELECT count(*) FROM crm.media_assets),'posts',(SELECT count(*) FROM crm.page_posts WHERE kind='post'),
 'stories',(SELECT count(*) FROM crm.page_posts WHERE kind='story'),'relationships',(SELECT count(*) FROM crm.presented_relationship_edges),
 'activities',(SELECT count(*) FROM crm.activities),'companies',(SELECT count(DISTINCT company_name) FROM crm.profile_structured_current),
 'schools',(SELECT count(DISTINCT school_name) FROM crm.profile_structured_current),
 'address_revisions',(SELECT coalesce(sum(value),0) FROM crm.page_entity_counts WHERE metric='address_revisions'),
 'unchanged_observations',(SELECT coalesce(sum(CASE WHEN metric LIKE '%.observations' THEN value ELSE -value END),0) FROM crm.page_entity_counts WHERE metric LIKE '%.observations' OR metric LIKE '%.source_revisions'),
 'revisions',(SELECT coalesce(sum(value),0) FROM crm.page_entity_counts WHERE metric='revisions'))"#).fetch_one(&mut **tx).await?)
}
async fn summary(State(s): State<AppState>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let value = summary_value(&mut tx).await?;
    tx.commit().await?;
    Ok(Json(value))
}
async fn people(State(s): State<AppState>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let mut result:Value=sqlx::query_scalar("SELECT jsonb_build_object('people',count(*) FILTER(WHERE profile_type='person'),'groups',count(*) FILTER(WHERE profile_type='group')) FROM crm.directory_profiles WHERE archived_at IS NULL").fetch_one(&mut *tx).await?;
    for (key, column) in [("companies", "company_name"), ("schools", "school_name")] {
        let rows:Vec<Value>=sqlx::query_scalar(&format!("SELECT jsonb_build_object('name',f.{column},'count',count(DISTINCT f.profile_id)) FROM crm.profile_structured_current f JOIN crm.directory_profiles p ON p.id=f.profile_id AND p.archived_at IS NULL WHERE f.{column} IS NOT NULL GROUP BY f.{column} ORDER BY count(DISTINCT f.profile_id) DESC,f.{column} LIMIT 8")).fetch_all(&mut *tx).await?;
        result[key] = json!(rows);
    }
    tx.commit().await?;
    Ok(Json(result))
}
#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
}
async fn snapshots(State(s): State<AppState>, Query(q): Query<Limit>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let values:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('metric_key',metric_key,'value',value_json,'source_watermark',source_watermark,'computed_at',computed_at,'calculation_version',calculation_version,'freshness_state',freshness_state) FROM crm.page_metric_snapshots WHERE scope='global' ORDER BY metric_key LIMIT $1").bind(q.limit.unwrap_or(100).clamp(1,200) as i64).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!(values)))
}
async fn profile(State(s): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let id = canonical(&mut tx, id).await?;
    let values:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('metric_key',metric_key,'value',value_json,'source_watermark',source_watermark,'computed_at',computed_at,'calculation_version',calculation_version,'freshness_state',freshness_state) FROM crm.page_metric_snapshots WHERE scope='profile' AND subject=$1 ORDER BY metric_key").bind(id).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!(values)))
}

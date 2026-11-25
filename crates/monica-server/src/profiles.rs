#[path = "activities.rs"]
pub(crate) mod activities;
#[path = "profile_dates.rs"]
pub(crate) mod dates;
#[path = "profile_details.rs"]
pub(crate) mod details;
#[cfg(test)]
#[path = "directory_policy_tests.rs"]
mod directory_policy_tests;
#[cfg(test)]
#[path = "graph_cache_tests.rs"]
mod graph_cache_tests;
#[path = "profile_read.rs"]
mod read;
#[path = "profile_relationships.rs"]
pub(crate) mod relationships;
#[path = "profile_summary.rs"]
pub(crate) mod summary;
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn invalid(message: &'static str) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message)
}
fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "profile not found")
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(details::routes())
        .merge(activities::routes())
        .merge(read::routes())
        .merge(relationships::routes())
        .merge(dates::routes())
        .merge(summary::routes())
        .route("/api/v1/profiles", get(list).post(create))
        .route("/api/v1/profiles/{id}/fields", patch(edit))
        .route("/api/v1/profiles/{id}/history", get(history))
        .route(
            "/api/v1/profiles/{id}/history/{revision}/restore",
            post(restore),
        )
        .route("/api/v1/tags", get(tags).post(create_tag))
        .route(
            "/api/v1/profiles/{id}/tags/{tag}",
            post(attach).delete(detach),
        )
}
fn person() -> String {
    "person".into()
}
#[derive(Deserialize)]
struct Create {
    #[serde(default = "person")]
    profile_type: String,
    display_name: String,
    summary: Option<String>,
}
async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Create>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    if !["person", "group"].contains(&p.profile_type.as_str())
        || p.display_name.trim().is_empty()
        || p.display_name.chars().count() > 255
    {
        return Err(invalid("invalid profile type or display name"));
    }
    let id = Uuid::new_v4();
    let mut tx = s.pool.begin().await?;
    let row:Value=sqlx::query_scalar("INSERT INTO crm.profiles(id,profile_type,display_name,summary) VALUES($1,$2,$3,$4) RETURNING to_jsonb(profiles)-'generation'")
 .bind(id).bind(p.profile_type).bind(p.display_name).bind(p.summary).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) VALUES($1,'profile.created','profile',$2)").bind(actor).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
#[derive(Deserialize)]
struct List {
    q: Option<String>,
    limit: Option<i64>,
    cursor: Option<Uuid>,
    profile_type: Option<String>,
    tag: Option<Uuid>,
    company: Option<String>,
    school: Option<String>,
}
async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<List>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let profile_type = q
        .profile_type
        .filter(|x| ["person", "group"].contains(&x.as_str()));
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(p)-'generation' FROM crm.directory_profiles p WHERE archived_at IS NULL AND ($1::text IS NULL OR display_name ILIKE '%'||$1||'%' OR summary ILIKE '%'||$1||'%') AND ($2::uuid IS NULL OR id>$2) AND ($3::text IS NULL OR profile_type=$3) AND ($4::uuid IS NULL OR EXISTS(SELECT 1 FROM crm.presented_tag_memberships t WHERE t.profile_id=p.id AND tag_id=$4)) AND ($6::text IS NULL OR $6='' OR EXISTS(SELECT 1 FROM crm.profile_structured_current f WHERE f.profile_id=p.id AND f.company_name=$6)) AND ($7::text IS NULL OR $7='' OR EXISTS(SELECT 1 FROM crm.profile_structured_current f WHERE f.profile_id=p.id AND f.school_name=$7)) ORDER BY id LIMIT $5")
 .bind(q.q).bind(q.cursor).bind(profile_type).bind(q.tag).bind(q.limit.unwrap_or(50).clamp(1,100)).bind(q.company).bind(q.school).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn lock(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<()> {
    sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 AND archived_at IS NULL FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(missing)?;
    Ok(())
}
#[derive(Deserialize)]
pub(crate) struct Field {
    pub(crate) field_key: String,
    pub(crate) value: Value,
    pub(crate) source_type: Option<String>,
}
async fn change(
    s: &AppState,
    id: Uuid,
    actor: Uuid,
    field: Field,
    operation: &str,
) -> Result<Value> {
    let mut tx = s.pool.begin().await?;
    let row = change_locked(&mut tx, id, actor, field, operation).await?;
    tx.commit().await?;
    Ok(row)
}
pub(crate) async fn change_locked(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    actor: Uuid,
    mut field: Field,
    operation: &str,
) -> Result<Value> {
    monica_core::identity::validate_json(&field.value)
        .map_err(|_| invalid("nonfinite JSON number"))?;
    field.value = monica_core::identity::json_safe(&field.value);
    if field.field_key.is_empty()
        || field.field_key.chars().count() > 120
        || field.source_type.as_deref().is_some_and(|x| x != "manual")
    {
        return Err(invalid("invalid field"));
    }
    if ["display_name", "summary"].contains(&field.field_key.as_str())
        && (field.value.as_str().is_none()
            || (field.field_key == "display_name"
                && field
                    .value
                    .as_str()
                    .is_some_and(|x| x.trim().is_empty() || x.chars().count() > 255)))
    {
        return Err(invalid(
            "Name and summary must be text, and name cannot be empty",
        ));
    }
    lock(tx, id).await?;
    let current:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(r) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key=$2").bind(id).bind(&field.field_key).fetch_optional(&mut **tx).await?;
    if let Some(ref previous) = current
        && monica_core::identity::canonical_json(&previous["value"])
            == monica_core::identity::canonical_json(&field.value)
        && previous["source_type"] == "manual"
    {
        return Ok(previous.clone());
    }
    let content = monica_core::content::register(tx, "field_value", &field.value)
        .await
        .map_err(|error| {
            tracing::error!(%error,"field content registration failed");
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Field content storage unavailable",
            )
        })?;
    let revision = Uuid::new_v4();
    let previous = current.and_then(|v| v["id"].as_str().and_then(|x| Uuid::parse_str(x).ok()));
    let row:Value=sqlx::query_scalar("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,previous_revision_id,content_object_id) VALUES($1,$2,$3,$4,$5,'manual',$6,$7,$8) RETURNING to_jsonb(field_revisions)")
 .bind(revision).bind(id).bind(&field.field_key).bind(&field.value).bind(content.hash).bind(operation).bind(previous).bind(content.id).fetch_one(&mut **tx).await?;
    sqlx::query("INSERT INTO crm.field_current VALUES($1,$2,$3) ON CONFLICT(profile_id,field_key) DO UPDATE SET revision_id=excluded.revision_id").bind(id).bind(&field.field_key).bind(revision).execute(&mut **tx).await?;
    sqlx::query("UPDATE crm.profiles SET generation=generation+1,display_name=CASE WHEN $2='display_name' THEN $3 ELSE display_name END,summary=CASE WHEN $2='summary' THEN $3 ELSE summary END WHERE id=$1").bind(id).bind(&field.field_key).bind(field.value.as_str()).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'profile.field.edited','profile',$2,$3)").bind(actor).bind(id).bind(json!({"field_key":field.field_key})).execute(&mut **tx).await?;
    Ok(row)
}
async fn edit(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(field): Json<Field>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    Ok(Json(revision_out(
        &change(&s, id, actor, field, "manual_edit").await?,
        false,
    )))
}
fn revision_out(row: &Value, with_value: bool) -> Value {
    let mut result = json!({});
    for key in [
        "id",
        "profile_id",
        "field_key",
        "content_hash",
        "source_type",
        "source_account_id",
        "source_record_id",
        "operation",
        "observed_at",
        "is_conflict",
    ] {
        result[key] = row[key].clone();
    }
    // PostgreSQL sequences can exceed JavaScript's exact integer range.
    result["cursor"] = row["revision_seq"]
        .as_i64()
        .map(|sequence| json!(sequence.to_string()))
        .unwrap_or(Value::Null);
    result["value"] = if with_value {
        row["value"].clone()
    } else {
        Value::Null
    };
    result
}
#[derive(Deserialize)]
struct History {
    field_key: Option<String>,
    limit: Option<i64>,
    before: Option<i64>,
}
async fn history(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<History>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    // Bound each disjoint owner stream before merging. An OR/EXISTS over the
    // global sequence can scan other people's entire imported histories.
    let rows:Vec<Value>=sqlx::query_scalar("WITH candidates AS ((SELECT r.* FROM crm.profile_aliases($1) owner JOIN LATERAL (SELECT r.* FROM crm.field_revisions r WHERE r.source_account_id IS NULL AND r.profile_id=owner AND ($2::text IS NULL OR field_key=$2) AND ($3::bigint IS NULL OR revision_seq<$3) ORDER BY revision_seq DESC LIMIT $4) r ON true) UNION ALL SELECT r.* FROM crm.source_account_links l JOIN LATERAL (SELECT r.* FROM crm.field_revisions r WHERE r.source_account_id=l.account_id AND ($2::text IS NULL OR field_key=$2) AND ($3::bigint IS NULL OR revision_seq<$3) ORDER BY revision_seq DESC LIMIT $4) r ON true WHERE l.profile_id=$1) SELECT to_jsonb(r) FROM candidates r ORDER BY revision_seq DESC LIMIT $4").bind(id).bind(q.field_key).bind(q.before).bind(q.limit.unwrap_or(100).clamp(1,200)).fetch_all(&s.pool).await?;
    Ok(Json(json!(
        rows.iter()
            .map(|row| revision_out(row, true))
            .collect::<Vec<_>>()
    )))
}
async fn restore(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, revision)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let (field_key,value):(String,Value)=sqlx::query_as("SELECT field_key,value FROM crm.field_revisions r WHERE id=$1 AND ((source_account_id IS NULL AND profile_id IN (SELECT crm.profile_aliases($2))) OR EXISTS(SELECT 1 FROM crm.source_account_links l WHERE l.account_id=r.source_account_id AND l.profile_id=$2)) AND value<>'null'::jsonb").bind(revision).bind(id).fetch_optional(&s.pool).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"restorable revision not found"))?;
    let row = change(
        &s,
        id,
        actor,
        Field {
            field_key,
            value,
            source_type: None,
        },
        "restore",
    )
    .await?;
    Ok(Json(revision_out(&row, false)))
}
#[derive(Deserialize)]
struct Name {
    name: String,
}
async fn create_tag(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(n): Json<Name>,
) -> Result<(StatusCode, Json<Value>)> {
    user(&s, &h).await?;
    if n.name.trim().is_empty() || n.name.chars().count() > 120 {
        return Err(invalid("invalid tag name"));
    }
    sqlx::query("INSERT INTO crm.tags VALUES($1,$2) ON CONFLICT(name) DO NOTHING")
        .bind(Uuid::new_v4())
        .bind(&n.name)
        .execute(&s.pool)
        .await?;
    let row: Value = sqlx::query_scalar("SELECT to_jsonb(t) FROM crm.tags t WHERE name=$1")
        .bind(n.name)
        .fetch_one(&s.pool)
        .await?;
    Ok((StatusCode::CREATED, Json(row)))
}
#[derive(Deserialize)]
struct Tags {
    profile_type: Option<String>,
}
async fn tags(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Tags>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if q.profile_type
        .as_ref()
        .is_some_and(|x| !["person", "group"].contains(&x.as_str()))
    {
        return Err(invalid("Unknown profile type"));
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',t.id,'name',t.name,'people',count(DISTINCT p.id),'sources',coalesce(jsonb_agg(DISTINCT m.source_type) FILTER(WHERE p.id IS NOT NULL),'[]'::jsonb)) FROM crm.tags t LEFT JOIN crm.presented_tag_memberships m ON m.tag_id=t.id LEFT JOIN crm.profiles p ON p.id=m.profile_id AND p.archived_at IS NULL AND ($1::text IS NULL OR p.profile_type=$1) GROUP BY t.id,t.name ORDER BY t.name,t.id").bind(q.profile_type).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn membership(s: &AppState, actor: Uuid, id: Uuid, tag: Uuid, attach: bool) -> Result<()> {
    let mut tx = s.pool.begin().await?;
    membership_locked(&mut tx, actor, id, tag, attach).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn membership_locked(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    id: Uuid,
    tag: Uuid,
    attach: bool,
) -> Result<()> {
    lock(tx, id).await?;
    if sqlx::query("SELECT id FROM crm.tags WHERE id=$1")
        .bind(tag)
        .fetch_optional(&mut **tx)
        .await?
        .is_none()
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "tag not found"));
    }
    if attach {
        sqlx::query("DELETE FROM crm.tag_membership_removals WHERE tag_id=$1 AND profile_id=$2")
            .bind(tag)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "INSERT INTO crm.tag_memberships VALUES($1,$2,'manual') ON CONFLICT DO NOTHING",
        )
        .bind(tag)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    } else {
        let present:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.presented_tag_memberships WHERE tag_id=$1 AND profile_id=$2)").bind(tag).bind(id).fetch_one(&mut **tx).await?;
        let n = sqlx::query("DELETE FROM crm.tag_memberships WHERE tag_id=$1 AND profile_id=$2")
            .bind(tag)
            .bind(id)
            .execute(&mut **tx)
            .await?
            .rows_affected();
        if n == 0 && !present {
            return Err(ApiError(StatusCode::NOT_FOUND, "tag not on profile"));
        }
        sqlx::query("INSERT INTO crm.tag_membership_removals(tag_id,profile_id,removed_by) VALUES($1,$2,$3) ON CONFLICT(tag_id,profile_id) DO UPDATE SET removed_by=excluded.removed_by,removed_at=now()").bind(tag).bind(id).bind(actor).execute(&mut **tx).await?;
    }
    sqlx::query("INSERT INTO crm.tag_membership_revisions(tag_id,profile_id,operation,actor_user_id) VALUES($1,$2,$3,$4)").bind(tag).bind(id).bind(if attach{"added"}else{"removed"}).bind(actor).execute(&mut **tx).await?;
    sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
async fn attach(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, tag)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    membership(&s, actor, id, tag, true).await?;
    Ok(Json(json!({"status":"attached"})))
}
async fn detach(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, tag)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let (actor, _) = user(&s, &h).await?;
    membership(&s, actor, id, tag, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    pub(super) async fn fixture(pool: &PgPool) -> (AppState, Uuid, Uuid, tempfile::TempDir) {
        let actor = Uuid::new_v4();
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic@example.invalid','unused-test-hash')").bind(actor).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic')").bind(id).execute(pool).await.unwrap();
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(
            pool.clone(),
            "synthetic-unit-test-secret-000000000",
            "http://localhost:17494".into(),
            root.path().into(),
        )
        .await
        .unwrap();
        (state, actor, id, root)
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn profile_history_pages_and_restore_keep_source_versions(pool: PgPool) {
        use axum::{
            body::{Body, to_bytes},
            http::Request,
        };
        use sha2::{Digest, Sha256};
        use tower::ServiceExt;
        async fn call(router: &Router, method: &str, path: &str, expected: StatusCode) -> Value {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("host", "localhost:17494")
                        .header(
                            "cookie",
                            "crm_session=history-session; crm_csrf=history-csrf",
                        )
                        .header("x-csrf-token", "history-csrf")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
            assert_eq!(
                status,
                expected,
                "{path}: {}",
                String::from_utf8_lossy(&body)
            );
            serde_json::from_slice(&body).unwrap()
        }
        let (s, actor, id, _root) = fixture(&pool).await;
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')")
            .bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}", Sha256::digest(b"history-session"))).execute(&pool).await.unwrap();
        sqlx::query("SELECT setval(pg_get_serial_sequence('crm.field_revisions','revision_seq'),9007199254740992,true)").execute(&pool).await.unwrap();
        let mut first = Value::Null;
        for name in ["Earlier name", "Current name"] {
            let row = change(
                &s,
                id,
                actor,
                Field {
                    field_key: "display_name".into(),
                    value: json!(name),
                    source_type: None,
                },
                "manual_edit",
            )
            .await
            .unwrap();
            if first.is_null() {
                first = row;
            }
        }
        sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation) SELECT gen_random_uuid(),$1,'biography',to_jsonb(n),'fixture','manual','manual_edit' FROM generate_series(1,125) n").bind(id).execute(&pool).await.unwrap();
        let account = Uuid::new_v4();
        let other = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Other person')").bind(other).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'linkedin','history','person','History source')").bind(account).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
            .bind(account)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        let value = json!({"title":"Historical role"});
        let content = monica_core::content::register(&mut tx, "structured_fact", &value)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("INSERT INTO crm.source_structured_revisions(id,source_account_id,fact_type,fact_key,content_object_id,content_hash,value,source_type,source_record_id,object_id,source_version,observed_at) SELECT gen_random_uuid(),$1,CASE WHEN n=206 THEN 'education' ELSE 'employment' END,'job-'||n,$2,$3,$4,'linkedin','history',gen_random_uuid(),1,'2026-01-01T12:00:00.123456Z' FROM generate_series(1,206)n").bind(account).bind(content.id).bind(&content.hash).bind(&value).execute(&pool).await.unwrap();
        // Notebook ownership has precedence over an account link. Exercise a
        // page boundary across streams, plus a record belonging to another person.
        sqlx::query("INSERT INTO crm.source_structured_revisions(id,source_account_id,notebook_profile_id,fact_type,fact_key,content_object_id,content_hash,value,source_type,source_record_id,object_id,source_version,observed_at) SELECT gen_random_uuid(),CASE WHEN n=42 THEN $1::uuid END,CASE WHEN n=42 THEN $2::uuid ELSE $3::uuid END,'address','note-'||n,$4,$5,$6,'monica','notebook',gen_random_uuid(),1,'2026-01-01T12:00:00.123456Z' FROM generate_series(1,42)n").bind(account).bind(other).bind(id).bind(content.id).bind(&content.hash).bind(&value).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,source_account_id,field_key,value,content_hash,source_type,operation) SELECT gen_random_uuid(),$1,$2,'linkedin.headline',to_jsonb(n),'fixture','linkedin','source_import' FROM generate_series(1,2)n").bind(other).bind(account).execute(&pool).await.unwrap();
        let router = crate::auth::routes(s);
        let mut cursor = String::new();
        let mut fields = Vec::new();
        loop {
            let page = call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{id}/history?limit=40{cursor}"),
                StatusCode::OK,
            )
            .await;
            let rows = page.as_array().unwrap();
            if rows.is_empty() {
                break;
            }
            let token = rows.last().unwrap()["cursor"].as_str().unwrap();
            assert!(token.parse::<i64>().unwrap() > 9_007_199_254_740_992);
            cursor = format!("&before={token}");
            fields.extend(rows.iter().map(|r| r["id"].as_str().unwrap().to_owned()));
        }
        assert_eq!(fields.len(), 129);
        assert_eq!(
            fields
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            129
        );
        let mut cursor = String::new();
        let mut facts = Vec::new();
        loop {
            let page = call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{id}/structured-history?limit=40{cursor}"),
                StatusCode::OK,
            )
            .await;
            let rows = page.as_array().unwrap();
            let Some(last) = rows.last() else { break };
            cursor = format!(
                "&before={}&before_id={}",
                last["observed_at"].as_str().unwrap().replace('+', "%2B"),
                last["id"].as_str().unwrap()
            );
            facts.extend(rows.iter().map(|r| r["id"].as_str().unwrap().to_owned()));
        }
        assert_eq!(facts.len(), 247);
        assert!(facts.windows(2).all(|pair| pair[0] > pair[1]));
        assert_eq!(
            call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{id}/structured-history?fact_type=education"),
                StatusCode::OK
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            1
        );
        call(
            &router,
            "GET",
            &format!("/api/v1/profiles/{id}/structured-history?before_id={account}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(
            call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{other}/structured-history"),
                StatusCode::OK
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            1
        );
        call(
            &router,
            "POST",
            &format!(
                "/api/v1/profiles/{other}/history/{}/restore",
                first["id"].as_str().unwrap()
            ),
            StatusCode::NOT_FOUND,
        )
        .await;
        let restored = call(
            &router,
            "POST",
            &format!(
                "/api/v1/profiles/{id}/history/{}/restore",
                first["id"].as_str().unwrap()
            ),
            StatusCode::OK,
        )
        .await;
        assert_eq!(restored["operation"], "restore");
        assert_eq!(
            call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{id}"),
                StatusCode::OK
            )
            .await["display_name"],
            "Earlier name"
        );
        let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM crm.field_revisions),(SELECT count(*) FROM crm.source_structured_revisions),(SELECT count(*) FROM crm.audit_events WHERE action='profile.field.edited')").fetch_one(&pool).await.unwrap();
        assert_eq!(counts, (130, 248, 3));
        // Source history follows the current owner after an explicit relink.
        sqlx::query("UPDATE crm.source_account_links SET profile_id=$2 WHERE account_id=$1")
            .bind(account)
            .bind(other)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{id}/structured-history"),
                StatusCode::OK
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            41
        );
        assert_eq!(
            call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{other}/structured-history?limit=200"),
                StatusCode::OK
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            200
        );
        assert!(
            call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{id}/history?field_key=linkedin.headline"),
                StatusCode::OK
            )
            .await
            .as_array()
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            call(
                &router,
                "GET",
                &format!("/api/v1/profiles/{other}/history?field_key=linkedin.headline"),
                StatusCode::OK
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            2
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn concurrent_noop_field_writes_have_one_revision(pool: PgPool) {
        let (s, actor, id, _root) = fixture(&pool).await;
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..12 {
            let s = s.clone();
            tasks.spawn(async move {
                change(
                    &s,
                    id,
                    actor,
                    Field {
                        field_key: "synthetic.fact".into(),
                        value: json!({"value":1e-7}),
                        source_type: None,
                    },
                    "manual_edit",
                )
                .await
                .unwrap()["id"]
                    .clone()
            });
        }
        let mut ids = std::collections::BTreeSet::new();
        while let Some(id) = tasks.join_next().await {
            ids.insert(id.unwrap().as_str().unwrap().to_owned());
        }
        assert_eq!(ids.len(), 1);
        let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM crm.field_revisions),(SELECT count(*) FROM crm.audit_events WHERE action='profile.field.edited')").fetch_one(&pool).await.unwrap();
        assert_eq!(counts, (1, 1));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn manual_fields_survive_source_refresh_and_relink(pool: PgPool) {
        let (s, actor, id, _root) = fixture(&pool).await;
        let other = Uuid::new_v4();
        let account = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Other synthetic')").bind(other).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat','synthetic','person','Synthetic source')").bind(account).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_fields(account_id,field_key,value,content_hash,object_id,source_version) VALUES($1,'wechat.nickname','\"Source\"','source-hash',$2,1)").bind(account).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        monica_core::linking::set_link(
            &pool,
            monica_core::linking::LinkChange {
                account_id: account,
                profile_id: Some(id),
                expected_profile_id: None,
            },
            Some(actor),
        )
        .await
        .unwrap();
        change(
            &s,
            id,
            actor,
            Field {
                field_key: "wechat.nickname".into(),
                value: json!("Manual"),
                source_type: None,
            },
            "manual_edit",
        )
        .await
        .unwrap();
        sqlx::query("UPDATE crm.source_fields SET value='\"New source\"',content_hash='new-source-hash',source_version=2 WHERE account_id=$1").bind(account).execute(&pool).await.unwrap();
        monica_core::linking::set_link(
            &pool,
            monica_core::linking::LinkChange {
                account_id: account,
                profile_id: Some(other),
                expected_profile_id: Some(id),
            },
            Some(actor),
        )
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        let old = read::load(&mut tx, id).await.unwrap();
        let new = read::load(&mut tx, other).await.unwrap();
        assert_eq!(old["fields"][0]["value"], "Manual");
        assert_eq!(new["fields"][0]["value"], "New source");
        assert_eq!(new["fields"][0]["current_source_type"], "wechat");
        tx.commit().await.unwrap();
        let source: Value =
            sqlx::query_scalar("SELECT value FROM crm.source_fields WHERE account_id=$1")
                .bind(account)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(source, "New source");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn contact_summary_deltas_correct_relink_rollback_and_delete(pool: PgPool) {
        let (_s, _actor, id, _root) = fixture(&pool).await;
        let other = Uuid::new_v4();
        let call = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Other')",
        )
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO crm.contact_events(id,provider,profile_id,kind,external_id,content) VALUES($1,'wechat',$2,'call','call', $3),($4,'wechat',$2,'interaction','reaction','{}')")
            .bind(call).bind(id).bind(json!({"media_type":"voice","status":"completed","direction":"in","duration_seconds":10,"media_type_confidence":"high"})).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let first = summary::contact_events(&mut tx, id).await.unwrap();
        assert_eq!(first["calls"]["total"], 1);
        assert_eq!(first["calls"]["duration_seconds"], 10);
        assert!(first.get("money").is_none());
        sqlx::query("UPDATE crm.contact_events SET content=jsonb_set(content,'{duration_seconds}','20') WHERE id=$1").bind(call).execute(&mut *tx).await.unwrap();
        assert_eq!(
            summary::contact_events(&mut tx, id).await.unwrap()["calls"]["duration_seconds"],
            20
        );
        tx.rollback().await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            summary::contact_events(&mut tx, id).await.unwrap()["calls"]["duration_seconds"],
            10
        );
        sqlx::query("UPDATE crm.contact_events SET profile_id=$2 WHERE id=$1")
            .bind(call)
            .bind(other)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(
            summary::contact_events(&mut tx, id)
                .await
                .unwrap()
                .get("calls")
                .is_none()
        );
        assert_eq!(
            summary::contact_events(&mut tx, other).await.unwrap()["calls"]["total"],
            1
        );
        sqlx::query(
            "UPDATE crm.contact_events SET content=content||'{\"is_deleted\":true}' WHERE id=$1",
        )
        .bind(call)
        .execute(&mut *tx)
        .await
        .unwrap();
        assert!(
            summary::contact_events(&mut tx, other)
                .await
                .unwrap()
                .get("calls")
                .is_none()
        );
        sqlx::query("DELETE FROM crm.contact_events WHERE id=$1")
            .bind(call)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(
            summary::contact_events(&mut tx, other)
                .await
                .unwrap()
                .get("calls")
                .is_none()
        );
        tx.commit().await.unwrap();
    }
}

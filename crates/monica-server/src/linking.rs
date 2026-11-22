#[path = "link_graph.rs"]
mod graph;
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
};
use monica_core::linking::{self, LinkChange};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn failure(e: anyhow::Error) -> ApiError {
    tracing::warn!(error=%e,"link change rejected");
    let detail = e.to_string();
    for (key, message) in [
        (
            "quarantined",
            "This legacy identity is quarantined and cannot be linked",
        ),
        (
            "unowned_media",
            "The current profile has an avatar or cover without a source account",
        ),
        (
            "unowned_tags",
            "The current profile has platform tags without a source account",
        ),
        (
            "unowned_relationships",
            "The current profile has platform relationships without a source account",
        ),
        (
            "account link changed",
            "The account link changed; refresh before confirming",
        ),
    ] {
        if detail.contains(key) {
            return ApiError(StatusCode::CONFLICT, message);
        }
    }
    if detail.contains("source account not found") {
        return ApiError(StatusCode::NOT_FOUND, "Source account not found");
    }
    if detail.contains("profile not found") || detail.contains("profile is archived") {
        return ApiError(StatusCode::NOT_FOUND, "Profile not found");
    }
    if detail.contains("people link to person accounts") {
        return ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "People link to person accounts; groups link to group accounts",
        );
    }
    ApiError(
        StatusCode::CONFLICT,
        "Account ownership changed or the requested link is invalid",
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(graph::routes())
        .route("/api/v1/profiles/{id}/merge-preview", get(merge_preview))
        .route("/api/v1/profiles/{id}/merge", post(merge_person))
        .route("/api/v1/profiles/{id}/avatar-history", get(avatar_history))
        .route("/api/v1/source-accounts", get(accounts))
        .route("/api/v1/source-accounts/{id}/link", put(link))
        .route("/api/v1/link-review", get(review))
        .route("/api/v1/archived-profiles", get(archived))
        .route("/api/v1/profiles/{id}/restore", post(restore))
        .route("/api/v1/link-review/linked", get(linked))
        .route("/api/v1/link-review/contacts", get(contacts))
        .route("/api/v1/link-review/confirm", post(confirm))
        .route("/api/v1/link-review/reject", post(reject))
        .route("/api/v1/link-review/skip", post(skip))
        .route("/api/v1/link-review/batch", post(batch))
        .route("/api/v1/identities", post(identity))
        .route("/api/v1/profiles/{id}/identities", post(profile_identity))
        .route(
            "/api/v1/identity-candidates",
            get(candidates).post(candidate),
        )
        .route(
            "/api/v1/identity-candidates/{id}/accept",
            post(accept_candidate),
        )
        .route(
            "/api/v1/identity-candidates/{id}/reject",
            post(reject_candidate),
        )
}
#[derive(Default, Deserialize)]
struct Filter {
    q: Option<String>,
    provider: Option<String>,
    profile_id: Option<Uuid>,
    cursor: Option<Uuid>,
    status_filter: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
    pair: Option<String>,
    sort: Option<String>,
    providers: Option<String>,
}
async fn accounts(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(a)||jsonb_build_object('profile_id',l.profile_id,'linked_profile_name',p.display_name,'ready',true,'blocked_reason',NULL) FROM crm.source_accounts a LEFT JOIN crm.source_account_links l ON l.account_id=a.id LEFT JOIN crm.profiles p ON p.id=l.profile_id WHERE ($1::text IS NULL OR a.provider=$1) AND ($2::uuid IS NULL OR l.profile_id=$2) AND ($3::uuid IS NULL OR a.id>$3) AND ($4::text IS NULL OR a.display_name ILIKE '%'||$4||'%' OR a.external_id ILIKE '%'||$4||'%' OR a.username ILIKE '%'||$4||'%') ORDER BY a.id LIMIT 50").bind(q.provider).bind(q.profile_id).bind(q.cursor).bind(q.q).fetch_all(&s.pool).await?;
    let mut rows = rows;
    let ids = rows
        .iter()
        .filter_map(|r| r["id"].as_str()?.parse().ok())
        .collect::<Vec<_>>();
    let mut tx = s.pool.begin().await?;
    let blockers = linking::link_blockers(&mut tx, &ids)
        .await
        .map_err(failure)?;
    tx.commit().await?;
    rows.retain(|r| {
        r["id"]
            .as_str()
            .and_then(|id| id.parse::<Uuid>().ok())
            .and_then(|id| blockers.get(&id))
            .is_none_or(|reason| reason != "quarantined")
    });
    for row in &mut rows {
        let reason = row["id"]
            .as_str()
            .and_then(|id| id.parse::<Uuid>().ok())
            .and_then(|id| blockers.get(&id));
        row["ready"] = json!(reason.is_none());
        row["blocked_reason"] = json!(reason);
        row.as_object_mut().unwrap().remove("last_seen_at");
    }
    Ok(Json(json!(rows)))
}
#[derive(Deserialize)]
struct Link {
    profile_id: Option<Uuid>,
    expected_profile_id: Option<Uuid>,
}
async fn link(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<Link>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    Ok(Json(
        linking::set_link(
            &s.pool,
            LinkChange {
                account_id: id,
                profile_id: p.profile_id,
                expected_profile_id: p.expected_profile_id,
            },
            Some(actor),
        )
        .await
        .map_err(failure)?,
    ))
}
#[derive(Clone, Deserialize)]
struct Decision {
    account_id: Uuid,
    profile_id: Uuid,
    #[serde(default)]
    expected_profile_id: Option<Uuid>,
    #[serde(default)]
    score: Option<i64>,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    action: Option<String>,
}
async fn decide(s: &AppState, actor: Uuid, p: Decision, action: &str) -> Result<Value> {
    let mut tx = s.pool.begin().await?;
    let result = if action == "confirmed" {
        linking::set_links(
            &mut tx,
            &[LinkChange {
                account_id: p.account_id,
                profile_id: Some(p.profile_id),
                expected_profile_id: p.expected_profile_id,
            }],
            Some(actor),
            &json!({"kind":"manual-confirmation","score":p.score,"reasons":p.reasons}),
        )
        .await
        .map_err(failure)?
        .remove(0)
    } else {
        json!({"status":action})
    };
    sqlx::query("INSERT INTO crm.platform_link_decisions(account_id,profile_id,status,evidence,actor_user_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT(account_id,profile_id) DO UPDATE SET status=excluded.status,evidence=excluded.evidence,actor_user_id=excluded.actor_user_id,updated_at=now()")
 .bind(p.account_id).bind(p.profile_id).bind(action).bind(json!({"score":p.score,"reasons":p.reasons})).bind(actor).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(result)
}
async fn confirm(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Decision>,
) -> Result<Json<Value>> {
    let (a, _) = user(&s, &h).await?;
    Ok(Json(decide(&s, a, p, "confirmed").await?))
}
async fn reject(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Decision>,
) -> Result<Json<Value>> {
    let (a, _) = user(&s, &h).await?;
    Ok(Json(decide(&s, a, p, "rejected").await?))
}
async fn skip(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Decision>,
) -> Result<Json<Value>> {
    let (a, _) = user(&s, &h).await?;
    Ok(Json(decide(&s, a, p, "skipped").await?))
}
#[derive(Deserialize)]
struct Batch {
    items: Vec<Decision>,
}
async fn batch(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Batch>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    if p.items.is_empty() || p.items.len() > 25 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "batch requires 1 to 25 decisions",
        ));
    }
    let mut output = vec![];
    for item in p.items {
        let id = item.account_id;
        let action = match item.action.as_deref() {
            Some("confirm") => "confirmed",
            Some("reject") => "rejected",
            Some("skip") => "skipped",
            _ => {
                return Err(ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid decision action",
                ));
            }
        };
        match decide(&s, actor, item, action).await {
            Ok(_) => output.push(json!({"account_id":id,"ok":true,"error":null})),
            Err(e) => output.push(json!({"account_id":id,"ok":false,"error":e.1})),
        }
    }
    Ok(Json(json!({"results":output})))
}
async fn linked(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let data = monica_core::link_matching::load(&s.pool)
        .await
        .map_err(failure)?;
    let required = q
        .providers
        .unwrap_or_default()
        .split(',')
        .filter(|p| monica_core::link_matching::PROVIDERS.contains(p))
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let needle = monica_core::link_matching::compact(q.q.as_deref().unwrap_or(""));
    let mut people = vec![];
    for (profile, present) in &data.providers {
        if present.len() < 2 || !required.is_subset(present) {
            continue;
        }
        let accounts = data
            .accounts
            .iter()
            .filter(|a| a.profile_id.as_ref() == Some(profile))
            .collect::<Vec<_>>();
        let first = accounts[0];
        let signals = data.signals.get(profile).cloned().unwrap_or_default();
        let mut haystack = vec![first.profile_name.clone()];
        for a in &accounts {
            haystack.extend([a.display_name.clone(), a.username.clone()]);
        }
        for signal in &signals {
            for v in signal["values"].as_array().into_iter().flatten() {
                haystack.push(v.as_str().unwrap_or("").into());
            }
        }
        if !needle.is_empty()
            && !monica_core::link_matching::compact(&haystack.join(" ")).contains(&needle)
        {
            continue;
        }
        let accounts=accounts.iter().map(|a|json!({"account_id":a.account_id,"provider":a.provider,"display_name":a.display_name,"username":if a.username.is_empty(){&a.external_id}else{&a.username},"profile_url":a.profile_url,"avatar_media_id":a.avatar_media_id})).collect::<Vec<_>>();
        people.push(json!({"profile_id":profile,"display_name":first.profile_name,"avatar_media_id":first.avatar_media_id,"providers":present,"accounts":accounts,"signals":signals}));
    }
    people.sort_by(|a, b| a["display_name"].as_str().cmp(&b["display_name"].as_str()));
    Ok(Json(json!(people)))
}
async fn contacts(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let query = q.q.unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(Json(json!([])));
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'display_name',p.display_name,'avatar_media_id',p.avatar_media_id,'accounts',coalesce((SELECT jsonb_agg(to_jsonb(a)) FROM crm.source_account_links l JOIN crm.source_accounts a ON a.id=l.account_id WHERE l.profile_id=p.id),'[]')) FROM crm.directory_profiles p WHERE p.archived_at IS NULL AND p.profile_type='person' AND p.display_name ILIKE '%'||$1||'%' AND ($2::text IS NULL OR NOT EXISTS(SELECT 1 FROM crm.source_account_links l JOIN crm.source_accounts a ON a.id=l.account_id WHERE l.profile_id=p.id AND a.provider=$2)) ORDER BY p.display_name LIMIT 30").bind(query).bind(q.provider).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn review(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let queue = matching_queue(&s).await?;
    let needle = monica_core::link_matching::compact(q.q.as_deref().unwrap_or(""));
    let status = q.status_filter.as_deref().unwrap_or("unresolved");
    let unresolved = queue.iter().filter(|r| r["status"] == "unresolved").count();
    let mut rows = queue
        .iter()
        .filter(|r| {
            if q.pair
                .as_deref()
                .is_some_and(|p| !r["pair"].as_str().unwrap_or("").starts_with(p))
            {
                return false;
            }
            let matched = match status {
                "recommended" => {
                    r["status"] == "unresolved" && r["score"].as_i64().unwrap_or(0) >= 70
                }
                "unresolved" | "matched" | "no_match" | "skipped" => r["status"] == status,
                _ => true,
            };
            if !matched {
                return false;
            }
            let mut haystack = vec![
                r["display_name"].as_str().unwrap_or(""),
                r["username"].as_str().unwrap_or(""),
                r["profile_name"].as_str().unwrap_or(""),
            ];
            if let Some(c) = r["candidates"].as_array() {
                haystack.extend(c.iter().filter_map(|v| v["display_name"].as_str()));
            }
            if let Some(signals) = r["signals"].as_array() {
                for signal in signals {
                    if let Some(values) = signal["values"].as_array() {
                        haystack.extend(values.iter().filter_map(Value::as_str));
                    }
                }
            }
            needle.is_empty()
                || monica_core::link_matching::compact(&haystack.join(" ")).contains(&needle)
        })
        .cloned()
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        if q.sort.as_deref().unwrap_or("confidence") == "confidence" {
            b["score"]
                .as_i64()
                .cmp(&a["score"].as_i64())
                .then_with(|| a["username"].as_str().cmp(&b["username"].as_str()))
        } else {
            a["username"]
                .as_str()
                .cmp(&b["username"].as_str())
                .then_with(|| b["score"].as_i64().cmp(&a["score"].as_i64()))
        }
    });
    let total = rows.len();
    let page = q.page.unwrap_or(1).max(1);
    let size = q.page_size.unwrap_or(50).clamp(1, 200);
    let mut rows = rows
        .into_iter()
        .skip(((page - 1) * size) as usize)
        .take(size as usize)
        .collect::<Vec<_>>();
    let media = rows
        .iter()
        .filter_map(|r| {
            r["avatar_media_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
        })
        .collect::<Vec<_>>();
    let matches = monica_core::faces::avatar_matches(&s.pool, &media)
        .await
        .map_err(failure)?;
    for row in &mut rows {
        let media = row["avatar_media_id"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok());
        if let Some(candidates) = row["candidates"].as_array_mut() {
            for candidate in candidates {
                let profile = candidate["profile_id"]
                    .as_str()
                    .and_then(|s| s.parse::<Uuid>().ok());
                candidate["face_score"] =
                    json!(media.zip(profile).and_then(|key| matches.get(&key)));
            }
        }
    }
    Ok(Json(
        json!({"pairs":monica_core::link_matching::PAIRS.map(|(id,label)|json!({"id":id,"label":label})),"total":total,"unresolved":unresolved,"page":page,"page_size":size,"queue":rows}),
    ))
}

// One bounded matching resource cache. Version reads only visible committed rows;
// cancelled callers cannot release the permit while the CPU closure still runs.
async fn matching_queue(s: &AppState) -> Result<std::sync::Arc<Vec<Value>>> {
    use std::sync::{Arc, OnceLock};
    type Entry = Option<(String, Arc<Vec<Value>>)>;
    static CACHE: OnceLock<tokio::sync::Mutex<Entry>> = OnceLock::new();
    static BUILD: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let pool = &s.pool;
    let version:String=sqlx::query_scalar("SELECT md5(coalesce(string_agg(v,',' ORDER BY v),'')) FROM (SELECT 'v'||domain||shard::text||version::text AS v FROM crm.cache_domain_versions WHERE domain IN('profile','identity','directory') UNION ALL SELECT 'a'||id::text||xmin::text FROM crm.source_accounts UNION ALL SELECT 'l'||account_id::text||xmin::text FROM crm.source_account_links UNION ALL SELECT 'p'||id::text||xmin::text FROM crm.profiles UNION ALL SELECT 'f'||profile_id::text||field_key||revision_id::text FROM crm.field_current UNION ALL SELECT 'd'||account_id::text||profile_id::text||xmin::text FROM crm.platform_link_decisions UNION ALL SELECT 'm'||entity_id::text||role||generation::text FROM crm.media_slots WHERE entity_type='source_account') versions").fetch_one(pool).await?;
    let cache = CACHE.get_or_init(Default::default);
    if let Some((v, rows)) = &*cache.lock().await
        && *v == version
    {
        return Ok(rows.clone());
    }
    let permit = BUILD
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "matching worker stopped"))?;
    if let Some((v, rows)) = &*cache.lock().await
        && *v == version
    {
        return Ok(rows.clone());
    }
    let snapshot = monica_core::link_matching::load(pool)
        .await
        .map_err(failure)?;
    let estimate = snapshot
        .accounts
        .iter()
        .map(|a| a.identity_values().iter().map(|v| v.len()).sum::<usize>() + 2048)
        .sum::<usize>()
        .saturating_mul(6)
        .max(16 * 1024 * 1024);
    let rows = s
        .resources
        .run_blocking(estimate, move || {
            let _permit = permit;
            Ok(monica_core::link_matching::queue(&snapshot))
        })
        .await
        .map_err(failure)?;
    let rows = Arc::new(rows);
    if serde_json::to_vec(&*rows)
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "matching serialization failed",
            )
        })?
        .len()
        <= 32 * 1024 * 1024
    {
        *cache.lock().await = Some((version, rows.clone()));
    }
    Ok(rows)
}
#[derive(Deserialize)]
struct Identity {
    profile_id: Option<Uuid>,
    provider: String,
    external_id: String,
    username: Option<String>,
    profile_url: Option<String>,
    display_name: Option<String>,
}
async fn write_identity(
    s: &AppState,
    mut p: Identity,
    actor: Option<Uuid>,
    import: bool,
) -> Result<Value> {
    if p.external_id.is_empty() || p.provider.is_empty() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "identity is required",
        ));
    }
    let mut tx = s.pool.begin().await?;
    if let Some(profile) = p.profile_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM crm.profiles WHERE id=$1 AND archived_at IS NULL)",
        )
        .bind(profile)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(ApiError(StatusCode::NOT_FOUND, "profile not found"));
        }
    }
    if ["wechat", "wechat_group", "instagram", "linkedin"].contains(&p.provider.as_str()) {
        let provider = match p.provider.as_str() {
            "instagram" => monica_core::Provider::Instagram,
            "linkedin" => monica_core::Provider::Linkedin,
            _ => monica_core::Provider::Wechat,
        };
        let account =
            monica_core::identity::account_id(provider, &p.external_id).map_err(failure)?;
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name,username,profile_url) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(id) DO NOTHING").bind(account).bind(provider.schema()).bind(&p.external_id).bind(if p.provider=="wechat_group"{"group"}else{"person"}).bind(p.display_name.as_deref().unwrap_or(&p.external_id)).bind(&p.username).bind(&p.profile_url).execute(&mut *tx).await?;
        sqlx::query("SELECT id FROM crm.source_accounts WHERE id=$1 FOR UPDATE")
            .bind(account)
            .fetch_one(&mut *tx)
            .await?;
        let previous: Option<Uuid> = sqlx::query_scalar(
            "SELECT profile_id FROM crm.source_account_links WHERE account_id=$1",
        )
        .bind(account)
        .fetch_optional(&mut *tx)
        .await?;
        if p.profile_id.is_none() {
            p.profile_id = previous;
        }
        if let Some(profile) = p.profile_id {
            linking::set_links(
                &mut tx,
                &[LinkChange {
                    account_id: account,
                    profile_id: Some(profile),
                    expected_profile_id: previous,
                }],
                actor,
                &json!({"kind":"user-confirmed-account"}),
            )
            .await
            .map_err(failure)?;
        }
        let row:Value=sqlx::query_scalar("INSERT INTO crm.identities(id,profile_id,source_account_id,provider,external_id,username,profile_url,display_name,status) VALUES($1,$2,$1,$3,$4,$5,$6,$7,CASE WHEN $2::uuid IS NULL THEN 'pending' ELSE 'active' END) ON CONFLICT(provider,external_id) DO UPDATE SET username=excluded.username,profile_url=excluded.profile_url,display_name=excluded.display_name,last_seen_at=now() RETURNING to_jsonb(identities)").bind(account).bind(p.profile_id).bind(provider.schema()).bind(&p.external_id).bind(&p.username).bind(&p.profile_url).bind(&p.display_name).fetch_optional(&mut *tx).await?.ok_or(ApiError(StatusCode::CONFLICT,"identity belongs to a different profile"))?;
        tx.commit().await?;
        return Ok(public_identity(row));
    }
    let row:Value=sqlx::query_scalar("INSERT INTO crm.identities(id,profile_id,provider,external_id,username,profile_url,display_name,status) VALUES($1,$2,$3,$4,$5,$6,$7,CASE WHEN $2::uuid IS NULL THEN 'pending' ELSE 'active' END) ON CONFLICT(provider,external_id) DO UPDATE SET profile_id=coalesce(excluded.profile_id,identities.profile_id),username=excluded.username,profile_url=excluded.profile_url,display_name=excluded.display_name,last_seen_at=now(),status=CASE WHEN $8 AND excluded.profile_id IS NOT NULL THEN 'active' ELSE identities.status END WHERE $8 OR crm.identities.profile_id IS NOT DISTINCT FROM excluded.profile_id RETURNING to_jsonb(identities)").bind(Uuid::new_v4()).bind(p.profile_id).bind(p.provider).bind(p.external_id).bind(p.username).bind(p.profile_url).bind(p.display_name).bind(import).fetch_optional(&mut *tx).await?.ok_or(ApiError(StatusCode::CONFLICT,"Confirm the source account link through account management"))?;
    tx.commit().await?;
    Ok(public_identity(row))
}
fn public_identity(value: Value) -> Value {
    let keys = [
        "id",
        "profile_id",
        "provider",
        "external_id",
        "username",
        "profile_url",
        "display_name",
        "status",
    ];
    Value::Object(
        keys.into_iter()
            .map(|k| (k.to_owned(), value[k].clone()))
            .collect(),
    )
}
async fn identity(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Identity>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    Ok((
        StatusCode::CREATED,
        Json(write_identity(&s, p, Some(actor), true).await?),
    ))
}
async fn profile_identity(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(mut p): Json<Identity>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    if ["wechat", "wechat_group", "instagram", "linkedin"].contains(&p.provider.as_str()) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Platform accounts are managed through source account links",
        ));
    }
    p.profile_id = Some(id);
    Ok((
        StatusCode::CREATED,
        Json(write_identity(&s, p, Some(actor), false).await?),
    ))
}
#[derive(Deserialize)]
struct Candidate {
    identity_id: Uuid,
    candidate_profile_id: Uuid,
    score: f64,
    #[serde(default)]
    evidence: Value,
}
async fn candidate(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Candidate>,
) -> Result<(StatusCode, Json<Value>)> {
    user(&s, &h).await?;
    if !(0.0..=1.0).contains(&p.score) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "score must be between zero and one",
        ));
    }
    let row:Value=sqlx::query_scalar("INSERT INTO crm.identity_candidates(id,identity_id,candidate_profile_id,score,evidence) VALUES($1,$2,$3,$4,$5) RETURNING to_jsonb(identity_candidates)").bind(Uuid::new_v4()).bind(p.identity_id).bind(p.candidate_profile_id).bind(p.score).bind(p.evidence).fetch_one(&s.pool).await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn candidates(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(c)||jsonb_build_object('source_profile_id',i.profile_id,'provider',i.provider,'external_id',i.external_id,'username',i.username,'candidate_display_name',p.display_name) FROM crm.identity_candidates c JOIN crm.identities i ON i.id=c.identity_id JOIN crm.directory_profiles p ON p.id=c.candidate_profile_id WHERE NOT(i.provider='wechat' AND crm.wechat_is_official(i.external_id)) AND c.status=$1 ORDER BY c.score DESC,c.id LIMIT 100").bind(q.status_filter.unwrap_or_else(||"pending".into())).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn accept_candidate(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    let row=sqlx::query("SELECT c.candidate_profile_id,i.source_account_id,i.profile_id FROM crm.identity_candidates c JOIN crm.identities i ON i.id=c.identity_id WHERE c.id=$1 AND c.status='pending' FOR UPDATE OF c").bind(id).fetch_optional(&mut *tx).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"candidate not found"))?;
    let account: Option<Uuid> = row.get("source_account_id");
    let account = account.ok_or(ApiError(
        StatusCode::CONFLICT,
        "identity has no source account",
    ))?;
    let profile: Uuid = row.get("candidate_profile_id");
    linking::set_links(
        &mut tx,
        &[LinkChange {
            account_id: account,
            profile_id: Some(profile),
            expected_profile_id: row.get("profile_id"),
        }],
        Some(actor),
        &json!({"kind":"accepted-candidate","candidate_id":id}),
    )
    .await
    .map_err(failure)?;
    sqlx::query("UPDATE crm.identity_candidates SET status='accepted',reviewed_by=$2,reviewed_at=now() WHERE id=$1").bind(id).bind(actor).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"status":"accepted","profile_id":profile})))
}
async fn reject_candidate(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    if sqlx::query("UPDATE crm.identity_candidates SET status='rejected',reviewed_by=$2,reviewed_at=now() WHERE id=$1 AND status='pending'").bind(id).bind(actor).execute(&s.pool).await?.rows_affected()!=1{return Err(ApiError(StatusCode::NOT_FOUND,"candidate not found"));}
    Ok(Json(json!({"status":"rejected"})))
}

async fn archived(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',p.id,'display_name',p.display_name,'archived_at',p.archived_at,'target_id',r.new_profile_id) FROM crm.profiles p JOIN crm.profile_redirects r ON r.old_profile_id=p.id WHERE p.archived_at IS NOT NULL AND r.evidence->>'kind'='empty-after-account-move' ORDER BY p.archived_at DESC").fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn restore(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    if !linking::restore(&s.pool, id, actor)
        .await
        .map_err(failure)?
    {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Automatically archived profile not found",
        ));
    }
    Ok(Json(json!({"id":id,"restored":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;
    async fn request(router: Router, method: &str, path: &str, body: Value) -> Value {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost:17494")
            .header("origin", "http://localhost:17494")
            .header("content-type", "application/json")
            .header(
                "cookie",
                "crm_session=fixture-session; crm_csrf=fixture-csrf",
            )
            .header("x-csrf-token", "fixture-csrf")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        assert!(
            status.is_success(),
            "{path}: {status} {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).unwrap()
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn matching_http_decisions_cache_invalidation_and_explicit_identity_link(
        pool: sqlx::PgPool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            pool.clone(),
            "synthetic-session-secret-32-characters",
            "http://localhost:17494".into(),
            dir.path().into(),
        )
        .await
        .unwrap();
        let router = crate::auth::routes(state);
        let actor = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'fixture@example.test','unused')").bind(actor).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')").bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"fixture-session"))).execute(&pool).await.unwrap();
        let profile = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Alice Smith')").bind(profile).execute(&pool).await.unwrap();
        let wechat=request(router.clone(),"POST","/api/v1/identities",json!({"provider":"wechat","external_id":"synthetic-wechat","display_name":"Alice Smith","profile_id":profile})).await;
        assert_eq!(wechat["profile_id"], json!(profile));
        let insta=request(router.clone(),"POST","/api/v1/identities",json!({"provider":"instagram","external_id":"synthetic-instagram","display_name":"Alice Smith"})).await;
        let review = request(router.clone(), "GET", "/api/v1/link-review", json!(null)).await;
        assert_eq!(review["total"], 1);
        assert_eq!(review["queue"][0]["pair"], "instagram-wechat");
        assert!(review["queue"][0]["score"].as_i64().unwrap() >= 90);
        assert_eq!(insta.as_object().unwrap().len(), 8);
        let decision = json!({"account_id":review["queue"][0]["account_id"],"profile_id":profile,"expected_profile_id":null});
        request(
            router.clone(),
            "POST",
            "/api/v1/link-review/reject",
            decision.clone(),
        )
        .await;
        let rejected = request(
            router.clone(),
            "GET",
            "/api/v1/link-review?status_filter=no_match",
            json!(null),
        )
        .await;
        assert_eq!(rejected["total"], 1);
        assert_eq!(rejected["queue"][0]["status"], "no_match");
        request(
            router.clone(),
            "POST",
            "/api/v1/link-review/confirm",
            decision,
        )
        .await;
        let linked = request(
            router.clone(),
            "GET",
            "/api/v1/link-review/linked?providers=instagram,wechat",
            json!(null),
        )
        .await;
        assert_eq!(linked.as_array().unwrap().len(), 1);
        assert_eq!(linked[0]["accounts"].as_array().unwrap().len(), 2);
        assert_eq!(
            request(router, "GET", "/api/v1/link-review", json!(null)).await["total"],
            0
        );
    }
}

#[derive(Deserialize)]
struct MergeTarget {
    target_id: Uuid,
}
async fn merge_preview(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<MergeTarget>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    let value = monica_core::profile_merge::preview(&mut tx, id, q.target_id)
        .await
        .map_err(failure)?;
    tx.commit().await?;
    Ok(Json(value))
}
async fn merge_person(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(q): Json<monica_core::profile_merge::Request>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    Ok(Json(
        monica_core::profile_merge::apply(&s.pool, id, &q, Some(actor))
            .await
            .map_err(failure)?,
    ))
}
#[derive(Default, Deserialize)]
struct AvatarPage {
    before: Option<i64>,
}
async fn avatar_history(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<AvatarPage>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let id = linking::resolve_profile(&s.pool, id)
        .await
        .map_err(failure)?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT (to_jsonb(o)-'profile_id')||jsonb_build_object('time_meaning','observation_only') FROM crm.profile_avatar_observations o WHERE o.profile_id=$1 AND ($2::bigint IS NULL OR o.sequence<$2) ORDER BY o.sequence DESC LIMIT 40")
        .bind(id).bind(q.before).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}

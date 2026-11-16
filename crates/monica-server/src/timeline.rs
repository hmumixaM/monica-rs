#[path = "timeline_metadata.rs"]
mod metadata;
#[path = "social_metrics.rs"]
mod social_metrics;
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn invalid() -> ApiError {
    ApiError(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Invalid timeline cursor, date or media filter",
    )
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/timeline", get(list))
        .route("/api/v1/timeline/summary", get(summary))
        .route("/api/v1/timeline/{id}", get(detail))
        .route("/api/v1/metrics/social", get(metrics))
}
#[derive(Default, Deserialize)]
struct Filter {
    profile_id: Option<Uuid>,
    provider: Option<String>,
    before: Option<String>,
    after: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
    media: Option<String>,
}
async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if q.media.as_deref().is_some_and(|v| v != "ready") {
        return Err(invalid());
    }
    for stamp in [&q.before, &q.after].into_iter().flatten() {
        chrono::DateTime::parse_from_rfc3339(stamp).map_err(|_| invalid())?;
    }
    let (mut at, mut id) = (None, None);
    if let Some(cursor) = q.cursor {
        let value: Value = serde_json::from_slice(&URL_SAFE.decode(cursor).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
        let a = value
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or_else(invalid)?;
        at = a[0].as_str().map(str::to_owned);
        id = Some(Uuid::parse_str(a[1].as_str().ok_or_else(invalid)?).map_err(|_| invalid())?);
        if let Some(at) = &at {
            chrono::DateTime::parse_from_rfc3339(at).map_err(|_| invalid())?;
        }
    }
    let query = r#"WITH candidates AS MATERIALIZED (
SELECT e.* FROM crm.timeline_feed e
WHERE e.status<>'deleted' AND ($1::uuid IS NULL OR e.profile_id=$1 OR (e.provider='manual' AND EXISTS(SELECT 1 FROM crm.activity_participants ap WHERE ap.activity_id=e.id AND ap.profile_id=$1)))
AND ($2::text IS NULL OR e.provider=$2) AND ($3::timestamptz IS NULL OR e.occurred_at<$3::timestamptz)
AND ($4::timestamptz IS NULL OR e.occurred_at>=$4::timestamptz)
AND ($5::uuid IS NULL OR CASE WHEN $6::timestamptz IS NULL THEN e.occurred_at IS NULL AND e.id<$5 ELSE e.occurred_at<$6::timestamptz OR e.occurred_at IS NULL OR (e.occurred_at=$6::timestamptz AND e.id<$5) END)
AND (NOT $7 OR EXISTS(SELECT 1 FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_id=e.id AND (e.provider<>'manual' OR m.media_type LIKE 'image/%' OR m.media_type LIKE 'video/%'))) ORDER BY e.occurred_at DESC NULLS LAST,e.id DESC LIMIT $8

)

SELECT jsonb_build_object(
 'id',e.id,'provider',e.provider,'external_id',e.external_id,'profile_id',e.profile_id,
 'profile_name',p.display_name,'avatar_media_id',p.avatar_media_id,'occurred_at',e.occurred_at,
 'title',CASE WHEN e.provider='manual' THEN e.content->>'title' WHEN e.event_type='story' THEN e.provider||' story' ELSE CASE e.provider WHEN 'wechat' THEN '朋友圈' WHEN 'instagram' THEN 'Instagram 动态' ELSE 'LinkedIn 动态' END END,
 'summary',CASE WHEN e.provider='manual' THEN e.content->>'body' ELSE coalesce(e.content->>'caption',e.content->>'text',e.content->>'title') END,
 'content_hash',CASE WHEN e.provider='manual' THEN '' ELSE e.content_hash END,
 'media_count',CASE WHEN jsonb_typeof(e.content->'media')='array' THEN jsonb_array_length(e.content->'media') ELSE 0 END,
 'like_count',CASE WHEN e.event_type='post' THEN coalesce(e.content->'like_count','0'::jsonb) ELSE '0'::jsonb END,
 'comment_count',CASE WHEN e.event_type='post' THEN coalesce(e.content->'comment_count','0'::jsonb) ELSE '0'::jsonb END,
 'location',e.content->'location','shared_link',e.content->'shared_link',
 'media',coalesce(e.content->'media','[]'::jsonb),'local_media',coalesce(media.items,'[]'::jsonb),
 '_content',e.content,'_event_type',e.event_type,'event_metadata',e.content->'metadata')
FROM candidates e LEFT JOIN crm.profiles p ON p.id=e.profile_id
LEFT JOIN LATERAL(SELECT jsonb_agg(jsonb_build_object('id',m.id,'media_type',m.media_type,'width',m.width,'height',m.height,'role',l.role,'sha256',m.sha256,'byte_length',m.byte_length,'source_urls',(SELECT jsonb_agg(DISTINCT source_url) FROM crm.media_provenance WHERE media_id=m.id AND source_url IS NOT NULL),'source_url',(SELECT min(source_url) FROM crm.media_provenance WHERE media_id=m.id)) ORDER BY l.role,m.id) AS items FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_id=e.id AND (e.provider<>'manual' OR m.media_type LIKE 'image/%' OR m.media_type LIKE 'video/%')) media ON true
ORDER BY e.occurred_at DESC NULLS LAST,e.id DESC
"#;
    // Scope through indexed IDs before filtering and hydrating this page. Keep
    // the original ownership predicate to handle manual/source UUID collisions.
    let query = if q.profile_id.is_some() {
        query.replace("FROM crm.timeline_feed e", "FROM crm.timeline_feed e JOIN (SELECT id FROM crm.timeline_events WHERE profile_id=$1 AND status<>'deleted' UNION SELECT activity_id FROM crm.activity_participants WHERE profile_id=$1) eligible ON eligible.id=e.id")
    } else {
        query.to_owned()
    };
    let rows: Vec<Value> = sqlx::query_scalar(&query)
        .bind(q.profile_id)
        .bind(q.provider)
        .bind(q.before)
        .bind(q.after)
        .bind(id)
        .bind(at)
        .bind(q.media.is_some())
        .bind(q.limit.unwrap_or(50).clamp(1, 100))
        .fetch_all(&s.pool)
        .await?;
    let mut rows = rows;
    let mut availability = std::collections::HashMap::new();
    for row in &mut rows {
        check_local_media(&s.media_root, &mut row["local_media"], &mut availability).await;
    }
    let mut rows = rows
        .into_iter()
        .map(|mut r| {
            let content = r.as_object_mut().unwrap().remove("_content").unwrap();
            let kind = r.as_object_mut().unwrap().remove("_event_type").unwrap();
            if r["provider"] != "manual" {
                r["event_metadata"] = metadata::normalize(
                    r["provider"].as_str().unwrap(),
                    kind.as_str().unwrap(),
                    &r["external_id"],
                    &r["occurred_at"],
                    &content,
                );
            }
            r["summary"] = display_text(&r["summary"]);
            let local = r
                .as_object_mut()
                .unwrap()
                .remove("local_media")
                .unwrap_or(json!([]));
            let mut media = merge_media(&r["media"], &local);
            for item in &mut media {
                item["ready"] =
                    json!(item.get("id").is_some_and(|v| !v.is_null()) && item["ready"] != false);
            }
            if q.media.is_some() {
                media.retain(|v| v["ready"] == true);
            }
            r["media"] = json!(media);
            r["cursor"] = json!(
                URL_SAFE.encode(serde_json::to_vec(&json!([r["occurred_at"], r["id"]])).unwrap())
            );
            r
        })
        .collect::<Vec<_>>();
    resolve_mentions(&s.pool, &mut rows).await?;
    Ok(Json(json!(rows)))
}
async fn detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let activity: Option<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',a.id,'provider','manual','external_id',a.id::text,'occurred_at',a.occurred_at,'title',a.title,'content',a.body,'activity_type',a.activity_type,'source_type',a.source_type,'source_record_id',a.source_record_id,'created_at',a.created_at,'event_type','activity') FROM crm.activities a WHERE a.id=$1 AND ($2::text IS NULL OR $2='manual')")
        .bind(id).bind(&q.provider).fetch_optional(&s.pool).await?;
    if let Some(mut activity) = activity {
        let participants: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',p.id,'display_name',p.display_name,'profile_type',p.profile_type) FROM crm.activity_participants ap JOIN crm.profiles p ON p.id=ap.profile_id WHERE ap.activity_id=$1 ORDER BY p.display_name,p.id")
            .bind(id).fetch_all(&s.pool).await?;
        let media: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(m)-'object_path'||jsonb_build_object('role',l.role,'ready',true) FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_type='activity' AND l.entity_id=$1 AND (m.media_type LIKE 'image/%' OR m.media_type LIKE 'video/%') ORDER BY l.role,m.id")
            .bind(id).fetch_all(&s.pool).await?;
        activity["participants"] = json!(participants);
        let mut media = json!(media);
        check_local_media(
            &s.media_root,
            &mut media,
            &mut std::collections::HashMap::new(),
        )
        .await;
        activity["media"] = media;
        return Ok(Json(activity));
    }
    let (mut row, source_object_id):(Value,Uuid)=sqlx::query_as("SELECT jsonb_build_object('id',e.id,'provider',e.provider,'external_id',e.external_id,'profile_id',e.profile_id,'occurred_at',e.occurred_at,'content_hash',e.content_hash,'profile_display_name',p.display_name,'content',e.content,'event_type',e.event_type)||CASE WHEN e.event_type='post' THEN jsonb_build_object('source_updated_at',e.source_updated_at,'last_seen_at',e.last_seen_at,'status',e.status) ELSE jsonb_build_object('expires_at',e.expires_at,'coverage_status',e.status) END, e.object_id FROM crm.timeline_events e LEFT JOIN crm.profiles p ON p.id=e.profile_id WHERE e.id=$1 AND ($2::text IS NULL OR e.provider=$2)").bind(id).bind(q.provider).fetch_optional(&s.pool).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"timeline event not found"))?;
    row["event_metadata"] = metadata::normalize(
        row["provider"].as_str().unwrap(),
        row["event_type"].as_str().unwrap(),
        &row["external_id"],
        &row["occurred_at"],
        &row["content"],
    );
    resolve_mentions(&s.pool, std::slice::from_mut(&mut row)).await?;
    let media:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',m.id,'role',l.role,'media_type',m.media_type,'byte_length',m.byte_length,'width',m.width,'height',m.height,'duration_ms',to_jsonb(m)->'duration_ms','capture_at',m.capture_at,'sha256',m.sha256,'source_urls',(SELECT jsonb_agg(DISTINCT source_url) FROM crm.media_provenance WHERE media_id=m.id AND source_url IS NOT NULL),'source_url',coalesce((SELECT min(source_url) FROM crm.media_provenance WHERE media_id=m.id),m.source_record_id)) FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_id=$1 ORDER BY l.role,m.id").bind(id).fetch_all(&s.pool).await?;
    let mut interactions:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',i.id,'interaction_type',i.interaction_type,'external_id',i.external_id,'author_profile_id',i.author_profile_id,'author_external_id',i.author_external_id,'author_name',i.author_name,'occurred_at',i.occurred_at,'metadata',i.metadata,'content',i.content,'local_media',coalesce((SELECT jsonb_agg(jsonb_build_object('id',m.id,'media_type',m.media_type,'width',m.width,'height',m.height,'role',l.role,'sha256',m.sha256,'byte_length',m.byte_length,'source_urls',(SELECT jsonb_agg(DISTINCT source_url) FROM crm.media_provenance WHERE media_id=m.id AND source_url IS NOT NULL),'source_url',(SELECT min(source_url) FROM crm.media_provenance WHERE media_id=m.id)) ORDER BY l.role) FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_type='social_interaction' AND l.entity_id=i.id),'[]')) FROM crm.social_interactions i WHERE post_id=$1 AND NOT is_deleted ORDER BY occurred_at NULLS LAST,id").bind(id).fetch_all(&s.pool).await?;
    let mut media = json!(media);
    let mut availability = std::collections::HashMap::new();
    check_local_media(&s.media_root, &mut media, &mut availability).await;
    for interaction in &mut interactions {
        check_local_media(
            &s.media_root,
            &mut interaction["local_media"],
            &mut availability,
        )
        .await;
    }
    row["media"] = json!(merge_media(&row["content"]["media"], &media));
    if row["event_type"] == "post" {
        row["interactions"] = json!(
            interactions
                .into_iter()
                .map(|mut i| {
                    i["media"] = json!(merge_media(&i["metadata"]["media"], &i["local_media"]));
                    i
                })
                .collect::<Vec<_>>()
        );
    }
    row["media_reading"] = reading(
        &s.pool,
        id,
        if row["event_type"] == "post" {
            "social_post"
        } else {
            "social_story"
        },
    )
    .await?;
    row["legacy_version_count"]=json!(sqlx::query_scalar::<_,i64>("SELECT (SELECT count(*) FROM crm.legacy_social_versions WHERE event_id=$1)+(SELECT count(*) FROM crm.legacy_chatlog_versions WHERE entity_type='post' AND entity_id=$1)").bind(id).fetch_one(&s.pool).await?);
    let mut revision_query = "SELECT content_hash,observed_at,source_record_id,revision_seq FROM crm.notebook_social_revisions WHERE event_id=$1 UNION ALL SELECT content_hash,observed_at,source_record_id,revision FROM crm.connection_social_revisions WHERE event_id=$1".to_string();
    if let Some(provider) = row["provider"]
        .as_str()
        .filter(|p| ["wechat", "instagram", "linkedin"].contains(p))
    {
        revision_query.push_str(&format!(" UNION ALL SELECT r.hash,m.observed_at,o.external_id,r.version FROM {provider}.revisions r JOIN {provider}.objects o ON o.id=r.object_id LEFT JOIN {provider}.revision_metadata m ON m.object_id=r.object_id AND m.version=r.version WHERE r.object_id=$2"));
    }
    let revision_sql = format!(
        "SELECT jsonb_build_object('content_hash',content_hash,'observed_at',observed_at,'source_record_id',source_record_id) FROM ({revision_query}) revisions ORDER BY observed_at DESC NULLS LAST,revision_seq DESC"
    );
    let mut query = sqlx::query_scalar::<_, Value>(&revision_sql).bind(id);
    if ["wechat", "instagram", "linkedin"].contains(&row["provider"].as_str().unwrap_or("")) {
        query = query.bind(source_object_id);
    }
    let revisions = query.fetch_all(&s.pool).await?;
    row["revisions"] = json!(
        revisions
            .into_iter()
            .map(|mut r| {
                if row["event_type"] == "story" {
                    r.as_object_mut().unwrap().remove("source_record_id");
                }
                r
            })
            .collect::<Vec<_>>()
    );
    Ok(Json(row))
}
async fn summary(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let id = q.profile_id.ok_or(ApiError(
        StatusCode::UNPROCESSABLE_ENTITY,
        "profile_id is required",
    ))?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.profiles WHERE id=$1)")
        .bind(id)
        .fetch_one(&s.pool)
        .await?;
    if !exists {
        return Err(ApiError(StatusCode::NOT_FOUND, "profile not found"));
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('provider',provider,'posts',count(*) FILTER(WHERE event_type='post'),'stories',count(*) FILTER(WHERE event_type='story'),'events',count(*),'latest',max(occurred_at),'likes',coalesce(sum(CASE WHEN jsonb_typeof(content->'like_count')='number' THEN (content->>'like_count')::bigint ELSE 0 END),0),'comments',coalesce(sum(CASE WHEN jsonb_typeof(content->'comment_count')='number' THEN (content->>'comment_count')::bigint ELSE 0 END),0),'media',coalesce(sum((SELECT count(*) FROM crm.media_links l WHERE l.entity_id=e.id)),0)) FROM crm.timeline_feed e WHERE (profile_id=$1 OR (e.provider='manual' AND EXISTS(SELECT 1 FROM crm.activity_participants ap WHERE ap.activity_id=e.id AND ap.profile_id=$1))) AND status<>'deleted' GROUP BY provider").bind(id).fetch_all(&s.pool).await?;
    let total = rows
        .iter()
        .map(|r| r["events"].as_i64().unwrap_or(0))
        .sum::<i64>();
    let mut providers = serde_json::Map::new();
    for mut row in rows {
        let key = row["provider"].as_str().unwrap().to_owned();
        row.as_object_mut().unwrap().remove("provider");
        providers.insert(key, row);
    }
    Ok(Json(
        json!({"profile_id":id,"providers":providers,"total_events":total}),
    ))
}
async fn metrics(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(social_metrics::read(&s.pool, q.profile_id).await?))
}

fn display_text(value: &Value) -> Value {
    let Some(text) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return Value::Null;
    };
    let lower = text.to_lowercase();
    let generic = [
        "partial",
        "complete",
        "unknown",
        "unavailable",
        "expired",
        "missing",
        "instagram 动态",
        "linkedin 动态",
        "朋友圈",
    ]
    .contains(&lower.as_str());
    let kind = ["", "instagram", "wechat", "linkedin"]
        .iter()
        .any(|provider| {
            lower.strip_prefix(provider).is_some_and(|rest| {
                ["story", "stories", "post", "动态", "快拍"].contains(&rest.trim_start())
            })
        });
    if generic || kind {
        Value::Null
    } else {
        json!(text)
    }
}

async fn check_local_media(
    root: &std::path::Path,
    media: &mut Value,
    checked: &mut std::collections::HashMap<(String, u64), bool>,
) {
    for item in media.as_array_mut().into_iter().flatten() {
        let hash = item["sha256"].as_str().unwrap_or("").to_owned();
        let bytes = item["byte_length"].as_u64();
        let available =
            if let (Ok(relative), Some(bytes)) = (monica_core::media::object_path(&hash), bytes) {
                let key = (hash, bytes);
                if let Some(value) = checked.get(&key) {
                    *value
                } else {
                    let value = tokio::fs::metadata(root.join(relative))
                        .await
                        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == bytes);
                    checked.insert(key, value);
                    value
                }
            } else {
                false
            };
        item["ready"] = json!(available);
    }
}

fn merge_media(remote: &Value, local: &Value) -> Vec<Value> {
    let local = local.as_array().cloned().unwrap_or_default();
    let by_url = local
        .iter()
        .filter(|media| media["ready"] != false)
        .flat_map(|media| {
            media["source_urls"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(std::iter::once(&media["source_url"]))
                .filter_map(move |url| {
                    url.as_str()
                        .filter(|url| !url.is_empty())
                        .map(|url| (url, media))
                })
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let exact_ids = remote
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| {
            ["source_url", "thumbnail_url"]
                .into_iter()
                .filter_map(|key| {
                    item[key]
                        .as_str()
                        .and_then(|url| by_url.get(url))
                        .map(|media| media["id"].to_string())
                })
        })
        .collect::<std::collections::BTreeSet<_>>();
    // Imported archives can keep file paths instead of the original CDN URL.
    // The entity's numbered media slot still identifies the same attachment.
    // Additive history can contain several assets for a role: do not guess then.
    let by_role = |role: &str| {
        let mut matches = local
            .iter()
            .filter(|m| m["ready"] != false && m["role"].as_str() == Some(role));
        let first = matches.next()?;
        let source = first["source_url"].as_str().unwrap_or("");
        (matches.all(|m| m["id"] == first["id"])
            && !exact_ids.contains(&first["id"].to_string())
            && (source.is_empty() || source.starts_with('/') || source.starts_with("file://")))
        .then_some(first)
    };
    let mut result = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (index, item) in remote
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m.is_object())
        .enumerate()
    {
        let url = item["source_url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .or(item["thumbnail_url"].as_str());
        let role = item["role"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("media-{}", index + 1));
        // Only the archived Instagram attachment identity permits positional
        // recovery. A current CDN URL miss can be a newer, uncached revision.
        let archived = ["source_url", "thumbnail_url"].into_iter().any(|key| {
            item[key]
                .as_str()
                .is_some_and(|url| url.starts_with("archive://instagram/"))
        });
        let archive_role = |role: &str| {
            archived
                .then(|| by_role(role))
                .flatten()
                .filter(|media| !seen.contains(&media["id"].to_string()))
        };
        let poster = item["thumbnail_url"]
            .as_str()
            .and_then(|u| by_url.get(u).copied())
            .or_else(|| {
                let index = role.strip_prefix("media-")?;
                [
                    format!("thumbnail-{index}"),
                    format!("thumbnail-{role}"),
                    format!("{role}-poster"),
                ]
                .iter()
                .find_map(|role| archive_role(role))
                .filter(|media| {
                    media["media_type"]
                        .as_str()
                        .is_some_and(|kind| kind.starts_with("image"))
                })
            });
        let local = url
            .and_then(|u| by_url.get(u).copied())
            .or_else(|| {
                archive_role(&role).filter(|media| {
                    let expected = item["media_type"].as_str().unwrap_or("").split('/').next();
                    let actual = media["media_type"].as_str().unwrap_or("").split('/').next();
                    expected == actual
                })
            })
            .or(poster);
        let mut resolved = item.clone();
        if let Some(local) = local {
            resolved
                .as_object_mut()
                .unwrap()
                .extend(local.as_object().unwrap().clone());
            // Keep the current record's evidence, including when a poster is used.
            if item["source_url"].is_string() {
                resolved["source_url"] = item["source_url"].clone();
            }
        }
        if item["media_type"].as_str().is_some_and(|s| !s.is_empty()) {
            resolved["source_media_type"] = item["media_type"].clone();
        }
        if let (Some(local), Some(poster)) = (local, poster)
            && local["id"] != poster["id"]
        {
            resolved["thumbnail_id"] = poster["id"].clone();
        }
        resolved["ready"] = json!(resolved["id"].is_string() && resolved["ready"] != false);
        resolved.as_object_mut().unwrap().remove("sha256");
        resolved.as_object_mut().unwrap().remove("source_urls");
        let key = resolved
            .get("id")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| json!(url))
            .to_string();
        if seen.insert(key) {
            result.push(resolved);
        }
        if let (Some(_), Some(poster)) = (local, poster) {
            seen.insert(poster["id"].to_string());
        }
    }
    for item in local {
        if seen.insert(item["id"].to_string()) {
            result.push(item);
        }
    }
    result
}

async fn resolve_mentions(pool: &sqlx::PgPool, rows: &mut [Value]) -> Result<()> {
    let ids = rows
        .iter()
        .flat_map(|r| {
            r["event_metadata"]["mentions"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .filter(|m| m.get("url").is_none())
        .filter_map(|m| m["external_id"].as_str())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(());
    }
    let people:Vec<(String,Uuid,String)>=sqlx::query_as("SELECT i.external_id,p.id,p.display_name FROM crm.identities i JOIN crm.profiles p ON p.id=i.profile_id WHERE i.provider='wechat' AND i.external_id=ANY($1)").bind(ids).fetch_all(pool).await?;
    for row in rows {
        if let Some(mentions) = row["event_metadata"]["mentions"].as_array_mut() {
            for mention in mentions.iter_mut().filter(|m| m.get("url").is_none()) {
                if let Some((_, id, name)) = people
                    .iter()
                    .find(|(external, _, _)| mention["external_id"] == *external)
                {
                    mention["label"] = json!(name);
                    mention["url"] = json!(format!("/profiles/{id}"));
                }
            }
        }
    }
    Ok(())
}

async fn reading(pool: &sqlx::PgPool, id: Uuid, kind: &str) -> Result<Value> {
    let mut result:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('summary',summary,'items',items,'model',model,'generated_at',generated_at) FROM crm.media_readings WHERE entity_id=$1 AND entity_type=$2").bind(id).bind(kind).fetch_optional(pool).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('media_id',r.media_id,'text',r.text,'description',r.description,'category',r.category,'summary',r.summary,'model',r.model,'generated_at',r.generated_at) FROM crm.media_links l JOIN crm.image_ocr_readings r ON r.media_id=l.media_id WHERE l.entity_id=$1 AND l.entity_type=$2 AND (r.status='complete' OR r.generated_at IS NOT NULL) AND (coalesce(btrim(r.text),'')<>'' OR coalesce(btrim(r.description),'')<>'') ORDER BY l.role").bind(id).bind(kind).fetch_all(pool).await?;
    // A separate query selects only the JSON column; all available OCR items are
    // useful even while the higher-level event summary is still pending.
    if rows.is_empty() {
        return Ok(result.unwrap_or(Value::Null));
    }
    let result=result.get_or_insert_with(||json!({"summary":"","items":[],"model":rows[0]["model"],"generated_at":rows[0]["generated_at"]}));
    let mut summaries = Vec::new();
    for row in rows {
        let summary = row["summary"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or(row["description"].as_str())
            .unwrap_or("");
        if !summary.is_empty() && !summaries.contains(&summary.to_owned()) {
            summaries.push(summary.to_owned());
        }
        let items = result["items"].as_array_mut().unwrap();
        if let Some(item) = items
            .iter_mut()
            .find(|item| item["media_id"] == row["media_id"])
        {
            item["text"] = row["text"].clone();
            if row["description"].as_str().is_some_and(|s| !s.is_empty()) {
                item["description"] = row["description"].clone();
                item["category"] = row["category"].clone();
            }
        } else {
            items.push(json!({"index":items.len()+1,"media_id":row["media_id"],"kind":"photo","category":row["category"].as_str().filter(|s|!s.is_empty()).unwrap_or("文字识别"),"description":row["description"],"text":row["text"]}));
        }
    }
    if result["summary"].as_str().is_none_or(|s| s.is_empty()) {
        result["summary"] = json!(summaries.join("；").chars().take(60).collect::<String>());
    }
    result["items"]
        .as_array_mut()
        .unwrap()
        .sort_by_key(|i| i["index"].as_u64().unwrap_or(0));
    Ok(result.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn availability_checks_real_cas_bytes_and_rechecks_after_removal() {
        let folder = tempfile::tempdir().unwrap();
        let hash = "a".repeat(64);
        let path = folder
            .path()
            .join(monica_core::media::object_path(&hash).unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"published").unwrap();
        let mut media = json!([
            {"id":"valid","sha256":hash,"byte_length":9},
            {"id":"truncated","sha256":hash,"byte_length":20},
            {"id":"absent","sha256":"b".repeat(64),"byte_length":9}
        ]);
        check_local_media(
            folder.path(),
            &mut media,
            &mut std::collections::HashMap::new(),
        )
        .await;
        assert_eq!(media[0]["ready"], true);
        assert_eq!(media[1]["ready"], false);
        assert_eq!(media[2]["ready"], false);
        std::fs::remove_file(path).unwrap();
        check_local_media(
            folder.path(),
            &mut media,
            &mut std::collections::HashMap::new(),
        )
        .await;
        assert_eq!(media[0]["ready"], false);
    }

    #[test]
    fn restored_url_aliases_fill_original_slots_without_duplicate_placeholders() {
        let remote = json!([
            {"role":"media-1","media_type":"image","source_url":"https://source.invalid/one"},
            {"role":"media-2","media_type":"image","source_url":"https://source.invalid/two"}
        ]);
        let local = json!([
            {"id":"two","role":"media-2","media_type":"image/jpeg","source_url":"/data/sns-cache/old.bin","source_urls":["https://other.invalid/alias","https://source.invalid/two"],"ready":true},
            {"id":"one","role":"media-1","media_type":"image/jpeg","source_urls":["https://source.invalid/one"],"ready":true}
        ]);
        let merged = merge_media(&remote, &local);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["id"], "one");
        assert_eq!(merged[1]["id"], "two");
        assert!(merged.iter().all(|item| item["ready"] == true));
        let mut missing = local.clone();
        missing[1]["ready"] = json!(false);
        let merged = merge_media(&remote, &missing);
        assert!(!merged[0]["id"].is_string());
        assert_eq!(merged[0]["ready"], false);
        assert_eq!(merged[1]["id"], "two");
    }

    #[test]
    fn archive_slots_replace_stale_references_and_pair_video_posters() {
        let remote = json!([
            {"media_type":"image","source_url":"archive://instagram/photo/image"},
            {"media_type":"video","source_url":"https://expired.invalid/video","thumbnail_url":"archive://instagram/video/image"},
            {"media_type":"image","source_url":"https://uncached.invalid/photo"}
        ]);
        // SQL role ordering and saved file paths differ from the source array.
        let local = json!([
            {"id":"movie","role":"media-2","media_type":"video/mp4","source_url":null},
            {"id":"cover","role":"thumbnail-2","media_type":"image/jpeg","source_url":null},
            {"id":"photo","role":"media-1","media_type":"image/jpeg","source_url":null}
        ]);
        let merged = merge_media(&remote, &local);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0]["id"], "photo");
        assert_eq!(merged[0]["source_url"], remote[0]["source_url"]);
        assert_eq!(merged[1]["id"], "movie");
        assert_eq!(merged[1]["thumbnail_id"], "cover");
        assert_eq!(merged[1]["source_media_type"], "video");
        assert!(merged[2]["id"].is_null());

        let ambiguous = json!([
            {"id":"older","role":"media-1"},
            {"id":"newer","role":"media-1"}
        ]);
        let merged = merge_media(&json!([remote[0]]), &ambiguous);
        assert!(merged[0]["id"].is_null());
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn current_and_reordered_media_never_reuse_a_historical_slot() {
        let a = json!({"source_url":"https://synthetic.invalid/A","media_type":"image"});
        let b = json!({"source_url":"https://synthetic.invalid/B","media_type":"image"});
        let cached = json!([{"id":"A","role":"media-2","source_url":"https://synthetic.invalid/A","media_type":"image/jpeg"}]);
        let merged = merge_media(&json!([a, b]), &cached);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["id"], "A");
        assert!(merged[1]["id"].is_null());
        assert_eq!(merged[1]["source_url"], b["source_url"]);

        let merged = merge_media(&json!([b, a]), &cached);
        assert!(merged[0]["id"].is_null());
        assert_eq!(merged[1]["id"], "A");
        let old = json!([{"id":"old-video","role":"media-1","media_type":"video/mp4","source_url":"/data/old.mp4"}]);
        let archived_image =
            json!({"media_type":"image","source_url":"archive://instagram/photo/image"});
        let merged = merge_media(&json!([archived_image]), &old);
        assert_eq!(merged.len(), 2);
        assert!(merged[0]["id"].is_null());
        assert_eq!(merged[0]["source_url"], archived_image["source_url"]);
        assert_eq!(merged[1]["id"], "old-video");
    }

    #[test]
    fn remote_video_order_posters_and_placeholder_text() {
        let remote = json!([{"source_url":"https://synthetic.invalid/video","thumbnail_url":"https://synthetic.invalid/poster","media_type":"video/mp4"},{"source_url":"https://synthetic.invalid/remote","media_type":"image/jpeg"}]);
        let local = json!([{"id":"video","source_url":"https://synthetic.invalid/video","media_type":"video/mp4"},{"id":"poster","source_url":"https://synthetic.invalid/poster","media_type":"image/jpeg"}]);
        let merged = merge_media(&remote, &local);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["thumbnail_id"], "poster");
        assert_eq!(merged[0]["source_media_type"], "video/mp4");
        let poster_only = merge_media(&remote, &json!([local[1]]));
        assert_eq!(poster_only.len(), 2);
        assert_eq!(poster_only[0]["id"], "poster");
        assert_eq!(poster_only[0]["media_type"], "image/jpeg");
        for text in [
            "",
            " unknown ",
            "INSTAGRAMstory",
            " Instagram 动态 ",
            "朋友圈",
        ] {
            assert_eq!(display_text(&json!(text)), Value::Null);
        }
        assert_eq!(
            display_text(&json!("  A real story  ")),
            json!("A real story")
        );
    }
}

#[cfg(test)]
mod read_tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;
    async fn page(router: &Router, path: &str) -> Vec<Value> {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "localhost:17494")
                    .header("cookie", "crm_session=timeline-fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap()
    }
    fn ids(rows: &[Value]) -> Vec<String> {
        rows.iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect()
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn migrated_detail_keeps_public_media_and_reads_source_object_history(
        pool: sqlx::PgPool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            pool.clone(),
            "synthetic-timeline-session-secret-32bytes",
            "http://localhost:17494".into(),
            dir.path().into(),
        )
        .await
        .unwrap();
        let actor = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'history@example.invalid','unused')")
            .bind(actor).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')")
            .bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"timeline-fixture"))).execute(&pool).await.unwrap();
        let source = monica_core::Record {
            provider: monica_core::Provider::Instagram,
            account: "history-source".into(),
            object: "history-post".into(),
            kind: "post".into(),
            event: "source-revision".into(),
            payload: json!({"caption":"Source content"}),
        };
        monica_core::ingest(&pool, &source).await.unwrap();
        let account = monica_core::identity::account_id(source.provider, &source.account).unwrap();
        let object = monica_core::identity::object_id(account, &source.kind, &source.object);
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram','history-source','person','Synthetic')")
            .bind(account).execute(&pool).await.unwrap();
        let event = Uuid::new_v4();
        assert_ne!(event, object);
        sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,event_type,external_id,content,content_hash,version) VALUES($1,'instagram',$2,$3,'post','history-post',$4,'legacy',1)")
            .bind(event).bind(object).bind(account)
            .bind(json!({"caption":"Legacy content","media":[{"source_url":"archive://instagram/history/image","media_type":"image"}]}))
            .execute(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let notebook =
            monica_core::content::register(&mut tx, "post", &json!({"text":"Notebook content"}))
                .await
                .unwrap();
        sqlx::query("INSERT INTO crm.notebook_social_revisions(id,event_id,content_object_id,content_hash,observed_at,source_record_id) VALUES($1,$2,$3,$4,now(),'notebook-history')")
            .bind(Uuid::new_v4()).bind(event).bind(notebook.id).bind(&notebook.hash).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let media = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,verified_at) VALUES($1,$2,'image/jpeg',1,'synthetic',now())")
            .bind(media).bind("e".repeat(64)).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('social_post',$1,$2,'media-1')")
            .bind(event).bind(media).execute(&pool).await.unwrap();
        let router = crate::auth::routes(state);
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/timeline/{event}"))
                    .header("host", "localhost:17494")
                    .header("cookie", "crm_session=timeline-fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let row: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap())
                .unwrap();
        assert_eq!(row["id"], event.to_string());
        assert_eq!(row["media"].as_array().unwrap().len(), 1);
        assert_eq!(row["media"][0]["id"], media.to_string());
        let hashes: Vec<_> = row["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["content_hash"].as_str().unwrap())
            .collect();
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains(&notebook.hash.as_str()));
        assert!(
            hashes.contains(&monica_core::identity::observation_hash(&source.payload).as_str())
        );
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn bounded_timeline_keeps_manual_ownership_null_cursors_and_ready_media(
        pool: sqlx::PgPool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            pool.clone(),
            "synthetic-timeline-session-secret-32bytes",
            "http://localhost:17494".into(),
            dir.path().into(),
        )
        .await
        .unwrap();
        let actor = Uuid::new_v4();
        let person = Uuid::new_v4();
        let other = Uuid::new_v4();
        let account = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'timeline@example.invalid','unused')").bind(actor).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')").bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"timeline-fixture"))).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Person'),($2,'person','Other')").bind(person).bind(other).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram','timeline-peer','person','Peer')").bind(account).execute(&pool).await.unwrap();
        let newest = Uuid::new_v4();
        let oldest = Uuid::new_v4();
        let missing_date = Uuid::new_v4();
        let elsewhere = Uuid::new_v4();
        let deleted = Uuid::new_v4();
        for (id, owner, at) in [
            (newest, person, Some("2026-01-03T00:00:00Z")),
            (oldest, person, Some("2026-01-01T00:00:00Z")),
            (missing_date, person, None),
            (elsewhere, other, Some("2026-01-04T00:00:00Z")),
            (deleted, person, Some("2026-01-05T00:00:00Z")),
        ] {
            sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version,status) VALUES($1,'instagram',$1,$2,$3,'post',$1::text,$4::timestamptz,jsonb_build_object('caption','Synthetic timeline'),'hash',1,CASE WHEN $1=$5 THEN 'deleted' ELSE 'active' END)").bind(id).bind(account).bind(owner).bind(at).bind(deleted).execute(&pool).await.unwrap();
        }
        let manual = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.activities(id,activity_type,occurred_at,title,body,source_type,content_hash) VALUES($1,'note','2026-01-02T00:00:00Z','Manual note','Manual body','manual','hash')").bind(manual).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.activity_participants(activity_id,profile_id) VALUES($1,$2)")
            .bind(manual)
            .bind(person)
            .execute(&pool)
            .await
            .unwrap();
        for (index, event, kind) in [
            (1, oldest, "image/png"),
            (2, elsewhere, "image/png"),
            (3, manual, "text/plain"),
        ] {
            let media = Uuid::new_v4();
            sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) VALUES($1,$2,$3,0,'synthetic')").bind(media).bind(format!("{index:064x}")).bind(kind).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('post',$1,$2,'media')").bind(event).bind(media).execute(&pool).await.unwrap();
        }
        let router = crate::auth::routes(state);
        let first = page(&router, "/api/v1/timeline?limit=2").await;
        assert_eq!(ids(&first), vec![elsewhere.to_string(), newest.to_string()]);
        let second = page(
            &router,
            &format!(
                "/api/v1/timeline?limit=2&cursor={}",
                first[1]["cursor"].as_str().unwrap()
            ),
        )
        .await;
        assert_eq!(ids(&second), vec![manual.to_string(), oldest.to_string()]);
        let third = page(
            &router,
            &format!(
                "/api/v1/timeline?limit=2&cursor={}",
                second[1]["cursor"].as_str().unwrap()
            ),
        )
        .await;
        assert_eq!(ids(&third), vec![missing_date.to_string()]);
        assert!(
            page(
                &router,
                &format!(
                    "/api/v1/timeline?cursor={}",
                    third[0]["cursor"].as_str().unwrap()
                )
            )
            .await
            .is_empty()
        );
        let own = page(&router, &format!("/api/v1/timeline?profile_id={person}")).await;
        assert_eq!(
            ids(&own),
            vec![
                newest.to_string(),
                manual.to_string(),
                oldest.to_string(),
                missing_date.to_string()
            ]
        );
        let ready = page(
            &router,
            &format!("/api/v1/timeline?profile_id={person}&media=ready&limit=1"),
        )
        .await;
        assert_eq!(ids(&ready), vec![oldest.to_string()]);
        assert_eq!(ready[0]["media"][0]["ready"], true);
        let provider = page(
            &router,
            &format!("/api/v1/timeline?profile_id={person}&provider=instagram"),
        )
        .await;
        assert_eq!(
            ids(&provider),
            vec![
                newest.to_string(),
                oldest.to_string(),
                missing_date.to_string()
            ]
        );
    }
}

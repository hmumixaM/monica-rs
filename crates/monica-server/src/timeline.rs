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
    let rows: Vec<Value> = sqlx::query_scalar(r#"
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
FROM crm.timeline_feed e LEFT JOIN crm.profiles p ON p.id=e.profile_id
LEFT JOIN LATERAL(SELECT jsonb_agg(jsonb_build_object('id',m.id,'media_type',m.media_type,'width',m.width,'height',m.height,'role',l.role,'source_url',(SELECT min(source_url) FROM crm.media_provenance WHERE media_id=m.id)) ORDER BY l.role,m.id) AS items FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_id=e.id AND (e.provider<>'manual' OR m.media_type LIKE 'image/%' OR m.media_type LIKE 'video/%')) media ON true
WHERE e.status<>'deleted' AND ($1::uuid IS NULL OR e.profile_id=$1 OR (e.provider='manual' AND EXISTS(SELECT 1 FROM crm.activity_participants ap WHERE ap.activity_id=e.id AND ap.profile_id=$1)))
AND ($2::text IS NULL OR e.provider=$2) AND ($3::timestamptz IS NULL OR e.occurred_at<$3::timestamptz)
AND ($4::timestamptz IS NULL OR e.occurred_at>=$4::timestamptz)
AND ($5::uuid IS NULL OR CASE WHEN $6::timestamptz IS NULL THEN e.occurred_at IS NULL AND e.id<$5 ELSE e.occurred_at<$6::timestamptz OR e.occurred_at IS NULL OR (e.occurred_at=$6::timestamptz AND e.id<$5) END)
AND (NOT $7 OR media.items IS NOT NULL) ORDER BY e.occurred_at DESC NULLS LAST,e.id DESC LIMIT $8
"#).bind(q.profile_id).bind(q.provider).bind(q.before).bind(q.after).bind(id).bind(at).bind(q.media.is_some()).bind(q.limit.unwrap_or(50).clamp(1,100)).fetch_all(&s.pool).await?;
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
                item["ready"] = json!(item.get("id").is_some_and(|v| !v.is_null()));
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
        activity["media"] = json!(media);
        return Ok(Json(activity));
    }
    let mut row:Value=sqlx::query_scalar("SELECT jsonb_build_object('id',e.id,'provider',e.provider,'external_id',e.external_id,'profile_id',e.profile_id,'occurred_at',e.occurred_at,'content_hash',e.content_hash,'profile_display_name',p.display_name,'content',e.content,'event_type',e.event_type)||CASE WHEN e.event_type='post' THEN jsonb_build_object('source_updated_at',e.source_updated_at,'last_seen_at',e.last_seen_at,'status',e.status) ELSE jsonb_build_object('expires_at',e.expires_at,'coverage_status',e.status) END FROM crm.timeline_events e LEFT JOIN crm.profiles p ON p.id=e.profile_id WHERE e.id=$1 AND ($2::text IS NULL OR e.provider=$2)").bind(id).bind(q.provider).fetch_optional(&s.pool).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"timeline event not found"))?;
    row["event_metadata"] = metadata::normalize(
        row["provider"].as_str().unwrap(),
        row["event_type"].as_str().unwrap(),
        &row["external_id"],
        &row["occurred_at"],
        &row["content"],
    );
    resolve_mentions(&s.pool, std::slice::from_mut(&mut row)).await?;
    let media:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',m.id,'media_type',m.media_type,'byte_length',m.byte_length,'width',m.width,'height',m.height,'duration_ms',to_jsonb(m)->'duration_ms','capture_at',m.capture_at,'source_url',coalesce((SELECT min(source_url) FROM crm.media_provenance WHERE media_id=m.id),m.source_record_id)) FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_id=$1 ORDER BY l.role,m.id").bind(id).fetch_all(&s.pool).await?;
    let interactions:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',i.id,'interaction_type',i.interaction_type,'external_id',i.external_id,'author_profile_id',i.author_profile_id,'author_external_id',i.author_external_id,'author_name',i.author_name,'occurred_at',i.occurred_at,'metadata',i.metadata,'content',i.content,'local_media',coalesce((SELECT jsonb_agg(jsonb_build_object('id',m.id,'media_type',m.media_type,'width',m.width,'height',m.height,'role',l.role,'source_url',(SELECT min(source_url) FROM crm.media_provenance WHERE media_id=m.id)) ORDER BY l.role) FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_type='social_interaction' AND l.entity_id=i.id),'[]')) FROM crm.social_interactions i WHERE post_id=$1 AND NOT is_deleted ORDER BY occurred_at NULLS LAST,id").bind(id).fetch_all(&s.pool).await?;
    row["media"] = json!(merge_media(&row["content"]["media"], &json!(media)));
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
    let mut revision_query = "SELECT content_hash,observed_at,source_record_id,revision_seq FROM crm.notebook_social_revisions WHERE event_id=$1 UNION ALL SELECT content_hash,observed_at,source_record_id,revision FROM crm.connection_social_revisions WHERE event_id=$1".to_string();
    if let Some(provider) = row["provider"]
        .as_str()
        .filter(|p| ["wechat", "instagram", "linkedin"].contains(p))
    {
        revision_query.push_str(&format!(" UNION ALL SELECT r.hash,m.observed_at,o.external_id,r.version FROM {provider}.revisions r JOIN {provider}.objects o ON o.id=r.object_id LEFT JOIN {provider}.revision_metadata m ON m.object_id=r.object_id AND m.version=r.version WHERE r.object_id=$1"));
    }
    let revisions:Vec<Value>=sqlx::query_scalar(&format!("SELECT jsonb_build_object('content_hash',content_hash,'observed_at',observed_at,'source_record_id',source_record_id) FROM ({revision_query}) revisions ORDER BY observed_at DESC NULLS LAST,revision_seq DESC")).bind(id).fetch_all(&s.pool).await?;
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

fn merge_media(remote: &Value, local: &Value) -> Vec<Value> {
    let local = local.as_array().cloned().unwrap_or_default();
    let by_url = local
        .iter()
        .filter_map(|m| {
            m["source_url"]
                .as_str()
                .filter(|u| !u.is_empty())
                .map(|u| (u, m))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut result = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for item in remote
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m.is_object())
    {
        let url = item["source_url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .or(item["thumbnail_url"].as_str());
        let poster = item["thumbnail_url"].as_str().and_then(|u| by_url.get(u));
        let local = url.and_then(|u| by_url.get(u)).or(poster);
        let mut resolved = item.clone();
        if let Some(local) = local {
            resolved
                .as_object_mut()
                .unwrap()
                .extend(local.as_object().unwrap().clone());
        }
        if item["media_type"].as_str().is_some_and(|s| !s.is_empty()) {
            resolved["source_media_type"] = item["media_type"].clone();
        }
        if let (Some(local), Some(poster)) = (local, poster)
            && local["id"] != poster["id"]
        {
            resolved["thumbnail_id"] = poster["id"].clone();
        }
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

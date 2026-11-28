use super::*;
use axum::{
    Json,
    extract::{Query, State},
    routing::get,
};
use monica_core::pages::{DIRECTORY_COLUMNS, text};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/directory", get(directory))
        .route("/api/v1/directory/posts", get(posts))
        .route("/api/v1/search", get(search))
}

pub(super) async fn rows(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Value>> {
    // Hydrate each source set once for the shared directory snapshot.
    Ok(sqlx::query_scalar(r#"WITH visible AS MATERIALIZED (SELECT * FROM crm.directory_profiles),
fields AS (
 SELECT c.profile_id,jsonb_object_agg(c.field_key,(SELECT r.value FROM crm.field_revisions r WHERE r.id=c.revision_id)) value
 FROM crm.field_current c JOIN visible v ON v.id=c.profile_id
 WHERE c.field_key IN('linkedin.headline','linkedin.location','monica.city','monica.residence','monica.nickname','monica.company','monica.job','wechat.city','wechat.province','monica.note','wechat.region','wechat.alias','wechat.remark','wechat.nickname','wechat.group_nicknames','wechat.group_names','instagram.username','instagram.full_name','monica.aliases','monica.custom_fields')
 GROUP BY c.profile_id
), facts AS (
 SELECT f.profile_id,jsonb_agg((CASE WHEN jsonb_typeof(f.value)='object' THEN f.value ELSE '{}'::jsonb END)||jsonb_build_object('type',f.fact_type,'company',coalesce(f.company_name,f.value->>'company_name',f.value->>'company'),'school',coalesce(f.school_name,f.value->>'school_name'),'company_id',f.company_id,'school_id',f.school_id) ORDER BY f.fact_type,f.fact_key) value
 FROM crm.profile_structured_current f JOIN visible v ON v.id=f.profile_id GROUP BY f.profile_id
), identities AS (
 SELECT i.profile_id,jsonb_agg(jsonb_build_array(i.display_name,i.username) ORDER BY i.id) value
 FROM crm.identities i JOIN visible v ON v.id=i.profile_id WHERE i.status NOT IN('invalid','merged') GROUP BY i.profile_id
), providers AS (
 SELECT a.profile_id,jsonb_agg(a.provider ORDER BY a.provider) value FROM (
  SELECT l.profile_id,a.provider FROM crm.source_account_links l JOIN crm.source_accounts a ON a.id=l.account_id
  UNION SELECT i.profile_id,i.provider FROM crm.identities i WHERE i.status NOT IN('invalid','merged')
 ) a JOIN visible v ON v.id=a.profile_id GROUP BY a.profile_id
), tags AS (
 SELECT m.profile_id,jsonb_agg(m.tag_id ORDER BY m.tag_id) value
 FROM crm.presented_tag_memberships m JOIN visible v ON v.id=m.profile_id GROUP BY m.profile_id
), members AS (
 SELECT m.group_profile_id,count(DISTINCT m.person_profile_id) value FROM crm.current_group_memberships m
 JOIN visible v ON v.id=m.person_profile_id AND v.profile_type='person' WHERE m.left_at IS NULL GROUP BY m.group_profile_id
), avatars AS (
 SELECT DISTINCT ON(l.entity_id) l.entity_id,l.media_id FROM crm.media_links l
 JOIN visible v ON v.id=l.entity_id AND v.avatar_media_id IS NULL
 WHERE l.entity_type='profile' AND l.role IN('wechat_avatar','instagram_avatar','linkedin_avatar')
 ORDER BY l.entity_id,l.created_at DESC,l.media_id
), conversations AS (
 SELECT DISTINCT ON(c.profile_id) c.profile_id,c.id FROM crm.conversations c JOIN visible v ON v.id=c.profile_id
 ORDER BY c.profile_id,c.conversation_type='direct' DESC,c.last_message_at DESC NULLS LAST,c.id
)
SELECT jsonb_build_object('id',p.id,'type',p.profile_type,'name',p.display_name,'summary',p.summary,
 'avatar_media_id',coalesce(p.avatar_media_id,a.media_id),'fields',coalesce(f.value,'{}'),
 'facts',coalesce(x.value,'[]'),'identities',coalesce(i.value,'[]'),'providers',coalesce(r.value,'[]'),
 'tag_ids',coalesce(t.value,'[]'),'birthday_md',(SELECT lpad(d.month::text,2,'0')||'-'||lpad(d.day::text,2,'0') FROM crm.profile_dates d WHERE d.profile_id=p.id AND d.kind='birthday'),
 'last_contact_at',s.last_contact_at,'direct_message_count',nullif(s.direct_message_count,0),
 'member_count',CASE WHEN p.profile_type='group' THEN coalesce(m.value,0) END,'conversation_id',c.id)
FROM visible p LEFT JOIN crm.page_contact_stats s ON s.profile_id=p.id
LEFT JOIN fields f ON f.profile_id=p.id LEFT JOIN facts x ON x.profile_id=p.id
LEFT JOIN identities i ON i.profile_id=p.id LEFT JOIN providers r ON r.profile_id=p.id
LEFT JOIN tags t ON t.profile_id=p.id LEFT JOIN members m ON m.group_profile_id=p.id
LEFT JOIN avatars a ON a.entity_id=p.id LEFT JOIN conversations c ON c.profile_id=p.id
ORDER BY p.display_name,p.id"#).fetch_all(&mut **tx).await?)
}

async fn directory(State(s): State<AppState>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let rows = rows(&mut tx).await?;
    let tags: Vec<Value> =
        sqlx::query_scalar("SELECT jsonb_build_array(id,name) FROM crm.tags ORDER BY name,id")
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(Json(build_directory(rows, tags)))
}

fn build_directory(rows: Vec<Value>, tags: Vec<Value>) -> Value {
    let tag_index: BTreeMap<_, _> = tags
        .iter()
        .enumerate()
        .map(|(i, v)| (v[0].as_str().unwrap().to_owned(), i))
        .collect();
    let mut organizations = [BTreeMap::<String, String>::new(), BTreeMap::new()];
    for row in &rows {
        for fact in row["facts"].as_array().unwrap() {
            for (index, kind) in ["company", "school"].iter().enumerate() {
                if let (Some(id), Some(name)) =
                    (fact[format!("{kind}_id")].as_str(), text(&fact[*kind]))
                {
                    organizations[index].insert(id.to_owned(), name);
                }
            }
        }
    }
    let dictionaries: Vec<Vec<Value>> = organizations
        .iter()
        .map(|entries| {
            let mut pairs: Vec<_> = entries.iter().collect();
            pairs.sort_by(|a, b| a.1.cmp(b.1).then_with(|| a.0.cmp(b.0)));
            pairs
                .into_iter()
                .map(|(id, name)| json!([id, name]))
                .collect()
        })
        .collect();
    let indexes: Vec<BTreeMap<String, usize>> = dictionaries
        .iter()
        .map(|entries| {
            entries
                .iter()
                .enumerate()
                .map(|(index, row)| (row[0].as_str().unwrap().to_owned(), index))
                .collect()
        })
        .collect();
    let profiles: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let aliases = monica_core::pages::aliases(
                r["name"].as_str().unwrap(),
                &r["fields"],
                r["identities"].as_array().unwrap(),
            );
            let mut ids = [BTreeSet::new(), BTreeSet::new()];
            for f in r["facts"].as_array().unwrap() {
                for (i, key) in ["company", "school"].iter().enumerate() {
                    if let Some(index) = f[format!("{key}_id")]
                        .as_str()
                        .and_then(|id| indexes[i].get(id))
                    {
                        ids[i].insert(*index);
                    }
                }
            }
            let facts =
                monica_core::pages::ordered_experience(r["facts"].as_array().unwrap().clone());
            let headline = if r["type"] == "person" {
                monica_core::pages::headline(&r["fields"], &facts)
            } else {
                Value::Null
            };
            let city = monica_core::pages::city(&r["fields"]);
            let names = monica_core::pages::group_names(
                &r["fields"]["wechat.group_nicknames"],
                r["name"].as_str().unwrap(),
                &aliases,
            );
            json!([
                r["id"],
                r["type"],
                r["name"],
                r["summary"],
                r["avatar_media_id"],
                r["tag_ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|v| tag_index.get(v.as_str()?))
                    .collect::<Vec<_>>(),
                ids[0],
                ids[1],
                aliases,
                r["providers"],
                headline,
                city,
                r["birthday_md"],
                r["last_contact_at"],
                r["direct_message_count"],
                r["member_count"],
                r["conversation_id"],
                names
            ])
        })
        .collect();
    json!({"columns":DIRECTORY_COLUMNS,"profiles":profiles,"tags":tags,"companies":dictionaries[0],"schools":dictionaries[1]})
}

async fn posts(State(s): State<AppState>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let posts:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array(id,profile_id,provider,occurred_at,left(text,200)) FROM crm.page_posts WHERE kind='post' AND btrim(text)<>'' ORDER BY occurred_at DESC NULLS LAST,id").fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"columns":["id","profile_id","provider","occurred_at","text"],"posts":posts}),
    ))
}
#[derive(Deserialize)]
struct Search {
    q: Option<String>,
    limit: Option<i64>,
}
async fn search(State(s): State<AppState>, Query(q): Query<Search>) -> Result<Json<Value>> {
    let mut tx = snapshot(&s).await?;
    let needle = q.q.unwrap_or_default().trim().to_owned();
    if needle.chars().count() < 2 {
        return Ok(Json(json!([])));
    }
    let escaped = needle
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let like = format!("%{escaped}%");
    let rows: Vec<Value> = sqlx::query_scalar(include_str!("search.sql"))
        .bind(like)
        .bind(format!("{escaped}%"))
        .bind(q.limit.unwrap_or(30).clamp(1, 100))
        .bind(needle.strip_prefix('@').unwrap_or(&needle))
        .bind(&needle)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!(rows)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    async fn timeline_hits(app: &Router, term: &str) -> Vec<Value> {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", term)
            .finish();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/search?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice::<Vec<Value>>(&bytes)
            .unwrap()
            .into_iter()
            .filter(|hit| hit["entity_type"] == "timeline")
            .collect()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn timeline_search_finds_linked_handles_mentions_and_saved_ocr(pool: sqlx::PgPool) {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(
            pool.clone(),
            "synthetic-timeline-search-secret",
            "http://testserver".into(),
            root.path().to_owned(),
        )
        .await
        .unwrap();
        let app =
            crate::auth::delegated_routes(state, (Uuid::new_v4(), "search@example.invalid".into()));
        let person = Uuid::new_v4();
        let post = Uuid::new_v4();
        let story = Uuid::new_v4();
        let image = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Author')",
        )
        .bind(person)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,username,status) VALUES(gen_random_uuid(),$1,'instagram','1001','Search_Alias','active')")
            .bind(person).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,content,content_hash,version,occurred_at)
            VALUES($1,'instagram',$1,$2,'post','p1',$4,'post-hash',1,'2026-01-02T01:00:00Z'),($3,'instagram',$3,NULL,'story','s1',$5,'story-hash',1,'2026-01-03T01:00:00Z')")
            .bind(post).bind(person).bind(story)
            .bind(json!({"text":"Ordinary caption","username":"Posting_Handle","tagged_users":[{"username":"tagged_handle"}]}))
            .bind(json!({"text":"Ordinary story","reel_mentions":[{"user":{"username":"Search_Alias"}}]}))
            .execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,source_type)
            VALUES($1,repeat('a',64),'image/png',1,'synthetic/search-image','manual')",
        )
        .bind(image)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO crm.media_links(media_id,entity_type,entity_id,role) VALUES($1,'social_post',$2,'media-1')")
            .bind(image).bind(post).execute(&pool).await.unwrap();

        let hits = timeline_hits(&app, "@SEARCH_ALIAS").await;
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0]["id"], json!(story));
        assert_eq!(hits[0]["match_source"], "mention");
        assert_eq!(hits[1]["id"], json!(post));
        assert_eq!(hits[1]["match_source"], "account");
        assert_eq!(
            timeline_hits(&app, "posting_handle").await[0]["id"],
            json!(post)
        );
        assert_eq!(
            timeline_hits(&app, "tagged_handle").await[0]["match_source"],
            "mention"
        );
        assert_eq!(
            timeline_hits(&app, "ordinary caption").await[0]["match_source"],
            "caption"
        );
        assert!(timeline_hits(&app, "searchXalias").await.is_empty());

        assert!(timeline_hits(&app, "2nd").await.is_empty());
        sqlx::query("INSERT INTO crm.image_ocr_readings(media_id,status,text,generated_at) VALUES($1,'complete',$2,now())")
            .bind(image).bind(format!("{} 2nd floor 票价", "intro ".repeat(3500))).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.media_readings(entity_id,entity_type,status,items) VALUES($1,'social_story','complete',$2)")
            .bind(story).bind(json!([{"text":"2nd entrance"}])).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.media_readings(entity_id,entity_type,status,items) VALUES($1,'social_post','complete',$2)")
            .bind(post).bind(json!([{"media_id":image,"text":"2nd stale text"}])).execute(&pool).await.unwrap();
        let hits = timeline_hits(&app, "2nd").await;
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0]["summary"], "2nd entrance");
        assert_eq!(hits[1]["match_source"], "ocr");
        assert!(hits[1]["summary"].as_str().unwrap().contains("2nd floor"));
        assert_eq!(timeline_hits(&app, "票价").await[0]["id"], json!(post));
        sqlx::query("UPDATE crm.image_ocr_readings SET text='Corrected label' WHERE media_id=$1")
            .bind(image)
            .execute(&pool)
            .await
            .unwrap();
        let hits = timeline_hits(&app, "2nd").await;
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0]["id"], json!(story));

        let wechat = Uuid::new_v4();
        let wechat_post = Uuid::new_v4();
        let wechat_mention = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','WeChat author')")
            .bind(wechat).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES(gen_random_uuid(),$1,'wechat','wxid_search','active')")
            .bind(wechat).execute(&pool).await.unwrap();
        sqlx::query("WITH alias AS (
             INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation)
             VALUES(gen_random_uuid(),$1,'wechat.alias','\"WechatAlias\"','alias-hash','wechat','observe') RETURNING id
            ) INSERT INTO crm.field_current(profile_id,field_key,revision_id) SELECT $1,'wechat.alias',id FROM alias")
            .bind(wechat).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,content,content_hash,version)
            VALUES($1,'wechat',$1,$2,'post','wx-post','{\"text\":\"WeChat caption\"}','wx-post-hash',1),
                  ($3,'wechat',$3,NULL,'post','wx-mention','{\"text\":\"WeChat mention\"}','wx-mention-hash',1)")
            .bind(wechat_post).bind(wechat).bind(wechat_mention).execute(&pool).await.unwrap();
        let before_mention = timeline_hits(&app, "wechatAlias").await;
        assert_eq!(before_mention.len(), 1, "{before_mention:?}");
        assert_eq!(before_mention[0]["id"], json!(wechat_post));
        sqlx::query("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,content,metadata)
            VALUES(gen_random_uuid(),$1,'wechat','wx-mentioned','mention','{}','{\"target_external_id\":\"wxid_search\"}')")
            .bind(wechat_mention).execute(&pool).await.unwrap();
        for term in ["wechatAlias", "wxid_search"] {
            let hits = timeline_hits(&app, term).await;
            assert_eq!(hits.len(), 2, "{hits:?}");
            assert!(
                hits.iter()
                    .any(|hit| hit["id"] == json!(wechat_post) && hit["match_source"] == "account")
            );
            assert!(
                hits.iter()
                    .any(|hit| hit["id"] == json!(wechat_mention)
                        && hit["match_source"] == "mention")
            );
        }
        sqlx::query("INSERT INTO crm.media_links(media_id,entity_type,entity_id,role) VALUES($1,'social_post',$2,'media-1')")
            .bind(image).bind(wechat_post).execute(&pool).await.unwrap();
        assert_eq!(
            timeline_hits(&app, "Corrected label")
                .await
                .iter()
                .filter(|hit| hit["provider"] == "wechat" && hit["match_source"] == "ocr")
                .count(),
            1
        );
    }
}

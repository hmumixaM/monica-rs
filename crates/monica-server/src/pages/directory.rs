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
 SELECT c.profile_id,jsonb_object_agg(c.field_key,r.value) value
 FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id JOIN visible v ON v.id=c.profile_id
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

async fn directory(State(s): State<AppState>, h: HeaderMap) -> Result<Response> {
    let mut tx = snapshot(&s, &h).await?;
    let rows = rows(&mut tx).await?;
    let tags: Vec<Value> =
        sqlx::query_scalar("SELECT jsonb_build_array(id,name) FROM crm.tags ORDER BY name,id")
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    cached(&s, &h, "directory".into(), build_directory(rows, tags)).await
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

async fn posts(State(s): State<AppState>, h: HeaderMap) -> Result<Response> {
    let mut tx = snapshot(&s, &h).await?;
    let posts:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array(id,profile_id,provider,occurred_at,left(text,200)) FROM crm.page_posts WHERE kind='post' AND btrim(text)<>'' ORDER BY occurred_at DESC NULLS LAST,id").fetch_all(&mut *tx).await?;
    tx.commit().await?;
    cached(
        &s,
        &h,
        "directory-posts".into(),
        json!({"columns":["id","profile_id","provider","occurred_at","text"],"posts":posts}),
    )
    .await
}
#[derive(Deserialize)]
struct Search {
    q: Option<String>,
    limit: Option<i64>,
}
async fn search(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Search>,
) -> Result<Json<Value>> {
    let mut tx = snapshot(&s, &h).await?;
    let needle = q.q.unwrap_or_default().trim().to_owned();
    if needle.chars().count() < 2 {
        return Ok(Json(json!([])));
    }
    let escaped = needle
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let like = format!("%{escaped}%");
    let rows:Vec<Value>=sqlx::query_scalar(r#"SELECT to_jsonb(hits) FROM (
 (SELECT p.id,'profile' entity_type,p.display_name title,coalesce(p.summary,'') summary,NULL::timestamptz occurred_at,p.id profile_id,p.display_name profile_name,p.profile_type kind,p.avatar_media_id,NULL::text provider FROM crm.directory_profiles p WHERE p.archived_at IS NULL AND(p.display_name ILIKE $1 OR p.summary ILIKE $1) ORDER BY p.display_name ILIKE $2 DESC,length(p.display_name),p.display_name LIMIT $3)
 UNION ALL (SELECT t.id,'tag',t.name,'',NULL,NULL,NULL,NULL,NULL,NULL FROM crm.tags t WHERE t.name ILIKE $1 ORDER BY length(t.name),t.name LIMIT $3)
 UNION ALL (SELECT x.id,'timeline',coalesce(p.display_name,x.provider),left(x.text,240),x.occurred_at,x.profile_id,p.display_name,x.kind,p.avatar_media_id,x.provider FROM crm.page_posts x LEFT JOIN crm.directory_profiles p ON p.id=x.profile_id WHERE x.text ILIKE $1 ORDER BY x.occurred_at DESC NULLS LAST,x.id LIMIT $3)
 UNION ALL (SELECT a.id,'activity',a.title,coalesce(a.body,''),a.occurred_at,p.id,p.display_name,a.activity_type,NULL,a.source_type FROM crm.activities a LEFT JOIN LATERAL(SELECT p.id,p.display_name FROM crm.activity_participants ap JOIN crm.directory_profiles p ON p.id=ap.profile_id WHERE ap.activity_id=a.id ORDER BY p.display_name,p.id LIMIT 1) p ON true WHERE a.activity_type NOT IN('wechat_direct_summary','wechat_group_summary') AND(a.title ILIKE $1 OR a.body ILIKE $1) ORDER BY a.occurred_at DESC,a.id LIMIT $3)
 ) hits"#).bind(like).bind(format!("{escaped}%")).bind(q.limit.unwrap_or(30).clamp(1,100)).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!(rows)))
}

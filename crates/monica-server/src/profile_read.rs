//! Read current indexed facts in one snapshot; never scan revision history for a page.
use super::{Result, missing};
use crate::auth::{AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/profiles/{id}", get(detail))
        .route(
            "/api/v1/profiles/{id}/structured-history",
            get(structured_history),
        )
}

pub(super) async fn header(tx: &mut Transaction<'_, Postgres>, requested: Uuid) -> Result<Value> {
    sqlx::query_scalar("WITH RECURSIVE chain(id,depth,seen) AS (SELECT $1::uuid,0,ARRAY[$1::uuid] UNION ALL SELECT r.new_profile_id,c.depth+1,c.seen||r.new_profile_id FROM crm.profile_redirects r JOIN chain c ON r.old_profile_id=c.id WHERE c.depth<32 AND NOT r.new_profile_id=ANY(c.seen)) SELECT to_jsonb(p)-'generation' FROM chain c JOIN crm.profiles p ON p.id=c.id WHERE p.archived_at IS NULL AND NOT EXISTS(SELECT 1 FROM crm.profile_redirects r WHERE r.old_profile_id=p.id)")
        .bind(requested).fetch_optional(&mut **tx).await?.ok_or_else(missing)
}

pub(super) async fn load(tx: &mut Transaction<'_, Postgres>, requested: Uuid) -> Result<Value> {
    let mut profile = header(tx, requested).await?;
    let id =
        Uuid::parse_str(profile["id"].as_str().expect("database UUID")).expect("database UUID");
    let fields:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('field_key',c.field_key,'current_content_hash',r.content_hash,'current_source_type',r.source_type,'updated_at',r.observed_at,'value',r.value) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key !~ '(snapshot|raw_payload|direct_stats|group_stats|moments_summary|location_history)$' ORDER BY c.field_key")
        .bind(id).fetch_all(&mut **tx).await?;
    let facts:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('fact_type',f.fact_type,'current_content_hash',f.current_content_hash,'current_source_type',f.current_source_type,'updated_at',f.updated_at,'value',f.value,'company_name',f.company_name,'school_name',f.school_name) FROM crm.profile_structured_current f WHERE profile_id=$1 ORDER BY fact_type,updated_at DESC,revision_id,position")
        .bind(id).fetch_all(&mut **tx).await?;
    let identities:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(i)-'profile_id'-'source_account_id' FROM crm.identities i WHERE profile_id=$1 AND status NOT IN ('invalid','merged') ORDER BY provider,username NULLS LAST,id")
        .bind(id).fetch_all(&mut **tx).await?;
    let tags:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',t.id,'name',t.name,'source_type',m.source_type) FROM crm.presented_tag_memberships m JOIN crm.tags t ON t.id=m.tag_id WHERE m.profile_id=$1 ORDER BY t.name")
        .bind(id).fetch_all(&mut **tx).await?;
    let dates:Vec<Value>=sqlx::query_scalar("SELECT value FROM crm.profile_dates_presented WHERE profile_id=$1 ORDER BY kind='birthday' DESC,month NULLS LAST,day NULLS LAST,created_at")
        .bind(id).fetch_all(&mut **tx).await?;
    let members:Vec<Value>=sqlx::query_scalar("SELECT (to_jsonb(m)-'source_type')||jsonb_build_object('display_name',p.display_name,'profile_type',p.profile_type,'avatar_media_id',p.avatar_media_id) FROM crm.current_group_memberships m JOIN crm.profiles p ON p.id=m.person_profile_id AND p.profile_type='person' AND p.archived_at IS NULL JOIN crm.profiles g ON g.id=m.group_profile_id AND g.profile_type='group' WHERE m.group_profile_id=$1 AND m.left_at IS NULL ORDER BY CASE WHEN m.role IN ('owner','群主') THEN 0 ELSE 1 END,p.display_name,p.id LIMIT 16")
        .bind(id).fetch_all(&mut **tx).await?;
    let member_count:i64=sqlx::query_scalar("SELECT count(*) FROM crm.current_group_memberships m JOIN crm.profiles p ON p.id=m.person_profile_id AND p.profile_type='person' AND p.archived_at IS NULL JOIN crm.profiles g ON g.id=m.group_profile_id AND g.profile_type='group' WHERE m.group_profile_id=$1 AND m.left_at IS NULL")
        .bind(id).fetch_one(&mut **tx).await?;
    let groups:Vec<Value>=sqlx::query_scalar("SELECT (to_jsonb(m)-'source_type')||jsonb_build_object('display_name',p.display_name,'profile_type',p.profile_type,'avatar_media_id',p.avatar_media_id) FROM crm.current_group_memberships m JOIN crm.profiles p ON p.id=m.group_profile_id AND p.profile_type='group' AND p.archived_at IS NULL JOIN crm.profiles person ON person.id=m.person_profile_id AND person.profile_type='person' WHERE m.person_profile_id=$1 AND m.left_at IS NULL ORDER BY p.display_name,p.id")
        .bind(id).fetch_all(&mut **tx).await?;
    let media:Vec<Value>=sqlx::query_scalar("SELECT DISTINCT ON(l.role,l.source_account_id) jsonb_build_object('media_id',l.media_id,'role',l.role,'source_account_id',l.source_account_id,'source_record_id',a.source_record_id,'created_at',a.created_at) FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id WHERE entity_type='profile' AND entity_id=$1 AND role IN ('cover','linkedin_cover','linkedin_avatar','wechat_avatar','instagram_avatar') ORDER BY l.role,l.source_account_id,a.created_at DESC,l.media_id")
        .bind(id).fetch_all(&mut **tx).await?;
    for (role, key) in [
        ("cover", "cover_media_id"),
        ("linkedin_cover", "linkedin_cover_media_id"),
        ("linkedin_avatar", "linkedin_avatar_media_id"),
    ] {
        profile[key] = media
            .iter()
            .filter(|m| m["role"] == role)
            .max_by_key(|m| m["created_at"].as_str())
            .map(|m| m["media_id"].clone())
            .unwrap_or(Value::Null);
    }
    profile["platform_avatars"] = json!(
        media
            .iter()
            .filter_map(|m| {
                m["role"]
                    .as_str()?
                    .strip_suffix("_avatar")
                    .map(|provider| json!({"provider":provider,"media_id":m["media_id"],"source_account_id":m["source_account_id"]}))
            })
            .collect::<Vec<_>>()
    );
    profile["avatar_url"] = fields
        .iter()
        .find(|f| {
            matches!(
                f["field_key"].as_str(),
                Some("wechat.avatar_url" | "linkedin.avatar_url" | "instagram.profile_pic_url")
            ) && f["value"].as_str().is_some_and(|s| !s.is_empty())
        })
        .map(|f| f["value"].clone())
        .unwrap_or(Value::Null);
    profile["cover_url"] = media
        .iter()
        .filter(|m| m["role"] == "cover")
        .max_by_key(|m| m["created_at"].as_str())
        .and_then(|m| m["source_record_id"].as_str())
        .filter(|s| s.starts_with("http"))
        .map(|s| json!(s))
        .unwrap_or(Value::Null);
    profile["source_observations"] = json!([]);
    profile["group_summary"] = Value::Null;
    if profile["profile_type"] == "group" {
        let stats: Option<Value> = sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key='wechat.group_stats' AND jsonb_typeof(r.value)='object'").bind(id).fetch_optional(&mut **tx).await?;
        if let Some(stats) = stats {
            let integer = |v: &Value| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|v| v.parse::<i64>().ok()))
                    .unwrap_or(0)
            };
            profile["group_summary"] = json!({"messages":integer(&stats["n"]),"active_senders":integer(&stats["active_senders"]),"first":monica_core::source_time::iso(&stats["first"]),"last":monica_core::source_time::iso(&stats["last"])});
        }
    }
    profile["presentation"] = presentation(&fields, &facts);
    profile["fields"] = json!(fields);
    profile["identities"] = json!(monica_core::identity::present_identities(identities));
    profile["structured_facts"] = json!(facts);
    profile["tags"] = json!(tags);
    profile["birthday"] = dates
        .iter()
        .find(|d| d["kind"] == "birthday")
        .cloned()
        .unwrap_or(Value::Null);
    profile["significant_dates"] = json!(dates);
    profile["group_owners"] = json!(
        members
            .iter()
            .filter(|m| matches!(m["role"].as_str(), Some("owner" | "群主")))
            .collect::<Vec<_>>()
    );
    profile["group_members"] = json!(members);
    profile["group_member_count"] = json!(member_count);
    profile["linked_group_count"] = json!(groups.len());
    profile["member_of_groups"] = json!(groups);
    Ok(profile)
}

async fn detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let value = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(value))
}

fn clean(value: &Value) -> Value {
    if let Some(s) = value.as_str() {
        return json!(s.replace("\\r\\n", "\n").replace("\\n", "\n").trim());
    }
    if let Some(items) = value.as_array()
        && items.iter().all(Value::is_string)
    {
        let mut lines = Vec::new();
        for item in items {
            let text = clean(item);
            if !lines.contains(&text) {
                lines.push(text);
            }
        }
        return json!(
            lines
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    value.clone()
}

fn presentation(fields: &[Value], facts: &[Value]) -> Value {
    let mut values = serde_json::Map::new();
    if let Some(custom) = fields
        .iter()
        .find(|f| f["field_key"] == "monica.custom_fields")
        .and_then(|f| f["value"].as_object())
    {
        let aliases = [
            ("LinkedIn", "linkedin.profile_url"),
            ("LinkedIn About", "linkedin.about"),
            ("职业简介", "linkedin.headline"),
            ("LinkedIn Location", "linkedin.location"),
            ("LinkedIn 连接日期", "linkedin.connected_on"),
            ("LinkedIn 共同好友数", "linkedin.mutual_count"),
            ("Instagram", "instagram.profile_url"),
            ("Instagram Bio", "instagram.biography"),
            ("Instagram 简介", "instagram.biography"),
            ("Instagram 粉丝数", "instagram.followers_count"),
            ("Instagram 关注数", "instagram.followees_count"),
            ("Instagram 帖子数", "instagram.media_count"),
            ("微信号", "wechat.alias"),
            ("微信备注", "wechat.remark"),
            ("微信昵称", "wechat.nickname"),
            ("微信群内显示名", "wechat.group_nicknames"),
            ("微信首次群内出现", "wechat.first_group_seen"),
            ("微信最近群内出现", "wechat.last_group_seen"),
            ("微信个性签名", "wechat.signature"),
            ("微信好友来源", "wechat.add_source"),
            ("微信资料地区", "wechat.region"),
        ];
        for (label, value) in custom {
            if let Some((_, key)) = aliases.iter().find(|(candidate, _)| *candidate == label) {
                values.insert((*key).into(), clean(value));
            }
        }
    }
    for f in fields {
        if let Some(key) = f["field_key"].as_str()
            && key != "monica.custom_fields"
        {
            values.insert(key.into(), clean(&f["value"]));
        }
    }
    let mut accounts = Vec::<(String, serde_json::Map<String, Value>)>::new();
    for (key, value) in &values {
        if let Some(rest) = key.strip_prefix("instagram.accounts.")
            && let Some((account, field)) = rest.split_once('.')
        {
            let index = accounts
                .iter()
                .position(|(name, _)| name == account)
                .unwrap_or_else(|| {
                    accounts.push((account.into(), serde_json::Map::new()));
                    accounts.len() - 1
                });
            let item = &mut accounts[index].1;
            item.insert("account_id".into(), json!(account));
            item.insert(field.into(), value.clone());
        }
    }
    let mut sources = json!({"wechat":{},"linkedin":{},"instagram":{}});
    for provider in ["wechat", "linkedin", "instagram"] {
        for (key, value) in &values {
            if let Some(key) = key.strip_prefix(&format!("{provider}."))
                && !value.is_null()
                && value != ""
                && value != &json!([])
                && ![
                    "snapshot",
                    "stats",
                    "history",
                    "summary",
                    "partial",
                    "hash",
                    "raw",
                    "avatar",
                    "pic_url",
                    "sync",
                    "cover",
                    "mutuals",
                    "accounts.",
                ]
                .iter()
                .any(|word| key.contains(word))
            {
                sources[provider][key] = value.clone();
            }
        }
    }
    let mut ordered = accounts.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        a.get("username")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("username").and_then(Value::as_str).unwrap_or(""))
    });
    if let Some(first) = ordered.first() {
        sources["instagram"]
            .as_object_mut()
            .unwrap()
            .extend(first.clone());
    }
    let mut unique = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for fact in facts {
        let meaningful = if let Some(object) = fact["value"].as_object() {
            let mut selected = serde_json::Map::new();
            for key in [
                "title",
                "position_title",
                "degree",
                "name",
                "institution_name",
                "school_name",
                "start_date",
                "end_date",
                "start",
                "end",
                "location",
                "address",
                "description",
            ] {
                if let Some(value) = object.get(key).filter(|v| {
                    !v.is_null()
                        && *v != &json!("")
                        && *v != &json!(false)
                        && *v != &json!(0)
                        && *v != &json!([])
                        && *v != &json!({})
                }) {
                    selected.insert(key.to_owned(), value.clone());
                }
            }
            if selected.is_empty() {
                fact["value"].clone()
            } else {
                Value::Object(selected)
            }
        } else {
            clean(&fact["value"])
        };
        let signature = monica_core::identity::canonical_json(&json!([
            fact["fact_type"],
            fact["current_source_type"],
            fact["company_name"],
            fact["school_name"],
            meaningful
        ]));
        if seen.insert(signature) {
            unique.push(fact.clone());
        }
    }
    unique.sort_by(|a, b| {
        b["value"]["start_date"]
            .as_str()
            .unwrap_or("")
            .cmp(a["value"]["start_date"].as_str().unwrap_or(""))
    });
    json!({"sources":sources,"facts":unique,"instagram_accounts":accounts.iter().map(|(_,v)| v).collect::<Vec<_>>()})
}

#[derive(Deserialize)]
struct History {
    fact_type: Option<String>,
    limit: Option<i64>,
}
async fn structured_history(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<History>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if q.fact_type
        .as_ref()
        .is_some_and(|kind| !["address", "employment", "education"].contains(&kind.as_str()))
    {
        return Err(super::invalid("unsupported structured fact type"));
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',r.id,'profile_id',r.profile_id,'source_account_id',r.source_account_id,'fact_type',r.fact_type,'content_hash',r.content_hash,'source_type',r.source_type,'source_record_id',r.source_record_id,'observed_at',r.observed_at,'is_conflict',r.is_conflict,'value',r.value,'entity_name',r.entity_name) FROM crm.structured_revisions r WHERE r.profile_id=$1 AND ($2::text IS NULL OR r.fact_type=$2) ORDER BY observed_at DESC,id DESC LIMIT $3")
        .bind(id).bind(q.fact_type).bind(q.limit.unwrap_or(100).clamp(1,200)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn actual_python_presentation() {
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/contracts/profile-presentation.json"
        ))
        .unwrap();
        for case in cases.as_array().unwrap() {
            assert_eq!(
                super::presentation(
                    case["fields"].as_array().unwrap(),
                    case["facts"].as_array().unwrap()
                ),
                case["expected"],
                "{case}"
            );
        }
    }
}

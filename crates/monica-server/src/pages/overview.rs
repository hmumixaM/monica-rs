use super::*;
use axum::{
    extract::{Path, State},
    routing::get,
};
use monica_core::pages::field;
use serde_json::json;
pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/profiles/{id}/overview", get(overview))
}
async fn overview(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response> {
    let mut tx = snapshot(&s, &h).await?;
    let id = canonical(&mut tx, id).await?;
    let mut header:Value=sqlx::query_scalar("SELECT jsonb_build_object('id',p.id,'type',p.profile_type,'name',p.display_name,'summary',p.summary,'avatar_media_id',coalesce(p.avatar_media_id,(SELECT l.media_id FROM crm.media_links l WHERE l.entity_type='profile' AND l.entity_id=p.id AND l.role IN('wechat_avatar','instagram_avatar','linkedin_avatar') ORDER BY l.created_at DESC,l.media_id LIMIT 1)),'birthday',(SELECT value FROM crm.profile_dates_presented d WHERE d.profile_id=p.id AND d.kind='birthday'),'sources',coalesce((SELECT jsonb_agg(provider ORDER BY provider) FROM(SELECT a.provider FROM crm.source_accounts a JOIN crm.source_account_links l ON l.account_id=a.id WHERE l.profile_id=p.id UNION SELECT provider FROM crm.identities i WHERE i.profile_id=p.id AND i.status NOT IN('invalid','merged')) s),'[]')) FROM crm.profiles p WHERE p.id=$1").bind(id).fetch_one(&mut *tx).await?;
    let fields:Value=sqlx::query_scalar("SELECT coalesce(jsonb_object_agg(c.field_key,r.value),'{}') FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key IN('wechat.remark','linkedin.headline','monica.company','monica.job','monica.residence','wechat.city','linkedin.location','wechat.province')").bind(id).fetch_one(&mut *tx).await?;
    header["note"] = field(&fields, &["wechat.remark"]);
    header["city"] = monica_core::pages::city(&fields);
    let avatars:Vec<Value>=sqlx::query_scalar("SELECT DISTINCT ON(role) jsonb_build_object('provider',replace(role,'_avatar',''),'media_id',media_id) FROM crm.media_links WHERE entity_type='profile' AND entity_id=$1 AND role IN('wechat_avatar','instagram_avatar','linkedin_avatar') ORDER BY role,created_at DESC,media_id").bind(id).fetch_all(&mut *tx).await?;
    header["avatars"] = json!(avatars);
    let facts:Vec<Value>=sqlx::query_scalar("SELECT (CASE WHEN jsonb_typeof(value)='object' THEN value ELSE '{}'::jsonb END)||jsonb_build_object('type',fact_type,'company',coalesce(company_name,value->>'company_name',value->>'company'),'school',coalesce(school_name,value->>'school_name'),'company_id',company_id,'school_id',school_id,'source',current_source_type) FROM crm.profile_structured_current WHERE profile_id=$1 AND fact_type IN('employment','education') ORDER BY fact_type,fact_key").bind(id).fetch_all(&mut *tx).await?;
    let facts = monica_core::pages::ordered_experience(
        facts
            .into_iter()
            .map(monica_core::pages::presentation_experience)
            .collect(),
    );
    header["headline"] = if header["type"] == "person" {
        monica_core::pages::headline(&fields, &facts)
    } else {
        Value::Null
    };
    if let Some(job) = monica_core::pages::current_job(&facts) {
        header["company"] = job["company"].clone();
        header["title"] = job["title"].clone();
    }
    let linkedin: Vec<Value> = facts
        .into_iter()
        .filter(|f| f["source"] == "linkedin")
        .map(|mut f| {
            f.as_object_mut().unwrap().remove("source");
            f
        })
        .collect();
    let tags:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',t.id,'name',t.name) FROM crm.tag_memberships m JOIN crm.tags t ON t.id=m.tag_id WHERE m.profile_id=$1 ORDER BY t.name").bind(id).fetch_all(&mut *tx).await?;
    let mut result = json!({"header":monica_core::pages::compact(header),"tags":tags,"linkedin_experience":linkedin});
    if result["header"]["type"] == "person" {
        let events = crate::profiles::summary::contact_events(&mut tx, id).await?;
        result["contact_events"] = monica_core::pages::compact(json!({
            "calls":if events["calls"]["total"].as_i64().unwrap_or(0)>0 {events["calls"].clone()} else {Value::Null},
            "money":if events["money"]["total"].as_i64().unwrap_or(0)>0 {events["money"].clone()} else {Value::Null}
        }));
        if let Some(mut messages)=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('total',direct_message_count,'group_total',group_message_count,'first_contact_at',first_contact_at,'last_contact_at',last_contact_at,'last_group_at',last_group_at,'sent',coalesce((SELECT sum(sent) FROM crm.page_profile_months WHERE profile_id=$1),0),'received',coalesce((SELECT sum(received) FROM crm.page_profile_months WHERE profile_id=$1),0)) FROM crm.page_contact_stats WHERE profile_id=$1").bind(id).fetch_optional(&mut *tx).await?{
   let months:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('month',to_char(m,'YYYY-MM'),'sent',coalesce(c.sent,0),'received',coalesce(c.received,0)) FROM generate_series(date_trunc('month',now() AT TIME ZONE 'America/Chicago')-interval '23 months',date_trunc('month',now() AT TIME ZONE 'America/Chicago'),interval '1 month') m LEFT JOIN crm.page_profile_months c ON c.profile_id=$1 AND c.month=to_char(m,'YYYY-MM') ORDER BY m").bind(id).fetch_all(&mut *tx).await?;messages["months"]=json!(months);result["messages"]=messages;
  }
        let relationships:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'relationship_type',e.relationship_type,'direction',CASE WHEN count(DISTINCT e.from_profile_id)>1 THEN 'both' WHEN bool_or(e.from_profile_id=$1) THEN 'outgoing' ELSE 'incoming' END) FROM crm.relationship_edges e JOIN crm.profiles p ON p.id=CASE WHEN e.from_profile_id=$1 THEN e.to_profile_id ELSE e.from_profile_id END AND p.archived_at IS NULL WHERE(e.from_profile_id=$1 OR e.to_profile_id=$1) AND(e.valid_to IS NULL OR e.valid_to>now()) GROUP BY p.id,e.relationship_type ORDER BY p.display_name,e.relationship_type LIMIT 8").bind(id).fetch_all(&mut *tx).await?;
        result["relationships"] = json!(
            relationships
                .into_iter()
                .map(monica_core::pages::compact)
                .collect::<Vec<_>>()
        );
        let groups:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'role',m.role,'message_count',c.message_count,'last_message_at',c.last_message_at) FROM crm.current_group_memberships m JOIN crm.profiles p ON p.id=m.group_profile_id AND p.archived_at IS NULL AND p.profile_type='group' LEFT JOIN LATERAL(SELECT sum(message_count) message_count,max(last_message_at) last_message_at FROM crm.conversations WHERE profile_id=p.id AND conversation_type='group') c ON true WHERE m.person_profile_id=$1 AND m.left_at IS NULL ORDER BY c.last_message_at DESC NULLS LAST,c.message_count DESC NULLS LAST,p.display_name LIMIT 8").bind(id).fetch_all(&mut *tx).await?;
        result["groups"] = json!(
            groups
                .into_iter()
                .map(monica_core::pages::compact)
                .collect::<Vec<_>>()
        );
    }
    let stats:Value=sqlx::query_scalar("SELECT coalesce(jsonb_object_agg(provider,value),'{}') FROM(SELECT provider,jsonb_build_object('posts',count(*),'last_posted_at',max(occurred_at)) value FROM crm.page_posts WHERE profile_id=$1 AND kind='post' GROUP BY provider) s").bind(id).fetch_one(&mut *tx).await?;
    result["post_stats"] = stats;
    let posts:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',p.id,'provider',p.provider,'occurred_at',p.occurred_at,'text',p.text,'media_count',p.media_count,'thumbnail_media_id',cover.id,'width',cover.width,'height',cover.height) FROM crm.page_posts p LEFT JOIN LATERAL(SELECT a.id,a.width,a.height FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id WHERE l.entity_id=p.id AND l.entity_type IN('post','social_post') AND a.media_type LIKE 'image/%' ORDER BY l.created_at,l.media_id LIMIT 1) cover ON true WHERE p.profile_id=$1 AND p.kind='post' ORDER BY p.occurred_at DESC NULLS LAST,p.id DESC LIMIT 9").bind(id).fetch_all(&mut *tx).await?;
    result["recent_posts"] = json!(
        posts
            .into_iter()
            .map(|mut post| {
                post["text"] = monica_core::pages::excerpt(&post["text"], 120);
                monica_core::pages::compact(post)
            })
            .collect::<Vec<_>>()
    );
    let locations = super::maps::build(&mut tx, Some(id), &super::maps::Filter::default()).await?;
    result["places"]=json!(locations["points"].as_array().unwrap().iter().take(5).map(|p|json!({"label":p["label"],"latitude":p["latitude"],"longitude":p["longitude"],"count":p["count"]})).collect::<Vec<_>>());
    tx.commit().await?;
    cached(
        &s,
        &h,
        format!("overview:{id}"),
        monica_core::pages::compact(result),
    )
    .await
}

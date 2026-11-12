use super::Result;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
pub async fn read(pool: &PgPool, profile: Option<Uuid>) -> Result<Value> {
    let mut providers = json!({});
    for provider in ["wechat", "instagram", "linkedin"] {
        providers[provider] = json!({"posts":0,"likes":0,"comments":0,"authors":0,"local_photo_posts":0,"people":0,"monthly":[],"leaders":[],"charts":[]});
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('provider',provider,'posts',count(*),'authors',count(DISTINCT profile_id),'likes',coalesce(sum(CASE WHEN jsonb_typeof(content->'like_count')='number' THEN (content->>'like_count')::bigint ELSE 0 END),0),'comments',coalesce(sum(CASE WHEN jsonb_typeof(content->'comment_count')='number' THEN (content->>'comment_count')::bigint ELSE 0 END),0),'first',min(occurred_at),'latest',max(occurred_at)) FROM crm.timeline_events WHERE event_type='post' AND ($1::uuid IS NULL OR profile_id=$1) GROUP BY provider").bind(profile).fetch_all(pool).await?;
    for row in rows {
        let provider = row["provider"].as_str().unwrap();
        providers[provider]
            .as_object_mut()
            .unwrap()
            .extend(row.as_object().unwrap().clone());
    }
    for (sql, key) in [
        (
            "SELECT p.provider,count(*) n FROM crm.timeline_events p WHERE p.event_type='post' AND ($1::uuid IS NULL OR p.profile_id=$1) AND EXISTS(SELECT 1 FROM crm.media_links l WHERE l.entity_type='social_post' AND l.entity_id=p.id) GROUP BY p.provider",
            "local_photo_posts",
        ),
        (
            "SELECT i.provider,count(DISTINCT i.profile_id) n FROM crm.identities i JOIN crm.profiles p ON p.id=i.profile_id WHERE i.status NOT IN ('invalid','merged') AND p.profile_type='person' AND p.archived_at IS NULL AND ($1::uuid IS NULL OR p.id=$1) GROUP BY i.provider",
            "people",
        ),
    ] {
        let rows: Vec<(String, i64)> = sqlx::query_as(sql).bind(profile).fetch_all(pool).await?;
        for (provider, n) in rows {
            if let Some(p) = providers.get_mut(&provider) {
                p[key] = json!(n);
            }
        }
    }
    let monthly:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('provider',provider,'month',to_char(occurred_at AT TIME ZONE 'UTC','YYYY-MM'),'count',count(*)) FROM crm.timeline_events WHERE event_type='post' AND occurred_at IS NOT NULL AND ($1::uuid IS NULL OR profile_id=$1) GROUP BY provider,to_char(occurred_at AT TIME ZONE 'UTC','YYYY-MM') ORDER BY to_char(occurred_at AT TIME ZONE 'UTC','YYYY-MM')").bind(profile).fetch_all(pool).await?;
    for row in monthly {
        providers[row["provider"].as_str().unwrap()]["monthly"]
            .as_array_mut()
            .unwrap()
            .push(row.clone());
    }
    let leaders:Vec<Value>=sqlx::query_scalar("WITH totals AS(SELECT provider,profile_id,count(*) posts,sum(coalesce((content->>'like_count')::bigint,0)+coalesce((content->>'comment_count')::bigint,0)) value FROM crm.timeline_events WHERE event_type='post' AND profile_id IS NOT NULL AND ($1::uuid IS NULL OR profile_id=$1) GROUP BY provider,profile_id),ranked AS(SELECT t.*,p.display_name name,p.avatar_media_id,row_number() OVER(PARTITION BY t.provider ORDER BY t.value DESC,t.posts DESC,t.profile_id) ranking FROM totals t JOIN crm.profiles p ON p.id=t.profile_id WHERE p.archived_at IS NULL) SELECT to_jsonb(r) FROM ranked r WHERE ranking<=8 ORDER BY provider,ranking").bind(profile).fetch_all(pool).await?;
    for row in leaders {
        providers[row["provider"].as_str().unwrap()]["leaders"]
            .as_array_mut()
            .unwrap()
            .push(row.clone());
    }
    let archived:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'field_key',c.field_key,'data',r.value) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id JOIN crm.profiles p ON p.id=c.profile_id AND p.archived_at IS NULL WHERE c.field_key IN ('wechat.direct_stats','wechat.group_stats') AND ($1::uuid IS NULL OR p.id=$1) ORDER BY p.id,c.field_key").bind(profile).fetch_all(pool).await?;
    let mut direct = vec![];
    let mut groups = vec![];
    for row in archived {
        let v = &row["data"];
        if !v.is_object() {
            continue;
        }
        let item = json!({"profile_id":row["profile_id"],"name":row["name"],"value":v["n"].as_i64().unwrap_or(0),"sent":v.get("sent").unwrap_or(&json!(0)),"received":v.get("recv").unwrap_or(&json!(0)),"last":v["last"]});
        if row["field_key"] == "wechat.direct_stats" {
            direct.push(item);
        } else {
            groups.push(item);
        }
    }
    let mut spoken = false;
    if let Some(profile) = profile {
        let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',g.id,'name',coalesce(nullif(btrim(g.display_name),''),'未命名群'),'value',sum(m.messages)) FROM crm.conversation_sender_stats m JOIN crm.source_account_links l ON l.account_id=m.sender_account_id AND l.profile_id=$1 JOIN crm.conversations c ON c.id=m.conversation_id AND c.conversation_type='group' JOIN crm.profiles g ON g.id=c.profile_id AND g.profile_type='group' AND g.archived_at IS NULL WHERE m.messages>0 GROUP BY g.id,g.display_name ORDER BY sum(m.messages) DESC,g.display_name LIMIT 16").bind(profile).fetch_all(pool).await?;
        if !rows.is_empty() {
            groups = rows;
            spoken = true;
        }
    }
    let sum = |key: &str| {
        direct
            .iter()
            .map(|i| i[key].as_i64().unwrap_or(0))
            .sum::<i64>()
    };
    let mut leaders = direct.clone();
    leaders.sort_by_key(|v| std::cmp::Reverse(v["value"].as_i64().unwrap_or(0)));
    leaders.truncate(8);
    if !spoken {
        groups.sort_by_key(|v| std::cmp::Reverse(v["value"].as_i64().unwrap_or(0)));
        groups.truncate(8);
    }
    providers["wechat"]["chat"] = json!({"contacts":direct.iter().filter(|v|v["value"].as_i64().unwrap_or(0)>0).count(),"messages":sum("value"),"sent":sum("sent"),"received":sum("received"),"leaders":leaders,"group_leaders":groups});
    let buckets = ["无私聊", "1–99 条", "100–999 条", "1,000 条以上"];
    let bucket = |v: &Value| match v["value"].as_i64().unwrap_or(0) {
        0 => 0,
        1..=99 => 1,
        100..=999 => 2,
        _ => 3,
    };
    let mut rows = vec![];
    for (i, name) in buckets.iter().enumerate() {
        let matching = direct.iter().filter(|v| bucket(v) == i).collect::<Vec<_>>();
        if !matching.is_empty() {
            rows.push(
                json!({"name":name,"count":matching.len(),"profile_id":matching[0]["profile_id"]}),
            );
        }
    }
    providers["wechat"]["charts"] =
        json!([{"title":"私聊活跃度（归档摘要）","denominator":direct.len(),"rows":rows}]);
    if profile.is_none() {
        let groups: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM crm.profiles WHERE profile_type='group' AND archived_at IS NULL",
        )
        .fetch_one(pool)
        .await?;
        let messages: i64 = sqlx::query_scalar(
            "SELECT coalesce(sum(message_count),0)::bigint FROM crm.conversations",
        )
        .fetch_one(pool)
        .await?;
        let conversations: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.conversations")
            .fetch_one(pool)
            .await?;
        providers["wechat"]["groups"] = json!(groups);
        providers["wechat"]["indexed_messages"] = json!(messages);
        providers["wechat"]["conversations"] = json!(conversations);
        for (kind, title, filter) in [
            ("employment", "工作机构", "company"),
            ("education", "教育经历", "school"),
        ] {
            let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('name',coalesce(c.company_name,c.school_name),'count',count(DISTINCT c.profile_id),'profile_id',min(c.profile_id::text)) FROM crm.profile_structured_current c JOIN crm.profiles p ON p.id=c.profile_id AND p.archived_at IS NULL WHERE c.fact_type=$1 AND c.current_source_type='linkedin' AND coalesce(c.company_name,c.school_name) IS NOT NULL GROUP BY coalesce(c.company_name,c.school_name) ORDER BY count(DISTINCT c.profile_id) DESC,coalesce(c.company_name,c.school_name) LIMIT 6").bind(kind).fetch_all(pool).await?;
            let chart = json!({"title":title,"denominator":providers["linkedin"]["people"],"rows":rows,"filter":filter});
            providers["linkedin"]["charts"]
                .as_array_mut()
                .unwrap()
                .push(chart);
        }
        let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'count',(r.value#>>'{}')::bigint) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id JOIN crm.profiles p ON p.id=c.profile_id AND p.archived_at IS NULL WHERE c.field_key ~ '^instagram.accounts.[^.]+.followers_count$' AND jsonb_typeof(r.value)='number' ORDER BY (r.value#>>'{}')::bigint DESC LIMIT 8").fetch_all(pool).await?;
        providers["instagram"]["charts"] = json!([{"title":"粉丝数（采集时）","rows":rows,"denominator":providers["instagram"]["people"]}]);
    }
    Ok(
        json!({"providers":providers,"as_of":chrono::Utc::now().to_rfc3339(),"scope":"已收录社交动态；私聊数据来自 Chatlog 归档摘要","profile_id":profile}),
    )
}

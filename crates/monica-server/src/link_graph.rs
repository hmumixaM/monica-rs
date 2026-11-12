use super::{ApiError, AppState, Result, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/relationships/graph", get(graph))
        .route("/api/v1/profiles/{id}/moments-circle", get(circle))
}
#[derive(Default, Deserialize)]
struct Filter {
    limit: Option<i64>,
    source_type: Option<String>,
    relationship_type: Option<String>,
    min_confidence: Option<f64>,
    format: Option<String>,
    focus: Option<Uuid>,
}
async fn graph(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if q.format.as_deref() == Some("monica") {
        return Ok(Json(
            projection(&s.pool, q.limit.unwrap_or(900).clamp(50, 2000), q.focus).await?,
        ));
    }
    let edges:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',e.id,'from_profile_id',a.id,'to_profile_id',b.id,'from_display_name',a.display_name,'to_display_name',b.display_name,'from_profile_type',a.profile_type,'to_profile_type',b.profile_type,'relationship_type',e.relationship_type,'source_type',e.source_type,'confidence',e.confidence,'note',e.note,'created_at',e.created_at) FROM crm.relationship_edges e JOIN crm.profiles a ON a.id=e.from_profile_id JOIN crm.profiles b ON b.id=e.to_profile_id WHERE ($1::text IS NULL OR e.source_type=$1) AND ($2::text IS NULL OR e.relationship_type=$2) AND ($3<=0 OR coalesce(e.confidence,0)>=$3) ORDER BY coalesce(e.confidence,0) DESC,e.created_at DESC,e.id DESC LIMIT $4").bind(&q.source_type).bind(&q.relationship_type).bind(q.min_confidence.unwrap_or(0.0).min(1.0)).bind(q.limit.unwrap_or(900).clamp(50,5000)).fetch_all(&s.pool).await?;
    let mut nodes = Vec::<Value>::new();
    for edge in &edges {
        for prefix in ["from", "to"] {
            let id = &edge[format!("{prefix}_profile_id")];
            if let Some(node) = nodes.iter_mut().find(|n| n["id"] == *id) {
                node["degree"] = json!(node["degree"].as_i64().unwrap() + 1);
            } else {
                nodes.push(json!({"id":id,"display_name":edge[format!("{prefix}_display_name")],"profile_type":edge[format!("{prefix}_profile_type")],"degree":1}));
            }
        }
    }
    let total_edges:i64=sqlx::query_scalar("SELECT count(*) FROM crm.relationship_edges e WHERE ($1::text IS NULL OR e.source_type=$1) AND ($2::text IS NULL OR e.relationship_type=$2) AND ($3<=0 OR coalesce(e.confidence,0)>=$3)").bind(q.source_type).bind(q.relationship_type).bind(q.min_confidence.unwrap_or(0.0).min(1.0)).fetch_one(&s.pool).await?;
    let total_nodes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.profiles WHERE archived_at IS NULL")
            .fetch_one(&s.pool)
            .await?;
    let types:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('relationship_type',relationship_type,'edge_count',count(*)) FROM crm.relationship_edges GROUP BY relationship_type ORDER BY count(*) DESC,relationship_type").fetch_all(&s.pool).await?;
    let sources:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('source_type',source_type,'edge_count',count(*)) FROM crm.relationship_edges GROUP BY source_type ORDER BY count(*) DESC,source_type").fetch_all(&s.pool).await?;
    Ok(Json(
        json!({"nodes":nodes,"visible_edges":edges.len(),"edges":edges,"total_edges":total_edges,"total_nodes":total_nodes,"relationship_types":types,"sources":sources}),
    ))
}
#[derive(Default)]
struct Pair {
    ab: [i64; 4],
    ba: [i64; 4],
    direct: i64,
    group: i64,
    co: i64,
    last: Option<String>,
    relationships: Vec<String>,
}
fn ordered(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a < b { (a, b) } else { (b, a) }
}
async fn projection(pool: &PgPool, limit: i64, focus: Option<Uuid>) -> Result<Value> {
    let mut pairs: BTreeMap<(Uuid, Uuid), Pair> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let rows=sqlx::query(r#"WITH resolved AS (
 SELECT coalesce(s.author_profile_id,al.profile_id,ai.profile_id) a,p.profile_id owner,s.interaction_type,
 coalesce(s.content->>'target_external_id',s.metadata->>'target_external_id') target,
 coalesce(s.content->>'ref_author_external_id',s.metadata->>'ref_author_external_id') reference,s.occurred_at
 FROM crm.social_interactions s JOIN crm.timeline_events p ON p.id=s.post_id
 LEFT JOIN crm.source_account_links al ON al.account_id=s.author_account_id
 LEFT JOIN crm.identities ai ON ai.provider='wechat' AND ai.external_id=s.author_external_id AND ai.status NOT IN ('invalid','merged')
 WHERE s.provider='wechat' AND NOT s.is_deleted
 ), facts AS (
 SELECT r.a,CASE WHEN interaction_type='mention' THEN i.profile_id ELSE owner END b,
 CASE WHEN interaction_type='reply' THEN 'comment' ELSE interaction_type END kind,occurred_at
 FROM resolved r LEFT JOIN crm.identities i ON i.provider='wechat' AND i.external_id=r.target AND i.status NOT IN ('invalid','merged')
 UNION ALL SELECT r.a,i.profile_id,'reply',r.occurred_at FROM resolved r JOIN crm.identities i ON i.provider='wechat' AND i.external_id=r.reference AND i.status NOT IN ('invalid','merged') WHERE r.interaction_type='reply')
 SELECT f.a,f.b,f.kind,count(*) n,max(f.occurred_at)::date::text last FROM facts f
 JOIN crm.profiles a ON a.id=f.a AND a.archived_at IS NULL AND a.profile_type='person'
 JOIN crm.profiles b ON b.id=f.b AND b.archived_at IS NULL AND b.profile_type='person'
 WHERE f.a<>f.b AND ($1::uuid IS NULL OR f.a=$1 OR f.b=$1) GROUP BY f.a,f.b,f.kind ORDER BY count(*) DESC,f.a,f.b,f.kind LIMIT $2"#).bind(focus).bind(limit).fetch_all(pool).await?;
    for r in rows {
        let a: Uuid = r.get("a");
        let b: Uuid = r.get("b");
        ids.extend([a, b]);
        let pair = ordered(a, b);
        let e = pairs.entry(pair).or_default();
        let kind: String = r.get("kind");
        if let Some(i) = ["like", "comment", "reply", "mention"]
            .iter()
            .position(|k| *k == kind)
        {
            let n: i64 = r.get("n");
            if a == pair.0 {
                e.ab[i] += n;
            } else {
                e.ba[i] += n;
            }
            e.direct += n;
            let last: Option<String> = r.get("last");
            e.last = e.last.clone().max(last);
        }
    }
    if let Some(focus) = focus {
        ids.insert(focus);
    }
    let memberships:Vec<(Uuid,Uuid)>=sqlx::query_as("WITH small AS(SELECT group_profile_id FROM crm.current_group_memberships WHERE left_at IS NULL GROUP BY group_profile_id HAVING count(*)<=80) SELECT m.group_profile_id,m.person_profile_id FROM crm.current_group_memberships m JOIN small USING(group_profile_id) JOIN crm.profiles p ON p.id=m.person_profile_id AND p.profile_type='person' AND p.archived_at IS NULL JOIN crm.profiles g ON g.id=m.group_profile_id AND g.profile_type='group' AND g.archived_at IS NULL WHERE m.left_at IS NULL AND ($1::uuid IS NULL OR m.group_profile_id IN(SELECT group_profile_id FROM crm.current_group_memberships WHERE person_profile_id=$1 AND left_at IS NULL))").bind(focus).fetch_all(pool).await?;
    let mut by_person: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    for (g, p) in memberships {
        by_person.entry(p).or_default().insert(g);
    }
    if let Some(f) = focus {
        let fg = by_person.get(&f).cloned().unwrap_or_default();
        let mut peers = by_person
            .iter()
            .filter(|(p, _)| **p != f)
            .map(|(p, g)| (*p, fg.intersection(g).count()))
            .filter(|(_, n)| *n > 0)
            .collect::<Vec<_>>();
        peers.sort_by_key(|(p, n)| (std::cmp::Reverse(*n), *p));
        for (p, _) in peers.into_iter().take(limit as usize) {
            ids.insert(p);
            pairs.entry(ordered(f, p)).or_default();
        }
    }
    let relations:Vec<(Uuid,Uuid,String)>=sqlx::query_as("SELECT e.from_profile_id,e.to_profile_id,e.relationship_type FROM crm.relationship_edges e JOIN crm.profiles a ON a.id=e.from_profile_id AND a.archived_at IS NULL AND a.profile_type='person' JOIN crm.profiles b ON b.id=e.to_profile_id AND b.archived_at IS NULL AND b.profile_type='person' WHERE lower(e.relationship_type) NOT SIMILAR TO '%(group|member|owner|群)%' AND ($1::uuid IS NULL OR a.id=$1 OR b.id=$1) ORDER BY e.created_at DESC,e.id DESC LIMIT $2").bind(focus).bind(limit).fetch_all(pool).await?;
    for (a, b, name) in relations {
        ids.extend([a, b]);
        let e = pairs.entry(ordered(a, b)).or_default();
        e.direct += 1;
        e.relationships.push(name);
    }
    let audience:Vec<(Uuid,Uuid)>=sqlx::query_as("SELECT DISTINCT p.profile_id,coalesce(s.author_profile_id,l.profile_id,i.profile_id) FROM crm.social_interactions s JOIN crm.timeline_events p ON p.id=s.post_id LEFT JOIN crm.source_account_links l ON l.account_id=s.author_account_id LEFT JOIN crm.identities i ON i.provider='wechat' AND i.external_id=s.author_external_id AND i.status NOT IN ('invalid','merged') JOIN crm.profiles author ON author.id=coalesce(s.author_profile_id,l.profile_id,i.profile_id) AND author.archived_at IS NULL AND author.profile_type='person' WHERE s.provider='wechat' AND NOT s.is_deleted AND p.profile_id=ANY($1) AND p.profile_id<>author.id").bind(ids.iter().copied().collect::<Vec<_>>()).fetch_all(pool).await?;
    let mut audiences: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    for (owner, author) in audience {
        audiences.entry(owner).or_default().insert(author);
    }
    let mut shared_groups: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    let mut shared_audience: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    let empty = BTreeSet::new();
    for ((a, b), e) in &mut pairs {
        let groups = by_person
            .get(a)
            .unwrap_or(&empty)
            .intersection(by_person.get(b).unwrap_or(&empty))
            .copied()
            .collect::<BTreeSet<_>>();
        e.group = groups.len() as i64;
        let co = audiences
            .get(a)
            .unwrap_or(&empty)
            .intersection(audiences.get(b).unwrap_or(&empty))
            .filter(|p| **p != *a && **p != *b)
            .copied()
            .collect::<BTreeSet<_>>();
        e.co = co.len() as i64;
        for id in [a, b] {
            shared_groups.entry(*id).or_default().extend(&groups);
            shared_audience.entry(*id).or_default().extend(&co);
        }
    }
    let rows=sqlx::query("SELECT p.id,p.display_name,p.profile_type,(SELECT external_id FROM crm.identities i WHERE i.profile_id=p.id AND i.provider='wechat' AND i.status NOT IN ('invalid','merged') ORDER BY i.first_seen_at,i.id LIMIT 1) wxid FROM crm.profiles p WHERE p.id=ANY($1) AND p.archived_at IS NULL ORDER BY p.id").bind(ids.iter().copied().collect::<Vec<_>>()).fetch_all(pool).await?;
    let mut nodes = Vec::new();
    let mut index = BTreeMap::new();
    for r in rows {
        let id: Uuid = r.get("id");
        index.insert(id, nodes.len());
        nodes.push(json!({"u":r.get::<Option<String>,_>("wxid").unwrap_or(id.to_string()),"name":r.get::<String,_>("display_name"),"hash":id,"profile_id":id,"known":true,"profile_type":r.get::<String,_>("profile_type"),"x":0,"y":0,"community":0,"deg":0,"partners":0,"facts":[0,0,0,0,shared_audience.get(&id).map_or(0,BTreeSet::len),shared_groups.get(&id).map_or(0,BTreeSet::len)]}));
    }
    let mut edges = vec![];
    let mut weights = vec![vec![0.; nodes.len()]; nodes.len()];
    for ((a, b), e) in pairs {
        let (Some(&a), Some(&b)) = (index.get(&a), index.get(&b)) else {
            continue;
        };
        let mut edge = json!({"a":a,"b":b,"ab":e.ab,"ba":e.ba,"group":e.group,"direct":e.direct,"co":e.co,"w":e.direct,"combined":e.direct+e.group,"group_weight":e.group,"reciprocity":i32::from(e.ab.iter().any(|n|*n>0)&&e.ba.iter().any(|n|*n>0))});
        if let Some(last) = &e.last {
            edge["last"] = json!(last);
        }
        if !e.relationships.is_empty() {
            edge["relationship_types"] = json!(e.relationships);
        }
        for i in [a, b] {
            nodes[i]["deg"] = json!(nodes[i]["deg"].as_i64().unwrap() + 1);
            nodes[i]["partners"] = nodes[i]["deg"].clone();
            if let Some(last) = &e.last {
                nodes[i]["last"] = json!(nodes[i]["last"].as_str().unwrap_or("").max(last));
            }
            for (j, (outward, inward)) in e.ab.iter().zip(e.ba.iter()).enumerate() {
                nodes[i]["facts"][j] =
                    json!(nodes[i]["facts"][j].as_i64().unwrap() + outward + inward);
            }
        }
        weights[a][b] = (e.direct + e.group) as f64;
        weights[b][a] = weights[a][b];
        edges.push(edge);
    }
    let mut clusters = monica_core::pages::communities(&weights);
    clusters.sort_by_key(|m| (std::cmp::Reverse(m.len()), m.iter().min().copied()));
    let names: Vec<(String, String)> =
        sqlx::query_as("SELECT community_id,label FROM crm.community_names")
            .fetch_all(pool)
            .await?;
    let mut communities = vec![];
    for (community, mut members) in clusters.into_iter().enumerate() {
        let key = format!(
            "graph:{}",
            members
                .iter()
                .map(|i| nodes[*i]["profile_id"].as_str().unwrap())
                .min()
                .unwrap()
        );
        let mut c = json!({"index":community,"key":key});
        if let Some((_, label)) = names.iter().find(|(k, _)| *k == key) {
            c["label"] = json!(label);
            c["label_source"] = json!("manual");
        }
        communities.push(c);
        members.sort_by(|a, b| {
            nodes[*b]["deg"]
                .as_i64()
                .cmp(&nodes[*a]["deg"].as_i64())
                .then(nodes[*a]["name"].as_str().cmp(&nodes[*b]["name"].as_str()))
                .then(a.cmp(b))
        });
        let radius = 0.13 + 0.035 * (members.len() as f64).sqrt();
        for (rank, i) in members.iter().enumerate() {
            let distance = radius * ((rank as f64 + 0.5) / members.len() as f64).sqrt();
            let angle = rank as f64 * std::f64::consts::PI * (3. - 5_f64.sqrt());
            nodes[*i]["community"] = json!(community);
            nodes[*i]["x"] = json!(distance * angle.cos());
            nodes[*i]["y"] = json!(distance * angle.sin());
        }
    }
    Ok(
        json!({"stats":{"nodes":nodes.len(),"edges":edges.len()},"monica_mapping":{"mapped_nodes":nodes.len(),"source_nodes":nodes.len(),"match_rate":if nodes.is_empty(){0}else{1}},"nodes":nodes,"edges":edges,"version":2,"communities":communities,"generated_at":chrono::Utc::now().to_rfc3339(),"source":"resolved database interactions and confirmed relationships","scope":{"limit":limit,"focus":focus}}),
    )
}
async fn circle(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let kind: Option<String> =
        sqlx::query_scalar("SELECT profile_type FROM crm.profiles WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.pool)
            .await?;
    match kind.as_deref() {
        None => return Err(ApiError(StatusCode::NOT_FOUND, "profile not found")),
        Some("person") => {}
        _ => {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Moments belong to people, not groups",
            ));
        }
    }
    let graph = projection(&s.pool, 2000, Some(id)).await?;
    let nodes = graph["nodes"].as_array().unwrap();
    let center = nodes.iter().position(|n| n["profile_id"] == id.to_string());
    let mut neighbors = vec![];
    if let Some(ci) = center {
        for edge in graph["edges"].as_array().unwrap() {
            let a = edge["a"].as_u64().unwrap() as usize;
            let b = edge["b"].as_u64().unwrap() as usize;
            if a != ci && b != ci {
                continue;
            }
            let peer = &nodes[if a == ci { b } else { a }];
            let (out, inc) = if a == ci {
                (&edge["ab"], &edge["ba"])
            } else {
                (&edge["ba"], &edge["ab"])
            };
            let counts = (0..4)
                .map(|i| out[i].as_i64().unwrap() + inc[i].as_i64().unwrap())
                .collect::<Vec<_>>();
            let direct: i64 = counts.iter().zip([1, 3, 4, 12]).map(|(n, w)| n * w).sum();
            let group = edge["group"].as_i64().unwrap();
            if direct == 0 && group == 0 {
                continue;
            }
            let outgoing: i64 = out
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap())
                .sum();
            let incoming: i64 = inc
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap())
                .sum();
            neighbors.push(json!({"id":peer["profile_id"],"label":peer["name"],"url":format!("/profiles/{}",peer["profile_id"].as_str().unwrap()),"x":0,"y":0,"out":out,"in":inc,"direct":direct,"weight":direct+group,"group":group,"co":edge["co"],"groupOnly":direct==0,"last":edge["last"],"reciprocity":if outgoing+incoming>0{2.*outgoing.min(incoming)as f64/(outgoing+incoming)as f64}else{0.},"like":counts[0],"comment":counts[1],"reply":counts[2],"mention":counts[3]}));
        }
    }
    neighbors.sort_by(|a, b| {
        b["direct"]
            .as_i64()
            .cmp(&a["direct"].as_i64())
            .then(b["group"].as_i64().cmp(&a["group"].as_i64()))
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    for (i, n) in neighbors.iter_mut().enumerate() {
        let angle = i as f64 * std::f64::consts::PI * (3. - 5_f64.sqrt());
        n["x"] = json!(0.7 * angle.cos());
        n["y"] = json!(0.7 * angle.sin());
    }
    Ok(Json(
        json!({"center":{"id":id,"label":center.map(|i|nodes[i]["name"].clone()).unwrap_or(json!("当前联系人")),"url":format!("/profiles/{id}")},"maximumDirect":neighbors.iter().filter_map(|n|n["direct"].as_i64()).max().unwrap_or(0),"maximumWeight":neighbors.iter().filter_map(|n|n["weight"].as_i64()).max().unwrap_or(0),"neighbors":neighbors}),
    ))
}

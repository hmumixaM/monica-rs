use super::{Result, invalid, lock};
use crate::auth::{AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/groups/{id}/members", get(members).post(add_member))
        .route(
            "/api/v1/profiles/{id}/relationships",
            get(relationships).post(add_relationship),
        )
        .route("/api/v1/relationships", get(recent))
}

#[derive(Deserialize)]
struct Member {
    person_profile_id: Uuid,
    role: Option<String>,
    joined_at: Option<String>,
    left_at: Option<String>,
}
#[derive(Deserialize)]
struct Edge {
    to_profile_id: Uuid,
    relationship_type: String,
    note: Option<String>,
    valid_from: Option<String>,
    valid_to: Option<String>,
}
#[derive(Deserialize)]
struct Page {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn times(
    tx: &mut Transaction<'_, Postgres>,
    a: &Option<String>,
    b: &Option<String>,
) -> Result<()> {
    let valid:bool=sqlx::query_scalar("SELECT ($1::text IS NULL OR pg_input_is_valid($1,'timestamp with time zone')) AND ($2::text IS NULL OR pg_input_is_valid($2,'timestamp with time zone'))")
        .bind(a).bind(b).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(invalid("invalid timestamp"));
    }
    Ok(())
}

async fn pair(tx: &mut Transaction<'_, Postgres>, a: Uuid, b: Uuid) -> Result<(String, String)> {
    for id in std::collections::BTreeSet::from([a, b]) {
        lock(tx, id).await?;
    }
    Ok(sqlx::query_as("SELECT a.profile_type,b.profile_type FROM crm.profiles a CROSS JOIN crm.profiles b WHERE a.id=$1 AND b.id=$2")
        .bind(a).bind(b).fetch_one(&mut **tx).await?)
}

async fn add_member(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<Member>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    if id == p.person_profile_id {
        return Err(invalid("membership requires a group and a person"));
    }
    let types = pair(&mut tx, id, p.person_profile_id).await?;
    if types != ("group".into(), "person".into()) {
        return Err(invalid("membership requires a group and a person"));
    }
    times(&mut tx, &p.joined_at, &p.left_at).await?;
    let row:Option<Value>=sqlx::query_scalar("INSERT INTO crm.group_memberships(group_profile_id,person_profile_id,role,joined_at,left_at) VALUES($1,$2,$3,$4::timestamptz,$5::timestamptz) ON CONFLICT(group_profile_id,person_profile_id) DO UPDATE SET role=excluded.role,joined_at=excluded.joined_at,left_at=excluded.left_at,observed_at=now() WHERE (group_memberships.role,group_memberships.joined_at,group_memberships.left_at) IS DISTINCT FROM (excluded.role,excluded.joined_at,excluded.left_at) RETURNING to_jsonb(group_memberships)-'source_type'-'source_record_id'")
        .bind(id).bind(p.person_profile_id).bind(&p.role).bind(&p.joined_at).bind(&p.left_at).fetch_optional(&mut *tx).await?;
    let value = if let Some(row) = row {
        sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=ANY($1)")
            .bind(vec![id, p.person_profile_id])
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'group.membership.saved','profile',$2,$3)").bind(actor).bind(id).bind(&row).execute(&mut *tx).await?;
        row
    } else {
        sqlx::query_scalar("SELECT to_jsonb(m)-'source_type'-'source_record_id' FROM crm.group_memberships m WHERE group_profile_id=$1 AND person_profile_id=$2").bind(id).bind(p.person_profile_id).fetch_one(&mut *tx).await?
    };
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(value)))
}

async fn members(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(m)-'source_type'-'source_record_id'||jsonb_build_object('display_name',p.display_name,'profile_type',p.profile_type,'avatar_media_id',p.avatar_media_id) FROM crm.current_group_memberships m JOIN crm.profiles p ON p.id=m.person_profile_id AND p.profile_type='person' AND p.archived_at IS NULL JOIN crm.profiles g ON g.id=m.group_profile_id AND g.profile_type='group' WHERE m.group_profile_id=$1 AND m.left_at IS NULL ORDER BY CASE WHEN m.role IN ('owner','群主') THEN 0 ELSE 1 END,p.display_name,p.id LIMIT $2 OFFSET $3")
        .bind(id).bind(q.limit.unwrap_or(50).clamp(1,100)).bind(q.offset.unwrap_or(0).max(0)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}

async fn add_relationship(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<Edge>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    if !(1..=120).contains(&p.relationship_type.chars().count()) {
        return Err(invalid("invalid relationship type"));
    }
    if id == p.to_profile_id
        || ["group", "member", "owner", "群"]
            .iter()
            .any(|word| p.relationship_type.to_lowercase().contains(word))
    {
        return Err(invalid(
            "Relationships require two different people; use group membership for groups",
        ));
    }
    let mut tx = s.pool.begin().await?;
    if pair(&mut tx, id, p.to_profile_id).await? != ("person".into(), "person".into()) {
        return Err(invalid(
            "Relationships require two different people; use group membership for groups",
        ));
    }
    times(&mut tx, &p.valid_from, &p.valid_to).await?;
    let edge:Option<Uuid>=sqlx::query_scalar("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type,note,valid_from,valid_to) VALUES($1,$2,$3,$4,$5,$6::timestamptz,$7::timestamptz) ON CONFLICT(from_profile_id,to_profile_id,relationship_type) WHERE NOT legacy_source_owned AND evidence_kind='relationship' DO UPDATE SET note=excluded.note,valid_from=excluded.valid_from,valid_to=excluded.valid_to,updated_at=now() WHERE (relationship_edges.note,relationship_edges.valid_from,relationship_edges.valid_to) IS DISTINCT FROM (excluded.note,excluded.valid_from,excluded.valid_to) RETURNING id")
        .bind(Uuid::new_v4()).bind(id).bind(p.to_profile_id).bind(&p.relationship_type).bind(&p.note).bind(&p.valid_from).bind(&p.valid_to).fetch_optional(&mut *tx).await?;
    let edge = if let Some(edge) = edge {
        let payload = json!({"to_profile_id":p.to_profile_id,"relationship_type":p.relationship_type,"note":p.note,"valid_from":p.valid_from,"valid_to":p.valid_to});
        sqlx::query("INSERT INTO crm.relationship_edge_revisions(edge_id,operation,payload,actor_user_id) VALUES($1,'created',$2,$3)").bind(edge).bind(&payload).bind(actor).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'relationship.saved','relationship',$2,$3)").bind(actor).bind(edge).bind(payload).execute(&mut *tx).await?;
        sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=ANY($1)")
            .bind(vec![id, p.to_profile_id])
            .execute(&mut *tx)
            .await?;
        edge
    } else {
        sqlx::query_scalar("SELECT id FROM crm.relationship_edges WHERE from_profile_id=$1 AND to_profile_id=$2 AND relationship_type=$3 AND NOT legacy_source_owned AND evidence_kind='relationship'").bind(id).bind(p.to_profile_id).bind(p.relationship_type).fetch_one(&mut *tx).await?
    };
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":edge,"status":"created"})),
    ))
}

async fn relationships(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut rows:Vec<Value>=sqlx::query_scalar("SELECT (to_jsonb(e)-'created_at'-'updated_at')||jsonb_build_object('avatar_media_id',CASE WHEN e.from_profile_id=$1 THEN b.avatar_media_id ELSE a.avatar_media_id END,'from_display_name',a.display_name,'to_display_name',b.display_name,'direction',CASE WHEN e.from_profile_id=$1 THEN 'outgoing' ELSE 'incoming' END,'related_profile_id',CASE WHEN e.from_profile_id=$1 THEN b.id ELSE a.id END,'related_display_name',CASE WHEN e.from_profile_id=$1 THEN b.display_name ELSE a.display_name END,'related_profile_type','person') FROM crm.presented_relationship_edges e JOIN crm.profiles a ON a.id=e.from_profile_id AND a.profile_type='person' AND a.archived_at IS NULL JOIN crm.profiles b ON b.id=e.to_profile_id AND b.profile_type='person' AND b.archived_at IS NULL WHERE e.from_profile_id=$1 OR e.to_profile_id=$1 ORDER BY e.relationship_type,e.created_at DESC")
        .bind(id).fetch_all(&s.pool).await?;
    let memberships:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id','membership:'||m.group_profile_id::text||':'||m.person_profile_id::text,'from_profile_id',g.id,'to_profile_id',p.id,'relationship_type',CASE lower(m.role) WHEN 'owner' THEN '群主' WHEN 'admin' THEN '管理员' WHEN 'member' THEN '群成员' ELSE coalesce(m.role,CASE WHEN m.group_profile_id=$1 THEN '群成员' ELSE '微信群' END) END,'from_display_name',g.display_name,'to_display_name',p.display_name,'direction',CASE WHEN m.group_profile_id=$1 THEN 'outgoing' ELSE 'incoming' END,'related_profile_id',CASE WHEN m.group_profile_id=$1 THEN p.id ELSE g.id END,'related_display_name',CASE WHEN m.group_profile_id=$1 THEN p.display_name ELSE g.display_name END,'related_profile_type',CASE WHEN m.group_profile_id=$1 THEN 'person' ELSE 'group' END,'source_type',m.source_type,'note',NULL,'confidence',NULL,'valid_from',m.joined_at,'valid_to',m.left_at) FROM crm.current_group_memberships m JOIN crm.profiles g ON g.id=m.group_profile_id AND g.profile_type='group' AND g.archived_at IS NULL JOIN crm.profiles p ON p.id=m.person_profile_id AND p.profile_type='person' AND p.archived_at IS NULL WHERE (m.group_profile_id=$1 OR m.person_profile_id=$1) AND m.left_at IS NULL ORDER BY m.observed_at DESC")
        .bind(id).fetch_all(&s.pool).await?;
    rows.extend(memberships);
    Ok(Json(json!(rows)))
}

async fn recent(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',e.id,'from_profile_id',e.from_profile_id,'to_profile_id',e.to_profile_id,'from_display_name',a.display_name,'to_display_name',b.display_name,'relationship_type',e.relationship_type,'source_type',e.source_type,'confidence',e.confidence,'created_at',e.created_at) FROM crm.presented_relationship_edges e JOIN crm.profiles a ON a.id=e.from_profile_id JOIN crm.profiles b ON b.id=e.to_profile_id ORDER BY e.created_at DESC,e.id DESC LIMIT $1")
        .bind(q.limit.unwrap_or(10).clamp(1,50)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}

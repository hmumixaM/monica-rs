//! User-owned notes/tasks/reminders with transactional participants and media.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn invalid(message: &'static str) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message)
}
fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "manual record not found")
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/activities", get(list).post(create))
        .route("/api/v1/activities/{id}", put(update))
        .route("/api/v1/activities/{id}/state", patch(set_state))
}
#[derive(Deserialize)]
struct Write {
    activity_type: String,
    occurred_at: String,
    title: String,
    body: Option<String>,
    #[serde(default)]
    participant_profile_ids: Vec<Uuid>,
    due_at: Option<String>,
    media_ids: Option<Vec<Uuid>>,
}
#[derive(Deserialize)]
struct Page {
    profile_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
}
#[derive(Deserialize)]
struct StateWrite {
    state: String,
}

async fn validate(tx: &mut Transaction<'_, Postgres>, p: &mut Write) -> Result<()> {
    if !(1..=120).contains(&p.activity_type.chars().count())
        || !(1..=255).contains(&p.title.chars().count())
        || p.media_ids.as_ref().is_some_and(|v| v.len() > 50)
    {
        return Err(invalid("invalid activity"));
    }
    if p.participant_profile_ids.len() > 1000 {
        return Err(invalid("too many participants"));
    }
    p.title = p.title.replace('\0', "");
    p.body = p.body.as_ref().map(|s| s.replace('\0', ""));
    if p.title.is_empty() {
        return Err(invalid("invalid activity title"));
    }
    let valid:bool=sqlx::query_scalar("SELECT pg_input_is_valid($1,'timestamp with time zone') AND ($2::text IS NULL OR pg_input_is_valid($2,'timestamp with time zone'))")
        .bind(&p.occurred_at).bind(&p.due_at).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(invalid("invalid activity date"));
    }
    if let Some(ids) = &p.media_ids {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.media_assets WHERE id=ANY($1)")
            .bind(ids)
            .fetch_one(&mut **tx)
            .await?;
        if n as usize != ids.iter().collect::<std::collections::BTreeSet<_>>().len() {
            return Err(invalid("Select existing uploaded photos or files"));
        }
    }
    Ok(())
}

async fn profiles(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
    old: &[Uuid],
) -> Result<Vec<Uuid>> {
    let new = ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let all = new
        .iter()
        .copied()
        .chain(old.iter().copied())
        .collect::<std::collections::BTreeSet<_>>();
    for id in &all {
        let row: Option<(bool,)> =
            sqlx::query_as("SELECT archived_at IS NULL FROM crm.profiles WHERE id=$1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut **tx)
                .await?;
        if new.contains(id) && row != Some((true,)) {
            return Err(ApiError(StatusCode::NOT_FOUND, "participant not found"));
        }
    }
    Ok(all.into_iter().collect())
}

async fn media(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    ids: &[Uuid],
    actor: Uuid,
) -> Result<()> {
    sqlx::query("DELETE FROM crm.media_links WHERE entity_type='activity' AND entity_id=$1 AND role LIKE 'manual-%'").bind(id).execute(&mut **tx).await?;
    let mut seen = std::collections::BTreeSet::new();
    let mut position = 0;
    for media in ids {
        if seen.insert(media) {
            sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('activity',$1,$2,$3) ON CONFLICT DO NOTHING").bind(id).bind(media).bind(format!("manual-{position:03}")).execute(&mut **tx).await?;
            position += 1;
        }
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'activity.media.updated','activity',$2,$3)")
        .bind(actor).bind(id).bind(json!(ids)).execute(&mut **tx).await?;
    Ok(())
}

async fn snapshot(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value> {
    sqlx::query_scalar("SELECT to_jsonb(a)-'content_hash'-'created_at'-'updated_at'||jsonb_build_object('participants',coalesce((SELECT jsonb_agg(profile_id ORDER BY profile_id) FROM crm.activity_participants WHERE activity_id=a.id),'[]'::jsonb),'media',coalesce((SELECT jsonb_agg(jsonb_build_object('id',media_id,'role',role) ORDER BY role,media_id) FROM crm.media_links WHERE entity_type='activity' AND entity_id=a.id),'[]'::jsonb)) FROM crm.activities a WHERE id=$1")
        .bind(id).fetch_optional(&mut **tx).await?.ok_or_else(missing)
}

async fn record(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    actor: Uuid,
    operation: &str,
    affected: &[Uuid],
) -> Result<()> {
    let value = snapshot(tx, id).await?;
    let hash = monica_core::identity::observation_hash(&value);
    sqlx::query("UPDATE crm.activities SET content_hash=$2,updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(&hash)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO crm.activity_revisions(activity_id,operation,payload,content_hash,actor_user_id) VALUES($1,$2,$3,$4,$5)").bind(id).bind(operation).bind(&value).bind(hash).bind(actor).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,$2,'activity',$3,$4)").bind(actor).bind(format!("activity.{operation}")).bind(id).bind(value).execute(&mut **tx).await?;
    sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=ANY($1)")
        .bind(affected)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn save(s: &AppState, id: Option<Uuid>, actor: Uuid, mut p: Write) -> Result<Uuid> {
    let mut tx = s.pool.begin().await?;
    validate(&mut tx, &mut p).await?;
    let old = if let Some(id) = id {
        sqlx::query(
            "SELECT id FROM crm.activities WHERE id=$1 AND source_type='manual' FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(missing)?;
        Some(snapshot(&mut tx, id).await?)
    } else {
        None
    };
    let old_ids = old
        .as_ref()
        .and_then(|v| v["participants"].as_array())
        .map(|v| {
            v.iter()
                .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let affected = profiles(&mut tx, &p.participant_profile_ids, &old_ids).await?;
    let saved = id.unwrap_or_else(Uuid::new_v4);
    sqlx::query("INSERT INTO crm.activities(id,activity_type,occurred_at,title,body,source_type,created_by,due_at,content_hash) VALUES($1,$2,$3::timestamptz,$4,$5,'manual',$6,$7::timestamptz,'') ON CONFLICT(id) DO UPDATE SET activity_type=excluded.activity_type,occurred_at=excluded.occurred_at,title=excluded.title,body=excluded.body,due_at=excluded.due_at")
        .bind(saved).bind(p.activity_type).bind(p.occurred_at).bind(p.title).bind(p.body).bind(actor).bind(p.due_at).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM crm.activity_participants WHERE activity_id=$1")
        .bind(saved)
        .execute(&mut *tx)
        .await?;
    let participants = p
        .participant_profile_ids
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for participant in participants {
        sqlx::query("INSERT INTO crm.activity_participants VALUES($1,$2)")
            .bind(saved)
            .bind(participant)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(ids) = p.media_ids {
        media(&mut tx, saved, &ids, actor).await?;
    } else if id.is_none() {
        media(&mut tx, saved, &[], actor).await?;
    }
    let value = snapshot(&mut tx, saved).await?;
    if old.as_ref().is_some_and(|old| {
        monica_core::identity::canonical_json(old) == monica_core::identity::canonical_json(&value)
    }) {
        tx.rollback().await?;
        return Ok(saved);
    }
    record(
        &mut tx,
        saved,
        actor,
        if id.is_some() { "updated" } else { "created" },
        &affected,
    )
    .await?;
    monica_core::ai::memory::legacy::sync(&mut tx, "activity", &[saved])
        .await
        .map_err(|_| invalid("activity Memory update failed"))?;
    tx.commit().await?;
    Ok(saved)
}

async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(p): Json<Write>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    let id = save(&s, None, actor, p).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":id,"status":"created"})),
    ))
}
async fn update(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<Write>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    save(&s, Some(id), actor, p).await?;
    Ok(Json(json!({"id":id,"status":"updated"})))
}
async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(a)-'content_hash'-'updated_at'||jsonb_build_object('participants',(SELECT jsonb_agg(jsonb_build_object('id',p.id,'display_name',p.display_name) ORDER BY p.display_name,p.id) FROM crm.activity_participants ap JOIN crm.profiles p ON p.id=ap.profile_id WHERE ap.activity_id=a.id),'media',coalesce((SELECT jsonb_agg(to_jsonb(m)-'object_path' ORDER BY l.role,m.created_at,m.id) FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_type='activity' AND l.entity_id=a.id),'[]'::jsonb)) FROM crm.activities a WHERE activity_type NOT IN ('wechat_direct_summary','wechat_group_summary') AND ($1::uuid IS NULL OR EXISTS(SELECT 1 FROM crm.activity_participants ap WHERE ap.activity_id=a.id AND ap.profile_id=$1)) ORDER BY a.occurred_at DESC,a.id DESC LIMIT $2 OFFSET $3")
        .bind(q.profile_id).bind(q.limit.unwrap_or(50).clamp(1,100)).bind(q.offset.unwrap_or(0).max(0)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn set_state(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<StateWrite>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    if !["open", "done"].contains(&p.state.as_str()) {
        return Err(invalid("invalid activity state"));
    }
    let mut tx = s.pool.begin().await?;
    let old:String=sqlx::query_scalar("SELECT state FROM crm.activities WHERE id=$1 AND activity_type IN ('task','reminder') FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"task or reminder not found"))?;
    if old != p.state {
        let participants:Vec<Uuid>=sqlx::query_scalar("SELECT profile_id FROM crm.activity_participants WHERE activity_id=$1 ORDER BY profile_id").bind(id).fetch_all(&mut *tx).await?;
        let affected = profiles(&mut tx, &[], &participants).await?;
        sqlx::query("UPDATE crm.activities SET state=$2 WHERE id=$1")
            .bind(id)
            .bind(&p.state)
            .execute(&mut *tx)
            .await?;
        record(&mut tx, id, actor, "state_changed", &affected).await?;
        monica_core::ai::memory::legacy::sync(&mut tx, "activity", &[id])
            .await
            .map_err(|_| invalid("activity Memory update failed"))?;
    }
    tx.commit().await?;
    Ok(Json(json!({"id":id,"state":p.state})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[sqlx::test(migrations = "../../migrations")]
    async fn manual_activity_noop_revision_replay_and_source_guard(pool: sqlx::PgPool) {
        let (s, actor, person, _root) = crate::profiles::tests::fixture(&pool).await;
        let input = json!({"activity_type":"note","occurred_at":"2026-01-01T12:00:00Z","title":"Synthetic note","body":"A","participant_profile_ids":[person,person],"media_ids":[]});
        let id = save(
            &s,
            None,
            actor,
            serde_json::from_value(input.clone()).unwrap(),
        )
        .await
        .unwrap();
        save(
            &s,
            Some(id),
            actor,
            serde_json::from_value(input.clone()).unwrap(),
        )
        .await
        .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM crm.activity_revisions WHERE activity_id=$1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let mut changed = input.clone();
        changed["body"] = json!("B");
        save(
            &s,
            Some(id),
            actor,
            serde_json::from_value(changed).unwrap(),
        )
        .await
        .unwrap();
        save(
            &s,
            Some(id),
            actor,
            serde_json::from_value(input.clone()).unwrap(),
        )
        .await
        .unwrap();
        let hashes: Vec<String> = sqlx::query_scalar(
            "SELECT content_hash FROM crm.activity_revisions WHERE activity_id=$1 ORDER BY id",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(hashes.len(), 3);
        assert_eq!(hashes[0], hashes[2]);
        assert_ne!(hashes[0], hashes[1]);
        let mut invalid = input.clone();
        invalid["media_ids"] = json!([Uuid::new_v4()]);
        assert_eq!(
            save(
                &s,
                Some(id),
                actor,
                serde_json::from_value(invalid).unwrap()
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        sqlx::query("UPDATE crm.activities SET source_type='wechat' WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            save(&s, Some(id), actor, serde_json::from_value(input).unwrap())
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM crm.activity_revisions WHERE activity_id=$1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 3);
    }
}

//! Reuse saved public visual observations. This reducer never calls a model.
use anyhow::Result;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Move a small mutable queue batch before taking any person head. A crash after
/// this commit leaves claims visible to the existing publication fence.
pub async fn claim_pending(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    let admitted: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(11803479)")
        .fetch_one(&mut *tx)
        .await?;
    if !admitted {
        return Ok(0);
    }
    let count: i64 = sqlx::query_scalar("WITH picked AS (SELECT q.event_id FROM crm.memory_visual_pending q WHERE NOT EXISTS(SELECT 1 FROM crm.memory_visual_claims c WHERE c.event_id=q.event_id) ORDER BY q.requested_at,q.event_id LIMIT 25 FOR UPDATE OF q SKIP LOCKED), moved AS (DELETE FROM crm.memory_visual_pending q USING picked p WHERE q.event_id=p.event_id RETURNING q.*), saved AS (INSERT INTO crm.memory_visual_claims(event_id,profiles,version,requested_at) SELECT event_id,profiles,version,requested_at FROM moved RETURNING event_id) SELECT count(*) FROM saved")
        .fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(count as usize)
}

pub async fn refresh_inputs(pool: &PgPool) -> Result<usize> {
    claim_pending(pool).await?;
    let mut tx = pool.begin().await?;
    let admitted: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(11803479)")
        .fetch_one(&mut *tx)
        .await?;
    if !admitted {
        return Ok(0);
    }
    // Only mutable current heads are reserved. Saved OCR, captions, faces and
    // source evidence are ordinary MVCC reads, with no explicit source locks.
    let rows = sqlx::query("SELECT q.event_id,q.version,q.profiles,o.profile_id AS previous_owner,p.id AS owner,CASE WHEN p.id IS NOT NULL THEN crm.ai_social_visual(q.event_id) END AS payload FROM (SELECT * FROM crm.memory_visual_claims ORDER BY requested_at,event_id LIMIT 25) q LEFT JOIN crm.memory_visual_owners o USING(event_id) LEFT JOIN crm.timeline_events e ON e.id=q.event_id LEFT JOIN crm.memory_legacy_eligible_profiles p ON p.id=e.profile_id ORDER BY q.event_id")
        .fetch_all(&mut *tx).await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut people = std::collections::BTreeSet::new();
    for row in &rows {
        people.extend(row.get::<Vec<Uuid>, _>("profiles"));
        people.extend(row.get::<Option<Uuid>, _>("previous_owner"));
        people.extend(row.get::<Option<Uuid>, _>("owner"));
    }
    let people: Vec<Uuid> = people.into_iter().collect();
    sqlx::query("INSERT INTO crm.memory_heads(profile_id) SELECT id FROM crm.profiles WHERE id=ANY($1) ORDER BY id ON CONFLICT DO NOTHING")
        .bind(&people).execute(&mut *tx).await?;
    let people: Vec<Uuid> = sqlx::query_scalar("SELECT profile_id FROM crm.memory_heads WHERE profile_id=ANY($1) ORDER BY profile_id FOR UPDATE")
        .bind(&people).fetch_all(&mut *tx).await?;
    for person in people {
        let inputs: Vec<(String, Option<Value>)> = rows
            .iter()
            .map(|row| {
                let event: Uuid = row.get("event_id");
                let payload = if row.get::<Option<Uuid>, _>("owner") == Some(person) {
                    row.get("payload")
                } else {
                    None
                };
                (event.to_string(), payload)
            })
            .collect();
        super::memory::replace_sources(&mut tx, person, "visual", &inputs, false).await?;
    }
    for row in &rows {
        let event: Uuid = row.get("event_id");
        let owner: Option<Uuid> = row.get("owner");
        if let Some(owner) = owner.filter(|_| row.get::<Option<Value>, _>("payload").is_some()) {
            sqlx::query("INSERT INTO crm.memory_visual_owners(event_id,profile_id) VALUES($1,$2) ON CONFLICT(event_id) DO UPDATE SET profile_id=excluded.profile_id")
                .bind(event).bind(owner).execute(&mut *tx).await?;
        } else {
            sqlx::query("DELETE FROM crm.memory_visual_owners WHERE event_id=$1")
                .bind(event)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM crm.memory_visual_claims WHERE event_id=$1 AND version=$2")
            .bind(event)
            .bind(row.get::<Uuid, _>("version"))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(rows.len())
}

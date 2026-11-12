//! Bounded references for original messages, social events and private activities.
use anyhow::{Result, ensure};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;
/// Queue individual objects in the same transaction as their projection. Unlike
/// a profile-scoped sweep, this does not lock a shared Memory head on the hot path.
pub async fn queue_record(tx: &mut Transaction<'_, Postgres>, kind: &str, id: Uuid) -> Result<()> {
    ensure!(
        ["message", "social", "activity"].contains(&kind),
        "invalid legacy Memory kind"
    );
    sqlx::query("INSERT INTO crm.memory_legacy_pending_records(kind,entity_id,profiles) SELECT $1,$2,ARRAY(SELECT profile_id FROM crm.memory_legacy_owners WHERE kind=$1 AND entity_id=$2 UNION SELECT profile_id FROM crm.memory_legacy_current WHERE kind=$1 AND entity_id=$2 ORDER BY profile_id) ON CONFLICT(kind,entity_id) DO UPDATE SET profiles=ARRAY(SELECT DISTINCT unnest(crm.memory_legacy_pending_records.profiles||excluded.profiles)),requested_at=clock_timestamp()")
        .bind(kind).bind(id).execute(&mut **tx).await?;
    Ok(())
}
pub async fn sync(tx: &mut Transaction<'_, Postgres>, kind: &str, ids: &[Uuid]) -> Result<()> {
    ensure!(
        ["message", "social", "activity"].contains(&kind) && ids.len() <= 1000,
        "invalid legacy Memory batch"
    );
    if !ids.is_empty() {
        sqlx::query("SELECT crm.memory_sync_legacy($1,$2)")
            .bind(kind)
            .bind(ids)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}
pub async fn queue_account(
    tx: &mut Transaction<'_, Postgres>,
    account: Uuid,
    profiles: &[Uuid],
) -> Result<()> {
    sqlx::query("INSERT INTO crm.memory_legacy_scopes(scope,id,profiles) VALUES('account',$1,$2) ON CONFLICT(scope,id) DO UPDATE SET after_kind='',after_id='00000000-0000-0000-0000-000000000000',profiles=ARRAY(SELECT DISTINCT unnest(crm.memory_legacy_scopes.profiles||excluded.profiles)),updated_at=now()").bind(account).bind(profiles).execute(&mut **tx).await?;
    Ok(())
}
/// One page per recovery turn; no body leaves PostgreSQL during reattribution.
pub async fn drain(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    // Model lanes share one short recovery turn. A transaction can touch several
    // kinds and people; serialize these reducers without locking source writers.
    let admitted: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(11803479)")
        .fetch_one(&mut *tx)
        .await?;
    if !admitted {
        return Ok(0);
    }
    let records: Vec<(String, Uuid)> = sqlx::query_as("SELECT kind,entity_id FROM crm.memory_legacy_pending_records WHERE requested_at<=coalesce((SELECT min(updated_at) FROM crm.memory_legacy_scopes),'infinity'::timestamptz) ORDER BY requested_at,kind,entity_id LIMIT 1000 FOR UPDATE SKIP LOCKED")
        .fetch_all(&mut *tx).await?;
    if !records.is_empty() {
        for kind in ["message", "social", "activity"] {
            let ids = records
                .iter()
                .filter(|(k, _)| k == kind)
                .map(|(_, id)| *id)
                .collect::<Vec<_>>();
            sync(&mut tx, kind, &ids).await?;
            sqlx::query(
                "DELETE FROM crm.memory_legacy_pending_records WHERE kind=$1 AND entity_id=ANY($2)",
            )
            .bind(kind)
            .bind(ids)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        return Ok(records.len());
    }
    let row=sqlx::query("SELECT scope,id,after_kind,after_id FROM crm.memory_legacy_scopes ORDER BY updated_at,scope,id LIMIT 1 FOR UPDATE SKIP LOCKED").fetch_optional(&mut *tx).await?;
    let Some(row) = row else { return Ok(0) };
    let scope: String = row.get("scope");
    let id: Uuid = row.get("id");
    let query = if scope == "account" {
        "SELECT kind,entity_id FROM (
        (SELECT 'message'::text kind,message_id entity_id FROM crm.message_search WHERE sender_account_id=$1 AND ('message',message_id)>($2,$3) ORDER BY message_id LIMIT 1000)
        UNION (SELECT 'message',m.message_id FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.source_account_id=$1 AND c.connection_id IS NULL AND ('message',m.message_id)>($2,$3) ORDER BY m.message_id LIMIT 1000)
        UNION (SELECT 'social',id FROM crm.timeline_events WHERE source_account_id=$1 AND ('social',id)>($2,$3) ORDER BY id LIMIT 1000)
        ) e ORDER BY kind,entity_id LIMIT 1000"
    } else {
        "SELECT kind,entity_id FROM (
        (SELECT kind,entity_id FROM crm.memory_legacy_owners WHERE profile_id=$1 AND (kind,entity_id)>($2,$3) ORDER BY kind,entity_id LIMIT 1000)
        UNION (SELECT 'message',m.message_id FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.profile_id=$1 AND c.connection_id IS NULL AND ('message',m.message_id)>($2,$3) ORDER BY m.message_id LIMIT 1000)
        UNION (SELECT 'message',message_id FROM crm.message_search WHERE sender_profile_id=$1 AND ('message',message_id)>($2,$3) ORDER BY message_id LIMIT 1000)
        UNION (SELECT 'message',m.message_id FROM crm.message_search m JOIN crm.source_account_links l ON l.account_id=m.sender_account_id WHERE l.profile_id=$1 AND ('message',m.message_id)>($2,$3) ORDER BY m.message_id LIMIT 1000)
        UNION (SELECT 'social',id FROM crm.timeline_events WHERE profile_id=$1 AND ('social',id)>($2,$3) ORDER BY id LIMIT 1000)
        UNION (SELECT 'activity',activity_id FROM crm.activity_participants WHERE profile_id=$1 AND ('activity',activity_id)>($2,$3) ORDER BY activity_id LIMIT 1000)
        ) e ORDER BY kind,entity_id LIMIT 1000"
    };
    let rows: Vec<(String, Uuid)> = sqlx::query_as(query)
        .bind(id)
        .bind(row.get::<String, _>("after_kind"))
        .bind(row.get::<Uuid, _>("after_id"))
        .fetch_all(&mut *tx)
        .await?;
    for kind in ["message", "social", "activity"] {
        let ids = rows
            .iter()
            .filter(|(k, _)| k == kind)
            .map(|(_, id)| *id)
            .collect::<Vec<_>>();
        sync(&mut tx, kind, &ids).await?;
    }
    if rows.len() < 1000 {
        sqlx::query("DELETE FROM crm.memory_legacy_scopes WHERE scope=$1 AND id=$2")
            .bind(&scope)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    } else {
        let last = rows.last().unwrap();
        sqlx::query("UPDATE crm.memory_legacy_scopes SET after_kind=$3,after_id=$4,updated_at=now() WHERE scope=$1 AND id=$2").bind(&scope).bind(id).bind(&last.0).bind(last.1).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(rows.len().max(1))
}

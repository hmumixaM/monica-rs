//! Optional asynchronous WAL history. One single-database slot audits all source
//! schemas; it never schedules CRM projection or gates ingestion acknowledgements.
#[path = "cdc_codec.rs"]
pub mod codec;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgPool, Row};
const PUBLICATION: &str = "monica_rs_cdc";
const OWNER: &str = "database";
const LOCK: i64 = 846028474211;
const MAX_CAPTURE: usize = 32 * 1024 * 1024;
pub fn enabled() -> Result<bool> {
    Ok(crate::ai::optional_bool("MONICA_CDC_ENABLED")?.unwrap_or(false))
}
fn tables() -> Vec<(String, String)> {
    let mut values = vec![];
    for schema in [
        "wechat",
        "instagram",
        "linkedin",
        "whatsapp",
        "apple_messages",
        "apple_contacts",
        "mail",
    ] {
        for table in ["accounts", "objects"] {
            values.push((schema.into(), table.into()));
        }
    }
    for table in [
        "profiles",
        "identities",
        "source_account_links",
        "field_current",
        "activities",
        "media_links",
    ] {
        values.push(("crm".into(), table.into()));
    }
    values
}
async fn slot(connection: &mut sqlx::PgConnection) -> Result<String> {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(connection)
        .await?;
    Ok(format!("monica_rs_{:x}", Sha256::digest(database.as_bytes()))[..22].into())
}
/// Explicit maintenance command; never runs inside a business transaction.
pub async fn initialize(pool: &PgPool) -> Result<()> {
    let mut connection = pool.acquire().await?.detach();
    let level: String = sqlx::query_scalar("SHOW wal_level")
        .fetch_one(&mut connection)
        .await?;
    ensure!(
        level == "logical",
        "CDC requires PostgreSQL wal_level=logical"
    );
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(LOCK)
        .execute(&mut connection)
        .await?;
    let name = slot(&mut connection).await?;
    let recorded: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.cdc_consumers WHERE established)")
            .fetch_one(&mut connection)
            .await?;
    let old=sqlx::query("SELECT plugin,wal_status,database=current_database() same_database FROM pg_replication_slots WHERE slot_name=$1").bind(&name).fetch_optional(&mut connection).await?;
    if let Some(old) = old {
        ensure!(
            old.try_get::<String, _>("plugin")? == "pgoutput"
                && old.try_get::<bool, _>("same_database")?
                && old.try_get::<Option<String>, _>("wal_status")?.as_deref() != Some("lost"),
            "CDC slot incompatible or WAL lost; explicit recovery required"
        );
    } else {
        ensure!(
            !recorded,
            "established CDC slot disappeared; refusing to skip history"
        );
    }
    let mut published = vec![];
    for (schema, table) in tables() {
        let relation = format!("{schema}.{table}");
        sqlx::query(&format!("ALTER TABLE {relation} REPLICA IDENTITY FULL"))
            .execute(&mut connection)
            .await?;
        published.push(if table == "objects" {
            format!("{relation} WHERE (object_kind <> 'message')")
        } else {
            relation
        });
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_publication WHERE pubname=$1)")
            .bind(PUBLICATION)
            .fetch_one(&mut connection)
            .await?;
    sqlx::query(&format!(
        "{} PUBLICATION {PUBLICATION} {} TABLE {}",
        if exists { "ALTER" } else { "CREATE" },
        if exists { "SET" } else { "FOR" },
        published.join(",")
    ))
    .execute(&mut connection)
    .await?;
    if !recorded {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name=$1)",
        )
        .bind(&name)
        .fetch_one(&mut connection)
        .await?;
        if !exists {
            sqlx::query("SELECT * FROM pg_create_logical_replication_slot($1,'pgoutput')")
                .bind(&name)
                .execute(&mut connection)
                .await?;
        }
    }
    sqlx::query("INSERT INTO crm.cdc_consumers(provider,slot_name,established,state) VALUES($1,$2,true,'ready') ON CONFLICT(provider) DO UPDATE SET slot_name=excluded.slot_name,established=true,state='ready',error_code=NULL,heartbeat_at=now()").bind(OWNER).bind(name).execute(&mut connection).await?;
    connection.close().await?;
    Ok(())
}
fn strip_refresh(value: &Option<Value>) -> Option<Value> {
    value.as_ref().map(|value| {
        let mut value = value.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove("last_seen_at");
            object.remove("updated_at");
        }
        value
    })
}
/// A failed/cancelled decode cannot acknowledge WAL. On a post-persistence crash,
/// (schema,commit_lsn,ordinal) makes the next capture an exact idempotent replay.
pub async fn capture(pool: &PgPool) -> Result<usize> {
    capture_inner(pool, false).await
}
async fn capture_inner(pool: &PgPool, fail_before_ack: bool) -> Result<usize> {
    use futures_util::TryStreamExt;
    let mut source = pool.acquire().await?.detach();
    let owned: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LOCK)
        .fetch_one(&mut source)
        .await?;
    if !owned {
        return Ok(0);
    }
    let name = slot(&mut source).await?;
    let established:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.cdc_consumers WHERE provider=$1 AND established AND slot_name=$2)").bind(OWNER).bind(&name).fetch_one(&mut source).await?;
    ensure!(established, "CDC must be initialized before capture");
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name=$1 AND plugin='pgoutput' AND wal_status<>'lost')").bind(&name).fetch_one(&mut source).await?;
    ensure!(valid, "established CDC slot missing or WAL lost");
    let watermark: String = sqlx::query_scalar("SELECT pg_current_wal_flush_lsn()::text")
        .fetch_one(&mut source)
        .await?;
    let retained:i64=sqlx::query_scalar("SELECT coalesce(pg_wal_lsn_diff(pg_current_wal_lsn(),restart_lsn),0)::bigint FROM pg_replication_slots WHERE slot_name=$1").bind(&name).fetch_one(&mut source).await?;
    let mut decoder = codec::Decoder::default();
    let mut batches = vec![];
    let mut bytes = 0;
    {
        let mut rows=sqlx::query("SELECT data FROM pg_logical_slot_peek_binary_changes($1,$2::text::pg_lsn,500,'proto_version','1','publication_names',$3)").bind(&name).bind(&watermark).bind(PUBLICATION).fetch(&mut source);
        while let Some(row) = rows.try_next().await? {
            let data: Vec<u8> = row.try_get("data")?;
            bytes += data.len();
            ensure!(
                bytes <= MAX_CAPTURE,
                "CDC batch exceeds 32 MiB; WAL remains unacknowledged"
            );
            if let Some(batch) = decoder.feed(&data)? {
                batches.push(batch);
            }
        }
    }
    decoder.finish()?;
    let end = batches
        .last()
        .map(|b| b.end_lsn.as_str())
        .unwrap_or(&watermark);
    let allowed = tables();
    let mut count = 0;
    let mut tx = pool.begin().await?;
    for batch in &batches {
        let mut rows = vec![];
        for (ordinal, change) in batch.changes.iter().enumerate() {
            ensure!(
                allowed.contains(&(change.schema.clone(), change.relation.clone())),
                "unexpected published CDC relation"
            );
            if change.before.is_some()
                && change.after.is_some()
                && strip_refresh(&change.before) == strip_refresh(&change.after)
            {
                continue;
            }
            rows.push(json!({"provider":change.schema,"ordinal":ordinal,"relation":change.relation,"operation":change.operation,"before":change.before,"after":change.after}));
        }
        count+=sqlx::query("INSERT INTO crm.cdc_events(provider,commit_lsn,ordinal,relation_name,operation,before_row,after_row,committed_at) SELECT provider,$1::text::pg_lsn,ordinal,relation,operation,before,after,$3 FROM jsonb_to_recordset($2) AS r(provider text,ordinal integer,relation text,operation text,before jsonb,after jsonb) ORDER BY ordinal ON CONFLICT(provider,commit_lsn,ordinal) DO NOTHING")
   .bind(&batch.commit_lsn).bind(json!(rows)).bind(batch.committed_at).execute(&mut *tx).await?.rows_affected() as usize;
    }
    sqlx::query("UPDATE crm.cdc_consumers SET captured_lsn=$2::text::pg_lsn,retained_wal_bytes=$3,state='ready',error_code=NULL,heartbeat_at=now() WHERE provider=$1 AND ($4 OR heartbeat_at<now()-interval '15 seconds' OR state<>'ready')").bind(OWNER).bind(end).bind(retained).bind(!batches.is_empty()).execute(&mut *tx).await?;
    tx.commit().await?;
    ensure!(
        !fail_before_ack,
        "injected failure before WAL acknowledgement"
    );
    sqlx::query("SELECT * FROM pg_replication_slot_advance($1,$2::text::pg_lsn)")
        .bind(&name)
        .bind(end)
        .execute(&mut source)
        .await?;
    source.close().await?;
    Ok(count)
}
pub async fn mark_error(pool: &PgPool) -> Result<()> {
    // Operational category only: never place changed values or connection secrets in status.
    let mut connection = pool.acquire().await?;
    let name = slot(&mut connection).await?;
    sqlx::query("INSERT INTO crm.cdc_consumers(provider,slot_name,state,error_code) VALUES($1,$2,'error','capture_failed') ON CONFLICT(provider) DO UPDATE SET state='error',error_code='capture_failed',heartbeat_at=now()").bind(OWNER).bind(name).execute(&mut *connection).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[sqlx::test(migrations = "../../migrations")]
    async fn wal_replay_full_images_and_missing_slot_fail_closed(pool: PgPool) {
        initialize(&pool).await.unwrap();
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Before')",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(capture_inner(&pool, true).await.is_err());
        let first: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM crm.cdc_events WHERE relation_name='profiles'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(first, 1);
        capture(&pool).await.unwrap();
        let replay: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM crm.cdc_events WHERE relation_name='profiles'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(replay, first);
        sqlx::query("UPDATE crm.profiles SET display_name='After' WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        capture(&pool).await.unwrap();
        let row=sqlx::query("SELECT before_row,after_row FROM crm.cdc_events WHERE relation_name='profiles' AND operation='U'").fetch_one(&pool).await.unwrap();
        assert_eq!(row.get::<Value, _>("before_row")["display_name"], "Before");
        assert_eq!(row.get::<Value, _>("after_row")["display_name"], "After");
        let account = uuid::Uuid::new_v4();
        let object = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO mail.accounts(id,external_id) VALUES($1,'audit-synthetic')")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO mail.objects(id,account_id,external_id,object_kind) VALUES($1,$2,'message','message')").bind(object).bind(account).execute(&pool).await.unwrap();
        capture(&pool).await.unwrap();
        let messages: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM crm.cdc_events WHERE provider='mail' AND relation_name='objects'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(messages, 0);
        let name: String = sqlx::query_scalar("SELECT slot_name FROM crm.cdc_consumers")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query("SELECT pg_drop_replication_slot($1)")
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
        assert!(capture(&pool).await.is_err());
        assert!(initialize(&pool).await.is_err());
    }
}

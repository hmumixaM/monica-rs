use anyhow::Result;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Durably consume at most 100 manifest/raw entries. Offset and dependencies are
/// committed together, so a crash never acknowledges an unrecorded dependency.
pub async fn consume_imports(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO crm.media_import_progress(batch_id) SELECT id FROM crm.import_batches b WHERE NOT EXISTS(SELECT 1 FROM crm.media_import_progress p WHERE p.batch_id=b.id) ORDER BY received_at LIMIT 25 ON CONFLICT DO NOTHING").execute(&mut *tx).await?;
    let row=sqlx::query("SELECT p.batch_id,p.media_offset,p.raw_offset,b.source,i.payload FROM crm.media_import_progress p JOIN crm.import_batches b ON b.id=p.batch_id JOIN crm.import_batch_inputs i ON i.batch_id=b.id WHERE NOT p.complete ORDER BY p.updated_at,p.batch_id FOR UPDATE OF p SKIP LOCKED LIMIT 1").fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(0);
    };
    let id: Uuid = row.get("batch_id");
    let provider: String = row.get("source");
    let payload: Value = row.get("payload");
    let media = payload["media_manifest"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let raw = payload["raw_objects"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut mo = row.get::<i32, _>("media_offset") as usize;
    let mut ro = row.get::<i32, _>("raw_offset") as usize;
    let mut n = 0;
    while mo < media.len() && n < 100 {
        sqlx::query("INSERT INTO crm.media_dependencies(id,import_batch_id,ordinal,provider,record) VALUES($1,$2,$3,$4,$5) ON CONFLICT(import_batch_id,ordinal) DO NOTHING")
            .bind(Uuid::new_v4()).bind(id).bind(mo as i32).bind(&provider).bind(&media[mo]).execute(&mut *tx).await?;
        mo += 1;
        n += 1;
    }
    while ro < raw.len() && n < 100 {
        let hash = crate::identity::observation_hash(&raw[ro]);
        sqlx::query("INSERT INTO crm.import_raw_objects(batch_id,ordinal,sha256,payload) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(id).bind(ro as i32).bind(hash).bind(&raw[ro]).execute(&mut *tx).await?;
        ro += 1;
        n += 1;
    }
    sqlx::query("UPDATE crm.media_import_progress SET media_offset=$2,raw_offset=$3,complete=$4,updated_at=clock_timestamp() WHERE batch_id=$1").bind(id).bind(mo as i32).bind(ro as i32).bind(mo==media.len()&&ro==raw.len()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(n)
}

pub async fn resolve_dependencies(pool: &PgPool) -> Result<usize> {
    let mut completed = 0;
    for _ in 0..25 {
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='1s'")
            .execute(&mut *tx)
            .await?;
        let row=sqlx::query("SELECT d.*,b.observed_at FROM crm.media_dependencies d JOIN crm.import_batches b ON b.id=d.import_batch_id WHERE d.state='waiting' AND d.next_attempt_at<=now() ORDER BY d.next_attempt_at,d.id FOR UPDATE OF d SKIP LOCKED LIMIT 1").fetch_optional(&mut *tx).await?;
        let Some(row) = row else { break };
        let id: Uuid = row.get("id");
        let batch: Uuid = row.get("import_batch_id");
        let provider: String = row.get("provider");
        let record: Value = row.get("record");
        let entity = record["entity_type"].as_str().unwrap_or("social_post");
        let role = record["role"].as_str().unwrap_or("media-0");
        let table = match entity {
            "social_post" => Some(("crm.timeline_events", "id")),
            "social_story" => Some(("crm.timeline_events", "id")),
            "source_account" => Some(("crm.source_accounts", "id")),
            "profile" => Some(("crm.profiles", "id")),
            "activity" => Some(("crm.activities", "id")),
            "message_event" => Some(("crm.message_search", "message_id")),
            _ => None,
        };
        if table.is_none()
            || (record["additive_record_media"].as_bool() != Some(true)
                && (role.len() > 120 || role.starts_with("manual") || role.is_empty()))
        {
            sqlx::query("UPDATE crm.media_dependencies SET state='failed',reason='invalid manifest attachment',updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await?;
            tx.commit().await?;
            continue;
        }
        let media_id = record["media_id"]
            .as_str()
            .and_then(|x| Uuid::parse_str(x).ok());
        let hash = record["sha256"]
            .as_str()
            .or_else(|| record["content_hash"].as_str());
        let saved:Option<Uuid>=sqlx::query_scalar("SELECT id FROM crm.media_assets WHERE ($1::uuid IS NOT NULL OR $2::text IS NOT NULL) AND ($1::uuid IS NULL OR id=$1) AND ($2::text IS NULL OR sha256=$2) ORDER BY id LIMIT 1").bind(media_id).bind(hash).fetch_optional(&mut *tx).await?;
        let mut entity_id = record["entity_id"]
            .as_str()
            .and_then(|x| Uuid::parse_str(x).ok());
        let external = record["external_id"]
            .as_str()
            .or_else(|| record["source_record_id"].as_str());
        if entity_id.is_none()
            && let Some(external) = external
        {
            entity_id=match entity{
                "social_post"|"social_story"=>sqlx::query_scalar("SELECT id FROM crm.timeline_events WHERE provider=$1 AND external_id=$2 AND event_type=$3 AND (NOT $4 OR connection_id IS NOT DISTINCT FROM $5) ORDER BY id LIMIT 1").bind(&provider).bind(external).bind(if entity=="social_post"{"post"}else{"story"}).bind(record["additive_record_media"].as_bool()==Some(true)).bind(record["connection_id"].as_str().and_then(|v|Uuid::parse_str(v).ok())).fetch_optional(&mut *tx).await?,
                "source_account"=>sqlx::query_scalar("SELECT id FROM crm.source_accounts WHERE provider=$1 AND external_id=$2").bind(&provider).bind(external).fetch_optional(&mut *tx).await?,_=>None};
        }
        if let Some(target) = entity_id {
            let (table, key) = table.unwrap();
            let exists: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS(SELECT 1 FROM {table} WHERE {key}=$1)"
            ))
            .bind(target)
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                entity_id = None
            }
        }
        if let (Some(media), Some(target)) = (saved, entity_id) {
            if record["additive_record_media"].as_bool() == Some(true) {
                sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(entity).bind(target).bind(media).bind(role).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO crm.media_provenance(media_id,source,source_record_id,import_batch_id) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(media).bind(&provider).bind(external.unwrap_or("record")).bind(batch).execute(&mut *tx).await?;
                sqlx::query("UPDATE crm.media_dependencies SET state='complete',reason='linked',media_id=$2,updated_at=now() WHERE id=$1").bind(id).bind(media).execute(&mut *tx).await?;
                if matches!(entity, "social_post" | "social_story") {
                    crate::ai::post_media::enqueue_import(&mut tx, target, role).await?;
                }
                tx.commit().await?;
                completed += 1;
                continue;
            }
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("media-slot:{entity}:{target}:{role}"))
                .execute(&mut *tx)
                .await?;
            let newer_directory_photo = entity == "source_account"
                && matches!(role,"wechat_avatar"|"wechat_avatar_small"|"wechat_avatar_big")
                && sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM crm.wechat_avatar_requests WHERE account_id=$1 AND NOT native AND observed_at>=$2)")
                    .bind(target).bind(row.get::<chrono::DateTime<chrono::Utc>,_>("observed_at")).fetch_one(&mut *tx).await?;
            let applied = if newer_directory_photo {
                0
            } else {
                sqlx::query("INSERT INTO crm.media_slots(entity_type,entity_id,role,media_id,source_observed_at,source_batch_id,source_ordinal) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(entity_type,entity_id,role) DO UPDATE SET media_id=excluded.media_id,generation=crm.media_slots.generation+CASE WHEN crm.media_slots.media_id<>excluded.media_id THEN 1 ELSE 0 END,source_observed_at=excluded.source_observed_at,source_batch_id=excluded.source_batch_id,source_ordinal=excluded.source_ordinal,updated_at=now() WHERE crm.media_slots.source_observed_at IS NULL OR (crm.media_slots.source_observed_at,crm.media_slots.source_batch_id,crm.media_slots.source_ordinal)<(excluded.source_observed_at,excluded.source_batch_id,excluded.source_ordinal)").bind(entity).bind(target).bind(role).bind(media).bind(row.get::<chrono::DateTime<chrono::Utc>,_>("observed_at")).bind(batch).bind(row.get::<i32,_>("ordinal")).execute(&mut *tx).await?.rows_affected()
            };
            sqlx::query("INSERT INTO crm.media_provenance(media_id,source,source_record_id,source_url,import_batch_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(media).bind(&provider).bind(external.unwrap_or("manifest")).bind(record["source_url"].as_str().or_else(||record["url"].as_str())).bind(batch).execute(&mut *tx).await?;
            if applied == 0 {
                sqlx::query("UPDATE crm.media_dependencies SET state='complete',reason='superseded',media_id=$2,updated_at=now() WHERE id=$1").bind(id).bind(media).execute(&mut *tx).await?;
                tx.commit().await?;
                completed += 1;
                continue;
            }
            sqlx::query("DELETE FROM crm.media_links WHERE entity_type=$1 AND entity_id=$2 AND role=$3 AND media_id<>$4").bind(entity).bind(target).bind(role).bind(media).execute(&mut *tx).await?;
            let source_account = if entity == "source_account" {
                Some(target)
            } else {
                None
            };
            sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role,source_account_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(entity).bind(target).bind(media).bind(role).bind(source_account).execute(&mut *tx).await?;
            if entity == "source_account" {
                sqlx::query("INSERT INTO crm.media_profile_refresh(account_id) VALUES($1) ON CONFLICT(account_id) DO UPDATE SET generation=crm.media_profile_refresh.generation+1").bind(target).execute(&mut *tx).await?;
            }
            sqlx::query("UPDATE crm.media_dependencies SET state='complete',reason='linked',media_id=$2,updated_at=now() WHERE id=$1").bind(id).bind(media).execute(&mut *tx).await?;
            if matches!(entity, "social_post" | "social_story") {
                crate::ai::post_media::enqueue_import(&mut tx, target, role).await?;
            }
            completed += 1;
        } else {
            let reason = if saved.is_none() {
                "media_pending"
            } else {
                "source_pending"
            };
            sqlx::query("UPDATE crm.media_dependencies SET reason=$2,attempts=attempts+1,next_attempt_at=now()+interval '5 seconds',updated_at=now() WHERE id=$1").bind(id).bind(reason).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    Ok(completed)
}
/// Generation fencing keeps a concurrent slot update queued when refresh races
/// with it. Source-account/profile locking remains owned by the linking module.
pub async fn refresh_profiles(pool: &PgPool) -> Result<usize> {
    let rows = sqlx::query(
        "SELECT account_id,generation FROM crm.media_profile_refresh ORDER BY account_id LIMIT 10",
    )
    .fetch_all(pool)
    .await?;
    let mut count = 0;
    for row in rows {
        let account: Uuid = row.get("account_id");
        crate::linking::refresh_account(pool, account).await?;
        count += sqlx::query(
            "DELETE FROM crm.media_profile_refresh WHERE account_id=$1 AND generation=$2",
        )
        .bind(account)
        .bind(row.get::<i64, _>("generation"))
        .execute(pool)
        .await?
        .rows_affected() as usize;
    }
    Ok(count)
}

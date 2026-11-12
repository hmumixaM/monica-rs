//! Signed record media uses the same publication journal as uploads. Receipt
//! progress is committed only after real bytes and dependency rows are durable.
use super::{Asset, MediaStore};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

// Python bytes.fromhex accepts ASCII whitespace between complete byte pairs.
fn decode_hex(value: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(value.len() / 2);
    let mut high = None;
    for c in value.bytes() {
        if c.is_ascii_whitespace() || c == 0x0b {
            ensure!(high.is_none(), "media.bytes must be hex");
            continue;
        }
        let digit = (c as char)
            .to_digit(16)
            .ok_or_else(|| anyhow::anyhow!("media.bytes must be hex"))? as u8;
        if let Some(h) = high.take() {
            out.push(h * 16 + digit);
        } else {
            high = Some(digit);
        }
    }
    ensure!(high.is_none(), "media.bytes must be hex");
    Ok(out)
}
pub fn validate_record_media(record: &crate::imports::BatchRecord) -> Result<()> {
    if let Some(media) = record.extra.get("media") {
        let entries = media
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("media must be an array"))?;
        ensure!(entries.len() <= 10000, "too many record attachments");
        for item in entries {
            ensure!(item.is_object(), "media must contain objects");
            if let Some(bytes) = item["bytes"].as_str() {
                decode_hex(bytes)?;
            }
        }
    }
    Ok(())
}

/// One attachment per call bounds decoded memory to the admitted signed body.
/// Restart/replay may republish the same digest but never acknowledges lost data.
pub async fn consume_record_media(store: &MediaStore) -> Result<usize> {
    let mut tx = store.pool.begin().await?;
    let row = sqlx::query("SELECT p.batch_id,p.record_offset,p.media_offset,p.ordinal,b.record_count,b.source,b.stream,b.connection_id FROM crm.import_record_media_progress p JOIN crm.import_batches b ON b.id=p.batch_id WHERE b.status='complete' AND NOT p.complete AND (p.lease_until IS NULL OR p.lease_until<now()) ORDER BY p.updated_at,p.batch_id FOR UPDATE OF p SKIP LOCKED LIMIT 1").fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(0);
    };
    let id: Uuid = row.get("batch_id");
    let ri: i64 = row.get("record_offset");
    let mi: i64 = row.get("media_offset");
    if ri >= row.get::<i64, _>("record_count") {
        sqlx::query("UPDATE crm.import_record_media_progress SET complete=true,lease_token=NULL,lease_until=NULL WHERE batch_id=$1").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(1);
    }
    // JSONB indexed access avoids expanding every earlier record/attachment.
    // Only the one hex string and entity routing fields cross the DB boundary.
    let item=sqlx::query("SELECT jsonb_build_object('external_id',r->'external_id','entity_type',r->'entity_type','content',jsonb_build_object('entity_type',r->'content'->'entity_type'),'provider',r->'provider') record,r->'media'->$3::int media FROM crm.import_batch_inputs i CROSS JOIN LATERAL (SELECT i.payload->'records'->$2::int r) value WHERE i.batch_id=$1").bind(id).bind(ri as i32).bind(mi as i32).fetch_one(&mut *tx).await?;
    let Some(mut media): Option<Value> = item.get("media") else {
        sqlx::query("UPDATE crm.import_record_media_progress SET record_offset=record_offset+1,media_offset=0,updated_at=clock_timestamp(),lease_token=NULL,lease_until=NULL WHERE batch_id=$1").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(1);
    };
    let record: Value = item.get("record");
    let token = Uuid::new_v4();
    sqlx::query("UPDATE crm.import_record_media_progress SET lease_token=$2,lease_until=now()+interval '90 seconds',updated_at=clock_timestamp() WHERE batch_id=$1").bind(id).bind(token).execute(&mut *tx).await?;
    tx.commit().await?;
    // No write transaction or row lock survives into decoding or file I/O.
    ensure!(media.is_object(), "record media item must be an object");
    let source: String = row.get("source");
    let external = record["external_id"].as_str().unwrap_or("");
    if let Some(encoded) = media["bytes"].as_str() {
        let _disk = store
            .resources
            .acquire(crate::resources::TaskResources {
                disk: 1,
                ..Default::default()
            })
            .await?;
        let bytes = decode_hex(encoded)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        tokio::fs::create_dir_all(&store.root).await?;
        let file = tempfile::NamedTempFile::new_in(&store.root)?;
        tokio::fs::write(file.path(), &bytes).await?;
        let asset = Asset {
            id: Uuid::new_v4(),
            sha256: digest.clone(),
            media_type: media["media_type"]
                .as_str()
                .unwrap_or("application/octet-stream")
                .into(),
            byte_length: bytes.len() as i64,
            object_path: String::new(),
            file_name: None,
            width: None,
            height: None,
            duration_ms: None,
        };
        store
            .publish(file.path(), asset, &source, Some(external))
            .await?;
        if media["sha256"].as_str().is_none_or(str::is_empty) {
            media["sha256"] = json!(digest);
        }
    }
    let mut tx = store.pool.begin().await?;
    let owned: Option<Uuid> = sqlx::query_scalar("SELECT batch_id FROM crm.import_record_media_progress WHERE batch_id=$1 AND lease_token=$2 AND lease_until>now() FOR UPDATE").bind(id).bind(token).fetch_optional(&mut *tx).await?;
    ensure!(owned.is_some(), "record media lease lost");
    let stream: String = row.get("stream");
    let entity = record["entity_type"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            record["content"]["entity_type"]
                .as_str()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(match stream.as_str() {
            "posts" | "moments" => "post",
            "stories" => "story",
            _ => "",
        });
    let ordinal: i32 = row.get("ordinal");
    if matches!(entity, "post" | "story") {
        let fields = media.as_object_mut().unwrap();
        fields.remove("bytes");
        fields.remove("content_hash");
        fields.remove("entity_id");
        media["entity_type"] = json!(if entity == "post" {
            "social_post"
        } else {
            "social_story"
        });
        media["external_id"] = json!(external);
        media["additive_record_media"] = json!(true);
        media["connection_id"] = json!(row.get::<Option<Uuid>, _>("connection_id"));
        if media["role"].as_str().is_none() {
            media["role"] = json!(format!("media-{}", mi + 1));
        }
        // Explicit media_id has precedence over a supplied digest in the legacy contract.
        if media["media_id"].as_str().is_some_and(|s| !s.is_empty()) {
            media.as_object_mut().unwrap().remove("sha256");
        }
        let provider = record["provider"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&source);
        sqlx::query("INSERT INTO crm.media_dependencies(id,import_batch_id,ordinal,provider,record) VALUES($1,$2,$3,$4,$5) ON CONFLICT(import_batch_id,ordinal) DO NOTHING").bind(Uuid::new_v4()).bind(id).bind(-ordinal-1).bind(provider).bind(media).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE crm.import_record_media_progress SET media_offset=media_offset+1,ordinal=ordinal+1,lease_token=NULL,lease_until=NULL,updated_at=clock_timestamp() WHERE batch_id=$1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(1)
}

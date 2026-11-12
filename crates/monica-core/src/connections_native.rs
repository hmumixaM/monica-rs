use super::*;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeSnapshot {
    pub snapshot_id: Uuid,
    #[serde(default)]
    pub reset: bool,
    #[serde(default)]
    pub snapshot_complete: bool,
}
/// Persist a bounded native page and its snapshot membership in the same transaction.
/// Repeating a completed snapshot acknowledges it without replaying stale source facts.
#[allow(clippy::too_many_arguments)]
pub async fn commit_native_page(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    stream: &str,
    records: &[Value],
    next_cursor: &Value,
    snapshot: Option<&NativeSnapshot>,
) -> Result<u64> {
    if records.len() > 1000 {
        bail!("native page exceeds 1000 records")
    }
    lock_connection(tx, cid).await?;
    if let Some(snapshot) = snapshot {
        if provider.schema() != "apple_contacts" {
            bail!("snapshots require apple_contacts")
        }
        let generation: i64 = sqlx::query_scalar(
            "SELECT generation FROM crm.source_connections WHERE id=$1 AND provider=$2 AND enabled FOR UPDATE",
        )
        .bind(cid)
        .bind(provider.schema())
        .fetch_one(&mut **tx)
        .await?;
        let state:Option<(String,i64)>=sqlx::query_as("SELECT state,generation FROM crm.native_snapshots WHERE connection_id=$1 AND stream=$2 AND snapshot_id=$3").bind(cid).bind(stream).bind(snapshot.snapshot_id).fetch_optional(&mut **tx).await?;
        match state {
            Some((state, _)) if state == "complete" => return Ok(0),
            Some((state, old_generation))
                if state == "superseded" || (old_generation != generation && !snapshot.reset) =>
            {
                bail!("snapshot superseded")
            }
            None => {
                if !snapshot.reset {
                    bail!("snapshot reset required")
                }
                sqlx::query("UPDATE crm.native_snapshots SET state='superseded' WHERE connection_id=$1 AND stream=$2 AND state IN ('collecting','finalizing')").bind(cid).bind(stream).execute(&mut **tx).await?;
                sqlx::query("INSERT INTO crm.native_snapshots(connection_id,stream,snapshot_id,state,generation) VALUES($1,$2,$3,'collecting',$4)").bind(cid).bind(stream).bind(snapshot.snapshot_id).bind(generation).execute(&mut **tx).await?;
            }
            _ => {}
        }
        sqlx::query("UPDATE crm.native_snapshots SET generation=$4 WHERE connection_id=$1 AND stream=$2 AND snapshot_id=$3").bind(cid).bind(stream).bind(snapshot.snapshot_id).bind(generation).execute(&mut **tx).await?;
        let ids: Vec<&str> = records
            .iter()
            .map(|r| {
                r.get("external_id")
                    .and_then(Value::as_str)
                    .context("record external_id required")
            })
            .collect::<Result<_>>()?;
        sqlx::query("INSERT INTO crm.native_snapshot_seen SELECT $1,$2,$3,unnest($4::text[]) ON CONFLICT DO NOTHING").bind(cid).bind(stream).bind(snapshot.snapshot_id).bind(ids).execute(&mut **tx).await?;
        if snapshot.snapshot_complete {
            sqlx::query("UPDATE crm.native_snapshots SET state='finalizing' WHERE connection_id=$1 AND stream=$2 AND snapshot_id=$3 AND state='collecting'").bind(cid).bind(stream).bind(snapshot.snapshot_id).execute(&mut **tx).await?;
        }
    }
    commit_page(tx, provider, cid, stream, records, next_cursor).await
}
/// Reconcile at most `limit` missing contacts from one completed snapshot.
/// The keyset checkpoint and generated tombstones commit together; delivery uses
/// the ordinary journal, so a crash cannot acknowledge an unprojected deletion.
pub async fn finish_native_snapshots(pool: &PgPool, limit: i64) -> Result<usize> {
    finish_native_snapshots_bounded(pool, limit, 4 * 1024 * 1024).await
}
pub async fn finish_native_snapshots_bounded(
    pool: &PgPool,
    limit: i64,
    max_bytes: i64,
) -> Result<usize> {
    let candidate:Option<(Uuid,String,Uuid)>=sqlx::query_as("SELECT s.connection_id,s.stream,s.snapshot_id FROM crm.native_snapshots s JOIN crm.source_connections c ON c.id=s.connection_id AND c.enabled AND c.generation=s.generation WHERE s.state='finalizing' ORDER BY s.created_at,s.connection_id LIMIT 1").fetch_optional(pool).await?;
    let Some((cid, stream, sid)) = candidate else {
        return Ok(0);
    };
    let mut tx = pool.begin().await?;
    lock_connection(&mut tx, cid).await?;
    let state:Option<(String,String,i64,i64)>=sqlx::query_as("SELECT s.state,s.after_external_id,s.generation,c.generation FROM crm.native_snapshots s JOIN crm.source_connections c ON c.id=s.connection_id WHERE s.connection_id=$1 AND s.stream=$2 AND s.snapshot_id=$3 FOR UPDATE OF s,c").bind(cid).bind(&stream).bind(sid).fetch_optional(&mut *tx).await?;
    let Some((state, after, generation, current)) = state else {
        return Ok(0);
    };
    if state != "finalizing" {
        return Ok(0);
    }
    if generation != current {
        return Ok(0);
    }
    let enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM crm.source_connections WHERE id=$1")
            .bind(cid)
            .fetch_one(&mut *tx)
            .await?;
    if !enabled {
        return Ok(0);
    }
    let limit = limit.clamp(1, 1000);
    let rows:Vec<(String,Value)>=sqlx::query_as("WITH candidates AS MATERIALIZED(SELECT r.external_id,r.content_hash,p.byte_length FROM apple_contacts.connection_record_heads r JOIN apple_contacts.connection_payloads p USING(content_hash) WHERE r.connection_id=$1 AND r.external_id>$4 AND p.payload->>'kind'='contact' AND coalesce(p.payload->>'deleted','false') NOT IN ('true','1') AND NOT EXISTS(SELECT 1 FROM crm.native_snapshot_seen s WHERE s.connection_id=$1 AND s.stream=$2 AND s.snapshot_id=$3 AND s.external_id=r.external_id) ORDER BY r.external_id LIMIT $5),sized AS MATERIALIZED(SELECT *,sum(byte_length) OVER(ORDER BY external_id) total,row_number() OVER(ORDER BY external_id) position FROM candidates),chosen AS MATERIALIZED(SELECT * FROM sized WHERE total<=$6 OR position=1) SELECT c.external_id,p.payload FROM chosen c JOIN apple_contacts.connection_payloads p USING(content_hash) ORDER BY c.external_id").bind(cid).bind(&stream).bind(sid).bind(after).bind(limit).bind(max_bytes.max(1)).fetch_all(&mut *tx).await?;
    let records: Vec<Value> = rows
        .iter()
        .map(|(_, v)| {
            let mut value = v.clone();
            value["deleted"] = json!(true);
            value
        })
        .collect();
    if !records.is_empty() {
        commit_page(
            &mut tx,
            Provider::AppleContacts,
            cid,
            "_snapshot_sweep",
            &records,
            &json!({"snapshot_id":sid,"after":rows.last().unwrap().0}),
        )
        .await?;
    }
    let complete = rows.is_empty();
    sqlx::query("UPDATE crm.native_snapshots SET state=CASE WHEN $4 THEN 'complete' ELSE 'finalizing' END,completed_at=CASE WHEN $4 THEN now() ELSE NULL END,after_external_id=coalesce($5,after_external_id) WHERE connection_id=$1 AND stream=$2 AND snapshot_id=$3").bind(cid).bind(&stream).bind(sid).bind(complete).bind(rows.last().map(|r|&r.0)).execute(&mut *tx).await?;
    if complete {
        sqlx::query("DELETE FROM crm.native_snapshot_seen WHERE connection_id=$1 AND stream=$2 AND snapshot_id=$3").bind(cid).bind(&stream).bind(sid).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(rows.len().max(1))
}

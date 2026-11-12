//! Durable shared-volume IPC for optional external collector containers.
//! Import receipts commit before offsets/ACKs; replay after any crash is safe.
use super::{Config, Lease};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use uuid::Uuid;
const MAX_PAGE: u64 = 16 * 1024 * 1024;
fn identity(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".jsonl")?;
    let (sequence, hash) = stem.split_once('-')?;
    if sequence.len() != 20
        || !sequence.bytes().all(|b| b.is_ascii_digit())
        || hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    Some(hash)
}
fn signature(metadata: &std::fs::Metadata) -> Value {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        json!([
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ])
    }
    #[cfg(not(unix))]
    {
        json!([
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|t| t.as_nanos().to_string())
        ])
    }
}
async fn pages(root: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = tokio::fs::read_dir(root).await?;
    let mut found = std::collections::BTreeSet::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if identity(name).is_none() || !entry.file_type().await?.is_file() {
            continue;
        }
        if !tokio::fs::try_exists(entry.path().with_extension("ready")).await? {
            continue;
        }
        found.insert(entry.path());
        if found.len() > 128 {
            found.pop_last();
        }
    }
    Ok(found.into_iter().collect())
}
async fn run(pool: &PgPool, provider: &str) -> Result<Option<Lease>> {
    let mut tx = pool.begin().await?;
    let allowed: Option<bool> = sqlx::query_scalar(
        "SELECT enabled AND NOT paused AND (provider<>'linkedin_messages' OR EXISTS(SELECT 1 FROM crm.adapter_controls parent WHERE parent.provider='linkedin' AND parent.enabled AND NOT parent.paused)) FROM crm.adapter_controls WHERE provider=$1 FOR UPDATE",
    )
    .bind(provider)
    .fetch_optional(&mut *tx)
    .await?;
    if allowed != Some(true) {
        return Ok(None);
    }
    // Reading already-collected local pages has no platform request interval.
    sqlx::query("INSERT INTO crm.adapter_runs(id,provider,progress) SELECT $1,$2,'{\"phase\":\"publishing\"}'::jsonb WHERE NOT EXISTS(SELECT 1 FROM crm.adapter_runs WHERE provider=$2 AND status IN ('queued','running'))").bind(Uuid::new_v4()).bind(provider).execute(&mut *tx).await?;
    tx.commit().await?;
    super::claim(pool, provider).await
}
async fn ack(path: &Path, page: &str) -> Result<()> {
    let target = path.with_extension("ack");
    let temp = path.with_extension("ack.new");
    tokio::fs::write(
        &temp,
        serde_json::to_vec(&json!({"version":1,"page_id":page,"status":"complete"}))?,
    )
    .await?;
    tokio::fs::File::open(&temp).await?.sync_all().await?;
    tokio::fs::rename(&temp, target).await?;
    tokio::fs::rename(path, path.with_extension("consumed")).await?;
    let parent = path.parent().context("spool parent")?.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::File::open(parent)?.sync_all()).await??;
    Ok(())
}
pub async fn consume(
    pool: &PgPool,
    provider: &str,
    spool: &Path,
    work: &Path,
    resources: &crate::resources::ResourceBudget,
) -> Result<bool> {
    ensure!(
        spool.is_absolute() && work.is_absolute(),
        "spool paths must be absolute"
    );
    let allowed: bool = sqlx::query_scalar(
        "SELECT enabled AND NOT paused AND (provider<>'linkedin_messages' OR EXISTS(SELECT 1 FROM crm.adapter_controls parent WHERE parent.provider='linkedin' AND parent.enabled AND NOT parent.paused)) FROM crm.adapter_controls WHERE provider=$1",
    )
    .bind(provider)
    .fetch_one(pool)
    .await?;
    if !allowed {
        return Ok(false);
    }
    if !tokio::fs::try_exists(spool).await? {
        return Ok(false);
    }
    let roots = tokio::fs::canonicalize(work).await?;
    let spool = tokio::fs::canonicalize(spool).await?;
    ensure!(
        spool.starts_with(&roots),
        "spool outside explicit adapter data root"
    );
    for path in pages(&spool).await? {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("spool file name")?;
        let hash = identity(name).context("spool page identity")?;
        let page = path
            .file_stem()
            .and_then(|s| s.to_str())
            .context("spool stem")?;
        let state =
            sqlx::query("SELECT * FROM crm.adapter_spool_pages WHERE provider=$1 AND page_id=$2")
                .bind(provider)
                .bind(page)
                .fetch_optional(pool)
                .await?;
        if let Some(row) = &state {
            match row.get::<String, _>("status").as_str() {
                "complete" => {
                    ack(&path, page).await?;
                    return Ok(true);
                }
                "failed" => continue,
                _ => {}
            }
        }
        let Some(lease) = run(pool, provider).await? else {
            return Ok(false);
        };
        let _permit = resources
            .acquire_background(crate::resources::TaskResources {
                cpu: 1,
                disk: 1,
                memory_bytes: 32 * 1024 * 1024,
                ..Default::default()
            })
            .await?;
        let result = consume_page(pool, &lease, &path, page, hash, &roots, state).await;
        match result {
            Ok((offset, complete)) => {
                let mut tx = pool.begin().await?;
                if super::lease_guard(&mut tx, &lease).await.is_err() {
                    tx.rollback().await?;
                    release_interrupted(pool, &lease).await?;
                    return Ok(true);
                }
                sqlx::query("UPDATE crm.adapter_spool_pages SET byte_offset=$3,status=$4,run_id=$5,updated_at=now() WHERE provider=$1 AND page_id=$2").bind(provider).bind(page).bind(offset as i64).bind(if complete{"complete"}else{"queued"}).bind(lease.id).execute(&mut *tx).await?;
                sqlx::query("UPDATE crm.adapter_runs SET status=$3,token=NULL,lease_until=NULL,attempts=greatest(0,attempts-1),completed_at=CASE WHEN $4 THEN now() END,progress=progress||jsonb_build_object('page_id',$5::text,'byte_offset',$6::bigint) WHERE id=$1 AND token=$2").bind(lease.id).bind(lease.token).bind(if complete{"complete"}else{"queued"}).bind(complete).bind(page).bind(offset as i64).execute(&mut *tx).await?;
                tx.commit().await?;
                if complete {
                    ack(&path, page).await?;
                }
                return Ok(true);
            }
            Err(error) => {
                let mut renewed = tokio::time::Instant::now();
                if error.is::<super::Interrupted>()
                    || error.is::<sqlx::Error>()
                    || !super::keep_alive(pool, &lease, &mut renewed).await?
                {
                    // Pause/cancel/takeover is retryable. Do not quarantine an
                    // otherwise valid source page as permanently corrupt.
                    release_interrupted(pool, &lease).await?;
                    return Ok(true);
                }
                let mut tx = pool.begin().await?;
                let changed=sqlx::query("UPDATE crm.adapter_runs SET status='failed',error='spool page validation or protocol failed',token=NULL,lease_until=NULL,completed_at=now() WHERE id=$1 AND token=$2 AND status='running' AND lease_until>clock_timestamp()").bind(lease.id).bind(lease.token).execute(&mut *tx).await?.rows_affected();
                if changed == 1 {
                    sqlx::query("INSERT INTO crm.adapter_spool_pages(provider,page_id,sha256,signature,status,error,run_id) VALUES($1,$2,$3,'null'::jsonb,'failed','spool page validation or protocol failed',$4) ON CONFLICT(provider,page_id) DO UPDATE SET status='failed',error=excluded.error").bind(provider).bind(page).bind(hash).bind(lease.id).execute(&mut *tx).await?;
                }
                tx.commit().await?;
                return Ok(true);
            }
        }
    }
    Ok(false)
}
async fn release_interrupted(pool: &PgPool, lease: &Lease) -> Result<()> {
    sqlx::query("UPDATE crm.adapter_runs SET status='cancelled',token=NULL,lease_until=NULL,completed_at=now() WHERE id=$1 AND token=$2 AND status='running'").bind(lease.id).bind(lease.token).execute(pool).await?;
    Ok(())
}
async fn consume_page(
    pool: &PgPool,
    lease: &Lease,
    path: &Path,
    page: &str,
    expected: &str,
    work: &Path,
    state: Option<sqlx::postgres::PgRow>,
) -> Result<(u64, bool)> {
    let meta = tokio::fs::symlink_metadata(path).await?;
    ensure!(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.len() > 0
            && meta.len() <= MAX_PAGE,
        "spool page size/type invalid"
    );
    let sig = signature(&meta);
    let marker = path.with_extension("ready");
    let marker_meta = tokio::fs::symlink_metadata(&marker).await?;
    ensure!(
        !marker_meta.file_type().is_symlink() && marker_meta.len() <= 1024,
        "spool marker invalid"
    );
    let marker: Value = serde_json::from_slice(&tokio::fs::read(marker).await?)?;
    ensure!(
        marker["version"] == 1
            && marker["sha256"] == expected
            && marker["byte_length"].as_u64() == Some(meta.len()),
        "spool marker mismatch"
    );
    if state
        .as_ref()
        .is_none_or(|row| row.get::<Value, _>("signature") != sig)
    {
        let mut input = tokio::fs::File::open(path).await?.take(MAX_PAGE + 1);
        let mut bytes = [0u8; 65536];
        let mut hash = Sha256::new();
        loop {
            let n = input.read(&mut bytes).await?;
            if n == 0 {
                break;
            }
            hash.update(&bytes[..n]);
        }
        ensure!(
            format!("{:x}", hash.finalize()) == expected
                && signature(&tokio::fs::metadata(path).await?) == sig,
            "spool page changed or SHA mismatch"
        );
    }
    sqlx::query("INSERT INTO crm.adapter_spool_pages(provider,page_id,sha256,signature,run_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT(provider,page_id) DO UPDATE SET signature=excluded.signature,run_id=excluded.run_id").bind(&lease.provider).bind(page).bind(expected).bind(&sig).bind(lease.id).execute(pool).await?;
    let mut offset = state
        .as_ref()
        .map(|row| row.get::<i64, _>("byte_offset") as u64)
        .unwrap_or(0);
    ensure!(offset <= meta.len(), "spool cursor exceeds file");
    let mut input = tokio::fs::File::open(path).await?;
    input.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut input = BufReader::new(input);
    let config = Config {
        executable: PathBuf::from("/unused-spool-process"),
        args: vec![],
        work_dir: work.into(),
        deadline: Duration::from_secs(60),
    };
    for _ in 0..25 {
        let mut line = Vec::new();
        let n = (&mut input)
            .take(4 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            break;
        }
        ensure!(n <= 4 * 1024 * 1024, "spool frame exceeds budget");
        super::event(pool, lease, serde_json::from_slice(&line)?, &config).await?;
        offset += n as u64;
    }
    ensure!(
        signature(&tokio::fs::metadata(path).await?) == sig,
        "spool page changed during consumption"
    );
    Ok((offset, offset == meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_page_name_contract() {
        assert!(identity(&format!("{:020}-{}.jsonl", 1, "a".repeat(64))).is_some());
        assert!(identity("../page.jsonl").is_none());
        assert!(identity(&format!("1-{}.jsonl", "a".repeat(64))).is_none());
    }
}

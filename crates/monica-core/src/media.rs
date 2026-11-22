//! Durable content-addressed media, resumable uploads and isolated inspection.
#[path = "wechat_avatars.rs"]
mod wechat_avatars;
pub use wechat_avatars::refresh_wechat_avatar;
#[path = "media_import.rs"]
mod import;
#[path = "media_jobs.rs"]
mod jobs;
pub use import::{consume_record_media, validate_record_media};
#[path = "media_probe.rs"]
mod probe;
use crate::resources::{ResourceBudget, TaskResources};
use anyhow::{Context, Result, ensure};
pub use jobs::{consume_imports, refresh_profiles, resolve_dependencies};
pub use probe::Inspection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

pub const MAX_FILE: u64 = 500 * 1024 * 1024;
pub const MAX_CHUNK: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct MediaError(pub u16, pub &'static str);
impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}
impl std::error::Error for MediaError {}
pub(crate) fn fail(code: u16, message: &'static str) -> anyhow::Error {
    MediaError(code, message).into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asset {
    pub id: Uuid,
    pub sha256: String,
    pub media_type: String,
    pub byte_length: i64,
    pub object_path: String,
    pub file_name: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_ms: Option<i64>,
}
#[derive(Clone)]
pub struct MediaStore {
    pub pool: PgPool,
    pub root: PathBuf,
    pub resources: ResourceBudget,
    pub helper: PathBuf,
}

pub fn probe_entry() -> Result<bool> {
    probe::entry()
}
pub fn clean_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or("file")
        .chars()
        .filter(|x| !x.is_control())
        .take(255)
        .collect()
}
pub fn object_path(hash: &str) -> Result<String> {
    ensure!(
        hash.len() == 64
            && hash
                .bytes()
                .all(|x| x.is_ascii_digit() || matches!(x, b'a'..=b'f')),
        "invalid media digest"
    );
    Ok(format!("sha256/{}/{}/{}", &hash[..2], &hash[2..4], hash))
}
pub async fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut chunk = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        size += read as u64;
        ensure!(size <= MAX_FILE, "media exceeds maximum file size");
        hash.update(&chunk[..read]);
    }
    Ok((format!("{:x}", hash.finalize()), size))
}
async fn sync_directory(path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || std::fs::File::open(path)?.sync_all()).await??;
    Ok(())
}
// Cancellation must kill the decoder AND any ffprobe child, not just the parent.
struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

impl MediaStore {
    pub fn new(pool: PgPool, root: PathBuf, resources: ResourceBudget) -> Result<Self> {
        Ok(Self {
            pool,
            root,
            resources,
            helper: std::env::current_exe()?,
        })
    }
    pub async fn inspect(&self, path: &Path, name: &str) -> Result<Inspection> {
        Ok(serde_json::from_slice(
            &self
                .helper_run(&[
                    "--monica-media-probe".as_ref(),
                    path.as_os_str(),
                    name.as_ref(),
                ])
                .await?,
        )?)
    }
    async fn helper_run(&self, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>> {
        let mut command = tokio::process::Command::new(&self.helper);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let mut child = command
            .spawn()
            .context("cannot start isolated media decoder")?;
        let group = ProcessGroup(child.id());
        let stdout = child.stdout.take().context("decoder output missing")?;
        let status = match tokio::time::timeout(Duration::from_secs(8), child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                drop(group);
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(fail(422, "media inspection exceeded its execution limit"));
            }
        };
        // Kill any unexpected surviving descendants even when the helper exits.
        drop(group);
        if !status.success() {
            return Err(fail(
                422,
                "Upload a valid photo or video; required decoder must be installed",
            ));
        }
        let mut output = Vec::new();
        stdout.take(16385).read_to_end(&mut output).await?;
        ensure!(
            output.len() <= 16384,
            "media inspector returned excessive output"
        );
        Ok(output)
    }
    pub async fn preview(&self, asset: &Asset, crop: Option<&[f32]>) -> Result<(PathBuf, String)> {
        if !matches!(
            asset.media_type.as_str(),
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) {
            return Err(fail(422, "preview requires a supported image"));
        }
        let geometry = serde_json::to_string(&crop)?;
        let key = format!(
            "preview-v1-{}-{:x}",
            asset.sha256,
            Sha256::digest(geometry.as_bytes())
        );
        let folder = self.root.join(".previews").join(&asset.sha256[..2]);
        let target = folder.join(format!("{key}.webp"));
        if tokio::fs::try_exists(&target).await? {
            return Ok((target, key));
        }
        let _permit = self
            .resources
            .acquire(TaskResources {
                cpu: 1,
                disk: 1,
                memory_bytes: 192 * 1024 * 1024,
                ..Default::default()
            })
            .await?;
        tokio::fs::create_dir_all(&folder).await?;
        let temp = tempfile::NamedTempFile::new_in(&folder)?;
        let source = self.root.join(object_path(&asset.sha256)?);
        self.helper_run(&[
            "--monica-media-preview".as_ref(),
            source.as_os_str(),
            temp.path().as_os_str(),
            geometry.as_ref(),
        ])
        .await?;
        match tokio::fs::hard_link(temp.path(), &target).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        sync_directory(folder).await?;
        Ok((target, key))
    }
    pub async fn ingest_path(
        &self,
        path: &Path,
        name: Option<&str>,
        source: &str,
        source_record: Option<&str>,
        expected_hash: Option<&str>,
        expected_size: Option<u64>,
    ) -> Result<Asset> {
        let _permit = self
            .resources
            .acquire(TaskResources {
                cpu: 1,
                disk: 1,
                memory_bytes: 192 * 1024 * 1024,
                ..Default::default()
            })
            .await?;
        let (hash, size) = hash_file(path).await?;
        if size == 0 {
            return Err(fail(422, "nonempty file required"));
        }
        if expected_hash.is_some_and(|x| x != hash) {
            return Err(fail(422, "media sha256 does not match expected value"));
        }
        if expected_size.is_some_and(|x| x != size) {
            return Err(fail(422, "media byte length does not match expected value"));
        }
        let inspection = self.inspect(path, name.unwrap_or("")).await?;
        let asset = Asset {
            id: Uuid::new_v4(),
            sha256: hash,
            media_type: inspection.media_type,
            byte_length: size as i64,
            object_path: String::new(),
            file_name: name.map(clean_name),
            width: inspection.width.map(|x| x as i32),
            height: inspection.height.map(|x| x as i32),
            duration_ms: inspection.duration_ms.map(|x| x as i64),
        };
        self.publish(path, asset, source, source_record).await
    }
    /// Validated bytes are journaled before publication, then metadata is committed
    /// only after file/directory fsync. Recovery can finish the exact same record.
    pub async fn publish(
        &self,
        path: &Path,
        mut asset: Asset,
        source: &str,
        source_record: Option<&str>,
    ) -> Result<Asset> {
        asset.object_path = object_path(&asset.sha256)?;
        let manifest = json!({"asset":asset,"source":source,"record":source_record});
        let mut journal = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('media-gc:'||$1,0))")
            .bind(&asset.sha256)
            .execute(&mut *journal)
            .await?;
        sqlx::query("INSERT INTO crm.media_publications(sha256,manifest) VALUES($1,$2) ON CONFLICT(sha256) DO UPDATE SET manifest=excluded.manifest,status='pending',error=NULL,updated_at=clock_timestamp()")
            .bind(&asset.sha256).bind(&manifest).execute(&mut *journal).await?;
        journal.commit().await?;
        let target = self.root.join(&asset.object_path);
        let parent = target.parent().context("media parent missing")?;
        tokio::fs::create_dir_all(parent).await?;
        let input = tokio::fs::File::open(path).await?;
        input.sync_all().await?;
        drop(input);
        match tokio::fs::hard_link(path, &target).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let (existing, bytes) = hash_file(&target).await?;
                ensure!(
                    existing == asset.sha256 && bytes == asset.byte_length as u64,
                    "existing content-addressed blob is corrupt"
                );
            }
            Err(error) => return Err(error.into()),
        }
        let mut current = Some(parent);
        while let Some(dir) = current {
            sync_directory(dir.to_path_buf()).await?;
            if dir == self.root {
                break;
            }
            current = dir.parent();
        }
        self.register(&manifest).await
    }
    async fn register(&self, manifest: &Value) -> Result<Asset> {
        let asset: Asset = serde_json::from_value(manifest["asset"].clone())?;
        let source = manifest["source"].as_str().unwrap_or("manual");
        let record = manifest["record"].as_str().unwrap_or(&asset.sha256);
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,file_name,width,height,source_type,source_record_id,duration_ms,verified_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,now()) ON CONFLICT(sha256) DO UPDATE SET media_type=excluded.media_type,width=excluded.width,height=excluded.height,duration_ms=excluded.duration_ms,file_name=coalesce(media_assets.file_name,excluded.file_name),verified_at=excluded.verified_at WHERE media_assets.media_type='application/octet-stream' AND excluded.media_type<>'application/octet-stream'")
            .bind(asset.id).bind(&asset.sha256).bind(&asset.media_type).bind(asset.byte_length).bind(&asset.object_path).bind(&asset.file_name).bind(asset.width).bind(asset.height).bind(source).bind(record).bind(asset.duration_ms).execute(&mut *tx).await?;
        let saved:Value=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'sha256',sha256,'media_type',media_type,'byte_length',byte_length,'object_path',object_path,'file_name',file_name,'width',width,'height',height,'duration_ms',duration_ms) FROM crm.media_assets WHERE sha256=$1").bind(&asset.sha256).fetch_one(&mut *tx).await?;
        let saved: Asset = serde_json::from_value(saved)?;
        sqlx::query("INSERT INTO crm.media_provenance(media_id,source,source_record_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(saved.id).bind(source).bind(record).execute(&mut *tx).await?;
        sqlx::query("UPDATE crm.media_publications SET status='complete',updated_at=clock_timestamp() WHERE sha256=$1").bind(&asset.sha256).execute(&mut *tx).await?;
        sqlx::query("UPDATE crm.media_dependencies SET next_attempt_at=now() WHERE state='waiting' AND record->>'sha256'=$1").bind(&asset.sha256).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(saved)
    }
    pub async fn recover_publications(&self) -> Result<usize> {
        // A journal whose publisher died before writing bytes is retryable by a
        // later upload; it must not monopolize recovery's first page forever.
        let rows:Vec<Value>=sqlx::query_scalar("SELECT manifest FROM crm.media_publications WHERE status='pending' ORDER BY updated_at LIMIT 10").fetch_all(&self.pool).await?;
        let mut recovered = 0;
        for row in rows {
            let asset: Asset = serde_json::from_value(row["asset"].clone())?;
            let path = self.root.join(object_path(&asset.sha256)?);
            if !tokio::fs::try_exists(&path).await? {
                sqlx::query("UPDATE crm.media_publications SET status='failed',error='publication interrupted before writing bytes' WHERE sha256=$1 AND status='pending' AND updated_at<now()-interval '1 hour'").bind(&asset.sha256).execute(&self.pool).await?;
                continue;
            }
            let _permit = self
                .resources
                .acquire(TaskResources {
                    disk: 1,
                    memory_bytes: 256 * 1024,
                    ..Default::default()
                })
                .await?;
            let (hash, size) = hash_file(&path).await?;
            if hash != asset.sha256 || size != asset.byte_length as u64 {
                sqlx::query("UPDATE crm.media_publications SET status='failed',error='blob verification failed',updated_at=now() WHERE sha256=$1").bind(asset.sha256).execute(&self.pool).await?;
                continue;
            }
            self.register(&row).await?;
            recovered += 1;
        }
        Ok(recovered)
    }
    pub async fn initiate(
        &self,
        actor: Uuid,
        expected_hash: Option<String>,
        expected_size: Option<u64>,
        media_type: String,
        name: Option<String>,
    ) -> Result<Value> {
        if expected_hash
            .as_ref()
            .is_some_and(|x| object_path(x).is_err())
            || expected_size.is_some_and(|x| x > MAX_FILE)
            || media_type.len() > 255
        {
            return Err(fail(422, "invalid upload expectations"));
        }
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.media_uploads(id,expected_sha256,expected_byte_length,media_type,file_name,created_by) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(expected_hash).bind(expected_size.map(|x|x as i64)).bind(media_type).bind(name.map(|x|clean_name(&x))).bind(actor).execute(&self.pool).await?;
        Ok(json!({"upload_id":id,"next_chunk":0}))
    }
    pub async fn upload_status(&self, id: Uuid, actor: Uuid) -> Result<Value> {
        sqlx::query_scalar("SELECT jsonb_build_object('id',id,'expected_sha256',expected_sha256,'media_type',media_type,'expected_byte_length',expected_byte_length,'next_chunk',next_chunk,'received_bytes',received_bytes,'status',status,'created_at',created_at,'completed_at',completed_at) FROM crm.media_uploads WHERE id=$1 AND created_by=$2").bind(id).bind(actor).fetch_optional(&self.pool).await?.ok_or_else(||fail(404,"upload not found"))
    }
    pub async fn chunk(&self, id: Uuid, actor: Uuid, number: i32, bytes: &[u8]) -> Result<Value> {
        if number < 0 || bytes.is_empty() || bytes.len() > MAX_CHUNK {
            return Err(fail(422, "invalid or oversized upload chunk"));
        }
        let authorized:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.media_uploads WHERE id=$1 AND created_by=$2 AND expires_at>now())").bind(id).bind(actor).fetch_one(&self.pool).await?;
        if !authorized {
            return Err(fail(404, "upload not found"));
        }
        let _permit = self
            .resources
            .acquire(TaskResources {
                disk: 1,
                // The request owns and budgets its bytes before entering here.
                memory_bytes: 0,
                ..Default::default()
            })
            .await?;
        let hash = format!("{:x}", Sha256::digest(bytes));
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("SELECT next_chunk,received_bytes,expected_byte_length,status FROM crm.media_uploads WHERE id=$1 AND created_by=$2 AND expires_at>now() FOR UPDATE").bind(id).bind(actor).fetch_optional(&mut *tx).await?.ok_or_else(||fail(404,"upload not found"))?;
        let next: i32 = row.get("next_chunk");
        let received: i64 = row.get("received_bytes");
        if number < next {
            let previous: Option<String> = sqlx::query_scalar(
                "SELECT sha256 FROM crm.media_upload_chunks WHERE upload_id=$1 AND chunk=$2",
            )
            .bind(id)
            .bind(number)
            .fetch_optional(&mut *tx)
            .await?;
            if previous.as_deref() != Some(&hash) {
                return Err(fail(409, "chunk replay differs from saved bytes"));
            }
            return Ok(json!({"upload_id":id,"next_chunk":next,"received_bytes":received}));
        }
        if row.get::<String, _>("status") != "uploading" || number != next {
            return Err(fail(409, "upload is inactive or chunk is out of order"));
        }
        let total = received + bytes.len() as i64;
        if total > MAX_FILE as i64
            || row
                .get::<Option<i64>, _>("expected_byte_length")
                .is_some_and(|x| total > x)
        {
            return Err(fail(413, "upload exceeds expected or maximum length"));
        }
        let folder = self.root.join(".staging/uploads").join(id.to_string());
        tokio::fs::create_dir_all(&folder).await?;
        let relative = format!(".staging/uploads/{id}/{number}-{hash}");
        let target = self.root.join(&relative);
        let temp = tempfile::NamedTempFile::new_in(&folder)?;
        let mut file = tokio::fs::File::create(temp.path()).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        match tokio::fs::hard_link(temp.path(), &target).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        };
        sync_directory(folder).await?;
        sqlx::query("INSERT INTO crm.media_upload_chunks VALUES($1,$2,$3,$4,$5)")
            .bind(id)
            .bind(number)
            .bind(&hash)
            .bind(bytes.len() as i64)
            .bind(relative)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE crm.media_uploads SET next_chunk=next_chunk+1,received_bytes=$2 WHERE id=$1",
        )
        .bind(id)
        .bind(total)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(json!({"upload_id":id,"next_chunk":next+1,"received_bytes":total}))
    }
    pub async fn complete(&self, id: Uuid, actor: Uuid) -> Result<Asset> {
        let row = sqlx::query(
            "SELECT * FROM crm.media_uploads WHERE id=$1 AND created_by=$2 AND expires_at>now()",
        )
        .bind(id)
        .bind(actor)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| fail(404, "upload not found"))?;
        if let Some(media) = row.get::<Option<Uuid>, _>("media_id") {
            return self.asset(media).await;
        }
        let next: i32 = row.get("next_chunk");
        if next == 0 {
            return Err(fail(409, "upload chunks are incomplete"));
        }
        let staging = self.root.join(".staging");
        tokio::fs::create_dir_all(&staging).await?;
        let temp = tempfile::NamedTempFile::new_in(&staging)?;
        {
            let _permit = self
                .resources
                .acquire(TaskResources {
                    disk: 1,
                    memory_bytes: 512 * 1024,
                    ..Default::default()
                })
                .await?;
            let mut output = tokio::fs::File::create(temp.path()).await?;
            // Fetch only a bounded page of immutable chunk metadata and release
            // the database connection before touching disk.
            for start in (0..next).step_by(128) {
                let end = start.saturating_add(128).min(next);
                let chunks=sqlx::query("SELECT chunk,sha256,byte_length,object_path FROM crm.media_upload_chunks WHERE upload_id=$1 AND chunk>=$2 AND chunk<$3 ORDER BY chunk").bind(id).bind(start).bind(end).fetch_all(&self.pool).await?;
                if chunks.len() != (end - start) as usize {
                    return Err(fail(409, "upload chunks are incomplete"));
                }
                for (offset, chunk) in chunks.iter().enumerate() {
                    if chunk.get::<i32, _>("chunk") != start + offset as i32 {
                        return Err(fail(409, "upload chunk sequence is incomplete"));
                    }
                    let path = self.root.join(chunk.get::<String, _>("object_path"));
                    let (hash, size) = hash_file(&path).await?;
                    if hash != chunk.get::<String, _>("sha256")
                        || size != chunk.get::<i64, _>("byte_length") as u64
                    {
                        return Err(fail(422, "saved upload chunk is corrupt"));
                    }
                    let mut input = tokio::fs::File::open(path).await?;
                    tokio::io::copy(&mut input, &mut output).await?;
                }
            }
            output.sync_all().await?;
        }
        let name: Option<String> = row.get("file_name");
        let expected_hash: Option<String> = row.get("expected_sha256");
        let expected_size: Option<i64> = row.get("expected_byte_length");
        let asset = self
            .ingest_path(
                temp.path(),
                name.as_deref(),
                "manual",
                Some(&id.to_string()),
                expected_hash.as_deref(),
                expected_size.map(|x| x as u64),
            )
            .await?;
        let mut tx = self.pool.begin().await?;
        let current: i32 =
            sqlx::query_scalar("SELECT next_chunk FROM crm.media_uploads WHERE id=$1 AND status IN ('uploading','complete') AND expires_at>now() FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?.ok_or_else(||fail(409,"upload expired while completing"))?;
        if current != next {
            return Err(fail(
                409,
                "upload changed while completing; retry completion",
            ));
        }
        sqlx::query("UPDATE crm.media_uploads SET status='complete',media_id=$2,completed_at=now() WHERE id=$1").bind(id).bind(asset.id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(asset)
    }
    pub async fn asset(&self, id: Uuid) -> Result<Asset> {
        let row: Value =
            sqlx::query_scalar("SELECT to_jsonb(m) FROM crm.media_assets m WHERE id=$1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| fail(404, "media not found"))?;
        Ok(serde_json::from_value(row)?)
    }
    /// Claim expiry in SQL first, then remove only server-owned staging paths.
    /// A crash after the state change leaves another cleanup pass to finish it.
    pub async fn expire_uploads(&self) -> Result<usize> {
        let ids:Vec<Uuid>=sqlx::query_scalar("WITH selected AS (SELECT id FROM crm.media_uploads WHERE (expires_at<now() OR completed_at<now()-interval '1 day') AND status<>'cleaned' ORDER BY expires_at LIMIT 10 FOR UPDATE SKIP LOCKED) UPDATE crm.media_uploads u SET status='expired' FROM selected s WHERE u.id=s.id RETURNING u.id").fetch_all(&self.pool).await?;
        let mut count = 0;
        for id in ids {
            let _permit = self
                .resources
                .acquire(TaskResources {
                    disk: 1,
                    memory_bytes: 256 * 1024,
                    ..Default::default()
                })
                .await?;
            let path = self.root.join(".staging/uploads").join(id.to_string());
            match tokio::fs::remove_dir_all(path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let mut tx = self.pool.begin().await?;
            sqlx::query("DELETE FROM crm.media_upload_chunks WHERE upload_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE crm.media_uploads SET status='cleaned' WHERE id=$1 AND status='expired'",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            count += 1;
        }
        // Multipart temporaries have no durable row; only old direct children
        // are candidates. Upload directories are handled by their receipt above.
        if let Ok(mut entries) = tokio::fs::read_dir(self.root.join(".staging")).await {
            for _ in 0..100 {
                let Some(entry) = entries.next_entry().await? else {
                    break;
                };
                let meta = entry.metadata().await?;
                if entry.file_type().await?.is_dir()
                    && entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("mail-page-")
                    && meta.modified()?.elapsed().unwrap_or_default()
                        > Duration::from_secs(24 * 3600)
                {
                    tokio::fs::remove_dir_all(entry.path()).await?;
                    count += 1;
                    continue;
                }
                if meta.is_file()
                    && meta.modified()?.elapsed().unwrap_or_default()
                        > Duration::from_secs(24 * 3600)
                {
                    tokio::fs::remove_file(entry.path()).await?;
                    count += 1;
                }
            }
        }
        Ok(count)
    }
    /// One hash bucket per pass, at most 100 directory entries. Registered assets
    /// are retained; this removes only old unregistered and unjournaled bytes.
    pub async fn collect_orphans(&self, min_age: Duration) -> Result<usize> {
        let _permit = self
            .resources
            .acquire(TaskResources {
                disk: 1,
                memory_bytes: 256 * 1024,
                ..Default::default()
            })
            .await?;
        let row = sqlx::query("SELECT bucket,last_file FROM crm.media_maintenance WHERE singleton")
            .fetch_one(&self.pool)
            .await?;
        let bucket: i32 = row.get("bucket");
        let last: String = row.get("last_file");
        let path = self
            .root
            .join(format!("sha256/{:02x}/{:02x}", bucket / 256, bucket % 256));
        let mut selected = std::collections::BTreeSet::new();
        if let Ok(mut entries) = tokio::fs::read_dir(&path).await {
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name > last {
                    selected.insert(name);
                    if selected.len() > 101 {
                        selected.pop_last();
                    }
                }
            }
        }
        let more = selected.len() > 100;
        let names = selected.into_iter().take(100).collect::<Vec<_>>();
        let mut removed = 0;
        for name in &names {
            if object_path(name).is_err() {
                continue;
            }
            let file = path.join(name);
            let meta = tokio::fs::symlink_metadata(&file).await?;
            if !meta.is_file() || meta.modified()?.elapsed().unwrap_or_default() < min_age {
                continue;
            }
            let mut tx = self.pool.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('media-gc:'||$1,0))")
                .bind(name)
                .execute(&mut *tx)
                .await?;
            let protected:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.media_assets WHERE sha256=$1) OR EXISTS(SELECT 1 FROM crm.media_publications WHERE sha256=$1)").bind(name).fetch_one(&mut *tx).await?;
            if !protected {
                tokio::fs::remove_file(file).await?;
                removed += 1;
            }
            tx.commit().await?;
        }
        sqlx::query("UPDATE crm.media_maintenance SET bucket=$1,last_file=$2,updated_at=now() WHERE singleton").bind(if more{bucket}else{(bucket+1)%65536}).bind(if more{names.last().cloned().unwrap_or_default()}else{String::new()}).execute(&self.pool).await?;
        Ok(removed)
    }
}

/// Imported slots replace only their exact source attachment, preserving manual
/// additions and other references to the same content-addressed bytes.
pub async fn set_slot(
    pool: &PgPool,
    entity: &str,
    id: Uuid,
    role: &str,
    media: Uuid,
    source_account: Option<Uuid>,
) -> Result<()> {
    ensure!(!role.is_empty() && role.len() <= 120, "invalid media role");
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("media-slot:{entity}:{id}:{role}"))
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.media_slots(entity_type,entity_id,role,media_id) VALUES($1,$2,$3,$4) ON CONFLICT(entity_type,entity_id,role) DO UPDATE SET media_id=excluded.media_id,generation=crm.media_slots.generation+1,updated_at=now() WHERE crm.media_slots.media_id<>excluded.media_id").bind(entity).bind(id).bind(role).bind(media).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM crm.media_links WHERE entity_type=$1 AND entity_id=$2 AND role=$3 AND media_id<>$4").bind(entity).bind(id).bind(role).bind(media).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role,source_account_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(entity).bind(id).bind(media).bind(role).bind(source_account).execute(&mut *tx).await?;
    if entity == "source_account" {
        sqlx::query("INSERT INTO crm.media_profile_refresh(account_id) VALUES($1) ON CONFLICT(account_id) DO UPDATE SET generation=crm.media_profile_refresh.generation+1").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

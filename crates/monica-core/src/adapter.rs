//! Managed external runtimes. Stdout is a bounded JSONL protocol; raw stderr,
//! environment and command arguments are never stored or returned by the API.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
};
use uuid::Uuid;
#[path = "adapter_spool.rs"]
pub mod spool;

#[derive(Debug)]
struct Interrupted;
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("adapter paused, cancelled or lease lost")
    }
}
impl std::error::Error for Interrupted {}

#[derive(Clone, Debug)]
pub struct Lease {
    pub id: Uuid,
    pub provider: String,
    pub token: Uuid,
}
pub struct Config {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub work_dir: PathBuf,
    pub deadline: Duration,
}
fn source_provider(provider: &str) -> &str {
    match provider {
        "linkedin_messages" => "linkedin",
        "chatlog" => "wechat",
        _ => provider,
    }
}
async fn lease_guard(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, lease: &Lease) -> Result<()> {
    let providers = if lease.provider == "linkedin_messages" {
        vec!["linkedin", "linkedin_messages"]
    } else {
        vec![lease.provider.as_str()]
    };
    let controls=sqlx::query("SELECT enabled,paused FROM crm.adapter_controls WHERE provider=ANY($1) ORDER BY provider FOR SHARE").bind(&providers).fetch_all(&mut **tx).await?;
    if controls.len() != providers.len()
        || !controls
            .iter()
            .all(|r| r.get::<bool, _>("enabled") && !r.get::<bool, _>("paused"))
    {
        return Err(Interrupted.into());
    }
    if sqlx::query("SELECT id FROM crm.adapter_runs WHERE id=$1 AND token=$2 AND status='running' AND NOT cancel_requested AND lease_until>clock_timestamp() FOR UPDATE").bind(lease.id).bind(lease.token).fetch_optional(&mut **tx).await?.is_none() {return Err(Interrupted.into())}
    Ok(())
}
/// Independent bounded lane; profile collection/cooldowns cannot block this
/// actor. Parent LinkedIn pause is checked at claim, heartbeat and every commit.
pub async fn run_messages_configured(
    pool: &PgPool,
    resources: &crate::resources::ResourceBudget,
) -> Result<bool> {
    let enabled: bool = sqlx::query_scalar(
        "SELECT enabled AND NOT paused FROM crm.adapter_controls WHERE provider='linkedin'",
    )
    .fetch_one(pool)
    .await?;
    if !enabled {
        return Ok(false);
    }
    let work = std::env::var("MONICA_ADAPTER_LINKEDIN_MESSAGES_WORK_DIR")
        .ok()
        .or_else(|| std::env::var("MONICA_COLLECTOR_DATA_ROOT").ok());
    if let (Ok(path), Some(work)) = (
        std::env::var("MONICA_ADAPTER_LINKEDIN_MESSAGES_SPOOL"),
        work,
    ) && spool::consume(
        pool,
        "linkedin_messages",
        std::path::Path::new(&path),
        std::path::Path::new(&work),
        resources,
    )
    .await?
    {
        return Ok(true);
    }
    let Some(config) = runtime_config("linkedin_messages")? else {
        return Ok(false);
    };
    if enqueue(pool, "linkedin_messages").await.is_err() {
        return Ok(false);
    }
    let _permit = resources
        .acquire_background(crate::resources::TaskResources {
            cpu: 1,
            network: 1,
            memory_bytes: 32 * 1024 * 1024,
            ..Default::default()
        })
        .await?;
    run_one(pool, "linkedin_messages", &config).await
}
fn runtime_config(provider: &str) -> Result<Option<Config>> {
    let prefix = format!("MONICA_ADAPTER_{}", provider.to_uppercase());
    let explicit = std::env::var(format!("{prefix}_EXECUTABLE")).ok();
    let work = std::env::var(format!("{prefix}_WORK_DIR"))
        .ok()
        .or_else(|| std::env::var("MONICA_COLLECTOR_DATA_ROOT").ok());
    let bundled = matches!(
        provider,
        "instagram" | "linkedin" | "linkedin_messages" | "wechat"
    )
    .then(|| std::env::var("MONICA_COLLECTOR_BRIDGE").ok())
    .flatten();
    let (Some(executable), Some(work)) = (explicit.clone().or(bundled), work) else {
        return Ok(None);
    };
    let args = if let Ok(args) = std::env::var(format!("{prefix}_ARGS_JSON")) {
        serde_json::from_str::<Vec<String>>(&args)?
    } else if explicit.is_none() {
        vec![
            "--provider".into(),
            source_provider(provider).into(),
            "--data-root".into(),
            work.clone(),
            "--collect".into(),
        ]
    } else {
        Vec::new()
    };
    ensure!(
        PathBuf::from(&executable).is_absolute() && PathBuf::from(&work).is_absolute(),
        "adapter runtime paths must be absolute"
    );
    ensure!(
        args.len() <= 64 && args.iter().map(String::len).sum::<usize>() <= 16384,
        "adapter argument budget exceeded"
    );
    let mut args = args;
    if provider == "wechat" && explicit.is_none() {
        let (Ok(snapshots), Ok(account)) = (
            std::env::var("MONICA_CHATLOG_SNAPSHOT_ROOT"),
            std::env::var("MONICA_CHATLOG_ACCOUNT"),
        ) else {
            return Ok(None);
        };
        ensure!(
            std::path::Path::new(&snapshots).is_absolute() && !account.trim().is_empty(),
            "invalid explicit Chatlog source anchor"
        );
        // Tauri starts the backend before starting its owned Chatlog child.
        // An empty export folder is normal during initialization.
        if std::env::var_os("MONICA_NATIVE_MOMENTS_WORKER").is_some()
            && !std::path::Path::new(&snapshots)
                .join("contact/contact.db")
                .is_file()
        {
            return Ok(None);
        }
        args.extend([
            "--snapshot-root".into(),
            snapshots,
            "--account".into(),
            account,
        ]);
        if let Ok(own) = std::env::var("MONICA_CHATLOG_SELF_ID") {
            args.extend(["--self-id".into(), own]);
        }
    }
    if provider == "linkedin_messages" && explicit.is_none() {
        args.extend(["--lane".into(), "messages".into()]);
    }
    Ok(Some(Config {
        executable: executable.into(),
        args,
        work_dir: work.into(),
        deadline: Duration::from_secs(900),
    }))
}

pub async fn enqueue(pool: &PgPool, provider: &str) -> Result<Uuid> {
    ensure!(
        [
            "wechat",
            "instagram",
            "linkedin",
            "linkedin_messages",
            "chatlog"
        ]
        .contains(&provider),
        "unknown adapter"
    );
    let mut tx = pool.begin().await?;
    let allowed: bool = sqlx::query_scalar(
        "SELECT enabled AND NOT paused AND (provider<>'linkedin_messages' OR EXISTS(SELECT 1 FROM crm.adapter_controls parent WHERE parent.provider='linkedin' AND parent.enabled AND NOT parent.paused)) FROM crm.adapter_controls WHERE provider=$1 FOR UPDATE",
    )
    .bind(provider)
    .fetch_one(&mut *tx)
    .await?;
    ensure!(allowed, "adapter disabled or paused");
    let active: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM crm.adapter_runs WHERE provider=$1 AND status IN ('queued','running')",
    )
    .bind(provider)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = active {
        tx.commit().await?;
        return Ok(id);
    }
    let limited:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.adapter_runs r JOIN crm.adapter_controls c ON c.provider=r.provider WHERE r.provider=$1 AND r.started_at>now()-make_interval(secs=>greatest(c.minimum_interval_seconds,CASE WHEN r.provider='wechat' AND (r.progress->>'phase'='waiting' OR r.status='failed') THEN 60 ELSE 1 END)))").bind(provider).fetch_one(&mut *tx).await?;
    ensure!(!limited, "adapter platform interval not elapsed");
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.adapter_runs(id,provider) VALUES($1,$2)")
        .bind(id)
        .bind(provider)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.adapter_events(run_id,state) VALUES($1,'queued')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}
pub async fn claim(pool: &PgPool, provider: &str) -> Result<Option<Lease>> {
    let token = Uuid::new_v4();
    let id:Option<Uuid>=sqlx::query_scalar("WITH candidate AS (SELECT r.id FROM crm.adapter_runs r JOIN crm.adapter_controls c ON c.provider=r.provider WHERE r.provider=$1 AND c.enabled AND NOT c.paused AND (c.provider<>'linkedin_messages' OR EXISTS(SELECT 1 FROM crm.adapter_controls parent WHERE parent.provider='linkedin' AND parent.enabled AND NOT parent.paused)) AND NOT r.cancel_requested AND r.attempts<3 AND (r.status='queued' OR (r.status='running' AND r.lease_until<clock_timestamp())) ORDER BY r.created_at FOR UPDATE OF r SKIP LOCKED LIMIT 1) UPDATE crm.adapter_runs r SET status='running',token=$2,lease_until=clock_timestamp()+interval '90 seconds',attempts=attempts+1,started_at=coalesce(started_at,now()),heartbeat_at=now() FROM candidate c WHERE r.id=c.id RETURNING r.id").bind(provider).bind(token).fetch_optional(pool).await?;
    Ok(id.map(|id| Lease {
        id,
        provider: provider.into(),
        token,
    }))
}
pub async fn cancel(pool: &PgPool, id: Uuid) -> Result<bool> {
    Ok(sqlx::query("UPDATE crm.adapter_runs SET cancel_requested=true,status=CASE WHEN status='queued' THEN 'cancelled' ELSE status END,completed_at=CASE WHEN status='queued' THEN now() ELSE completed_at END WHERE id=$1 AND status IN ('queued','running')").bind(id).execute(pool).await?.rows_affected()==1)
}
async fn heartbeat(pool: &PgPool, lease: &Lease) -> Result<bool> {
    Ok(sqlx::query("UPDATE crm.adapter_runs r SET heartbeat_at=now(),lease_until=clock_timestamp()+interval '90 seconds' FROM crm.adapter_controls c WHERE r.id=$1 AND r.token=$2 AND r.status='running' AND r.lease_until>clock_timestamp() AND NOT r.cancel_requested AND c.provider=r.provider AND c.enabled AND NOT c.paused AND (c.provider<>'linkedin_messages' OR EXISTS(SELECT 1 FROM crm.adapter_controls parent WHERE parent.provider='linkedin' AND parent.enabled AND NOT parent.paused))").bind(lease.id).bind(lease.token).execute(pool).await?.rows_affected()==1)
}
async fn keep_alive(pool: &PgPool, lease: &Lease, last: &mut tokio::time::Instant) -> Result<bool> {
    let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.adapter_runs r JOIN crm.adapter_controls c ON c.provider=r.provider WHERE r.id=$1 AND r.token=$2 AND r.status='running' AND r.lease_until>clock_timestamp() AND NOT r.cancel_requested AND c.enabled AND NOT c.paused AND (c.provider<>'linkedin_messages' OR EXISTS(SELECT 1 FROM crm.adapter_controls parent WHERE parent.provider='linkedin' AND parent.enabled AND NOT parent.paused)))").bind(lease.id).bind(lease.token).fetch_one(pool).await?;
    if !allowed {
        return Ok(false);
    }
    if last.elapsed() >= Duration::from_secs(15) {
        if !heartbeat(pool, lease).await? {
            return Ok(false);
        }
        *last = tokio::time::Instant::now();
    }
    Ok(true)
}
pub async fn publish(pool: &PgPool, lease: &Lease, payload: Value) -> Result<Uuid> {
    ensure!(
        serde_json::to_vec(&payload)?.len() <= 4 * 1024 * 1024,
        "snapshot exceeds budget"
    );
    let batch: crate::imports::Batch = serde_json::from_value(payload)?;
    ensure!(
        batch.source.schema() == source_provider(&lease.provider),
        "snapshot provider mismatch"
    );
    batch.validate()?;
    let hash = crate::identity::observation_hash(&serde_json::to_value(&batch)?);
    let mut tx = pool.begin().await?;
    lease_guard(&mut tx, lease).await?;
    if let Some(cid) = batch.connection_id {
        ensure!(sqlx::query("SELECT id FROM crm.source_connections WHERE id=$1 AND provider=$2 AND enabled FOR SHARE").bind(cid).bind(batch.source.schema()).fetch_optional(&mut *tx).await?.is_some(),"collector source connection disabled");
    }
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT content_hash FROM crm.adapter_snapshots WHERE provider=$1 AND external_id=$2",
    )
    .bind(&lease.provider)
    .bind(&batch.batch_id)
    .fetch_optional(&mut *tx)
    .await?;
    ensure!(
        existing.as_ref().is_none_or(|old| old == &hash),
        "published snapshot identity reused"
    );
    let id = crate::imports::receive(&mut tx, &batch, &hash).await?;
    sqlx::query("INSERT INTO crm.adapter_snapshots(provider,external_id,content_hash,run_id,batch_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(&lease.provider).bind(&batch.batch_id).bind(hash).bind(lease.id).bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.adapter_runs SET progress=progress||jsonb_build_object('last_batch_id',$2::text,'last_progress_at',now()) WHERE id=$1").bind(lease.id).bind(id.to_string()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(id)
}
async fn event(pool: &PgPool, lease: &Lease, frame: Value, config: &Config) -> Result<()> {
    match frame["type"].as_str() {
        Some("connection_snapshot") => {
            let account = frame["account"]
                .as_str()
                .context("collector owning account required")?;
            ensure!(
                !account.trim().is_empty() && account.len() <= 1024,
                "invalid collector owning account"
            );
            let mut batch: crate::imports::Batch = serde_json::from_value(frame["batch"].clone())?;
            ensure!(
                batch.source.schema() == source_provider(&lease.provider),
                "collector connection provider mismatch"
            );
            let id = crate::connections::connection_id(batch.source.provider()?, account);
            let mut tx = pool.begin().await?;

            lease_guard(&mut tx, lease).await?;
            sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,state,enabled) VALUES($1,$2,$3,$3,'collector','connected',true) ON CONFLICT(provider,external_account) DO NOTHING").bind(id).bind(source_provider(&lease.provider)).bind(account).execute(&mut *tx).await?;
            let (connection,enabled):(Uuid,bool)=sqlx::query_as("SELECT id,enabled FROM crm.source_connections WHERE provider=$1 AND external_account=$2 FOR SHARE").bind(source_provider(&lease.provider)).bind(account).fetch_one(&mut *tx).await?;
            ensure!(enabled, "collector source connection disabled");
            tx.commit().await?;
            ensure!(
                batch.connection_id.is_none_or(|cid| cid == connection),
                "collector connection identity mismatch"
            );
            batch.connection_id = Some(connection);
            batch.batch_id = format!("{}:{connection}", batch.batch_id);
            batch.idempotency_key = format!("{}:{connection}", batch.idempotency_key);
            batch.validate()?;
            let receipt = publish(pool, lease, serde_json::to_value(&batch)?).await?;
            if let Some(path) = frame["ack_path"].as_str() {
                write_ack(config,path,&json!({"receipt_id":receipt,"batch_id":batch.batch_id,"connection_id":connection,"page_sha256":frame["page_sha256"]})).await?;
            }
        }
        Some("media") => {
            let root = tokio::fs::canonicalize(&config.work_dir).await?;
            let supplied =
                std::path::Path::new(frame["path"].as_str().context("media path required")?);
            let path = tokio::fs::canonicalize(if supplied.is_absolute() {
                supplied.to_path_buf()
            } else {
                root.join(supplied)
            })
            .await?;
            ensure!(
                path.starts_with(&root),
                "media path outside adapter work directory"
            );
            let hash = frame["sha256"].as_str().context("media SHA required")?;
            ensure!(
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "invalid media SHA"
            );
            let size = frame["byte_length"]
                .as_i64()
                .context("media byte length required")?;
            ensure!(
                size > 0 && size <= 512 * 1024 * 1024,
                "adapter media byte budget exceeded"
            );
            let mut tx = pool.begin().await?;

            lease_guard(&mut tx, lease).await?;
            sqlx::query("INSERT INTO crm.adapter_media_inputs(id,run_id,provider,path,sha256,byte_length) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(provider,sha256) DO NOTHING").bind(Uuid::new_v4()).bind(lease.id).bind(source_provider(&lease.provider)).bind(path.to_str().context("nonUTF8media path")?).bind(hash).bind(size).execute(&mut *tx).await?;
            tx.commit().await?;
        }
        Some("snapshot") => {
            publish(pool, lease, frame["batch"].clone()).await?;
        }
        Some("progress") => {
            // Whitelist numeric counters and fixed phase values. Never copy a
            // collector's arbitrary strings (URLs/cookies/account secrets).
            let phase = frame["phase"]
                .as_str()
                .filter(|s| {
                    [
                        "starting",
                        "collecting",
                        "publishing",
                        "waiting",
                        "complete",
                    ]
                    .contains(s)
                })
                .unwrap_or("collecting");
            let progress = json!({"phase":phase,"processed":frame["processed"].as_u64().unwrap_or(0),"total":frame["total"].as_u64(),"last_progress_at":chrono::Utc::now()});
            let count=sqlx::query("UPDATE crm.adapter_runs SET progress=progress||$3 WHERE id=$1 AND token=$2 AND status='running' AND NOT cancel_requested AND lease_until>clock_timestamp()").bind(lease.id).bind(lease.token).bind(progress).execute(pool).await?.rows_affected();
            if count != 1 {
                return Err(Interrupted.into());
            }
        }
        _ => ensure!(false, "unknown collector frame"),
    }
    Ok(())
}
async fn write_ack(config: &Config, relative: &str, payload: &Value) -> Result<()> {
    let path = std::path::Path::new(relative);
    ensure!(
        path.extension().and_then(|s| s.to_str()) == Some("ack")
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            && path.starts_with("raw"),
        "invalid collector acknowledgement path"
    );
    let root = tokio::fs::canonicalize(&config.work_dir).await?;
    let target = root.join(path);
    let parent = target
        .parent()
        .context("collector acknowledgement parent")?;
    let mut ancestor = parent;
    while !tokio::fs::try_exists(ancestor).await? {
        ancestor = ancestor
            .parent()
            .context("collector acknowledgement root")?;
    }
    ensure!(
        tokio::fs::canonicalize(ancestor).await?.starts_with(&root),
        "collector acknowledgement escapes data root"
    );
    tokio::fs::create_dir_all(parent).await?;
    ensure!(
        tokio::fs::canonicalize(parent).await?.starts_with(&root),
        "collector acknowledgement escapes data root"
    );
    let temporary = target.with_extension(format!("{}.new", Uuid::new_v4()));
    tokio::fs::write(&temporary, serde_json::to_vec(payload)?).await?;
    tokio::fs::File::open(&temporary).await?.sync_all().await?;
    tokio::fs::rename(temporary, &target).await?;
    let parent = parent.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::File::open(parent)?.sync_all()).await??;
    Ok(())
}
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
/// Drop kills the complete external process group, including grandchildren.
/// Pause/cancellation is polled every250ms; no database connection is held while
/// waiting for the external runtime. Independent providers keep their own rows.
pub async fn run_one(pool: &PgPool, provider: &str, config: &Config) -> Result<bool> {
    let Some(lease) = claim(pool, provider).await? else {
        return Ok(false);
    };
    let result = run_process(pool, &lease, config).await;
    let (status, error) = match result {
        Ok(true) => ("complete", None),
        Ok(false) => ("cancelled", None),
        Err(_) => ("failed", Some("adapter process or protocol failed")),
    };
    let mut tx = pool.begin().await?;
    let changed=sqlx::query("UPDATE crm.adapter_runs SET status=$3,error=$4,token=NULL,lease_until=NULL,completed_at=now() WHERE id=$1 AND token=$2 AND status='running' AND lease_until>clock_timestamp()").bind(lease.id).bind(lease.token).bind(status).bind(error).execute(&mut *tx).await?.rows_affected();
    if changed == 1 {
        sqlx::query("INSERT INTO crm.adapter_events(run_id,state) VALUES($1,$2)")
            .bind(lease.id)
            .bind(status)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(true)
}
async fn run_process(pool: &PgPool, lease: &Lease, config: &Config) -> Result<bool> {
    ensure!(
        config.executable.is_absolute() && config.work_dir.is_absolute(),
        "adapter paths must be absolute"
    );
    let mut command = Command::new(&config.executable);
    command
        .args(&config.args)
        .current_dir(&config.work_dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("MONICA_ADAPTER_PROVIDER", &lease.provider)
        .env("MONICA_ADAPTER_RUN_ID", lease.id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    let group = ProcessGroup(child.id().context("missing child pid")?);
    let stdout = child.stdout.take().context("missing stdout")?;
    let mut reader = BufReader::new(stdout);
    let deadline = tokio::time::Instant::now() + config.deadline.min(Duration::from_secs(3600));
    let mut line = Vec::new();
    let mut total = 0usize;
    let mut renewed = tokio::time::Instant::now();
    loop {
        line.clear();
        let size = {
            let mut limited = (&mut reader).take(4 * 1024 * 1024 + 1);
            let read = limited.read_until(b'\n', &mut line);
            tokio::pin!(read);
            loop {
                tokio::select! {
                    result=&mut read=>break result?,
                _=tokio::time::sleep(Duration::from_millis(250))=>{if !keep_alive(pool,lease,&mut renewed).await? {child.kill().await.ok();drop(group);return Ok(false)}},
                    _=tokio::time::sleep_until(deadline)=>{child.kill().await.ok();anyhow::bail!("adapter deadline exceeded")}
                }
            }
        };
        if size == 0 {
            break;
        }
        ensure!(size <= 4 * 1024 * 1024, "collector frame exceeds budget");
        total += size;
        ensure!(
            total <= 64 * 1024 * 1024,
            "collector output exceeds per-run budget"
        );
        event(pool, lease, serde_json::from_slice(&line)?, config).await?;
    }
    let exit = loop {
        tokio::select! {
            result=child.wait()=>break result?,
            _=tokio::time::sleep(Duration::from_millis(250))=>{if !keep_alive(pool,lease,&mut renewed).await?{child.kill().await.ok();drop(group);return Ok(false)}},
            _=tokio::time::sleep_until(deadline)=>{child.kill().await.ok();anyhow::bail!("adapter deadline exceeded")}
        }
    };
    drop(group);
    ensure!(exit.success(), "collector exited unsuccessfully");
    heartbeat(pool, lease).await
}
/// The desktop shell supplies this trusted, bundled Moments worker separately
/// from the collector protocol. It needs its own local database connection for
/// verified publication, and runs independently of Chatlog source scans.
pub(crate) async fn run_native_wechat_media(
    pool: &PgPool,
    resources: &crate::resources::ResourceBudget,
    images: bool,
) -> Result<()> {
    let (Ok(executable), Ok(work), Ok(account), Ok(snapshot), Ok(media), Ok(database)) = (
        std::env::var("MONICA_NATIVE_MOMENTS_WORKER"),
        std::env::var("MONICA_COLLECTOR_DATA_ROOT"),
        std::env::var("MONICA_CHATLOG_ACCOUNT"),
        std::env::var("MONICA_CHATLOG_SNAPSHOT_ROOT"),
        std::env::var("MONICA_MEDIA_ROOT"),
        std::env::var("MONICA_DATABASE_URL"),
    ) else {
        return Ok(());
    };
    let cache = std::env::var("MONICA_NATIVE_WECHAT_CACHE")
        .ok()
        .filter(|_| images);
    let due: bool = if images {
        if cache.is_none() {
            return Ok(());
        }
        sqlx::query_scalar("SELECT EXISTS(SELECT FROM crm.wechat_image_control WHERE enabled AND NOT paused) AND EXISTS(SELECT FROM crm.source_connections s JOIN crm.conversations c ON c.connection_id=s.id WHERE s.provider='wechat' AND s.external_account=$1 AND s.enabled AND NOT EXISTS(SELECT FROM crm.wechat_image_runs r WHERE r.account=s.id::text AND r.heartbeat_at>now()-interval '5 minutes'))")
            .bind(&account).fetch_one(pool).await?
    } else {
        sqlx::query_scalar("SELECT EXISTS(SELECT FROM crm.native_moments_media q JOIN crm.source_connections s ON s.id=q.connection_id WHERE s.provider='wechat' AND s.external_account=$1 AND s.enabled AND q.retry_at<=now())")
            .bind(&account).fetch_one(pool).await?
    };
    let enabled: bool = sqlx::query_scalar(
        "SELECT enabled AND NOT paused FROM crm.adapter_controls WHERE provider='wechat'",
    )
    .fetch_one(pool)
    .await?;
    if !due || !enabled {
        return Ok(());
    }
    let _permit = resources
        .acquire_background(crate::resources::TaskResources {
            cpu: 1,
            network: 1,
            disk: 1,
            memory_bytes: 128 * 1024 * 1024,
        })
        .await?;
    let mut command = Command::new(executable);
    command
        .arg(if images { "images" } else { "moments" })
        .current_dir(work)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("MONICA_CHATLOG_ACCOUNT", account)
        .env("MONICA_CHATLOG_SNAPSHOT_ROOT", snapshot)
        .env("MONICA_MEDIA_ROOT", media)
        .env("MONICA_DATABASE_URL", database)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(cache) = cache {
        command.env("MONICA_NATIVE_WECHAT_CACHE", cache);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    let _group = ProcessGroup(
        child
            .id()
            .context("missing desktop WeChat media worker pid")?,
    );
    loop {
        tokio::select! {
            result=child.wait()=>{
                ensure!(result?.success(), "desktop WeChat media worker failed");
                return Ok(());
            },
            _=tokio::time::sleep(Duration::from_secs(1))=>{
                let enabled: bool = sqlx::query_scalar("SELECT enabled AND NOT paused FROM crm.adapter_controls WHERE provider='wechat'")
                    .fetch_one(pool).await?;
                if !enabled { return Ok(()); }
            }
        }
    }
}

/// Only explicit runtime paths can start external adapters. Nothing discovers
/// sessions or credentials from existing installations or collector checkouts.
pub async fn run_configured(
    pool: &PgPool,
    resources: &crate::resources::ResourceBudget,
) -> Result<bool> {
    let providers: Vec<String> = sqlx::query_scalar(
        "SELECT provider FROM crm.adapter_controls WHERE enabled AND NOT paused AND provider<>'linkedin_messages' ORDER BY provider",
    )
    .fetch_all(pool)
    .await?;
    for provider in providers {
        let prefix = format!("MONICA_ADAPTER_{}", provider.to_uppercase());
        let work = std::env::var(format!("{prefix}_WORK_DIR"))
            .ok()
            .or_else(|| std::env::var("MONICA_COLLECTOR_DATA_ROOT").ok());
        if let (Ok(spool), Some(work)) = (std::env::var(format!("{prefix}_SPOOL")), work) {
            match spool::consume(
                pool,
                &provider,
                std::path::Path::new(&spool),
                std::path::Path::new(&work),
                resources,
            )
            .await
            {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(_) => {
                    log::warn!("external collector spool unavailable: {provider}");
                    continue;
                }
            }
        }
        let config = match runtime_config(&provider) {
            Ok(Some(config)) => config,
            Ok(None) => continue,
            Err(_) => {
                log::warn!("collector runtime configuration invalid: {provider}");
                continue;
            }
        };
        if enqueue(pool, &provider).await.is_err() {
            continue;
        }
        let _permit = resources
            .acquire_background(crate::resources::TaskResources {
                cpu: 1,
                network: 1,
                disk: if provider == "wechat" { 1 } else { 0 },
                memory_bytes: 32 * 1024 * 1024,
            })
            .await?;
        if run_one(pool, &provider, &config).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn configure_from_env(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await?;
    for provider in ["wechat", "instagram", "linkedin", "chatlog"] {
        let prefix = format!("MONICA_ADAPTER_{}", provider.to_uppercase());
        let enabled = crate::ai::optional_bool(&format!("{prefix}_ENABLED"))?;
        let paused = crate::ai::optional_bool(&format!("{prefix}_PAUSED"))?;
        let interval = std::env::var(format!("{prefix}_MIN_INTERVAL_SECONDS"))
            .ok()
            .map(|s| s.parse::<i32>())
            .transpose()?;
        if let Some(n) = interval {
            ensure!((1..=86400).contains(&n), "invalid adapter interval")
        }
        for name in ["EXECUTABLE", "WORK_DIR"] {
            if let Ok(value) = std::env::var(format!("{prefix}_{name}")) {
                ensure!(
                    std::path::Path::new(&value).is_absolute(),
                    "adapter paths must be absolute"
                )
            }
        }
        if enabled.is_some() || paused.is_some() || interval.is_some() {
            sqlx::query("UPDATE crm.adapter_controls SET enabled=coalesce($2,enabled),paused=coalesce($3,paused),minimum_interval_seconds=coalesce($4,minimum_interval_seconds),updated_at=now() WHERE provider=$1").bind(provider).bind(enabled).bind(paused).bind(interval).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Run from media recovery, after releasing adapter process permits. Decode and
/// publication use MediaStore's existing shared192MiB/CPU/disk limits.
pub async fn consume_media(
    pool: &PgPool,
    root: &std::path::Path,
    resources: &crate::resources::ResourceBudget,
) -> Result<bool> {
    let token = Uuid::new_v4();
    let row=sqlx::query("WITH candidate AS (SELECT id FROM crm.adapter_media_inputs WHERE status='queued' OR (status='running' AND lease_until<clock_timestamp()) ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE crm.adapter_media_inputs m SET status='running',token=$1,lease_until=clock_timestamp()+interval '5 minutes' FROM candidate c WHERE m.id=c.id RETURNING m.*").bind(token).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(false) };
    let id: Uuid = row.get("id");
    let path = PathBuf::from(row.get::<String, _>("path"));
    let hash: String = row.get("sha256");
    let size: i64 = row.get("byte_length");
    let provider: String = row.get("provider");
    let store = crate::media::MediaStore::new(pool.clone(), root.to_path_buf(), resources.clone())?;
    let result = store
        .ingest_path(
            &path,
            path.file_name().and_then(|n| n.to_str()),
            &provider,
            Some(&id.to_string()),
            Some(&hash),
            Some(size as u64),
        )
        .await;
    let (media, error) = match result {
        Ok(asset) => (Some(asset.id), None),
        Err(_) => (None, Some("adapter media unavailable or invalid")),
    };
    sqlx::query("UPDATE crm.adapter_media_inputs SET status=$3,media_id=$4,error=$5,token=NULL,lease_until=NULL WHERE id=$1 AND token=$2 AND lease_until>clock_timestamp()").bind(id).bind(token).bind(if media.is_some(){"complete"}else{"failed"}).bind(media).bind(error).execute(pool).await?;
    Ok(true)
}

//! Fixed, bounded background partitions over the durable source/message queues.
//!
//! A partition is selected in SQL before a lease is taken. The worker does not
//! hold a lease while waiting for an in-memory object mutex or a resource permit.
//! Object versions and coalesced conversation generations remain DB-authoritative.
#[path = "scheduler_adapter.rs"]
mod adapter_lane;
#[path = "scheduler_ai.rs"]
mod ai_lane;
#[path = "scheduler_cdc.rs"]
mod cdc_lane;
use crate::{Lease, Provider, messages, resources::ResourceBudget, resources::TaskResources};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Row, pool::PoolConnection};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, RwLock, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, MissedTickBehavior},
};
use uuid::Uuid;

const SCHEDULER_LOCK: i64 = 11_803_477;
const BACKGROUND_MEMORY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    pub start_paused: bool,
    pub partitions: usize,
    pub poll_interval: Duration,
    pub heartbeat_interval: Duration,
    pub task_timeout: Duration,
    pub drain_timeout: Duration,
    pub retry_delay: Duration,
    pub max_attempts: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            start_paused: false,
            partitions: 4,
            poll_interval: Duration::from_millis(250),
            heartbeat_interval: Duration::from_secs(30),
            task_timeout: Duration::from_secs(60),
            drain_timeout: Duration::from_secs(90),
            retry_delay: Duration::from_secs(1),
            max_attempts: 5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Running,
    Paused,
    Draining,
    Stopping,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Draining => "draining",
            Self::Stopping => "stopping",
        }
    }
}

struct Shared {
    instance: Uuid,
    config: SchedulerConfig,
    background: PgPool,
    control: PgPool,
    resources: ResourceBudget,
    media: Option<crate::media::MediaStore>,
    credentials: Option<crate::credentials::CredentialCipher>,
    whatsapp_binary: Option<std::path::PathBuf>,
    mode: watch::Sender<Mode>,
    admission: RwLock<()>,
    commands: Mutex<()>,
    active: AtomicUsize,
    completed: AtomicU64,
    failed: AtomicU64,
    import_chunks: AtomicU64,
    stopped: AtomicBool,
    activity: watch::Sender<usize>,
    supervisor: Mutex<Option<JoinHandle<Result<()>>>>,
}

#[derive(Clone)]
pub struct SchedulerHandle {
    shared: Arc<Shared>,
}

#[derive(Debug, Serialize)]
pub struct SchedulerStatus {
    pub instance_id: Uuid,
    pub state: String,
    pub paused: bool,
    pub draining: bool,
    pub stopped: bool,
    pub partitions: usize,
    pub active_jobs: usize,
    pub pending_object_jobs: i64,
    pub pending_summary_jobs: i64,
    pub pending_import_batches: i64,
    pub pending_ai_jobs: i64,
    pub quarantined_jobs: i64,
    pub completed_jobs: u64,
    pub failed_attempts: u64,
    pub import_recovery_attempts: u64,
    pub resources: crate::resources::ResourceStatus,
}

pub struct Scheduler;

impl Scheduler {
    pub async fn start(
        background: PgPool,
        control: PgPool,
        resources: ResourceBudget,
        config: SchedulerConfig,
    ) -> Result<SchedulerHandle> {
        Self::start_inner(background, control, resources, config, None, None, None).await
    }
    pub async fn start_with_media(
        background: PgPool,
        control: PgPool,
        resources: ResourceBudget,
        config: SchedulerConfig,
        root: std::path::PathBuf,
    ) -> Result<SchedulerHandle> {
        let media = crate::media::MediaStore::new(background.clone(), root, resources.clone())?;
        Self::start_inner(
            background,
            control,
            resources,
            config,
            Some(media),
            None,
            None,
        )
        .await
    }
    pub async fn start_with_sources(
        background: PgPool,
        control: PgPool,
        resources: ResourceBudget,
        config: SchedulerConfig,
        root: std::path::PathBuf,
        credentials: crate::credentials::CredentialCipher,
    ) -> Result<SchedulerHandle> {
        Self::start_with_source_bridge(
            background,
            control,
            resources,
            config,
            root,
            credentials,
            std::env::var_os("MONICA_WHATSAPP_BINARY").map(std::path::PathBuf::from),
        )
        .await
    }
    pub async fn start_with_source_bridge(
        background: PgPool,
        control: PgPool,
        resources: ResourceBudget,
        config: SchedulerConfig,
        root: std::path::PathBuf,
        credentials: crate::credentials::CredentialCipher,
        whatsapp_binary: Option<std::path::PathBuf>,
    ) -> Result<SchedulerHandle> {
        let media = crate::media::MediaStore::new(background.clone(), root, resources.clone())?;
        Self::start_inner(
            background,
            control,
            resources,
            config,
            Some(media),
            Some(credentials),
            whatsapp_binary,
        )
        .await
    }
    async fn start_inner(
        background: PgPool,
        control: PgPool,
        resources: ResourceBudget,
        config: SchedulerConfig,
        media: Option<crate::media::MediaStore>,
        credentials: Option<crate::credentials::CredentialCipher>,
        whatsapp_binary: Option<std::path::PathBuf>,
    ) -> Result<SchedulerHandle> {
        if config.partitions == 0
            || config.partitions > 64
            || config.poll_interval.is_zero()
            || config.heartbeat_interval.is_zero()
            || config.heartbeat_interval >= Duration::from_secs(60)
            || config.task_timeout.is_zero()
            || config.drain_timeout < config.task_timeout
            || config.retry_delay.is_zero()
            || config.max_attempts == 0
        {
            bail!("invalid scheduler configuration");
        }
        resources.validate_request(background_request())?;
        if credentials.is_some() {
            resources.validate_background_request(source_lane::request())?;
        }
        if media.is_some() {
            resources.validate_background_request(crate::ai::background_resources())?;
        }
        // This lease protects the coordinator, not business objects. It avoids
        // accidentally multiplying one host's disk/memory budget with a second
        // scheduler. A crashed connection automatically releases the lease.
        let mut leader = control.acquire().await?;
        leader.close_on_drop();
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(SCHEDULER_LOCK)
            .fetch_one(&mut *leader)
            .await?;
        if !acquired {
            bail!("another Monica scheduler already owns this database");
        }
        if config.start_paused {
            sqlx::query("UPDATE crm.scheduler_control SET paused=true,updated_at=clock_timestamp() WHERE singleton")
                .execute(&mut *leader).await?;
        }
        let paused: bool =
            sqlx::query_scalar("SELECT paused FROM crm.scheduler_control WHERE singleton")
                .fetch_one(&mut *leader)
                .await?;
        let initial = if paused { Mode::Paused } else { Mode::Running };
        let (mode, _) = watch::channel(initial);
        let (activity, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            instance: Uuid::new_v4(),
            config,
            background,
            control,
            resources,
            media,
            credentials,
            whatsapp_binary,
            mode,
            admission: RwLock::new(()),
            commands: Mutex::new(()),
            active: AtomicUsize::new(0),
            completed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            import_chunks: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            activity,
            supervisor: Mutex::new(None),
        });
        sqlx::query(
            "INSERT INTO crm.scheduler_runs(instance_id,partitions,state) VALUES($1,$2,$3)",
        )
        .bind(shared.instance)
        .bind(shared.config.partitions as i32)
        .bind(initial.name())
        .execute(&mut *leader)
        .await?;
        let cloned = shared.clone();
        *shared.supervisor.lock().await =
            Some(tokio::spawn(async move { supervise(cloned, leader).await }));
        Ok(SchedulerHandle { shared })
    }
}

impl SchedulerHandle {
    pub fn is_stopped(&self) -> bool {
        self.shared.stopped.load(Ordering::Acquire)
    }
    /// Persistently stop new claims; already admitted transactions finish.
    pub async fn pause(&self) -> Result<()> {
        let _command = self.shared.commands.lock().await;
        self.set_mode(Mode::Paused, Some(true)).await
    }

    pub async fn resume(&self) -> Result<()> {
        let _command = self.shared.commands.lock().await;
        self.set_mode(Mode::Running, Some(false)).await
    }

    async fn set_mode(&self, mode: Mode, persist: Option<bool>) -> Result<()> {
        let _gate = self.shared.admission.write().await;
        if self.shared.stopped.load(Ordering::Acquire)
            || *self.shared.mode.borrow() == Mode::Stopping
        {
            bail!("scheduler is stopped");
        }
        if let Some(paused) = persist {
            sqlx::query("UPDATE crm.scheduler_control SET paused=$1,updated_at=clock_timestamp() WHERE singleton")
                .bind(paused)
                .execute(&self.shared.control)
                .await?;
        }
        self.shared.mode.send_replace(mode);
        Ok(())
    }

    /// Pause and wait until every claimed transaction has completed or rolled back.
    /// Queued work remains durable, so a drain never means discarding the backlog.
    pub async fn drain(&self) -> Result<()> {
        let _command = self.shared.commands.lock().await;
        self.set_mode(Mode::Draining, Some(true)).await?;
        let mut activity = self.shared.activity.subscribe();
        tokio::time::timeout(self.shared.config.drain_timeout, async {
            loop {
                if self.shared.active.load(Ordering::Acquire) == 0 {
                    break;
                }
                activity
                    .changed()
                    .await
                    .context("scheduler activity closed")?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("scheduler drain deadline exceeded")??;
        let _gate = self.shared.admission.write().await;
        if *self.shared.mode.borrow() == Mode::Draining {
            self.shared.mode.send_replace(Mode::Paused);
        }
        Ok(())
    }

    /// Stop admission and join all fixed workers. No task is detached or killed.
    pub async fn shutdown(&self) -> Result<()> {
        let _command = self.shared.commands.lock().await;
        {
            let _gate = self.shared.admission.write().await;
            self.shared.mode.send_replace(Mode::Stopping);
        }
        let mut supervisor = self.shared.supervisor.lock().await;
        if let Some(task) = supervisor.as_mut() {
            // Keep the handle stored if this wait times out; a later shutdown
            // call can observe actual completion instead of losing supervision.
            let result = tokio::time::timeout(self.shared.config.drain_timeout, task)
                .await
                .context("scheduler shutdown deadline exceeded; workers remain supervised")?;
            supervisor.take();
            result.context("scheduler supervisor panicked")??;
        }
        Ok(())
    }

    pub async fn status(&self) -> Result<SchedulerStatus> {
        let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM crm.jobs),(SELECT count(*) FROM crm.conversation_work),(SELECT count(*) FROM crm.scheduler_failures WHERE quarantined),(SELECT count(*) FROM crm.import_batches WHERE status IN ('received','running')),(SELECT count(*) FROM crm.ai_jobs WHERE status IN ('queued','running'))",
        )
        .fetch_one(&self.shared.control)
        .await?;
        let mode = *self.shared.mode.borrow();
        let stopped = self.shared.stopped.load(Ordering::Acquire);
        Ok(SchedulerStatus {
            instance_id: self.shared.instance,
            state: if stopped { "stopped" } else { mode.name() }.into(),
            paused: matches!(mode, Mode::Paused | Mode::Draining),
            draining: mode == Mode::Draining,
            stopped,
            partitions: self.shared.config.partitions,
            active_jobs: self.shared.active.load(Ordering::Acquire),
            pending_object_jobs: counts.0,
            pending_summary_jobs: counts.1,
            pending_import_batches: counts.3,
            pending_ai_jobs: counts.4,
            quarantined_jobs: counts.2,
            completed_jobs: self.shared.completed.load(Ordering::Relaxed),
            failed_attempts: self.shared.failed.load(Ordering::Relaxed),
            import_recovery_attempts: self.shared.import_chunks.load(Ordering::Relaxed),
            resources: self.shared.resources.status(),
        })
    }

    /// Explicit operator retry; failed work is never silently dropped to unblock
    /// later versions of the same object.
    pub async fn retry_failed_jobs(&self) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM crm.scheduler_failures WHERE quarantined")
                .execute(&self.shared.control)
                .await?
                .rows_affected(),
        )
    }

    pub fn resources(&self) -> ResourceBudget {
        self.shared.resources.clone()
    }
}

struct Active(Arc<Shared>);
struct SupervisorStopped(Arc<Shared>);
impl Drop for SupervisorStopped {
    fn drop(&mut self) {
        self.0.mode.send_replace(Mode::Stopping);
        self.0.stopped.store(true, Ordering::Release);
        self.0
            .activity
            .send_replace(self.0.active.load(Ordering::Acquire));
    }
}
impl Active {
    fn new(shared: Arc<Shared>) -> Self {
        let n = shared.active.fetch_add(1, Ordering::AcqRel) + 1;
        shared.activity.send_replace(n);
        Self(shared)
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        let n = self.0.active.fetch_sub(1, Ordering::AcqRel) - 1;
        self.0.activity.send_replace(n);
    }
}

async fn supervise(shared: Arc<Shared>, mut leader: PoolConnection<Postgres>) -> Result<()> {
    let _stopped = SupervisorStopped(shared.clone());
    let mut workers = JoinSet::new();
    for partition in 0..shared.config.partitions {
        workers.spawn(worker(shared.clone(), partition));
    }
    workers.spawn(recover_imports(shared.clone()));
    workers.spawn(adapter_lane::run(shared.clone()));
    workers.spawn(adapter_lane::run_messages(shared.clone()));
    workers.spawn(cdc_lane::run(shared.clone()));
    if shared.credentials.is_some() {
        workers.spawn(idle_lane::run(shared.clone()));
        workers.spawn(whatsapp_lane::run(shared.clone()));
        for _ in 0..2 {
            workers.spawn(source_lane::run(shared.clone()));
        }
    }
    if shared.media.is_some() {
        for _ in 0..3 {
            workers.spawn(ai_lane::run(shared.clone()));
        }
    }
    let mut pulse = tokio::time::interval(Duration::from_secs(5));
    pulse.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut failure = None;
    while !workers.is_empty() {
        tokio::select! {
            result = workers.join_next() => {
                let error = match result {
                    Some(Ok(Ok(()))) => None,
                    Some(Ok(Err(error))) => Some(error),
                    Some(Err(error)) => Some(anyhow::Error::new(error)),
                    None => None,
                };
                if let Some(error) = error {
                    failure.get_or_insert(error);
                    shared.mode.send_replace(Mode::Stopping);
                }
            }
            _ = pulse.tick() => {
                // This command uses the pinned connection, proving that the
                // coordinator's session/advisory lease is still alive.
                let mode = *shared.mode.borrow();
                if let Err(error) = sqlx::query("UPDATE crm.scheduler_runs SET heartbeat_at=clock_timestamp(),active_jobs=$2,completed_jobs=$3,failed_attempts=$4,state=$5,import_recovery_attempts=$6 WHERE instance_id=$1")
                    .bind(shared.instance)
                    .bind(shared.active.load(Ordering::Acquire) as i32)
                    .bind(shared.completed.load(Ordering::Relaxed) as i64)
                    .bind(shared.failed.load(Ordering::Relaxed) as i64)
                    .bind(mode.name())
                    .bind(shared.import_chunks.load(Ordering::Relaxed) as i64)
                    .execute(&mut *leader).await {
                    failure.get_or_insert(anyhow::Error::new(error));
                    shared.mode.send_replace(Mode::Stopping);
                }
            }
        }
    }
    shared.stopped.store(true, Ordering::Release);
    shared.activity.send_replace(0);
    let update = sqlx::query("UPDATE crm.scheduler_runs SET heartbeat_at=clock_timestamp(),stopped_at=clock_timestamp(),state='stopped',active_jobs=0,completed_jobs=$2,failed_attempts=$3,import_recovery_attempts=$4 WHERE instance_id=$1")
        .bind(shared.instance)
        .bind(shared.completed.load(Ordering::Relaxed) as i64)
        .bind(shared.failed.load(Ordering::Relaxed) as i64)
        .bind(shared.import_chunks.load(Ordering::Relaxed) as i64)
        .execute(&mut *leader)
        .await;
    // close_on_drop ensures this session-level lock cannot leak back into a pool.
    leader.close().await?;
    if let Some(error) = failure {
        return Err(error);
    }
    update?;
    Ok(())
}

enum Work {
    Object(Lease),
    Summary(messages::SummaryLease),
}

struct Claimed {
    work: Work,
    key: String,
}

impl Claimed {
    fn queue(&self) -> &'static str {
        match self.work {
            Work::Object(_) => "object",
            Work::Summary(_) => "summary",
        }
    }
}

async fn claim_object(shared: &Shared, partition: usize) -> Result<Option<Claimed>> {
    let token = Uuid::new_v4();
    let row = sqlx::query("WITH candidate AS (
      SELECT j.id FROM crm.jobs j LEFT JOIN crm.projections p
        ON p.provider=j.provider AND p.object_id=j.object_id
      WHERE (j.state='queued' OR (j.state='running' AND j.lease_until<clock_timestamp()))
        AND j.version=coalesce(p.version,0)+1
        AND mod(hashtextextended(j.provider||':'||j.object_id::text,0) & 9223372036854775807,$2)=$3
        AND NOT EXISTS(SELECT 1 FROM crm.scheduler_failures f WHERE f.queue='object'
          AND f.work_key=j.id::text AND (f.quarantined OR f.retry_at>clock_timestamp()))
      ORDER BY j.id FOR UPDATE OF j SKIP LOCKED LIMIT 1)
      UPDATE crm.jobs j SET state='running',token=$1,lease_until=clock_timestamp()+interval '2 minutes',attempts=attempts+1
      FROM candidate c WHERE j.id=c.id RETURNING j.id,j.provider,j.object_id,j.version")
        .bind(token)
        .bind(shared.config.partitions as i64)
        .bind(partition as i64)
        .fetch_optional(&shared.background)
        .await?;
    row.map(|row| {
        let id: i64 = row.get("id");
        let provider: Provider =
            serde_json::from_value(serde_json::Value::String(row.get("provider")))?;
        Ok(Claimed {
            key: id.to_string(),
            work: Work::Object(Lease {
                id,
                token,
                provider,
                object: row.get("object_id"),
                version: row.get("version"),
            }),
        })
    })
    .transpose()
}

async fn claim_summary(shared: &Shared, partition: usize) -> Result<Option<Claimed>> {
    let token = Uuid::new_v4();
    let row = sqlx::query("WITH candidate AS (
      SELECT w.provider,w.account,w.conversation FROM crm.conversation_work w
      WHERE (w.token IS NULL OR w.lease_until<clock_timestamp())
        AND mod(hashtextextended(jsonb_build_array(w.provider,w.account,w.conversation)::text,0) & 9223372036854775807,$2)=$3
        AND NOT EXISTS(SELECT 1 FROM crm.scheduler_failures f WHERE f.queue='summary'
          AND f.work_key=jsonb_build_array(w.provider,w.account,w.conversation)::text
          AND (f.quarantined OR f.retry_at>clock_timestamp()))
      ORDER BY w.provider,w.account,w.conversation FOR UPDATE SKIP LOCKED LIMIT 1)
      UPDATE crm.conversation_work w SET token=$1,lease_until=clock_timestamp()+interval '2 minutes'
      FROM candidate c WHERE (w.provider,w.account,w.conversation)=(c.provider,c.account,c.conversation)
      RETURNING w.provider,w.account,w.conversation,jsonb_build_array(w.provider,w.account,w.conversation)::text AS work_key")
        .bind(token)
        .bind(shared.config.partitions as i64)
        .bind(partition as i64)
        .fetch_optional(&shared.background)
        .await?;
    Ok(row.map(|row| Claimed {
        key: row.get("work_key"),
        work: Work::Summary(messages::SummaryLease {
            provider: row.get("provider"),
            account: row.get("account"),
            conversation: row.get("conversation"),
            token,
        }),
    }))
}

fn background_request() -> TaskResources {
    // Includes decoded JSON, handler clones and SQLx serialization, rather than
    // pretending the serialized payload size equals its in-memory working set.
    TaskResources {
        cpu: 1,
        memory_bytes: BACKGROUND_MEMORY_BYTES,
        ..TaskResources::default()
    }
}

/// Exactly one recovery lane. Each call performs at most one bounded import
/// chunk and commits its processed_count with source facts/jobs. It shares pause,
/// backpressure and drain admission with normal projection workers.
async fn recover_imports(shared: Arc<Shared>) -> Result<()> {
    let mut metrics_at = tokio::time::Instant::now();
    let mut mode = shared.mode.subscribe();
    loop {
        let current = *mode.borrow_and_update();
        match current {
            Mode::Stopping => return Ok(()),
            Mode::Paused | Mode::Draining => {
                mode.changed().await?;
                continue;
            }
            Mode::Running => {}
        }
        tokio::select! {
            _ = shared.resources.background_turn() => {}
            result = mode.changed() => { result?; continue; }
        }
        let permit = tokio::select! {
            permit = shared.resources.acquire(background_request()) => permit?,
            result = mode.changed() => { result?; continue; }
        };
        let gate = shared.admission.read().await;
        if *shared.mode.borrow() != Mode::Running {
            drop(gate);
            drop(permit);
            continue;
        }
        let active = Active::new(shared.clone());
        drop(gate);
        match tokio::time::timeout(shared.config.task_timeout, async {
            let attempted = crate::imports::resume_pending(&shared.background).await?;
            if let Some(media) = &shared.media {
                for _ in 0..25 {
                    if crate::media::consume_record_media(media).await? == 0 {
                        break;
                    }
                }
                crate::media::consume_imports(&shared.background).await?;
                crate::media::resolve_dependencies(&shared.background).await?;
                crate::media::refresh_profiles(&shared.background).await?;
            }
            // Maintenance reserves its own I/O memory. Release the import
            // allocation first to preserve the global acquisition order.
            drop(permit);
            if let Some(media) = &shared.media {
                crate::adapter::consume_media(&shared.background, &media.root, &shared.resources)
                    .await?;
                media.recover_publications().await?;
                media.expire_uploads().await?;
                media
                    .collect_orphans(Duration::from_secs(24 * 3600))
                    .await?;
            }
            if metrics_at.elapsed() >= Duration::from_secs(15) {
                crate::pages::refresh_metrics(&shared.background).await?;
                metrics_at = tokio::time::Instant::now();
            }
            Ok::<_, anyhow::Error>(attempted)
        })
        .await
        {
            Ok(Ok(attempted)) => {
                shared
                    .import_chunks
                    .fetch_add(attempted as u64, Ordering::Relaxed);
            }
            Ok(Err(_)) | Err(_) => {
                // The importer owns retry/quarantine metadata. A cancelled chunk
                // rolls back its facts and progress together and is replayable.
                shared.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
        drop(active);
        // resume_pending also returns one for a busy SKIP LOCKED batch. A fixed
        // delay prevents the recovery lane from spinning against an HTTP owner.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = mode.changed() => {}
        }
    }
}

async fn worker(shared: Arc<Shared>, partition: usize) -> Result<()> {
    let mut mode = shared.mode.subscribe();
    let mut prefer_summary = partition % 2 == 1;
    loop {
        let current = *mode.borrow_and_update();
        match current {
            Mode::Stopping => return Ok(()),
            Mode::Paused | Mode::Draining => {
                mode.changed().await?;
                continue;
            }
            Mode::Running => {}
        }
        tokio::select! {
            _ = shared.resources.background_turn() => {}
            result = mode.changed() => { result?; continue; }
        }
        let permit = tokio::select! {
            permit = shared.resources.acquire(background_request()) => permit?,
            result = mode.changed() => { result?; continue; }
        };
        let admission = shared.admission.read().await;
        if *shared.mode.borrow() != Mode::Running {
            drop(admission);
            drop(permit);
            continue;
        }
        let first = if prefer_summary {
            claim_summary(&shared, partition).await
        } else {
            claim_object(&shared, partition).await
        };
        let claim = match first {
            Ok(None) if prefer_summary => claim_object(&shared, partition).await,
            Ok(None) => claim_summary(&shared, partition).await,
            result => result,
        };
        let claim = match claim {
            Ok(claim) => claim,
            Err(_) => {
                // A temporary connection/lock failure must not kill the other
                // partitions or hold a permit while backing off.
                shared.failed.fetch_add(1, Ordering::Relaxed);
                drop(admission);
                drop(permit);
                tokio::select! {
                    _ = tokio::time::sleep(shared.config.retry_delay) => {}
                    _ = mode.changed() => {}
                }
                continue;
            }
        };
        if let Some(claim) = claim {
            let active = Active::new(shared.clone());
            drop(admission);
            prefer_summary = !prefer_summary;
            let result = execute(&shared, &claim).await;
            match result {
                Ok(()) => {
                    shared.completed.fetch_add(1, Ordering::Relaxed);
                    // A completed result is already durable; failure to clean
                    // operational metadata must never rerun business writes.
                    let _ = sqlx::query(
                        "DELETE FROM crm.scheduler_failures WHERE queue=$1 AND work_key=$2",
                    )
                    .bind(claim.queue())
                    .bind(&claim.key)
                    .execute(&shared.control)
                    .await;
                }
                Err(error) => {
                    shared.failed.fetch_add(1, Ordering::Relaxed);
                    // On DB loss the existing lease safely expires. Never fake
                    // acknowledgement or remove work merely to clear status.
                    let _ = release_failed(&shared, &claim, &error).await;
                }
            }
            drop(active);
            drop(permit);
        } else {
            drop(admission);
            drop(permit);
            tokio::select! {
                _ = tokio::time::sleep(shared.config.poll_interval) => {}
                _ = mode.changed() => {}
            }
        }
    }
}

async fn execute(shared: &Shared, claim: &Claimed) -> Result<()> {
    let operation = async {
        match &claim.work {
            Work::Object(lease) => crate::project(&shared.background, lease).await,
            Work::Summary(lease) => {
                let summary = messages::snapshot(&shared.background, lease).await?;
                messages::finish(&shared.background, lease, &summary).await
            }
        }
    };
    tokio::pin!(operation);
    let deadline = tokio::time::sleep(shared.config.task_timeout);
    tokio::pin!(deadline);
    let renewer = async {
        let mut pulse = tokio::time::interval_at(
            Instant::now() + shared.config.heartbeat_interval,
            shared.config.heartbeat_interval,
        );
        pulse.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            pulse.tick().await;
            if !renew(shared, claim).await? {
                bail!("task lease lost");
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! {
        result = &mut operation => result,
        _ = &mut deadline => bail!("task execution deadline exceeded"),
        result = renewer => result,
    }
}

async fn renew(shared: &Shared, claim: &Claimed) -> Result<bool> {
    let mut tx = shared.control.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='1s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout='3s'")
        .execute(&mut *tx)
        .await?;
    let update = match &claim.work {
        Work::Object(lease) => sqlx::query("UPDATE crm.jobs SET lease_until=clock_timestamp()+interval '2 minutes' WHERE id=$1 AND token=$2 AND state='running' AND lease_until>clock_timestamp()")
            .bind(lease.id).bind(lease.token).execute(&mut *tx).await,
        Work::Summary(lease) => sqlx::query("UPDATE crm.conversation_work SET lease_until=clock_timestamp()+interval '2 minutes' WHERE provider=$1 AND account=$2 AND conversation=$3 AND token=$4 AND lease_until>clock_timestamp()")
            .bind(&lease.provider).bind(&lease.account).bind(&lease.conversation).bind(lease.token).execute(&mut *tx).await,
    };
    match update {
        Ok(result) => {
            tx.commit().await?;
            Ok(result.rows_affected() == 1)
        }
        Err(error)
            if error.as_database_error().and_then(|e| e.code()).as_deref() == Some("55P03") =>
        {
            tx.rollback().await?;
            // A short final transaction may own the row while it commits. Read
            // committed ownership without extending an expired/stalled lease.
            let valid: bool = match &claim.work {
                Work::Object(lease) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.jobs WHERE id=$1 AND token=$2 AND state='running' AND lease_until>clock_timestamp())")
                    .bind(lease.id).bind(lease.token).fetch_one(&shared.control).await?,
                Work::Summary(lease) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.conversation_work WHERE provider=$1 AND account=$2 AND conversation=$3 AND token=$4 AND lease_until>clock_timestamp())")
                    .bind(&lease.provider).bind(&lease.account).bind(&lease.conversation).bind(lease.token).fetch_one(&shared.control).await?,
            };
            Ok(valid)
        }
        Err(error) => Err(error.into()),
    }
}

async fn release_failed(shared: &Shared, claim: &Claimed, error: &anyhow::Error) -> Result<()> {
    let mut tx = shared.control.begin().await?;
    let owned = match &claim.work {
        Work::Object(lease) => sqlx::query("SELECT id FROM crm.jobs WHERE id=$1 AND token=$2 AND state='running' FOR UPDATE")
            .bind(lease.id).bind(lease.token).fetch_optional(&mut *tx).await?.is_some(),
        Work::Summary(lease) => sqlx::query("SELECT token FROM crm.conversation_work WHERE provider=$1 AND account=$2 AND conversation=$3 AND token=$4 FOR UPDATE")
            .bind(&lease.provider).bind(&lease.account).bind(&lease.conversation).bind(lease.token).fetch_optional(&mut *tx).await?.is_some(),
    };
    if !owned {
        return Ok(());
    }
    let error_code = error
        .downcast_ref::<sqlx::Error>()
        .and_then(sqlx::Error::as_database_error)
        .and_then(|error| error.code())
        .map_or_else(|| "handler_error".to_owned(), |code| code.to_string());
    let failures: i32 = sqlx::query_scalar("INSERT INTO crm.scheduler_failures(queue,work_key,failures,retry_at,error_code)
      VALUES($1,$2,1,clock_timestamp(),$3) ON CONFLICT(queue,work_key) DO UPDATE SET failures=crm.scheduler_failures.failures+1,error_code=excluded.error_code,updated_at=clock_timestamp() RETURNING failures")
        .bind(claim.queue()).bind(&claim.key).bind(error_code).fetch_one(&mut *tx).await?;
    let delay = shared.config.retry_delay.as_millis() as f64
        * f64::from(1u32 << (failures.saturating_sub(1) as u32).min(5));
    sqlx::query("UPDATE crm.scheduler_failures SET retry_at=clock_timestamp()+($3*interval '1 millisecond'),quarantined=$4 WHERE queue=$1 AND work_key=$2")
        .bind(claim.queue()).bind(&claim.key).bind(delay.min(60_000.0)).bind(failures as u32 >= shared.config.max_attempts)
        .execute(&mut *tx).await?;
    match &claim.work {
        Work::Object(lease) => {
            sqlx::query("UPDATE crm.jobs SET state='queued',token=NULL,lease_until=NULL WHERE id=$1 AND token=$2")
            .bind(lease.id).bind(lease.token).execute(&mut *tx).await?;
        }
        Work::Summary(lease) => {
            sqlx::query("UPDATE crm.conversation_work SET token=NULL,lease_until=NULL WHERE provider=$1 AND account=$2 AND conversation=$3 AND token=$4")
            .bind(&lease.provider).bind(&lease.account).bind(&lease.conversation).bind(lease.token).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

#[path = "scheduler_source.rs"]
mod source_lane;

#[path = "scheduler_idle.rs"]
mod idle_lane;

#[path = "scheduler_whatsapp.rs"]
mod whatsapp_lane;

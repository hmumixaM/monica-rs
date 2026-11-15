//! Shared admission limits for interactive and background work.
//!
//! These are process-local permits. A deployment runs one scheduler process per
//! data directory; separate processes must not each assume they own the same disk
//! budget. Database ownership and durable job ordering never rely on these locks.
use anyhow::{Result, bail};
use serde::Serialize;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MEMORY_UNIT: usize = 64 * 1024;
const INTERACTIVE_MEMORY_RESERVE: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct PoolBudget {
    pub interactive: u32,
    pub background: u32,
    pub control: u32,
    pub acquire_timeout: Duration,
}

impl Default for PoolBudget {
    fn default() -> Self {
        Self {
            interactive: 8,
            background: 4,
            control: 3,
            acquire_timeout: Duration::from_secs(5),
        }
    }
}

/// Independent pools reserve connections for requests and lease/control traffic.
#[derive(Clone)]
pub struct DatabasePools {
    pub interactive: PgPool,
    pub background: PgPool,
    pub control: PgPool,
}

impl DatabasePools {
    /// Explicit schema administration has its own bounded connection budget.
    /// Slow DDL must not inherit the deadlines used by ordinary requests.
    pub async fn connect_migration(url: &str, statement_timeout_seconds: u32) -> Result<PgPool> {
        if !(1..=86_400).contains(&statement_timeout_seconds) {
            bail!("migration statement timeout must be between 1 and 86400 seconds");
        }
        let options = url
            .parse::<PgConnectOptions>()?
            .application_name("monica-migration")
            .options([
                (
                    "statement_timeout",
                    (u64::from(statement_timeout_seconds) * 1000).to_string(),
                ),
                ("lock_timeout", "30000".to_owned()),
                ("idle_in_transaction_session_timeout", "300000".to_owned()),
            ]);
        Ok(PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(options)
            .await?)
    }

    pub async fn connect(url: &str, budget: PoolBudget) -> Result<Self> {
        if budget.interactive == 0 || budget.background == 0 || budget.control < 2 {
            bail!(
                "database budgets require interactive/background capacity and at least two control connections"
            );
        }
        async fn connect_pool(
            url: &str,
            limit: u32,
            timeout: Duration,
            role: &'static str,
            statement_ms: i64,
        ) -> Result<PgPool> {
            Ok(PgPoolOptions::new()
                .acquire_time_level(log::LevelFilter::Debug)
                .max_connections(limit)
                .min_connections(0)
                .acquire_timeout(timeout)
                .after_connect(move |connection, _| {
                    Box::pin(async move {
                        // Request and bounded worker queries must not spend
                        // their deadline compiling LLVM plans. Explicit bulk
                        // migrations use their separate connection policy.
                        sqlx::query("SELECT set_config('application_name',$1,false),set_config('statement_timeout',$2,false),set_config('lock_timeout','1000',false),set_config('idle_in_transaction_session_timeout','30000',false),set_config('jit','off',false)")
                            .bind(role)
                            .bind(statement_ms.to_string())
                            .execute(connection)
                            .await?;
                        Ok(())
                    })
                })
                .connect(url)
                .await?)
        }
        let interactive = connect_pool(
            url,
            budget.interactive,
            budget.acquire_timeout,
            "monica-interactive",
            10_000,
        )
        .await?;
        let background = match connect_pool(
            url,
            budget.background,
            budget.acquire_timeout,
            "monica-background",
            15_000,
        )
        .await
        {
            Ok(pool) => pool,
            Err(error) => {
                interactive.close().await;
                return Err(error);
            }
        };
        let control = match connect_pool(
            url,
            budget.control,
            budget.acquire_timeout,
            "monica-control",
            5_000,
        )
        .await
        {
            Ok(pool) => pool,
            Err(error) => {
                interactive.close().await;
                background.close().await;
                return Err(error);
            }
        };
        Ok(Self {
            interactive,
            background,
            control,
        })
    }

    pub async fn close(&self) {
        tokio::join!(
            self.interactive.close(),
            self.background.close(),
            self.control.close()
        );
    }
}

#[derive(Clone, Debug)]
pub struct ResourceLimits {
    pub cpu: u32,
    pub network: u32,
    pub disk: u32,
    pub memory_bytes: usize,
    pub interactive_target: Duration,
    pub yield_for: Duration,
}

impl ResourceLimits {
    pub fn mac() -> Self {
        Self {
            cpu: std::thread::available_parallelism()
                .map_or(2, usize::from)
                .saturating_sub(1)
                .clamp(1, 4) as u32,
            network: 4,
            disk: 2,
            memory_bytes: 384 * 1024 * 1024,
            interactive_target: Duration::from_millis(500),
            yield_for: Duration::from_millis(250),
        }
    }

    pub fn nas() -> Self {
        Self {
            cpu: 2,
            network: 2,
            disk: 1,
            memory_bytes: 256 * 1024 * 1024,
            ..Self::mac()
        }
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Self::mac()
        } else {
            Self::nas()
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TaskResources {
    pub cpu: u32,
    pub network: u32,
    pub disk: u32,
    pub memory_bytes: usize,
}

#[derive(Clone)]
pub struct ResourceBudget {
    inner: Arc<Resources>,
}

struct Resources {
    limits: ResourceLimits,
    cpu: Arc<Semaphore>,
    network: Arc<Semaphore>,
    disk: Arc<Semaphore>,
    memory: Arc<Semaphore>,
    background_memory: Arc<Semaphore>,
    background_network: Arc<Semaphore>,
    clock: Instant,
    yield_until_ms: AtomicU64,
    slow_interactive_requests: AtomicU64,
}

/// Permits are released on every return, cancellation and panic path.
pub struct ResourcePermit {
    _background_memory: Option<OwnedSemaphorePermit>,
    _background_network: Option<OwnedSemaphorePermit>,
    _memory: Option<OwnedSemaphorePermit>,
    _cpu: Option<OwnedSemaphorePermit>,
    _network: Option<OwnedSemaphorePermit>,
    _disk: Option<OwnedSemaphorePermit>,
}

#[derive(Debug, Serialize)]
pub struct ResourceStatus {
    pub cpu_available: usize,
    pub network_available: usize,
    pub disk_available: usize,
    pub memory_available_bytes: usize,
    pub background_memory_available_bytes: usize,
    pub background_network_available: usize,
    pub yielding_to_interactive: bool,
    pub slow_interactive_requests: u64,
}

impl ResourceBudget {
    pub fn new(limits: ResourceLimits) -> Result<Self> {
        if limits.cpu == 0 || limits.network == 0 || limits.disk == 0 {
            bail!("resource capacities must be positive");
        }
        let memory_units = limits.memory_bytes / MEMORY_UNIT;
        if memory_units == 0 || memory_units > u32::MAX as usize {
            bail!("invalid memory budget");
        }
        if limits.yield_for.is_zero() || limits.interactive_target.is_zero() {
            bail!("interaction feedback durations must be positive");
        }
        Ok(Self {
            inner: Arc::new(Resources {
                cpu: Arc::new(Semaphore::new(limits.cpu as usize)),
                network: Arc::new(Semaphore::new(limits.network as usize)),
                disk: Arc::new(Semaphore::new(limits.disk as usize)),
                memory: Arc::new(Semaphore::new(memory_units)),
                background_memory: Arc::new(Semaphore::new(
                    memory_units.saturating_sub(INTERACTIVE_MEMORY_RESERVE / MEMORY_UNIT),
                )),
                background_network: Arc::new(Semaphore::new(
                    limits.network.saturating_sub(1) as usize
                )),
                limits,
                clock: Instant::now(),
                yield_until_ms: AtomicU64::new(0),
                slow_interactive_requests: AtomicU64::new(0),
            }),
        })
    }

    /// Call once after each real interactive request, including pool/SQL time.
    pub fn record_interactive(&self, elapsed: Duration) {
        if elapsed >= self.inner.limits.interactive_target {
            self.inner
                .slow_interactive_requests
                .fetch_add(1, Ordering::Relaxed);
            let until = self
                .now_ms()
                .saturating_add(self.inner.limits.yield_for.as_millis() as u64);
            self.inner
                .yield_until_ms
                .fetch_max(until, Ordering::Relaxed);
        }
    }

    fn now_ms(&self) -> u64 {
        self.inner.clock.elapsed().as_millis() as u64
    }

    /// New background work yields; an already-open transaction is never paused.
    pub async fn background_turn(&self) {
        loop {
            let remaining = self
                .inner
                .yield_until_ms
                .load(Ordering::Relaxed)
                .saturating_sub(self.now_ms());
            if remaining == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(remaining)).await;
        }
    }

    pub async fn acquire(&self, request: TaskResources) -> Result<ResourcePermit> {
        self.validate_request(request)?;
        let memory = request.memory_bytes.div_ceil(MEMORY_UNIT);
        // One global acquisition order prevents requests for multiple resources
        // from deadlocking each other. No DB lease has been claimed at this point.
        // A zero-sized reservation must skip the fair semaphore queue. A
        // staged request may already own its memory while acquiring only disk;
        // waiting behind a larger memory request would otherwise deadlock it.
        async fn take(
            semaphore: &Arc<Semaphore>,
            amount: u32,
        ) -> Result<Option<OwnedSemaphorePermit>> {
            if amount == 0 {
                Ok(None)
            } else {
                Ok(Some(semaphore.clone().acquire_many_owned(amount).await?))
            }
        }
        let memory = take(&self.inner.memory, memory as u32).await?;
        let cpu = take(&self.inner.cpu, request.cpu).await?;
        let network = take(&self.inner.network, request.network).await?;
        let disk = take(&self.inner.disk, request.disk).await?;
        Ok(ResourcePermit {
            _background_memory: None,
            _background_network: None,
            _memory: memory,
            _cpu: cpu,
            _network: network,
            _disk: disk,
        })
    }

    /// Background admission is a subset of the global budget. Take these
    /// permits first so queued background work never enters the global fair
    /// semaphore queue ahead of an interactive request using the reserved slice.
    pub async fn acquire_background(&self, request: TaskResources) -> Result<ResourcePermit> {
        self.validate_background_request(request)?;
        let memory = request.memory_bytes.div_ceil(MEMORY_UNIT) as u32;
        let memory = if memory == 0 {
            None
        } else {
            Some(
                self.inner
                    .background_memory
                    .clone()
                    .acquire_many_owned(memory)
                    .await?,
            )
        };
        let network = if request.network == 0 {
            None
        } else {
            Some(
                self.inner
                    .background_network
                    .clone()
                    .acquire_many_owned(request.network)
                    .await?,
            )
        };
        let mut permit = self.acquire(request).await?;
        permit._background_memory = memory;
        permit._background_network = network;
        Ok(permit)
    }
    pub fn validate_background_request(&self, request: TaskResources) -> Result<()> {
        self.validate_request(request)?;
        if request.network > self.inner.limits.network.saturating_sub(1) {
            bail!(
                "background network capacity requires a reserved interactive slot; configure at least two network slots"
            );
        }
        let available = (self.inner.limits.memory_bytes / MEMORY_UNIT)
            .saturating_sub(INTERACTIVE_MEMORY_RESERVE / MEMORY_UNIT);
        if request.memory_bytes.div_ceil(MEMORY_UNIT) > available {
            bail!("background task exceeds memory capacity after the 16 MiB interactive reserve");
        }
        Ok(())
    }

    /// Reject impossible requests before starting a worker or waiting on a queue.
    pub fn validate_request(&self, request: TaskResources) -> Result<()> {
        let memory = request.memory_bytes.div_ceil(MEMORY_UNIT);
        if request.cpu > self.inner.limits.cpu
            || request.network > self.inner.limits.network
            || request.disk > self.inner.limits.disk
            || memory > self.inner.limits.memory_bytes / MEMORY_UNIT
        {
            bail!("task exceeds resource capacity");
        }
        Ok(())
    }

    /// A cancelled caller cannot release the permit while native work still runs.
    pub async fn run_blocking<F, T>(&self, memory_bytes: usize, operation: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.background_turn().await;
        let permit = self
            .acquire(TaskResources {
                cpu: 1,
                memory_bytes,
                ..TaskResources::default()
            })
            .await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await?
    }

    pub fn status(&self) -> ResourceStatus {
        ResourceStatus {
            cpu_available: self.inner.cpu.available_permits(),
            network_available: self.inner.network.available_permits(),
            disk_available: self.inner.disk.available_permits(),
            memory_available_bytes: self.inner.memory.available_permits() * MEMORY_UNIT,
            background_memory_available_bytes: self.inner.background_memory.available_permits()
                * MEMORY_UNIT,
            background_network_available: self.inner.background_network.available_permits(),
            yielding_to_interactive: self.inner.yield_until_ms.load(Ordering::Relaxed)
                > self.now_ms(),
            slow_interactive_requests: self.inner.slow_interactive_requests.load(Ordering::Relaxed),
        }
    }
}

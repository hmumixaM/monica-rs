use monica_core::{
    Provider, Record, ingest, messages,
    resources::{DatabasePools, PoolBudget, ResourceBudget, ResourceLimits, TaskResources},
    scheduler::{Scheduler, SchedulerConfig, SchedulerHandle},
};
use serde_json::json;
use sqlx::{ConnectOptions, PgPool, postgres::PgPoolOptions};
use std::time::{Duration, Instant};

fn config() -> SchedulerConfig {
    SchedulerConfig {
        start_paused: false,
        partitions: 4,
        poll_interval: Duration::from_millis(20),
        heartbeat_interval: Duration::from_millis(200),
        task_timeout: Duration::from_secs(10),
        drain_timeout: Duration::from_secs(15),
        retry_delay: Duration::from_millis(30),
        max_attempts: 2,
    }
}

fn resources() -> ResourceBudget {
    ResourceBudget::new(ResourceLimits {
        cpu: 4,
        memory_bytes: 64 * 1024 * 1024,
        ..ResourceLimits::nas()
    })
    .unwrap()
}

async fn pools(pool: &PgPool) -> DatabasePools {
    DatabasePools::connect(
        pool.connect_options().to_url_lossy().as_str(),
        PoolBudget::default(),
    )
    .await
    .unwrap()
}

fn record(object: &str, event: &str, text: &str) -> Record {
    Record {
        provider: Provider::Wechat,
        account: "scheduler-fixture".into(),
        object: object.into(),
        kind: "post".into(),
        event: event.into(),
        payload: json!({"text":text}),
    }
}

async fn empty(handle: &SchedulerHandle) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let status = handle.status().await.unwrap();
            assert!(
                !status.stopped,
                "scheduler unexpectedly stopped: {status:?}"
            );
            if status.pending_object_jobs == 0
                && status.pending_summary_jobs == 0
                && status.pending_import_batches == 0
                && status.active_jobs == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("scheduler did not finish actual queued work");
}

#[sqlx::test(migrations = "../../migrations")]
async fn actual_queues_pause_resume_and_restart_preserve_work(pool: PgPool) {
    sqlx::query("UPDATE crm.scheduler_control SET paused=true")
        .execute(&pool)
        .await
        .unwrap();
    for object in 0..12 {
        for (event, value) in [("1", "A"), ("2", "B"), ("3", "A")] {
            ingest(&pool, &record(&format!("post-{object}"), event, value))
                .await
                .unwrap();
        }
    }
    messages::append(
        &pool,
        Provider::Wechat,
        &[messages::Message {
            account: "scheduler-fixture".into(),
            conversation: "synthetic-room".into(),
            external_id: "message-1".into(),
            sent_at_ms: 1234,
            sender: "synthetic-person".into(),
            body: "Synthetic message".into(),
        }],
    )
    .await
    .unwrap();
    let pools = pools(&pool).await;
    let handle = Scheduler::start(
        pools.background.clone(),
        pools.control.clone(),
        resources(),
        config(),
    )
    .await
    .unwrap();
    let status = handle.status().await.unwrap();
    assert!(status.paused);
    assert_eq!(status.pending_object_jobs, 36);
    assert_eq!(status.pending_summary_jobs, 1);
    handle.resume().await.unwrap();
    empty(&handle).await;
    let projected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.projections p JOIN wechat.revisions r ON r.object_id=p.object_id AND r.version=p.version JOIN wechat.payloads v ON v.hash=r.hash WHERE p.provider='wechat' AND p.version=3 AND v.value->>'text'='A'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(projected, 12);
    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.job_history")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(history, 36);
    let summary: (i64, i64) =
        sqlx::query_as("SELECT message_count,generation FROM crm.conversation_summaries")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(summary, (1, 1));
    handle.pause().await.unwrap();
    ingest(&pool, &record("after-pause", "1", "pending"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(handle.status().await.unwrap().pending_object_jobs, 1);
    handle.shutdown().await.unwrap();
    assert!(handle.status().await.unwrap().stopped);
    let restarted = Scheduler::start(
        pools.background.clone(),
        pools.control.clone(),
        resources(),
        config(),
    )
    .await
    .unwrap();
    assert!(restarted.status().await.unwrap().paused);
    assert_eq!(restarted.status().await.unwrap().pending_object_jobs, 1);
    restarted.resume().await.unwrap();
    empty(&restarted).await;
    restarted.drain().await.unwrap();
    assert!(restarted.status().await.unwrap().paused);
    restarted.shutdown().await.unwrap();
    pools.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn committed_import_receipt_and_media_recover_with_unavailable_ai_credentials(pool: PgPool) {
    let mut batch: monica_core::imports::Batch = serde_json::from_value(json!({
        "source":"wechat", "stream":"posts", "batch_id":"crashed-request",
        "idempotency_key":"crashed-request", "observed_at":"2026-01-01T00:00:00Z",
        "cursor_after":"synthetic-checkpoint",
        "records":(0..230).map(|i|json!({
            "external_id":format!("recovered-{i}"),"entity_type":"post",
            "source_account_external_id":"recovery-fixture",
            "content":{"text":format!("Synthetic recovered post {i}")}
        })).collect::<Vec<_>>()
    }))
    .unwrap();
    batch.records[0].extra.insert(
        "media".into(),
        json!([{"bytes":"414243","media_type":"application/octet-stream"}]),
    );
    let saved_cipher = monica_core::credentials::CredentialCipher::new(
        "synthetic-previous-installation-secret-at-least-32",
    )
    .unwrap();
    let credentials = saved_cipher
        .encrypt(&json!({"api_key":"synthetic-only-key"}))
        .unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=true,provider='openai',base_url='http://127.0.0.1:9/v1',credential_ciphertext=$1")
        .bind(credentials).execute(&pool).await.unwrap();
    let cipher = monica_core::credentials::CredentialCipher::new(
        "synthetic-current-installation-secret-at-least-32",
    )
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let id = monica_core::imports::receive(&mut tx, &batch, "synthetic-request-hash")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Simulate the HTTP process disappearing after acceptance, before any source
    // processing. No original database, archive, collector or real user is read.
    let pools = pools(&pool).await;
    let media_root = tempfile::tempdir().unwrap();
    let handle = Scheduler::start_with_sources(
        pools.background.clone(),
        pools.control.clone(),
        ResourceBudget::new(ResourceLimits::nas()).unwrap(),
        config(),
        media_root.path().to_path_buf(),
        cipher,
    )
    .await
    .unwrap();
    empty(&handle).await;
    let receipt = monica_core::imports::receipt(&pool, id).await.unwrap();
    assert_eq!(receipt["status"], "complete");
    assert_eq!(receipt["processed_count"], 230);
    assert_eq!(receipt["inserted_count"], 230);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.projections")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 230);
    let cursor: String = sqlx::query_scalar(
        "SELECT cursor FROM crm.source_cursors WHERE source='wechat' AND stream='posts'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cursor, "synthetic-checkpoint");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let linked: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM crm.media_links WHERE entity_type='social_post'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if linked == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("AI configuration errors must not stop attachment consumption");
    assert!(handle.status().await.unwrap().import_recovery_attempts >= 3);
    assert!(handle.status().await.unwrap().failed_attempts > 0);
    handle.shutdown().await.unwrap();
    pools.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn only_one_coordinator_can_own_host_resource_budgets(pool: PgPool) {
    let pools = pools(&pool).await;
    let handle = Scheduler::start(
        pools.background.clone(),
        pools.control.clone(),
        resources(),
        config(),
    )
    .await
    .unwrap();
    assert!(
        Scheduler::start(
            pools.background.clone(),
            pools.control.clone(),
            resources(),
            config(),
        )
        .await
        .is_err()
    );
    handle.shutdown().await.unwrap();
    pools.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn blocked_subject_yields_other_partition_and_drain_waits_for_commit(pool: PgPool) {
    let blocked = record("blocked", "1", "initial");
    let blocked_id = monica_core::object_id(&blocked).unwrap();
    ingest(&pool, &blocked).await.unwrap();
    let first = monica_core::claim(&pool).await.unwrap().unwrap();
    monica_core::project(&pool, &first).await.unwrap();
    let partition: i64 = sqlx::query_scalar(
        "SELECT mod(hashtextextended('wechat:'||$1::text,0) & 9223372036854775807,4)",
    )
    .bind(blocked_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut independent = None;
    for index in 0..50 {
        let candidate = record(&format!("other-{index}"), "1", "independent");
        let other: i64 = sqlx::query_scalar(
            "SELECT mod(hashtextextended('wechat:'||$1::text,0) & 9223372036854775807,4)",
        )
        .bind(monica_core::object_id(&candidate).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
        if other != partition {
            independent = Some(candidate);
            break;
        }
    }
    let independent = independent.unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT object_id FROM crm.projections WHERE object_id=$1 FOR UPDATE")
        .bind(blocked_id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    ingest(&pool, &record("blocked", "2", "new")).await.unwrap();
    ingest(&pool, &independent).await.unwrap();
    let mut pools = pools(&pool).await;
    // This scenario intentionally holds a projection lock across scheduler
    // coordination. Keep the production pool's 1s contention deadline out of
    // the fixture, so CI load cannot turn the held lock into a retry.
    pools.background.close().await;
    pools.background = PgPoolOptions::new()
        .max_connections(PoolBudget::default().background)
        .acquire_timeout(PoolBudget::default().acquire_timeout)
        .connect_with(
            pool.connect_options()
                .as_ref()
                .clone()
                .application_name("monica-background")
                .options([
                    ("lock_timeout", "15000"),
                    ("statement_timeout", "20000"),
                    ("idle_in_transaction_session_timeout", "30000"),
                ]),
        )
        .await
        .unwrap();
    let mut scheduler_config = config();
    scheduler_config.task_timeout = Duration::from_secs(30);
    scheduler_config.drain_timeout = Duration::from_secs(35);
    let handle = Scheduler::start(
        pools.background.clone(),
        pools.control.clone(),
        resources(),
        scheduler_config,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let progressed: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM crm.projections WHERE object_id=$1)",
            )
            .bind(monica_core::object_id(&independent).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
            if progressed && handle.status().await.unwrap().active_jobs == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Exercise a real lease heartbeat while the result transaction holds its
    // job row. The heartbeat must not deadlock the task or falsely release it.
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let renewing: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND application_name='monica-control' AND wait_event_type='Lock' AND query LIKE 'UPDATE crm.jobs SET lease_until=%')",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if renewing {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lease heartbeat did not reach the held result transaction");
    let drain_handle = handle.clone();
    let draining = tokio::spawn(async move { drain_handle.drain().await });
    tokio::time::timeout(Duration::from_secs(4), async {
        while !handle.status().await.unwrap().draining {
            assert!(
                !draining.is_finished(),
                "drain completed before entering draining state"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("drain did not enter its observable draining state");
    assert!(
        !draining.is_finished(),
        "drain passed the held projection lock"
    );
    assert!(handle.status().await.unwrap().draining);
    blocker.rollback().await.unwrap();
    draining.await.unwrap().unwrap();
    let status = handle.status().await.unwrap();
    assert!(status.paused);
    assert_eq!(status.active_jobs, 0);
    assert_eq!(status.failed_attempts, 0);
    let version: i64 = sqlx::query_scalar("SELECT version FROM crm.projections WHERE object_id=$1")
        .bind(blocked_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 2);
    handle.shutdown().await.unwrap();
    pools.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn failures_quarantine_without_losing_work_or_starving_other_objects(pool: PgPool) {
    let broken = record("rejected", "1", "must retry");
    let broken_id = monica_core::object_id(&broken).unwrap();
    ingest(&pool, &broken).await.unwrap();
    sqlx::query(&format!(
        "ALTER TABLE crm.projections ADD CONSTRAINT synthetic_rejection CHECK(object_id<>'{broken_id}'::uuid)"
    ))
    .execute(&pool)
    .await
    .unwrap();
    ingest(&pool, &record("healthy", "1", "accepted"))
        .await
        .unwrap();
    let pools = pools(&pool).await;
    let handle = Scheduler::start(
        pools.background.clone(),
        pools.control.clone(),
        resources(),
        config(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = handle.status().await.unwrap();
            if status.quarantined_jobs == 1 && status.completed_jobs == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (pending, failed): (i64, i32) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM crm.jobs),(SELECT failures FROM crm.scheduler_failures)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((pending, failed), (1, 2));
    assert_eq!(handle.status().await.unwrap().failed_attempts, 2);
    sqlx::query("ALTER TABLE crm.projections DROP CONSTRAINT synthetic_rejection")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(handle.retry_failed_jobs().await.unwrap(), 1);
    empty(&handle).await;
    let completed: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.job_history")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(completed, 2);
    handle.shutdown().await.unwrap();
    pools.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn saturated_background_pool_does_not_use_interactive_or_control_reserve(pool: PgPool) {
    let pools = DatabasePools::connect(
        pool.connect_options().to_url_lossy().as_str(),
        PoolBudget {
            interactive: 1,
            background: 1,
            control: 2,
            acquire_timeout: Duration::from_millis(200),
        },
    )
    .await
    .unwrap();
    let background = pools.background.acquire().await.unwrap();
    assert!(pools.background.acquire().await.is_err());
    for reserved in [&pools.interactive, &pools.control] {
        let value: i32 = tokio::time::timeout(
            Duration::from_secs(1),
            sqlx::query_scalar("SELECT 1").fetch_one(reserved),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(value, 1);
    }
    drop(background);
    pools.close().await;
}

#[tokio::test]
async fn disk_and_memory_limits_block_and_cancel_without_leaking_permits() {
    assert_eq!(ResourceLimits::mac().disk, 2);
    assert_eq!(ResourceLimits::nas().disk, 1);
    let budgets = ResourceBudget::new(ResourceLimits {
        cpu: 1,
        memory_bytes: 64 * 1024,
        ..ResourceLimits::nas()
    })
    .unwrap();
    let permit = budgets
        .acquire(TaskResources {
            disk: 1,
            memory_bytes: 64 * 1024,
            ..TaskResources::default()
        })
        .await
        .unwrap();
    assert_eq!(budgets.status().disk_available, 0);
    assert_eq!(budgets.status().memory_available_bytes, 0);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            budgets.acquire(TaskResources {
                disk: 1,
                ..TaskResources::default()
            })
        )
        .await
        .is_err()
    );
    assert!(
        budgets
            .acquire(TaskResources {
                memory_bytes: 64 * 1024 + 1,
                ..TaskResources::default()
            })
            .await
            .is_err()
    );
    drop(permit);
    assert_eq!(budgets.status().disk_available, 1);
    assert_eq!(budgets.status().memory_available_bytes, 64 * 1024);
}

#[tokio::test]
async fn interactive_latency_delays_new_background_admission() {
    let budgets = ResourceBudget::new(ResourceLimits {
        interactive_target: Duration::from_millis(1),
        yield_for: Duration::from_millis(60),
        ..ResourceLimits::nas()
    })
    .unwrap();
    budgets.record_interactive(Duration::from_millis(5));
    assert!(budgets.status().yielding_to_interactive);
    let start = Instant::now();
    budgets.background_turn().await;
    assert!(start.elapsed() >= Duration::from_millis(45));
    assert!(!budgets.status().yielding_to_interactive);
    assert_eq!(budgets.status().slow_interactive_requests, 1);
}

#[tokio::test]
async fn cancelled_native_caller_does_not_release_a_still_running_cpu_task() {
    let budgets = ResourceBudget::new(ResourceLimits {
        cpu: 1,
        ..ResourceLimits::nas()
    })
    .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let cloned = budgets.clone();
    let caller = tokio::spawn(async move {
        cloned
            .run_blocking(64 * 1024, move || {
                let _ = started_tx.send(());
                stop_rx.recv().unwrap();
                Ok(())
            })
            .await
    });
    started_rx.await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert_eq!(budgets.status().cpu_available, 0);
    stop_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while budgets.status().cpu_available == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn staged_disk_request_does_not_wait_behind_unavailable_memory() {
    let resources = ResourceBudget::new(ResourceLimits::nas()).unwrap();
    let buffered = resources
        .acquire(TaskResources {
            memory_bytes: 256 * 1024 * 1024,
            ..Default::default()
        })
        .await
        .unwrap();
    let large = resources.acquire(TaskResources {
        memory_bytes: 192 * 1024 * 1024,
        ..Default::default()
    });
    tokio::pin!(large);
    std::future::poll_fn(|cx| {
        assert!(large.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let disk = tokio::time::timeout(
        Duration::from_secs(1),
        resources.acquire(TaskResources {
            disk: 1,
            ..Default::default()
        }),
    )
    .await
    .expect("disk-only work must not enter the memory wait queue")
    .unwrap();
    drop(disk);
    drop(buffered);
    drop(large.await.unwrap());
    assert_eq!(resources.status().memory_available_bytes, 256 * 1024 * 1024);
}

#[sqlx::test(migrations = "../../migrations")]
async fn configured_ai_lane_does_not_use_test_fake_provider(pool: PgPool) {
    let db = pools(&pool).await;
    let root = tempfile::tempdir().unwrap();
    let profile = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic AI lane')").bind(profile).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,provider='fake'")
        .execute(&pool)
        .await
        .unwrap();
    let job = monica_core::ai::enqueue(&pool, "ai.profile", &profile.to_string())
        .await
        .unwrap();
    let scheduler = Scheduler::start_with_media(
        db.background.clone(),
        db.control.clone(),
        ResourceBudget::new(ResourceLimits::nas()).unwrap(),
        config(),
        root.path().to_path_buf(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(scheduler.status().await.unwrap().pending_ai_jobs, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT attempts FROM crm.ai_jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    scheduler.pause().await.unwrap();
    scheduler.drain().await.unwrap();
    assert_eq!(scheduler.status().await.unwrap().active_jobs, 0);
    scheduler.shutdown().await.unwrap();
    db.close().await;
}

#[tokio::test]
async fn nas_background_models_leave_interactive_network_and_memory_available() {
    let budget = ResourceBudget::new(ResourceLimits::nas()).unwrap();
    let model = monica_core::ai::background_resources();
    let first = budget.acquire_background(model).await.unwrap();
    let other = budget.clone();
    let waiting = tokio::spawn(async move { other.acquire_background(model).await });
    tokio::task::yield_now().await;
    let interactive = tokio::time::timeout(
        Duration::from_millis(100),
        budget.acquire(TaskResources {
            network: 1,
            memory_bytes: 4 * 1024 * 1024,
            ..Default::default()
        }),
    )
    .await
    .expect("foreground request must not wait behind queued background model")
    .unwrap();
    assert!(!waiting.is_finished());
    assert_eq!(budget.status().network_available, 0);
    drop(interactive);
    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(second);
    assert_eq!(budget.status().memory_available_bytes, 256 * 1024 * 1024);
    assert_eq!(
        budget.status().background_memory_available_bytes,
        240 * 1024 * 1024
    );
    assert_eq!(budget.status().background_network_available, 1);
}
#[tokio::test]
async fn background_capacity_and_cancel_are_explicit_without_cpu_reserve() {
    let budget = ResourceBudget::new(ResourceLimits {
        network: 1,
        cpu: 1,
        ..ResourceLimits::nas()
    })
    .unwrap();
    assert!(
        budget
            .acquire_background(TaskResources {
                network: 1,
                ..Default::default()
            })
            .await
            .is_err()
    );
    assert!(
        budget
            .acquire_background(TaskResources {
                memory_bytes: 241 * 1024 * 1024,
                ..Default::default()
            })
            .await
            .is_err()
    );
    let cpu = budget
        .acquire_background(TaskResources {
            cpu: 1,
            memory_bytes: 32 * 1024 * 1024,
            ..Default::default()
        })
        .await
        .unwrap();
    drop(cpu);
    let budget = ResourceBudget::new(ResourceLimits::nas()).unwrap();
    let first = budget
        .acquire_background(TaskResources {
            network: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    let other = budget.clone();
    let waiting = tokio::spawn(async move {
        other
            .acquire_background(TaskResources {
                network: 1,
                memory_bytes: 128 * 1024 * 1024,
                ..Default::default()
            })
            .await
    });
    tokio::task::yield_now().await;
    waiting.abort();
    let _ = waiting.await;
    drop(first);
    assert_eq!(
        budget.status().background_memory_available_bytes,
        240 * 1024 * 1024
    );
    assert_eq!(budget.status().background_network_available, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unavailable_optional_bridge_does_not_stop_mail_or_scheduler(pool: PgPool) {
    let pools = pools(&pool).await;
    let root = tempfile::tempdir().unwrap();
    let cid = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES($1,'mail','offline@example.invalid','Offline synthetic mail','archive')").bind(cid).execute(&pool).await.unwrap();
    let scheduler = Scheduler::start_with_source_bridge(
        pools.background.clone(),
        pools.control.clone(),
        ResourceBudget::new(ResourceLimits::nas()).unwrap(),
        config(),
        root.path().to_owned(),
        monica_core::credentials::CredentialCipher::new(
            "synthetic-bridge-test-installation-secret",
        )
        .unwrap(),
        Some(root.path().join("missing-optional-binary")),
    )
    .await
    .unwrap();
    // No enabled QR account: even the invalid configured path must be inert.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!scheduler.is_stopped());
    let qr = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,'whatsapp','synthetic-device','Synthetic missing bridge','qr',true)").bind(qr).execute(&pool).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state: String =
                sqlx::query_scalar("SELECT state FROM crm.source_connections WHERE id=$1")
                    .bind(qr)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            if state == "needs_bridge" {
                break;
            }
            assert!(!scheduler.is_stopped());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    monica_core::connections::commit_page(&mut tx,Provider::Mail,cid,"offline",&[json!({"external_id":"one","kind":"message","thread_id":"thread","body":"mail still recovers after bridge failure","included":true,"participants":[],"occurred_at":"2024-01-01T00:00:00Z"})],&json!({"complete":true})).await.unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM crm.connection_evidence WHERE connection_id=$1",
            )
            .bind(cid)
            .fetch_one(&pool)
            .await
            .unwrap();
            if count == 1 {
                break;
            }
            assert!(!scheduler.is_stopped());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(!scheduler.is_stopped());
    let state: String = sqlx::query_scalar("SELECT state FROM crm.source_connections WHERE id=$1")
        .bind(qr)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "needs_bridge");
    scheduler.shutdown().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn whatsapp_connection_pause_retains_keys_and_disconnect_erases_them(pool: PgPool) {
    let pools = pools(&pool).await;
    let root = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4();
    let device = root.path().join("sources/whatsapp").join(id.to_string());
    tokio::fs::create_dir_all(&device).await.unwrap();
    tokio::fs::write(device.join("device.sqlite"), b"synthetic-device-keys")
        .await
        .unwrap();
    tokio::fs::write(device.join("history.json"), b"synthetic-history")
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,'whatsapp','paused-device','Synthetic device','qr',false,'paused')")
        .bind(id).execute(&pool).await.unwrap();
    let scheduler = Scheduler::start_with_source_bridge(
        pools.background.clone(),
        pools.control.clone(),
        ResourceBudget::new(ResourceLimits::nas()).unwrap(),
        config(),
        root.path().to_owned(),
        monica_core::credentials::CredentialCipher::new(
            "synthetic-device-pause-installation-secret",
        )
        .unwrap(),
        Some(root.path().join("missing-optional-bridge")),
    )
    .await
    .unwrap();
    async fn control(path: &std::path::Path, enabled: bool, paused: bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = tokio::fs::read(path.join("control.json"))
                    .await
                    .ok()
                    .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok());
                if value.is_some_and(|v| v["enabled"] == enabled && v["paused"] == paused) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    control(&device, true, true).await;
    assert!(device.join("device.sqlite").exists());
    sqlx::query("UPDATE crm.source_connections SET enabled=true,state='connecting',generation=generation+1 WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    control(&device, true, false).await;
    assert!(device.join("device.sqlite").exists());
    scheduler.pause().await.unwrap();
    control(&device, true, true).await;
    assert!(device.join("device.sqlite").exists());
    sqlx::query("UPDATE crm.source_connections SET enabled=false,state='disconnected',generation=generation+1 WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    control(&device, false, true).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while device.join("device.sqlite").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(device.join("history.json").exists());
    scheduler.shutdown().await.unwrap();
    pools.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn whatsapp_disconnect_retries_storage_failure_after_restart(pool: PgPool) {
    async fn start(pools: &DatabasePools, root: &std::path::Path) -> SchedulerHandle {
        Scheduler::start_with_source_bridge(
            pools.background.clone(),
            pools.control.clone(),
            ResourceBudget::new(ResourceLimits::nas()).unwrap(),
            config(),
            root.to_owned(),
            monica_core::credentials::CredentialCipher::new(
                "synthetic-disconnect-recovery-installation-secret",
            )
            .unwrap(),
            Some(root.join("missing-optional-bridge")),
        )
        .await
        .unwrap()
    }
    let pools = pools(&pool).await;
    // Both failure points must preserve the operation across server restart.
    for obstructed in ["control.json", "device.sqlite"] {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let device = root.path().join("sources/whatsapp").join(id.to_string());
        tokio::fs::create_dir_all(device.join(obstructed))
            .await
            .unwrap();
        if obstructed != "device.sqlite" {
            tokio::fs::write(device.join("device.sqlite"), b"synthetic-device-keys")
                .await
                .unwrap();
        }
        tokio::fs::write(device.join("device.sqlite-wal"), b"synthetic-device-wal")
            .await
            .unwrap();
        tokio::fs::write(device.join("history.json"), b"synthetic-history")
            .await
            .unwrap();
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,'whatsapp',$2,'Synthetic disconnect recovery','qr',false,'disconnected')")
            .bind(id).bind(id.to_string()).execute(&pool).await.unwrap();
        let scheduler = start(&pools, root.path()).await;
        let state = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (state, error): (String, Option<String>) =
                    sqlx::query_as("SELECT state,error FROM crm.source_connections WHERE id=$1")
                        .bind(id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                if error.is_some() {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        scheduler.shutdown().await.unwrap();
        assert_eq!(
            state, "disconnected",
            "lost disconnect intent after {obstructed} failure"
        );
        assert!(device.join("device.sqlite-wal").exists());
        tokio::fs::remove_dir(device.join(obstructed))
            .await
            .unwrap();
        let scheduler = start(&pools, root.path()).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = tokio::fs::read(device.join("status.json"))
                    .await
                    .ok()
                    .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok());
                let error: Option<String> =
                    sqlx::query_scalar("SELECT error FROM crm.source_connections WHERE id=$1")
                        .bind(id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                if !device.join("device.sqlite").exists()
                    && !device.join("device.sqlite-wal").exists()
                    && status.is_some_and(|v| v["state"] == "disconnected")
                    && error.is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let control: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(device.join("control.json")).await.unwrap())
                .unwrap();
        assert_eq!(control, json!({"enabled":false,"paused":true}));
        assert_eq!(
            tokio::fs::read(device.join("history.json")).await.unwrap(),
            b"synthetic-history"
        );
        scheduler.shutdown().await.unwrap();
        sqlx::query("DELETE FROM crm.source_connections WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    pools.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn committed_ingest_backlog_runs_without_idle_delay_and_memory_recovers_afterward(
    pool: PgPool,
) {
    use monica_core::{identity, imports};
    let person = uuid::Uuid::new_v4();
    let peer = identity::account_id(Provider::Wechat, "backlog-peer").unwrap();
    sqlx::query(
        "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Backlog peer')",
    )
    .bind(person)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat','backlog-peer','person','Backlog peer')").bind(peer).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
        .bind(peer)
        .bind(person)
        .execute(&pool)
        .await
        .unwrap();
    for i in 0..12 {
        let batch:imports::Batch=serde_json::from_value(json!({"source":"wechat","stream":"messages","batch_id":format!("backlog-{i}"),"idempotency_key":format!("backlog-{i}"),"observed_at":"2026-01-01T00:00:00Z","records":[{"external_id":format!("backlog-message-{i}"),"content":{"account":"self","conversation":"backlog-peer","sender":"backlog-peer","sent_at_ms":i,"text":format!("Backlog body {i}")}}]})).unwrap();
        let mut tx = pool.begin().await.unwrap();
        imports::receive(&mut tx, &batch, &format!("hash-{i}"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    // One actual chunk commits an outbox instead of doing Memory materialization.
    assert_eq!(imports::resume_pending(&pool).await.unwrap(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind='chat'"
        )
        .bind(person)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert!(sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_pending_records WHERE profiles @> ARRAY[$1::uuid])").bind(person).fetch_one(&pool).await.unwrap());
    // A different import waiting for its retry must not starve derived work.
    let delayed: imports::Batch = serde_json::from_value(json!({
        "source":"wechat","stream":"messages","batch_id":"delayed-retry","idempotency_key":"delayed-retry",
        "observed_at":"2026-01-01T00:00:00Z","records":[]
    })).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let delayed_id = imports::receive(&mut tx, &delayed, "delayed-hash")
        .await
        .unwrap();
    sqlx::query("UPDATE crm.import_batches SET retry_after=now()+interval '1 hour' WHERE id=$1")
        .bind(delayed_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Restart with a durable pending reference and11 real batches. The old
    // unconditional1s wait cannot finish11 turns within this4s regression bound.
    let pools = pools(&pool).await;
    let handle = Scheduler::start(
        pools.background.clone(),
        pools.control.clone(),
        resources(),
        config(),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM crm.import_batches WHERE status='complete'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if n == 12 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if result.is_err() {
        handle.shutdown().await.unwrap();
    }
    result.expect("productive imports were rate-limited by idle recovery sleeps");
    tokio::time::timeout(Duration::from_secs(8),async {
        loop {
            let done:bool=sqlx::query_scalar("SELECT (SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind='chat')=12 AND NOT EXISTS(SELECT 1 FROM crm.memory_legacy_pending_records)").bind(person).fetch_one(&pool).await.unwrap();
            if done {break;}
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("durable deferred Memory inputs did not recover");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM crm.import_batches WHERE id=$1")
            .bind(delayed_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "received"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.message_search")
            .fetch_one(&pool)
            .await
            .unwrap(),
        12
    );
    handle.shutdown().await.unwrap();
    pools.close().await;
}

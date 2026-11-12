use monica_core::{adapter, collector};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

#[test]
fn linkedin_normalization_and_chatlog_identity_filter() {
    assert_eq!(
        collector::canonical_linkedin("https://www.linkedin.com/in/Fixture-Person/?trk=test")
            .unwrap(),
        (
            "fixture-person".into(),
            "https://www.linkedin.com/in/fixture-person".into()
        )
    );
    assert_eq!(
        collector::canonical_linkedin("Straße").unwrap().0,
        "strasse"
    );
    assert_eq!(
        collector::canonical_linkedin("%E5%90%88%E6%88%90")
            .unwrap()
            .0,
        "合成"
    );
    for invalid in [
        "https://linkedin.com.evil/in/person",
        "https://www.linkedin.com/company/person",
        "https://secret@linkedin.com/in/person",
        "../bad",
        "%é",
        "..",
        "abc/def",
        "https://linkedin.com:444/in/person",
    ] {
        assert!(collector::canonical_linkedin(invalid).is_err(), "{invalid}")
    }
    let keys = ["synthetic-person".to_string()].into_iter().collect();
    let messages = json!([{"sender":"same-display-name","content":"Not this contact"},{"sender":"synthetic-person","content":"synthetic-person:\n<title>Fixture title</title>","time":"2026-01-01"},{"sender":"other","content":"synthetic-person:\nSynthetic match"}]);
    let found = collector::filter_messages(messages.as_array().unwrap(), &keys, "", 50);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0]["text"], "Fixture title");
    assert_eq!(
        collector::filter_messages(messages.as_array().unwrap(), &keys, "MATCH", 50).len(),
        1
    );
    assert_eq!(collector::casefold("ᎠΣςẞﬃİ"), "Ꭰσσssffii\u{307}");
}
fn batch() -> Value {
    json!({"source":"wechat","stream":"profiles","schema_version":1,"batch_id":"synthetic-snapshot-1","idempotency_key":"synthetic-adapter-snapshot-1","observed_at":"2026-01-01T00:00:00Z","cursor_after":"synthetic-cursor-1","records":[{"external_id":"synthetic-contact","source_account_external_id":"synthetic-contact","content":{"display_name":"Synthetic Contact"}}]})
}
async fn enable(pool: &PgPool, provider: &str) {
    sqlx::query(
        "UPDATE crm.adapter_controls SET enabled=true,minimum_interval_seconds=1 WHERE provider=$1",
    )
    .bind(provider)
    .execute(pool)
    .await
    .unwrap();
}
#[sqlx::test(migrations = "../../migrations")]
async fn durable_snapshot_replay_conflict_and_fencing(pool: PgPool) {
    assert!(adapter::enqueue(&pool, "wechat").await.is_err());
    enable(&pool, "wechat").await;
    let run = adapter::enqueue(&pool, "wechat").await.unwrap();
    assert_eq!(run, adapter::enqueue(&pool, "wechat").await.unwrap());
    let old = adapter::claim(&pool, "wechat").await.unwrap().unwrap();
    let bid = adapter::publish(&pool, &old, batch()).await.unwrap();
    assert_eq!(bid, adapter::publish(&pool, &old, batch()).await.unwrap());
    let mut conflict = batch();
    conflict["records"][0]["content"]["display_name"] = json!("conflict");
    assert!(adapter::publish(&pool, &old, conflict).await.is_err());
    sqlx::query("UPDATE crm.adapter_runs SET lease_until=now()-interval '1 second' WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    let current = adapter::claim(&pool, "wechat").await.unwrap().unwrap();
    assert_ne!(old.token, current.token);
    assert!(adapter::publish(&pool, &old, batch()).await.is_err());
    assert!(adapter::cancel(&pool, run).await.unwrap());
    assert!(adapter::publish(&pool, &current, batch()).await.is_err());
    let receipt = monica_core::imports::resume(&pool, bid).await.unwrap();
    assert_eq!(receipt["status"], "complete");
    let cursor: String = sqlx::query_scalar(
        "SELECT cursor FROM crm.source_cursors WHERE source='wechat' AND stream='profiles'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cursor, "synthetic-cursor-1");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.adapter_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn fake_process_publication_failure_isolation_and_platform_limit(pool: PgPool) {
    enable(&pool, "wechat").await;
    enable(&pool, "instagram").await;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("synthetic-collector.sh");
    let snapshot = json!({"type":"snapshot","batch":batch()}).to_string();
    let script = format!(
        "printf '%s\\n' '{{\"type\":\"progress\",\"phase\":\"collecting\",\"processed\":1,\"secret\":\"MUST-NOT-PERSIST\"}}'\nprintf '%s\\n' '{snapshot}'\n"
    );
    tokio::fs::write(&path, script).await.unwrap();
    let config = adapter::Config {
        executable: "/bin/sh".into(),
        args: vec![path.to_string_lossy().into_owned()],
        work_dir: root.path().to_path_buf(),
        deadline: Duration::from_secs(5),
    };
    let run = adapter::enqueue(&pool, "wechat").await.unwrap();
    assert!(adapter::run_one(&pool, "wechat", &config).await.unwrap());
    let state: Value = sqlx::query_scalar("SELECT to_jsonb(r) FROM crm.adapter_runs r WHERE id=$1")
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state["status"], "complete");
    assert!(!state.to_string().contains("MUST-NOT-PERSIST"));
    assert!(adapter::enqueue(&pool, "wechat").await.is_err());
    let bad = root.path().join("synthetic-bad.sh");
    tokio::fs::write(&bad, b"printf 'not-json\\n'\nexit 1\n")
        .await
        .unwrap();
    let failed = adapter::enqueue(&pool, "instagram").await.unwrap();
    let config = adapter::Config {
        args: vec![bad.to_string_lossy().into_owned()],
        ..config
    };
    assert!(adapter::run_one(&pool, "instagram", &config).await.unwrap());
    let statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM crm.adapter_runs WHERE id=ANY($1) ORDER BY provider",
    )
    .bind(vec![run, failed])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(statuses.contains(&"complete".into()) && statuses.contains(&"failed".into()));
}
#[sqlx::test(migrations = "../../migrations")]
async fn fake_process_cancellation_kills_child_group(pool: PgPool) {
    enable(&pool, "wechat").await;
    let run = adapter::enqueue(&pool, "wechat").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("synthetic-slow.sh");
    tokio::fs::write(&path, b"sleep 60 &\necho $! > child.pid\nwait\n")
        .await
        .unwrap();
    let config = adapter::Config {
        executable: "/bin/sh".into(),
        args: vec![path.to_string_lossy().into_owned()],
        work_dir: root.path().to_path_buf(),
        deadline: Duration::from_secs(5),
    };
    let worker_pool = pool.clone();
    let worker =
        tokio::spawn(async move { adapter::run_one(&worker_pool, "wechat", &config).await });
    for _ in 0..100 {
        if root.path().join("child.pid").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid: i32 = tokio::fs::read_to_string(root.path().join("child.pid"))
        .await
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(adapter::cancel(&pool, run).await.unwrap());
    assert!(
        tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    );
    let status: String = sqlx::query_scalar("SELECT status FROM crm.adapter_runs WHERE id=$1")
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    #[cfg(unix)]
    {
        for _ in 0..100 {
            if unsafe { libc::kill(pid, 0) } != 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "fake grandchild survived cancellation"
        );
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn linkedin_manual_target_durable_source_and_competing_owner(pool: PgPool) {
    let user = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic-adapter@example.test','unused')").bind(user).execute(&pool).await.unwrap();
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    for id in ids {
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic Target')").bind(id).execute(&pool).await.unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    let first = collector::save_linkedin(
        &mut tx,
        ids[0],
        "https://linkedin.com/in/Synthetic-Target",
        user,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        collector::save_linkedin(&mut tx, ids[1], "synthetic-target", user)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let targets = collector::targets(&pool).await.unwrap();
    assert_eq!(targets["targets"].as_array().unwrap().len(), 1);
    assert_eq!(targets["targets"][0]["external_id"], "synthetic-target");
    let aid = first["account_id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let durable: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM linkedin.accounts WHERE id=$1)")
            .bind(aid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(durable);
    sqlx::query("DELETE FROM crm.source_account_links WHERE account_id=$1")
        .bind(aid)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        collector::targets(&pool).await.unwrap()["targets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

async fn spool_page(root: &std::path::Path, frames: &[Value]) -> std::path::PathBuf {
    let spool = root.join("spool");
    tokio::fs::create_dir_all(&spool).await.unwrap();
    let bytes = frames
        .iter()
        .map(|frame| format!("{}\n", serde_json::to_string(frame).unwrap()))
        .collect::<String>()
        .into_bytes();
    let hash = monica_core::identity::bytes_hash(&bytes);
    let path = spool.join(format!("{:020}-{hash}.jsonl", 1));
    tokio::fs::write(&path, &bytes).await.unwrap();
    tokio::fs::write(
        path.with_extension("ready"),
        serde_json::to_vec(&json!({"version":1,"sha256":hash,"byte_length":bytes.len()})).unwrap(),
    )
    .await
    .unwrap();
    path
}
#[sqlx::test(migrations = "../../migrations")]
async fn external_spool_partial_page_crash_replay_receipt_and_ack(pool: PgPool) {
    enable(&pool, "wechat").await;
    let root = tempfile::tempdir().unwrap();
    let mut frames = (0..30)
        .map(|n| json!({"type":"progress","phase":"publishing","processed":n}))
        .collect::<Vec<_>>();
    frames.push(json!({"type":"snapshot","batch":batch()}));
    let page = spool_page(root.path(), &frames).await;
    let resources = monica_core::resources::ResourceBudget::new(Default::default()).unwrap();
    assert!(
        adapter::spool::consume(
            &pool,
            "wechat",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    let offset: i64 = sqlx::query_scalar("SELECT byte_offset FROM crm.adapter_spool_pages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(offset > 0 && page.exists());
    // Simulate a process death after receipt durability but before spool offset.
    let lease = adapter::claim(&pool, "wechat").await.unwrap().unwrap();
    adapter::publish(&pool, &lease, batch()).await.unwrap();
    sqlx::query("UPDATE crm.adapter_runs SET lease_until=now()-interval '1 second' WHERE id=$1")
        .bind(lease.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        adapter::spool::consume(
            &pool,
            "wechat",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    assert!(!page.exists());
    assert!(page.with_extension("ack").exists());
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(receipts, 1);
    spool_page(root.path(), &frames).await;
    assert!(
        adapter::spool::consume(
            &pool,
            "wechat",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        receipts, 1,
        "completed page replay must not duplicate source receipts"
    );
    assert!(
        !adapter::spool::consume(
            &pool,
            "wechat",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn spool_rejects_corruption_and_relative_media_escape(pool: PgPool) {
    enable(&pool, "wechat").await;
    let root = tempfile::tempdir().unwrap();
    let frames = vec![json!({"type":"snapshot","batch":batch()})];
    let page = spool_page(root.path(), &frames).await;
    tokio::fs::write(&page, b"corrupt but published\n")
        .await
        .unwrap();
    let resources = monica_core::resources::ResourceBudget::new(Default::default()).unwrap();
    assert!(
        adapter::spool::consume(
            &pool,
            "wechat",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(receipts, 0);
    let status: String = sqlx::query_scalar("SELECT status FROM crm.adapter_spool_pages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "failed");
    let outside = tempfile::tempdir().unwrap();
    let hidden = outside.path().join("synthetic.png");
    tokio::fs::write(&hidden, b"synthetic-secret-outside-root")
        .await
        .unwrap();
    spool_page(root.path(),&[json!({"type":"media","path":hidden.to_string_lossy(),"sha256":"a".repeat(64),"byte_length":27})]).await;
    assert!(
        adapter::spool::consume(
            &pool,
            "wechat",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    let media: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.adapter_media_inputs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(media, 0);
}

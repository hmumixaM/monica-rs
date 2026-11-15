use monica_core::{Provider, adapter, connections, imports};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
async fn page(root: &std::path::Path, frame: &Value, sequence: u32) -> std::path::PathBuf {
    let spool = root.join("spool");
    tokio::fs::create_dir_all(&spool).await.unwrap();
    let bytes = format!("{}\n", serde_json::to_string(frame).unwrap());
    let hash = monica_core::identity::bytes_hash(bytes.as_bytes());
    let file = spool.join(format!("{sequence:020}-{hash}.jsonl"));
    tokio::fs::write(&file, &bytes).await.unwrap();
    tokio::fs::write(
        file.with_extension("ready"),
        serde_json::to_vec(&json!({"version":1,"sha256":hash,"byte_length":bytes.len()})).unwrap(),
    )
    .await
    .unwrap();
    file
}
#[sqlx::test(migrations = "../../migrations")]
async fn independent_linkedin_frame_connection_journal_ack_and_disabled_fence(pool: PgPool) {
    sqlx::query("UPDATE crm.adapter_controls SET enabled=true WHERE provider='linkedin'")
        .execute(&pool)
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let resources = monica_core::resources::ResourceBudget::new(Default::default()).unwrap();
    let body = json!({"external_id":"synthetic-message","thread_id":"synthetic-thread","kind":"message","body":"Synthetic independent LinkedIn message","occurred_at":"2026-01-01T00:00:00Z","is_group":false,"direction":"received","participants":[{"id":"synthetic-peer","name":"Synthetic Peer","role":"sender","is_self":false},{"id":"synthetic-own","name":"Synthetic Self","role":"participant","is_self":true}]});
    let frame = json!({"type":"connection_snapshot","account":"synthetic-own","page_sha256":"a".repeat(64),"ack_path":"raw/linkedin/messages/fixture/acks/1.ack","batch":{"source":"linkedin","stream":"messages","schema_version":1,"batch_id":"synthetic-linked-message-batch","idempotency_key":"synthetic-linked-message-batch","observed_at":"2026-01-01T00:00:00Z","records":[{"external_id":"synthetic-message","entity_type":"message","conversation_external_id":"synthetic-thread","conversation_type":"direct","occurred_at":"2026-01-01T00:00:00Z","content":body}]}});
    page(root.path(), &frame, 1).await;
    assert!(
        adapter::spool::consume(
            &pool,
            "linkedin",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    let ack: Value = serde_json::from_slice(
        &tokio::fs::read(root.path().join("raw/linkedin/messages/fixture/acks/1.ack"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(ack["page_sha256"], "a".repeat(64));
    let receipt = ack["receipt_id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let cid = connections::connection_id(Provider::Linkedin, "synthetic-own");
    assert_eq!(ack["connection_id"], cid.to_string());
    imports::resume(&pool, receipt).await.unwrap();
    connections::deliver(&pool, Provider::Linkedin, cid, 1000)
        .await
        .unwrap();
    let payload:Value=sqlx::query_scalar("SELECT payload FROM linkedin.connection_records WHERE connection_id=$1 AND external_id='synthetic-message'").bind(cid).fetch_one(&pool).await.unwrap();
    assert_eq!(payload["body"], "Synthetic independent LinkedIn message");
    assert_eq!(payload["participants"][0]["role"], "sender");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.connection_evidence WHERE connection_id=$1")
            .bind(cid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    page(root.path(), &frame, 2).await;
    assert!(
        adapter::spool::consume(
            &pool,
            "linkedin",
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
        "same account/message batch must reuse the durable receipt"
    );
    sqlx::query(
        "UPDATE crm.source_connections SET enabled=false,generation=generation+1 WHERE id=$1",
    )
    .bind(cid)
    .execute(&pool)
    .await
    .unwrap();
    let mut later = frame;
    later["batch"]["batch_id"] = json!("later-batch");
    later["batch"]["idempotency_key"] = json!("later-batch");
    page(root.path(), &later, 3).await;
    assert!(
        adapter::spool::consume(
            &pool,
            "linkedin",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    let enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM crm.source_connections WHERE id=$1")
            .bind(cid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        !enabled,
        "collector facts must never re-enable a disabled connection"
    );
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(receipts, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn chatlog_python_snapshot_process_receipt_journal_projection_and_replay(pool: PgPool) {
    use std::{path::PathBuf, time::Duration};
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let snapshots = root.join("owned-chatlog-snapshots");
    std::fs::create_dir_all(&snapshots).unwrap();
    let fixture = r#"
import hashlib,os,sqlite3,sys
from pathlib import Path
root=Path(sys.argv[1]);(root/'contact').mkdir(exist_ok=True);(root/'message').mkdir(exist_ok=True)
db=sqlite3.connect(root/'contact/contact.db');db.execute('CREATE TABLE contact(id INTEGER PRIMARY KEY,username TEXT,local_type INTEGER,delete_flag INTEGER,remark TEXT)');db.execute('INSERT INTO contact VALUES(1,?,0,0,?)',('wxid_synthetic_peer','Synthetic Contact'));db.commit();db.close()
table='Msg_'+hashlib.md5(b'wxid_synthetic_peer').hexdigest();db=sqlite3.connect(root/'message/message_0.db');db.execute('CREATE TABLE Name2Id(user_name TEXT)');db.executemany('INSERT INTO Name2Id VALUES(?)',[('wxid_synthetic_peer',),('wxid_synthetic_own',)]);db.execute('CREATE TABLE '+table+'(local_id INTEGER PRIMARY KEY,server_id INTEGER,create_time INTEGER,local_type INTEGER,real_sender_id INTEGER,message_content BLOB,compress_content BLOB)');db.executemany('INSERT INTO '+table+' VALUES(?,?,?,?,?,?,?)',[(1,101,1767225600,1,1,b'Synthetic received',None),(2,102,1767225601,1,2,b'Synthetic sent',None)]);db.commit();db.close()
"#;
    assert!(
        tokio::process::Command::new("/usr/bin/python3")
            .arg("-c")
            .arg(fixture)
            .arg(&snapshots)
            .status()
            .await
            .unwrap()
            .success()
    );
    sqlx::query("UPDATE crm.adapter_controls SET enabled=true,minimum_interval_seconds=1 WHERE provider='wechat'").execute(&pool).await.unwrap();
    let config = adapter::Config {
        executable: PathBuf::from("/usr/bin/python3"),
        args: vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/collector-bridge.py")
                .to_str()
                .unwrap()
                .into(),
            "--provider".into(),
            "wechat".into(),
            "--data-root".into(),
            root.to_str().unwrap().into(),
            "--snapshot-root".into(),
            snapshots.to_str().unwrap().into(),
            "--account".into(),
            "synthetic-chatlog-source".into(),
            "--self-id".into(),
            "wxid_synthetic_own".into(),
        ],
        work_dir: root.clone(),
        deadline: Duration::from_secs(10),
    };
    let cid = connections::connection_id(Provider::Wechat, "synthetic-chatlog-source");
    for _ in 0..12 {
        adapter::enqueue(&pool, "wechat").await.unwrap();
        assert!(adapter::run_one(&pool, "wechat", &config).await.unwrap());
        let receipts: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM crm.import_batches WHERE status<>'complete' ORDER BY received_at",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        for receipt in receipts {
            imports::resume(&pool, receipt).await.unwrap();
        }
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM crm.source_connections WHERE id=$1)",
        )
        .bind(cid)
        .fetch_one(&pool)
        .await
        .unwrap()
        {
            connections::deliver(&pool, Provider::Wechat, cid, 1000)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE crm.adapter_runs SET started_at=now()-interval '2 minutes' WHERE provider='wechat'").execute(&pool).await.unwrap();
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 2,
        "one contact page and one message page; unchanged scans create no new receipts"
    );
    let bodies:Vec<String>=sqlx::query_scalar("SELECT body FROM crm.connection_evidence WHERE connection_id=$1 AND kind='message' ORDER BY occurred_at").bind(cid).fetch_all(&pool).await.unwrap();
    assert_eq!(bodies, vec!["Synthetic received", "Synthetic sent"]);
    let directions: Vec<String> = sqlx::query_scalar(
        "SELECT direction FROM crm.message_search WHERE account=$1 ORDER BY occurred_at",
    )
    .bind(cid.to_string())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(directions, vec!["received", "sent"]);
    let contact = Uuid::new_v5(&cid, b"contact:wxid_synthetic_peer");
    let attributed:Vec<(Option<Uuid>,Option<Uuid>)>=sqlx::query_as("SELECT c.profile_id,m.sender_profile_id FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.connection_id=$1 ORDER BY m.occurred_at").bind(cid).fetch_all(&pool).await.unwrap();
    assert_eq!(
        attributed,
        vec![(Some(contact), Some(contact)), (Some(contact), None)],
        "source rows without CRM annotations must use the already-resolved contact"
    );
    let people:i64=sqlx::query_scalar("SELECT count(*) FROM crm.connection_people WHERE connection_id=$1 AND external_id='wxid_synthetic_peer' AND display_name='wxid_synthetic_peer'").bind(cid).fetch_one(&pool).await.unwrap();
    assert_eq!(people, 1);
    // A disabled connection preserves unacknowledged local source work.
    sqlx::query(
        "UPDATE crm.source_connections SET enabled=false,generation=generation+1 WHERE id=$1",
    )
    .bind(cid)
    .execute(&pool)
    .await
    .unwrap();
    let change = r#"
import hashlib,os,sqlite3,sys
from pathlib import Path
root=Path(sys.argv[1]);old=root/'message/message_0.db';new=old.with_suffix('.new');source=sqlite3.connect(old);db=sqlite3.connect(new);source.backup(db);source.close();table='Msg_'+hashlib.md5(b'wxid_synthetic_peer').hexdigest();db.execute('INSERT INTO '+table+' VALUES(3,103,1767225602,1,1,?,NULL)',(b'Disabled source pending',));db.commit();db.close();os.replace(new,old)
"#;
    assert!(
        tokio::process::Command::new("/usr/bin/python3")
            .arg("-c")
            .arg(change)
            .arg(&snapshots)
            .status()
            .await
            .unwrap()
            .success()
    );
    adapter::enqueue(&pool, "wechat").await.unwrap();
    assert!(adapter::run_one(&pool, "wechat", &config).await.unwrap());
    let status: String = sqlx::query_scalar("SELECT status FROM crm.adapter_runs WHERE provider='wechat' ORDER BY created_at DESC LIMIT 1").fetch_one(&pool).await.unwrap();
    assert_eq!(status, "failed");
    let later: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        later, count,
        "disabled source must not durably acknowledge new snapshot data"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn independent_message_spool_parent_pause_keeps_page_replayable(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let resources = monica_core::resources::ResourceBudget::new(Default::default()).unwrap();
    let frame = json!({"type":"snapshot","batch":{"source":"linkedin","stream":"profiles","schema_version":1,"batch_id":"child-spool","idempotency_key":"child-spool","observed_at":"2026-01-01T00:00:00Z","records":[]}});
    let path = page(root.path(), &frame, 1).await;
    // Child's local preference is enabled by migration; parent is off.
    assert!(
        !adapter::spool::consume(
            &pool,
            "linkedin_messages",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    assert!(path.exists());
    assert!(!path.with_extension("ack").exists());
    sqlx::query("UPDATE crm.adapter_controls SET enabled=true WHERE provider='linkedin'")
        .execute(&pool)
        .await
        .unwrap();
    // Hold the source import mutation so the consumer has a live child lease,
    // then pause the parent before the next receipt can commit.
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE crm.import_batches IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mut consume = {
        let pool = pool.clone();
        let root = root.path().to_path_buf();
        let resources = resources.clone();
        tokio::spawn(async move {
            adapter::spool::consume(
                &pool,
                "linkedin_messages",
                &root.join("spool"),
                &root,
                &resources,
            )
            .await
        })
    };
    // The writer holds the same control share-lock while waiting on the import
    // table. Parent pause must wait for that committed receipt, then gate the
    // final checkpoint; no source is acknowledged before its receipt.
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            let leased: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.adapter_runs WHERE provider='linkedin_messages' AND status='running')").fetch_one(&pool).await.unwrap();
            if leased {
                break;
            }
            if consume.is_finished() {
                let result = (&mut consume).await;
                panic!("consumer exited before claiming its independent child lane: {result:?}");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("consumer must claim its independent child lane");
    blocker.rollback().await.unwrap();
    assert!(consume.await.unwrap().unwrap());
    // A new immutable page must remain untouched while the parent is paused.
    let later=page(root.path(),&json!({"type":"snapshot","batch":{"source":"linkedin","stream":"profiles","schema_version":1,"batch_id":"child-later","idempotency_key":"child-later","observed_at":"2026-01-01T00:00:00Z","records":[]}}),2).await;
    sqlx::query("UPDATE crm.adapter_controls SET paused=true WHERE provider='linkedin'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !adapter::spool::consume(
            &pool,
            "linkedin_messages",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    assert!(later.exists());
    assert!(!later.with_extension("ack").exists());
    sqlx::query("UPDATE crm.adapter_controls SET paused=false WHERE provider='linkedin'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        adapter::spool::consume(
            &pool,
            "linkedin_messages",
            &root.path().join("spool"),
            root.path(),
            &resources
        )
        .await
        .unwrap()
    );
    assert!(later.with_extension("ack").exists());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

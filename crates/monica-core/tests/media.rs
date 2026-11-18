use monica_core::{
    media::{self, Asset, MediaStore},
    resources::{ResourceBudget, ResourceLimits},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

fn store(pool: PgPool, root: &std::path::Path) -> MediaStore {
    MediaStore::new(
        pool,
        root.into(),
        ResourceBudget::new(ResourceLimits::nas()).unwrap(),
    )
    .unwrap()
}
fn asset(bytes: &[u8]) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        media_type: "image/png".into(),
        byte_length: bytes.len() as i64,
        object_path: String::new(),
        file_name: Some("synthetic.png".into()),
        width: Some(2),
        height: Some(2),
        duration_ms: None,
    }
}
async fn actor(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic@example.invalid','not-a-login-hash')").bind(id).execute(pool).await.unwrap();
    id
}

#[sqlx::test(migrations = "../../migrations")]
async fn publication_deduplicates_concurrent_bytes_and_recovers_after_crash(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let media = store(pool.clone(), root.path());
    let bytes = b"synthetic-content";
    let input = root.path().join("input");
    tokio::fs::write(&input, bytes).await.unwrap();
    let (a, b) = tokio::join!(
        media.publish(&input, asset(bytes), "fixture-a", Some("a")),
        media.publish(&input, asset(bytes), "fixture-b", Some("b"))
    );
    let a = a.unwrap();
    assert_eq!(a.id, b.unwrap().id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_provenance")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    // Simulate a process dying after fsync/atomic publish but before metadata commit.
    let bytes = b"crash-recovery-fixture";
    let mut pending = asset(bytes);
    pending.object_path = media::object_path(&pending.sha256).unwrap();
    let destination = root.path().join(&pending.object_path);
    tokio::fs::create_dir_all(destination.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&destination, bytes).await.unwrap();
    sqlx::query("INSERT INTO crm.media_publications(sha256,manifest,updated_at) VALUES($1,$2,now()-interval '2 hours')").bind(&pending.sha256).bind(json!({"asset":pending,"source":"synthetic","record":"crash"})).execute(&pool).await.unwrap();
    assert_eq!(media.recover_publications().await.unwrap(), 1);
    assert_eq!(media.recover_publications().await.unwrap(), 0);
    assert_eq!(
        media.asset(pending.id).await.unwrap().sha256,
        pending.sha256
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn resumable_chunks_have_atomic_order_identity_and_expiry(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let media = store(pool.clone(), root.path());
    let user = actor(&pool).await;
    let upload = media
        .initiate(
            user,
            None,
            Some(6),
            "text/plain".into(),
            Some("fixture.txt".into()),
        )
        .await
        .unwrap();
    let id: Uuid = serde_json::from_value(upload["upload_id"].clone()).unwrap();
    assert!(media.chunk(id, Uuid::new_v4(), 0, b"abc").await.is_err());
    assert!(
        !root
            .path()
            .join(".staging/uploads")
            .join(id.to_string())
            .exists()
    );
    assert!(media.chunk(id, user, 1, b"abc").await.is_err());
    let (a, b) = tokio::join!(
        media.chunk(id, user, 0, b"abc"),
        media.chunk(id, user, 0, b"abc")
    );
    assert_eq!(a.unwrap()["next_chunk"], 1);
    assert_eq!(b.unwrap()["next_chunk"], 1);
    assert!(media.chunk(id, user, 0, b"xyz").await.is_err());
    assert!(media.chunk(id, user, 1, b"abcd").await.is_err());
    assert_eq!(
        media.chunk(id, user, 1, b"def").await.unwrap()["received_bytes"],
        6
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_upload_chunks")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    sqlx::query("UPDATE crm.media_uploads SET expires_at=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(media.expire_uploads().await.unwrap(), 1);
    assert!(media.chunk(id, user, 2, b"z").await.is_err());
    assert!(
        !root
            .path()
            .join(".staging/uploads")
            .join(id.to_string())
            .exists()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn gc_respects_journal_and_registered_assets(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let media = store(pool.clone(), root.path());
    let hash = "1234".to_string() + &"a".repeat(60);
    let protected = "1234".to_string() + &"b".repeat(60);
    for h in [&hash, &protected] {
        let path = root.path().join(media::object_path(h).unwrap());
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, b"orphan bytes").await.unwrap();
    }
    sqlx::query("INSERT INTO crm.media_publications(sha256,manifest) VALUES($1,'{}')")
        .bind(&protected)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE crm.media_maintenance SET bucket=4660")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(media.collect_orphans(Duration::ZERO).await.unwrap(), 1);
    assert!(
        !root
            .path()
            .join(media::object_path(&hash).unwrap())
            .exists()
    );
    assert!(
        root.path()
            .join(media::object_path(&protected).unwrap())
            .exists()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn manifest_receipts_are_durable_bounded_and_wait_for_real_source(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let media = store(pool.clone(), root.path());
    let bytes = b"dependency";
    let input = root.path().join("input");
    tokio::fs::write(&input, bytes).await.unwrap();
    let a = media
        .publish(&input, asset(bytes), "synthetic", None)
        .await
        .unwrap();
    let batch:monica_core::imports::Batch=serde_json::from_value(json!({"source":"instagram","stream":"posts","batch_id":"synthetic-media","idempotency_key":"synthetic-media","observed_at":"2026-01-01T00:00:00Z","records":[],"raw_objects":(0..105).map(|n|json!({"synthetic":n})).collect::<Vec<_>>(),"media_manifest":[{"entity_type":"source_account","external_id":"fixture-account","sha256":a.sha256,"role":"instagram_avatar"}]})).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let receipt = monica_core::imports::receive(&mut tx, &batch, "fixture-hash")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(media::consume_imports(&pool).await.unwrap(), 100);
    assert_eq!(media::consume_imports(&pool).await.unwrap(), 6);
    assert_eq!(media::consume_imports(&pool).await.unwrap(), 0);
    assert_eq!(media::resolve_dependencies(&pool).await.unwrap(), 0);
    let account = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram','fixture-account','person','Synthetic')").bind(account).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.media_dependencies SET next_attempt_at=now()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(media::resolve_dependencies(&pool).await.unwrap(), 1);
    assert_eq!(media::resolve_dependencies(&pool).await.unwrap(), 0);
    let state: Value =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM crm.media_import_progress p WHERE batch_id=$1")
            .bind(receipt)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state["complete"], true);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.import_raw_objects")
            .fetch_one(&pool)
            .await
            .unwrap(),
        105
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT generation FROM crm.media_slots")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[cfg(unix)]
#[sqlx::test(migrations = "../../migrations")]
async fn cancelling_inspector_kills_descendants(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let mut media = store(pool, root.path());
    let helper = root.path().join("decoder");
    let marker = root.path().join("escaped-child");
    let started = root.path().join("started");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\n( sleep 1; echo escaped > '{}' ) &\necho started > '{}'\nwait\n",
            marker.display(),
            started.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    media.helper = helper;
    let task = tokio::spawn(async move {
        media
            .inspect(std::path::Path::new("unused"), "fixture")
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert!(
        !marker.exists(),
        "cancelled decoder child continued executing"
    );
}

#[cfg(unix)]
#[sqlx::test(migrations = "../../migrations")]
async fn completion_pages_metadata_and_rejects_corrupt_chunks(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let mut media = store(pool.clone(), root.path());
    let actor = actor(&pool).await;
    let helper = root.path().join("synthetic-inspector");
    std::fs::write(
        &helper,
        "#!/bin/sh\nprintf '%s' '{\"media_type\":\"text/plain\",\"width\":null,\"height\":null}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    media.helper = helper;
    let bytes = (0u8..130).collect::<Vec<_>>();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let initiated = media
        .initiate(
            actor,
            Some(hash.clone()),
            Some(bytes.len() as u64),
            "text/plain".into(),
            Some("fixture.txt".into()),
        )
        .await
        .unwrap();
    let id = serde_json::from_value(initiated["upload_id"].clone()).unwrap();
    for (number, byte) in bytes.iter().enumerate() {
        media
            .chunk(id, actor, number as i32, &[*byte])
            .await
            .unwrap();
    }
    let first: String = sqlx::query_scalar(
        "SELECT object_path FROM crm.media_upload_chunks WHERE upload_id=$1 AND chunk=0",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    tokio::fs::write(root.path().join(&first), b"corrupt")
        .await
        .unwrap();
    assert!(media.complete(id, actor).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_assets")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    tokio::fs::write(root.path().join(&first), &bytes[..1])
        .await
        .unwrap();
    let asset = media.complete(id, actor).await.unwrap();
    assert_eq!(asset.sha256, hash);
    assert_eq!(
        tokio::fs::read(root.path().join(&asset.object_path))
            .await
            .unwrap(),
        bytes
    );
    assert_eq!(media.complete(id, actor).await.unwrap().id, asset.id);
    assert!(media.chunk(id, actor, 130, b"extra").await.is_err());
    assert_eq!(
        std::fs::read_dir(root.path().join(".staging/uploads").join(id.to_string()))
            .unwrap()
            .count(),
        130
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn delayed_old_blob_cannot_replace_a_newer_imported_slot(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let media = store(pool.clone(), root.path());
    let account = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram','ordered-avatar','person','Synthetic')").bind(account).execute(&pool).await.unwrap();
    for (key, stamp, bytes) in [
        ("old", "2026-01-01T00:00:00Z", b"old-avatar".as_slice()),
        ("new", "2026-01-02T00:00:00Z", b"new-avatar".as_slice()),
    ] {
        let batch:monica_core::imports::Batch=serde_json::from_value(json!({"source":"instagram","stream":"media","batch_id":key,"idempotency_key":key,"observed_at":stamp,"records":[],"media_manifest":[{"entity_type":"source_account","entity_id":account,"sha256":format!("{:x}",Sha256::digest(bytes)),"role":"instagram_avatar"}]})).unwrap();
        let mut tx = pool.begin().await.unwrap();
        monica_core::imports::receive(&mut tx, &batch, key)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(media::consume_imports(&pool).await.unwrap(), 1);
    }
    let new_path = root.path().join("new");
    tokio::fs::write(&new_path, b"new-avatar").await.unwrap();
    let current = media
        .publish(&new_path, asset(b"new-avatar"), "fixture", None)
        .await
        .unwrap();
    assert_eq!(media::resolve_dependencies(&pool).await.unwrap(), 1);
    let old_path = root.path().join("old");
    tokio::fs::write(&old_path, b"old-avatar").await.unwrap();
    media
        .publish(&old_path, asset(b"old-avatar"), "fixture", None)
        .await
        .unwrap();
    sqlx::query("UPDATE crm.media_dependencies SET next_attempt_at=now()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(media::resolve_dependencies(&pool).await.unwrap(), 1);
    assert_eq!(sqlx::query_scalar::<_,Uuid>("SELECT media_id FROM crm.media_links WHERE entity_type='source_account' AND entity_id=$1").bind(account).fetch_one(&pool).await.unwrap(),current.id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT generation FROM crm.media_slots WHERE entity_id=$1")
            .bind(account)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.media_dependencies WHERE reason='superseded'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn signed_record_media_archives_links_and_replays_without_fabrication(pool: PgPool) {
    use monica_core::imports::{self, Batch};
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = store(pool.clone(), root.path());
    let hash = format!("{:x}", Sha256::digest(b"ABC"));
    let batch: Batch = serde_json::from_value(json!({"source":"monica","stream":"posts","batch_id":"inline","idempotency_key":"inline","observed_at":"2026-01-01T00:00:00Z","records":[
        {"external_id":"post","content":{"text":"Synthetic post"},"media":[{"bytes":"41 \u{000b}42\n43","media_type":"application/octet-stream"},{"sha256":hash},{"sha256":"f".repeat(64)}]},
        {"external_id":"story","entity_type":"story","content":{"text":"Synthetic story"},"media":[{"bytes":"414243","role":"cover"}]}
    ]})).unwrap();
    batch.validate().unwrap();
    let mut invalid = batch.clone();
    invalid.records[0].extra["media"][0]["bytes"] = json!("4 1");
    assert!(invalid.validate().is_err());
    let mut tx = pool.begin().await.unwrap();
    let id = imports::receive(&mut tx, &batch, "synthetic-inline")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    imports::resume(&pool, id).await.unwrap();
    for _ in 0..8 {
        media::consume_record_media(&store).await.unwrap();
    }
    media::resolve_dependencies(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_assets")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_links")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
    let id: Uuid = sqlx::query_scalar("SELECT id FROM crm.media_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(store.root.join(store.asset(id).await.unwrap().object_path))
            .await
            .unwrap(),
        b"ABC"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.media_dependencies WHERE state='waiting'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    // Simulated lost acknowledgment: publication/provenance/links remain exactly once.
    sqlx::query("UPDATE crm.import_record_media_progress SET complete=false,record_offset=0,media_offset=0,ordinal=0").execute(&pool).await.unwrap();
    for _ in 0..8 {
        media::consume_record_media(&store).await.unwrap();
    }
    media::resolve_dependencies(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_links")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
    let mut second = batch.clone();
    second.batch_id = "second".into();
    second.idempotency_key = "second".into();
    second.records.truncate(1);
    second.records[0].extra["media"] = json!([{"media_id":id,"sha256":"e".repeat(64),"role":"by-id"},{"bytes":"44","role":"media-1"}]);
    let mut tx = pool.begin().await.unwrap();
    let receipt = imports::receive(&mut tx, &second, "second").await.unwrap();
    tx.commit().await.unwrap();
    imports::resume(&pool, receipt).await.unwrap();
    for _ in 0..5 {
        media::consume_record_media(&store).await.unwrap();
    }
    media::resolve_dependencies(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_links")
            .fetch_one(&pool)
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_assets")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn record_media_io_releases_progress_lock_and_fences_lost_lease(pool: PgPool) {
    use monica_core::imports::{self, Batch};
    let root = tempfile::tempdir().unwrap();
    let store = store(pool.clone(), root.path());
    let batch:Batch=serde_json::from_value(json!({"source":"monica","stream":"posts","batch_id":"fenced","idempotency_key":"fenced","observed_at":"2026-01-01T00:00:00Z","records":[{"external_id":"fenced-post","content":{"text":"Synthetic"},"media":[{"bytes":"414243"}]}]})).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let id = imports::receive(&mut tx, &batch, "fenced").await.unwrap();
    tx.commit().await.unwrap();
    imports::resume(&pool, id).await.unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('media-gc:'||$1,0))")
        .bind(format!("{:x}", Sha256::digest(b"ABC")))
        .execute(&mut *blocker)
        .await
        .unwrap();
    let running = store.clone();
    let task = tokio::spawn(async move { media::consume_record_media(&running).await });
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM crm.import_record_media_progress WHERE lease_token IS NOT NULL)").fetch_one(&pool).await.unwrap(){break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    // Publication is blocked on a real store lock; the progress row is free.
    tokio::time::timeout(Duration::from_secs(1),sqlx::query("UPDATE crm.import_record_media_progress SET lease_token=NULL,lease_until=NULL WHERE batch_id=$1").bind(id).execute(&pool)).await.unwrap().unwrap();
    blocker.rollback().await.unwrap();
    assert!(task.await.unwrap().is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT ordinal FROM crm.import_record_media_progress")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    media::consume_record_media(&store).await.unwrap();
    media::resolve_dependencies(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_assets")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_links")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn signed_connection_media_waits_for_projection_without_cross_account_links(pool: PgPool) {
    use monica_core::{
        Provider, connections,
        imports::{self, Batch},
    };
    let root = tempfile::tempdir().unwrap();
    let store = store(pool.clone(), root.path());
    let mut expected = Vec::new();
    for (account, bytes) in [("first", b"A"), ("second", b"B")] {
        let cid = connections::connection_id(Provider::Wechat, account);
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,'wechat',$2,$2,'local',true,'connected')").bind(cid).bind(account).execute(&pool).await.unwrap();
        let batch:Batch=serde_json::from_value(json!({"source":"wechat","connection_id":cid,"stream":"moments","batch_id":account,"idempotency_key":account,"observed_at":"2026-01-01T00:00:00Z","records":[{"external_id":"same-post","entity_type":"post","content":{"text":"Synthetic scoped post","participants":[]},"media":[{"bytes":format!("{:02x}",bytes[0])}]}]})).unwrap();
        let mut tx = pool.begin().await.unwrap();
        let id = imports::receive(&mut tx, &batch, account).await.unwrap();
        tx.commit().await.unwrap();
        imports::resume(&pool, id).await.unwrap();
        expected.push((cid, format!("{:x}", Sha256::digest(bytes))));
    }
    for _ in 0..8 {
        media::consume_record_media(&store).await.unwrap();
    }
    media::resolve_dependencies(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_links")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    for (cid, _) in &expected {
        connections::deliver(&pool, Provider::Wechat, *cid, 100)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE crm.media_dependencies SET next_attempt_at=now()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(media::resolve_dependencies(&pool).await.unwrap(), 2);
    let actual:Vec<(Uuid,String)>=sqlx::query_as("SELECT e.connection_id,a.sha256 FROM crm.media_links l JOIN crm.timeline_events e ON e.id=l.entity_id JOIN crm.media_assets a ON a.id=l.media_id ORDER BY e.connection_id").fetch_all(&pool).await.unwrap();
    expected.sort();
    assert_eq!(actual, expected);
}

#[sqlx::test(migrations = "../../migrations")]
async fn validated_publication_enriches_raw_blob_without_changing_identity(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let media = store(pool.clone(), root.path());
    let bytes = b"same-content-addressed-source-bytes";
    let input = root.path().join("source");
    tokio::fs::write(&input, bytes).await.unwrap();
    let mut raw = asset(bytes);
    raw.media_type = "application/octet-stream".into();
    raw.file_name = None;
    raw.width = None;
    raw.height = None;
    let original = media
        .publish(&input, raw.clone(), "mail", Some("raw-original"))
        .await
        .unwrap();
    let image = media
        .publish(&input, asset(bytes), "manual", Some("validated-image"))
        .await
        .unwrap();
    assert_eq!(image.id, original.id);
    assert_eq!(image.media_type, "image/png");
    assert_eq!((image.width, image.height), (Some(2), Some(2)));
    assert_eq!(image.file_name.as_deref(), Some("synthetic.png"));
    let repeated = media
        .publish(&input, raw, "mail", Some("second-original"))
        .await
        .unwrap();
    assert_eq!(repeated.id, image.id);
    assert_eq!(repeated.media_type, "image/png");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.media_provenance WHERE media_id=$1")
            .bind(image.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
}

use monica_core::{
    Provider,
    connections::{self, Fence},
    mail::{self, MailFuture, MailPage, MailTransport},
    media::MediaStore,
    resources::{ResourceBudget, ResourceLimits},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
struct Fake {
    page: MailPage,
    bodies: Arc<Mutex<Vec<String>>>,
    fail_body: bool,
}
impl MailTransport for Fake {
    fn scan<'a>(&'a mut self, _folder: &'a str, _cursor: &'a Value) -> MailFuture<'a, MailPage> {
        Box::pin(async move { Ok(self.page.clone()) })
    }
    fn metadata<'a>(&'a mut self, id: &'a str) -> MailFuture<'a, Value> {
        Box::pin(async move {
            Ok(self
                .page
                .records
                .iter()
                .find(|r| r["external_id"] == id)
                .cloned()
                .unwrap_or(json!({"external_id":id,"deleted":true})))
        })
    }
    fn body<'a>(&'a mut self, id: &'a str) -> MailFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.bodies.lock().unwrap().push(id.into());
            anyhow::ensure!(!self.fail_body, "synthetic body failure");
            Ok(format!("From: Alice <alice@example.invalid>\r\nTo: Owner <owner@example.invalid>\r\nSubject: Invoice\r\nMessage-ID: <shared-original>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nOriginal evidence {id}\r\n> old quote\r\n").into_bytes())
        })
    }
}
async fn setup(pool: &PgPool) -> (tempfile::TempDir, MediaStore, Fence) {
    let root = tempfile::tempdir().unwrap();
    let store = MediaStore::new(
        pool.clone(),
        root.path().to_owned(),
        ResourceBudget::new(ResourceLimits::mac()).unwrap(),
    )
    .unwrap();
    let cid = Uuid::new_v4();
    let owner = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,lease_owner,lease_token,lease_until) VALUES($1,'mail','owner@example.invalid','Synthetic mailbox','gmail',true,$2,$3,now()+interval '10 minutes')").bind(cid).bind(owner).bind(Uuid::new_v4()).execute(pool).await.unwrap();
    (
        root,
        store,
        Fence {
            connection_id: cid,
            owner,
            generation: 1,
        },
    )
}
fn page() -> MailPage {
    MailPage {
        records: vec![
            json!({"external_id":"pending","headers":{"from":"stranger@example.invalid","subject":"Hi"},"preview":"uncertain"}),
            json!({"external_id":"marketing","headers":{"from":"sales@example.invalid","subject":"Discount newsletter","list-id":"offers"}}),
            json!({"external_id":"receipt","headers":{"from":"alice@example.invalid","subject":"Invoice"}}),
        ],
        cursor: json!({"history_id":"saved-10"}),
        complete: true,
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn header_first_and_atomic_cursor_replay(pool: PgPool) {
    let (_root, store, fence) = setup(&pool).await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let mut fake = Fake {
        page: page(),
        bodies: bodies.clone(),
        fail_body: false,
    };
    mail::sync::sync_page(&store, &fence, "gmail", &json!({}), &mut fake)
        .await
        .unwrap();
    assert_eq!(*bodies.lock().unwrap(), vec!["receipt"]);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM mail.connection_records")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);
    let stored: Value = sqlx::query_scalar(
        "SELECT payload FROM mail.connection_records WHERE external_id='receipt'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored["body"], "Original evidence receipt");
    assert!(stored["raw_body"].as_str().unwrap().contains("old quote"));
    assert!(
        store
            .root
            .join(
                monica_core::media::object_path(stored["original"]["blob"].as_str().unwrap())
                    .unwrap()
            )
            .exists()
    );
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        connections::cursor(&mut tx, Provider::Mail, fence.connection_id, "mailbox:live")
            .await
            .unwrap()["history_id"],
        "saved-10"
    );
    tx.commit().await.unwrap();
    mail::sync::sync_page(&store, &fence, "gmail", &json!({}), &mut fake)
        .await
        .unwrap();
    assert_eq!(bodies.lock().unwrap().len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mail.connection_revisions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn body_failure_and_stale_fence_never_advance_cursor(pool: PgPool) {
    let (_root, store, fence) = setup(&pool).await;
    let mut fake = Fake {
        page: page(),
        bodies: Arc::new(Mutex::new(Vec::new())),
        fail_body: true,
    };
    assert!(
        mail::sync::sync_page(&store, &fence, "gmail", &json!({}), &mut fake)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mail.connection_cursors")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    fake.fail_body = false;
    sqlx::query("UPDATE crm.source_connections SET generation=generation+1 WHERE id=$1")
        .bind(fence.connection_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        mail::sync::sync_page(&store, &fence, "gmail", &json!({}), &mut fake)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mail.connection_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn classification_cache_requires_exact_header_fingerprint_and_rule_wins(pool: PgPool) {
    let (_root, store, fence) = setup(&pool).await;
    let mut fake = Fake {
        page: page(),
        bodies: Arc::new(Mutex::new(Vec::new())),
        fail_body: false,
    };
    let p = MailPage {
        records: vec![page().records[0].clone()],
        cursor: json!({}),
        complete: true,
    };
    sqlx::query("INSERT INTO crm.mail_classifications(connection_id,external_id,header_hash,category) VALUES($1,'pending',$2,'personal')").bind(fence.connection_id).bind(mail::fingerprint(&p.records[0])).execute(&pool).await.unwrap();
    let result = mail::sync::process_page(&store, fence.connection_id, &mut fake, &p)
        .await
        .unwrap();
    assert_eq!(result[0]["classification_reason"], "model");
    assert_eq!(fake.bodies.lock().unwrap().len(), 1);
    sqlx::query("INSERT INTO crm.mail_rules VALUES($1,'message','pending','marketing')")
        .bind(fence.connection_id)
        .execute(&pool)
        .await
        .unwrap();
    let result = mail::sync::process_page(&store, fence.connection_id, &mut fake, &p)
        .await
        .unwrap();
    assert_eq!(result[0]["category"], "marketing");
    assert_eq!(result[0]["classification_reason"], "user_rule");
    assert_eq!(fake.bodies.lock().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_drains_all_seven_sources_in_bounded_batches(pool: PgPool) {
    for provider in [
        Provider::Wechat,
        Provider::Instagram,
        Provider::Linkedin,
        Provider::Whatsapp,
        Provider::AppleMessages,
        Provider::AppleContacts,
        Provider::Mail,
    ] {
        let cid = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,$2,$3,'Synthetic recovery','local',false)").bind(cid).bind(provider.schema()).bind(format!("synthetic-{}",provider.schema())).execute(&pool).await.unwrap();
        let count = if provider.schema() == "apple_messages" {
            1200
        } else {
            1
        };
        let records:Vec<_>=(0..count).map(|i|json!({"external_id":format!("event-{i}"),"thread_id":"synthetic-thread","kind":"message","occurred_at":"2024-01-01T00:00:00Z","body":"synthetic recovery evidence","included":true,"participants":[]})).collect();
        let mut tx = pool.begin().await.unwrap();
        connections::commit_page(
            &mut tx,
            provider,
            cid,
            "messages",
            &records,
            &json!({"complete":true}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    for _ in 0..12 {
        if !mail::sync::recover_one(&pool).await.unwrap() {
            break;
        }
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.connection_evidence")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1206
    );
    assert!(!mail::sync::recover_one(&pool).await.unwrap());
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_fairly_serves_new_account_before_old_backlog_finishes(pool: PgPool) {
    let mut ids = Vec::new();
    for (account, count) in [("old", 3000), ("new", 1)] {
        let cid = Uuid::new_v4();
        ids.push(cid);
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES($1,'apple_messages',$2,$2,'local')").bind(cid).bind(account).execute(&pool).await.unwrap();
        let records:Vec<_>=(0..count).map(|i|json!({"external_id":i.to_string(),"kind":"message","thread_id":"chat","occurred_at":"2024-01-01T00:00:00Z","body":"synthetic fairness","participants":[]})).collect();
        let mut tx = pool.begin().await.unwrap();
        connections::commit_page(
            &mut tx,
            Provider::AppleMessages,
            cid,
            "messages",
            &records,
            &json!({}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    assert!(mail::sync::recover_one(&pool).await.unwrap());
    assert!(mail::sync::recover_one(&pool).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.connection_evidence WHERE connection_id=$1"
        )
        .bind(ids[1])
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM apple_messages.connection_outbox WHERE connection_id=$1 AND delivered_at IS NULL").bind(ids[0]).fetch_one(&pool).await.unwrap(),2000);
}

#[sqlx::test(migrations = "../../migrations")]
async fn device_page_is_retained_on_fence_failure_and_acknowledged_after_commit(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let store = MediaStore::new(
        pool.clone(),
        root.path().join("media"),
        ResourceBudget::new(ResourceLimits::nas()).unwrap(),
    )
    .unwrap();
    let cid = Uuid::new_v4();
    let owner = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,generation,lease_owner,lease_until) VALUES($1,'whatsapp','synthetic-device','Synthetic device','qr',true,2,$2,now()+interval '10 minutes')").bind(cid).bind(owner).execute(&pool).await.unwrap();
    let device = root.path().join("device");
    tokio::fs::create_dir_all(device.join("pending"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(device.join("attachments"))
        .await
        .unwrap();
    let content = b"synthetic attachment original";
    let hash = monica_core::identity::bytes_hash(content);
    tokio::fs::write(device.join("attachments").join(&hash), content)
        .await
        .unwrap();
    let page = json!({"records":[{"external_id":"chat:one","thread_id":"chat","kind":"message","body":"Synthetic device evidence","occurred_at":"2024-01-01T00:00:00Z","participants":[],"raw_message":{"conversation":"synthetic"},"attachments":[{"filename":"fixture.bin","local_blob":hash}]}]});
    let pending = device.join("pending/0001.json");
    tokio::fs::write(&pending, serde_json::to_vec(&page).unwrap())
        .await
        .unwrap();
    let mut fence = Fence {
        connection_id: cid,
        owner,
        generation: 1,
    };
    assert!(
        monica_core::whatsapp::consume_device_page(&store, &fence, &device)
            .await
            .is_err()
    );
    assert!(pending.exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM whatsapp.connection_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    fence.generation = 2;
    assert!(
        monica_core::whatsapp::consume_device_page(&store, &fence, &device)
            .await
            .unwrap()
    );
    assert!(!pending.exists());
    let saved: Value = sqlx::query_scalar(
        "SELECT payload FROM whatsapp.connection_records WHERE connection_id=$1",
    )
    .bind(cid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(saved["attachments"][0]["blob"], hash);
    assert!(saved["attachments"][0].get("local_blob").is_none());
    tokio::fs::write(&pending, serde_json::to_vec(&page).unwrap())
        .await
        .unwrap();
    assert!(
        monica_core::whatsapp::consume_device_page(&store, &fence, &device)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM whatsapp.connection_revisions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

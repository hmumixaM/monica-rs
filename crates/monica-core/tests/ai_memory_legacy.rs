use monica_core::{Provider, ai::memory::legacy, identity, imports, linking, messages::Message};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
async fn person(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name,summary) VALUES($1,'person',$2,'independent notebook')").bind(id).bind(name).execute(pool).await.unwrap();
    id
}
async fn account(pool: &PgPool, name: &str, profile: Option<Uuid>) -> Uuid {
    let id = identity::account_id(Provider::Wechat, name).unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat',$2,CASE WHEN right($2,9)='@chatroom' THEN 'group' ELSE 'person' END,$2)").bind(id).bind(name).execute(pool).await.unwrap();
    if let Some(profile) = profile {
        sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
            .bind(id)
            .bind(profile)
            .execute(pool)
            .await
            .unwrap();
    }
    id
}
fn message(index: usize, conversation: &str, sender: &str) -> Message {
    Message {
        account: "self".into(),
        conversation: conversation.into(),
        external_id: format!("m{index:05}"),
        sender: sender.into(),
        sent_at_ms: 1767225600000 + index as i64,
        body: format!("synthetic body {index}"),
    }
}
async fn append(pool: &PgPool, records: &[Message]) {
    let mut tx = pool.begin().await.unwrap();
    imports::append_messages(&mut tx, Provider::Wechat, records)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
async fn generation(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT generation FROM crm.memory_heads WHERE profile_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn count(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1 AND payload ? '_legacy_id'",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn bulk_reference_noop_correction_tombstone_and_private_flags(pool: PgPool) {
    let profile = person(&pool, "Direct").await;
    account(&pool, "friend", Some(profile)).await;
    let records = (0..1000)
        .map(|i| message(i, "friend", "friend"))
        .collect::<Vec<_>>();
    append(&pool, &records).await;
    assert_eq!(count(&pool, profile).await, 1000);
    assert_eq!(generation(&pool, profile).await, 1);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.memory_inputs WHERE source_kind='chat' AND private AND NOT payload ? 'text' AND octet_length(payload::text)<120").fetch_one(&pool).await.unwrap(),1000);
    append(&pool, &records).await;
    assert_eq!(generation(&pool, profile).await, 1);
    imports::correct_message(
        &pool,
        Provider::Wechat,
        "self",
        "friend",
        "m00000",
        "edit",
        Some("Changed body"),
    )
    .await
    .unwrap();
    assert_eq!(generation(&pool, profile).await, 2);
    let text:String=sqlx::query_scalar("SELECT payload->>'text' FROM crm.memory_legacy_current WHERE kind='message' AND payload->>'text'='Changed body'").fetch_one(&pool).await.unwrap();
    assert_eq!(text, "Changed body");
    imports::correct_message(
        &pool,
        Provider::Wechat,
        "self",
        "friend",
        "m00000",
        "delete",
        None,
    )
    .await
    .unwrap();
    assert_eq!(count(&pool, profile).await, 999);
    assert_eq!(generation(&pool, profile).await, 3);
}
#[sqlx::test(migrations = "../../migrations")]
async fn group_uses_sender_and_self_profiles_are_removed_then_restored(pool: PgPool) {
    let a = person(&pool, "Sender A").await;
    let b = person(&pool, "Sender B").await;
    account(&pool, "sender-a", Some(a)).await;
    account(&pool, "sender-b", Some(b)).await;
    account(&pool, "room@chatroom", None).await;
    append(
        &pool,
        &[
            message(1, "room@chatroom", "sender-a"),
            message(2, "room@chatroom", "sender-b"),
            message(3, "room@chatroom", "self"),
        ],
    )
    .await;
    assert_eq!(count(&pool, a).await, 1);
    assert_eq!(count(&pool, b).await, 1);
    sqlx::query("INSERT INTO crm.person_identifiers(profile_id,kind,value,is_self) VALUES($1,'email','self@example.test',true)").bind(a).execute(&pool).await.unwrap();
    while legacy::drain(&pool).await.unwrap() > 0 {}
    assert_eq!(count(&pool, a).await, 0);
    assert_eq!(count(&pool, b).await, 1);
    sqlx::query("UPDATE crm.person_identifiers SET is_self=false WHERE profile_id=$1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    while legacy::drain(&pool).await.unwrap() > 0 {}
    assert_eq!(count(&pool, a).await, 1);
    sqlx::query("UPDATE crm.profiles SET archived_at=now() WHERE id=$1")
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    while legacy::drain(&pool).await.unwrap() > 0 {}
    assert_eq!(count(&pool, b).await, 0);
    sqlx::query("UPDATE crm.profiles SET archived_at=NULL WHERE id=$1")
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    while legacy::drain(&pool).await.unwrap() > 0 {}
    assert_eq!(count(&pool, b).await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn relink_sweeps_bounded_pages_and_removes_old_owner(pool: PgPool) {
    let a = person(&pool, "Old").await;
    let b = person(&pool, "New").await;
    let source = account(&pool, "friend", Some(a)).await;
    append(
        &pool,
        &(0..1000)
            .map(|i| message(i, "friend", "friend"))
            .collect::<Vec<_>>(),
    )
    .await;
    append(&pool, &[message(1000, "friend", "friend")]).await;
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: source,
            profile_id: Some(b),
            expected_profile_id: Some(a),
        },
        None,
    )
    .await
    .unwrap();
    assert!(sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_scopes WHERE profiles @> ARRAY[$1::uuid])").bind(a).fetch_one(&pool).await.unwrap());
    assert_eq!(legacy::drain(&pool).await.unwrap(), 1000);
    assert_eq!(legacy::drain(&pool).await.unwrap(), 1);
    assert_eq!(legacy::drain(&pool).await.unwrap(), 0);
    assert_eq!(count(&pool, a).await, 0);
    assert_eq!(count(&pool, b).await, 1001);
}
#[sqlx::test(migrations = "../../migrations")]
async fn social_and_activity_references_delete_and_participant_changes(pool: PgPool) {
    let a = person(&pool, "One").await;
    let b = person(&pool, "Two").await;
    let event = Uuid::new_v4();
    let activity = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,content,content_hash,version) VALUES($1,'monica',$1,$2,'post','post','{\"caption\":\"public text\"}','hash',1)").bind(event).bind(a).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.activities(id,activity_type,occurred_at,title,body,source_type,content_hash) VALUES($1,'note',now(),'Private note','private text','manual','hash')").bind(activity).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.activity_participants VALUES($1,$2)")
        .bind(activity)
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    legacy::sync(&mut tx, "social", &[event]).await.unwrap();
    legacy::sync(&mut tx, "activity", &[activity])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let flags:Vec<(String,bool)>=sqlx::query_as("SELECT source_kind,private FROM crm.memory_inputs WHERE profile_id=$1 ORDER BY source_kind").bind(a).fetch_all(&pool).await.unwrap();
    assert_eq!(flags, vec![("note".into(), true), ("social".into(), false)]);
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE crm.activity_participants SET profile_id=$2 WHERE activity_id=$1")
        .bind(activity)
        .bind(b)
        .execute(&mut *tx)
        .await
        .unwrap();
    legacy::sync(&mut tx, "activity", &[activity])
        .await
        .unwrap();
    sqlx::query("DELETE FROM crm.timeline_events WHERE id=$1")
        .bind(event)
        .execute(&mut *tx)
        .await
        .unwrap();
    legacy::sync(&mut tx, "social", &[event]).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(count(&pool, a).await, 0);
    assert_eq!(count(&pool, b).await, 1);
    let stored: Value =
        sqlx::query_scalar("SELECT payload FROM crm.memory_inputs WHERE profile_id=$1")
            .bind(b)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        json!({"_legacy_kind":"activity","_legacy_id":activity})
    );
}

struct Capturing(std::sync::Mutex<Vec<Value>>);
impl monica_core::ai::provider::Provider for Capturing {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn complete<'a>(
        &'a self,
        lease: &'a monica_core::ai::Lease,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send + 'a>>
    {
        Box::pin(async move {
            self.0.lock().unwrap().push(lease.input.clone());
            Ok(json!({"summary":"synthetic reduction","facts":[],"gaps":[],"items":[]}))
        })
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn dirty_bucket_resolves_references_with_separate_chat_and_note_privacy(pool: PgPool) {
    let profile = person(&pool, "Reference privacy").await;
    account(&pool, "friend", Some(profile)).await;
    let mut m = message(0, "friend", "friend");
    m.body = "PRIVATE_CHAT_MARKER".into();
    append(&pool, &[m]).await;
    let post = Uuid::new_v4();
    let activity = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,content,content_hash,version) VALUES($1,'wechat',$1,$2,'post','public','{\"text\":\"PUBLIC_MARKER\"}','public',1)").bind(post).bind(profile).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.activities(id,activity_type,occurred_at,title,body,source_type,content_hash) VALUES($1,'note',now(),'Note','PRIVATE_NOTE_MARKER','manual','note')").bind(activity).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.activity_participants VALUES($1,$2)")
        .bind(activity)
        .bind(profile)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    legacy::sync(&mut tx, "social", &[post]).await.unwrap();
    legacy::sync(&mut tx, "activity", &[activity])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let capture = Capturing(Default::default());
    for (chats, notes) in [(false, false), (true, false), (true, true)] {
        sqlx::query(
            "UPDATE crm.ai_settings SET enabled=true,include_messages=$1,include_private_notes=$2",
        )
        .bind(chats)
        .bind(notes)
        .execute(&pool)
        .await
        .unwrap();
        capture.0.lock().unwrap().clear();
        monica_core::ai::memory::enqueue_pending(&pool)
            .await
            .unwrap();
        for attempt in 0..128 {
            if !monica_core::ai::run_one(&pool, &capture).await.unwrap() {
                break;
            }
            assert!(attempt < 127, "bounded Memory never completed");
        }
        let captured = serde_json::to_string(&*capture.0.lock().unwrap()).unwrap();
        if !chats {
            assert!(captured.contains("PUBLIC_MARKER"));
        }
        assert_eq!(
            captured.contains("PRIVATE_CHAT_MARKER"),
            chats,
            "message privacy is independent of note privacy"
        );
        assert_eq!(captured.contains("PRIVATE_NOTE_MARKER"), notes);
    }
}

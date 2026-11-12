use monica_core::ai::{self, memory, provider::Provider};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{future::Future, pin::Pin, sync::Mutex};
use uuid::Uuid;
struct Counting {
    inputs: Mutex<Vec<(String, Value)>>,
}
impl Provider for Counting {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn complete<'a>(
        &'a self,
        lease: &'a ai::Lease,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            self.inputs
                .lock()
                .unwrap()
                .push((lease.kind.clone(), lease.input.clone()));
            Ok(
                json!({"summary":format!("Synthetic summary {}",monica_core::identity::observation_hash(&lease.input)),"facts":[],"gaps":[],"items":[]}),
            )
        })
    }
}
#[test]
fn stable_radix_siblings_unicode_fragments_and_addresses() {
    let a = format!("00{}", "a".repeat(62));
    let b = format!("ff{}", "b".repeat(62));
    let rows = vec![
        (a.clone(), json!({"text":"A".repeat(20000)})),
        (b.clone(), json!({"text":"B".repeat(20000)})),
    ];
    let before = memory::leaves("", &rows);
    let changed = vec![
        (a, json!({"text":"Changed".repeat(4000)})),
        (b, json!({"text":"B".repeat(20000)})),
    ];
    let after = memory::leaves("", &changed);
    let unaffected = before.iter().find(|n| n.key.starts_with("leaf:f")).unwrap();
    assert!(
        after
            .iter()
            .any(|n| n.key == unaffected.key && n.hash == unaffected.hash)
    );
    let value = json!({"text":"合成🦀".repeat(20000)});
    let fragments = memory::leaves("ab", &[(format!("ab{}", "c".repeat(62)), value.clone())]);
    let reconstructed = fragments
        .iter()
        .flat_map(|n| n.input["sources"].as_array().unwrap())
        .map(|v| v["serialized_source_fragment"].as_str().unwrap())
        .collect::<String>();
    assert_eq!(reconstructed, serde_json::to_string(&value).unwrap());
    assert!(fragments.len() > 1);
}
async fn run_idle(pool: &PgPool, provider: &Counting) {
    for _ in 0..128 {
        if !ai::run_one(pool, provider).await.unwrap() {
            return;
        }
    }
    panic!("Memory did not finish bounded turns")
}
async fn add(pool: &PgPool, id: Uuid, value: &str) -> bool {
    let mut tx = pool.begin().await.unwrap();
    let changed = memory::replace_sources(
        &mut tx,
        id,
        "social",
        &[(
            "stable-public-source".into(),
            Some(json!({"id":"stable-public-source","text":value})),
        )],
        false,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    changed
}
#[sqlx::test(migrations = "../../migrations")]
async fn incremental_cache_noop_aba_privacy_and_generation_fences(pool: PgPool) {
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic Memory Person')").bind(id).execute(&pool).await.unwrap();
    assert!(add(&pool, id, "Synthetic A").await);
    assert!(!add(&pool, id, "Synthetic A").await);
    let mut tx = pool.begin().await.unwrap();
    memory::replace_sources(
        &mut tx,
        id,
        "chat",
        &[(
            "private-source".into(),
            Some(json!({"text":"PRIVATE-CHAT-MARKER"})),
        )],
        false,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let provider = Counting {
        inputs: Mutex::new(Vec::new()),
    };
    ai::memory::enqueue_pending(&pool).await.unwrap();
    run_idle(&pool, &provider).await;
    let initial = provider.inputs.lock().unwrap().len();
    assert!(initial > 0);
    assert!(
        !provider
            .inputs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, v)| v.to_string().contains("PRIVATE-CHAT-MARKER"))
    );
    ai::memory::enqueue_pending(&pool).await.unwrap();
    run_idle(&pool, &provider).await;
    assert_eq!(
        provider.inputs.lock().unwrap().len(),
        initial,
        "unchanged inputs must not call the model"
    );
    assert!(add(&pool, id, "Synthetic B").await);
    ai::memory::enqueue_pending(&pool).await.unwrap();
    run_idle(&pool, &provider).await;
    let changed = provider.inputs.lock().unwrap().len();
    assert!(changed > initial);
    assert!(add(&pool, id, "Synthetic A").await);
    ai::memory::enqueue_pending(&pool).await.unwrap();
    run_idle(&pool, &provider).await;
    assert_eq!(
        provider.inputs.lock().unwrap().len(),
        changed,
        "A-B-A must reuse immutable leaf, reduction, final and suggestion outputs"
    );
    sqlx::query("UPDATE crm.ai_settings SET include_messages=true")
        .execute(&pool)
        .await
        .unwrap();
    ai::memory::enqueue_pending(&pool).await.unwrap();
    run_idle(&pool, &provider).await;
    assert!(
        provider
            .inputs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, v)| v.to_string().contains("PRIVATE-CHAT-MARKER"))
    );
    let generation: i64 =
        sqlx::query_scalar("SELECT generation FROM crm.memory_heads WHERE profile_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    ai::enqueue(&pool, "ai.profile", &id.to_string())
        .await
        .unwrap();
    sqlx::query("UPDATE crm.ai_jobs SET status='queued' WHERE kind='ai.profile' AND subject=$1")
        .bind(id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    let lease = ai::claim(&pool).await.unwrap().unwrap();
    assert!(add(&pool, id, "Synthetic C").await);
    assert!(
        !ai::finish(
            &pool,
            &lease,
            json!({"summary":"Stale","_memory_generation":generation})
        )
        .await
        .unwrap()
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn batch_connection_inputs_group_author_filter_copies_and_relink(pool: PgPool) {
    let connection = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,'apple_messages','synthetic-own','Synthetic source','native',true)").bind(connection).execute(&pool).await.unwrap();
    let author = Uuid::new_v4();
    let recipient = Uuid::new_v4();
    let group = Uuid::new_v4();
    for (id, kind) in [(author, "person"), (recipient, "person"), (group, "group")] {
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,$2,'Synthetic Memory Contact')").bind(id).bind(kind).execute(&pool).await.unwrap();
    }
    let conversation = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,profile_id,conversation_type,connection_id) VALUES($1,'apple_messages','synthetic-own','synthetic-room',$2,'group',$3)").bind(conversation).bind(group).bind(connection).execute(&pool).await.unwrap();
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    for (index, id) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO crm.connection_evidence(id,connection_id,external_id,revision,content_hash,kind,title,body,included,dedup_key,conversation_id) VALUES($1,$2,$3,1,'synthetic','message','','Synthetic group remark',true,'synthetic-copy-key',$4)").bind(id).bind(connection).bind(format!("message-{index}")).bind(conversation).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO crm.connection_evidence_people VALUES($1,$2,'sender'),($1,$3,'recipient')",
        )
        .bind(id)
        .bind(author)
        .bind(recipient)
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    memory::connection_evidence_many(&mut tx, &ids)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let owner: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT profile_id FROM crm.memory_inputs WHERE source_kind='connection'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(owner, vec![author]);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1")
            .bind(author)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1, "identical account copies must materialize once");
    let bytes: i64 = sqlx::query_scalar(
        "SELECT sum(octet_length(payload::text))::bigint FROM crm.memory_inputs",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        bytes < 200,
        "input index stores tiny immutable evidence references, not repeated bodies"
    );
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE crm.connection_evidence SET deleted=true WHERE id=$1")
        .bind(ids[0])
        .execute(&mut *tx)
        .await
        .unwrap();
    memory::connection_evidence_many(&mut tx, &ids[..1])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1")
            .bind(author)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1, "retained second copy preserves source input");
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM crm.connection_evidence_people WHERE evidence_id=$1")
        .bind(ids[1])
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.connection_evidence_people VALUES($1,$2,'sender')")
        .bind(ids[1])
        .bind(recipient)
        .execute(&mut *tx)
        .await
        .unwrap();
    memory::connection_evidence_many(&mut tx, &ids[1..])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let owners: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT profile_id FROM crm.memory_inputs WHERE source_kind='connection'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        owners,
        vec![recipient],
        "relink removes former owner immediately"
    );
}

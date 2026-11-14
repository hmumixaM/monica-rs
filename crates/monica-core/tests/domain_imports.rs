use monica_core::{Provider, Record, claim, domain, identity, imports, linking, project};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn profile(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2)")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}
fn record(kind: &str, object: &str, event: &str, payload: Value) -> Record {
    Record {
        provider: Provider::Instagram,
        account: "fixture-account".into(),
        object: object.into(),
        kind: kind.into(),
        event: event.into(),
        payload,
    }
}
async fn drain(pool: &PgPool) {
    while let Some(lease) = claim(pool).await.unwrap() {
        project(pool, &lease).await.unwrap();
    }
}
fn batch(records: Vec<imports::BatchRecord>) -> imports::Batch {
    serde_json::from_value(json!({"source":"instagram","stream":"posts","batch_id":Uuid::new_v4().to_string(),"idempotency_key":Uuid::new_v4().to_string(),"observed_at":"2026-01-01T12:00:00Z","records":records,"cursor_after":"complete"})).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn structured_linkedin_facts_do_not_become_generic_field_suggestions(pool: PgPool) {
    use monica_core::ai;
    let profile = profile(&pool, "Structured person").await;
    let mut r = record(
        "profile",
        "profile",
        "1",
        json!({"fields":{"linkedin.headline":"Engineer at Synthetic Company"},"experience":[{"company":"Synthetic Company","title":"Engineer","start_date":"2020-01"}],"education":[{"school":"Synthetic University","degree":"MS"}]}),
    );
    r.provider = Provider::Linkedin;
    monica_core::ingest(&pool, &r).await.unwrap();
    drain(&pool).await;
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: identity::account_id(r.provider, &r.account).unwrap(),
            profile_id: Some(profile),
            expected_profile_id: None,
        },
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=false")
        .execute(&pool)
        .await
        .unwrap();
    let post = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) VALUES($1,'linkedin',$1,$2,'post','new-job',now(),'{\"text\":\"I joined New Startup today.\"}','new',1)")
        .bind(post).bind(profile).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let snapshot = ai::snapshot(&mut tx, "ai.profile", &profile.to_string())
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let known = snapshot.input["known_structured"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(known.len(), 2);
    assert!(known.iter().any(|f| f["kind"] == "employment"
        && f["company"] == "Synthetic Company"
        && f["job"] == "Engineer"
        && f["source_type"] == "linkedin"));
    let field = snapshot.input["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["field_key"] == "linkedin.headline")
        .unwrap()["id"]
        .clone();
    let lease = ai::Lease {
        id: Uuid::new_v4(),
        token: Uuid::new_v4(),
        kind: "ai.profile".into(),
        subject: profile.to_string(),
        model: snapshot.model,
        input: snapshot.input,
        input_hash: snapshot.hash,
        privacy: json!({}),
    };
    let output=ai::validate_output(&lease,json!({"summary":"Known career","items":[
        {"kind":"company","value":"Synthetic Company","evidence":[field],"quote":"Engineer at Synthetic Company","confidence":"high"},
        {"kind":"job","value":"工程师","evidence":[field],"quote":"Engineer at Synthetic Company","confidence":"high"},
        {"kind":"school","value":"合成大学","evidence":[field],"quote":"Synthetic University","confidence":"high"},
        {"kind":"company","value":"New Startup","evidence":[post],"quote":"I joined New Startup today.","confidence":"high"}
    ]})).unwrap();
    let items = output["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["value"], "New Startup");
    // Imported historical suggestions can lack a provider/field key. Their
    // original English quote still identifies the already-recorded fact.
    assert!(ai::known::represented(
        &json!({"kind":"school","value":"合成大学","quote":"Master of Science at Synthetic University","evidence":[{"kind":"profile"}]}),
        &known
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_projection_link_relink_and_manual_precedence(pool: PgPool) {
    let p1 = profile(&pool, "First").await;
    let p2 = profile(&pool, "Second").await;
    let r = record(
        "profile",
        "profile",
        "a",
        json!({"fields":{"instagram.bio":"Source text"},"display_name":"Fixture"}),
    );
    monica_core::ingest(&pool, &r).await.unwrap();
    drain(&pool).await;
    let account = identity::account_id(Provider::Instagram, "fixture-account").unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.profiles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2, "source discovery must not create people");
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: account,
            profile_id: Some(p1),
            expected_profile_id: None,
        },
        None,
    )
    .await
    .unwrap();
    let value:Value=sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1").bind(p1).fetch_one(&pool).await.unwrap();
    assert_eq!(value, json!("Source text"));
    let manual = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation) VALUES($1,$2,'instagram.bio','\"Manual text\"','manual','manual','edit')").bind(manual).bind(p1).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.field_current SET revision_id=$2 WHERE profile_id=$1")
        .bind(p1)
        .bind(manual)
        .execute(&pool)
        .await
        .unwrap();
    let r = record(
        "profile",
        "profile",
        "b",
        json!({"fields":{"instagram.bio":"Updated source"}}),
    );
    monica_core::ingest(&pool, &r).await.unwrap();
    drain(&pool).await;
    let value:Value=sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1").bind(p1).fetch_one(&pool).await.unwrap();
    assert_eq!(value, json!("Manual text"));
    let conflict: bool = sqlx::query_scalar("SELECT is_conflict FROM crm.field_revisions WHERE profile_id=$1 AND source_account_id=$2 AND field_key='instagram.bio' ORDER BY revision_seq DESC LIMIT 1")
        .bind(p1).bind(account).fetch_one(&pool).await.unwrap();
    assert!(
        conflict,
        "the preserved manual value must expose the source update as a conflict"
    );
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: account,
            profile_id: Some(p2),
            expected_profile_id: Some(p1),
        },
        None,
    )
    .await
    .unwrap();
    assert!(
        linking::set_link(
            &pool,
            linking::LinkChange {
                account_id: account,
                profile_id: Some(p1),
                expected_profile_id: Some(p1)
            },
            None
        )
        .await
        .is_err()
    );
    let value:Value=sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1").bind(p2).fetch_one(&pool).await.unwrap();
    assert_eq!(value, json!("Updated source"));
    let conflict: bool = sqlx::query_scalar("SELECT r.is_conflict FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key='instagram.bio'")
        .bind(p2).fetch_one(&pool).await.unwrap();
    assert!(
        !conflict,
        "relinking to a person without an override must not inherit a conflict"
    );
    let source_count: i64 = sqlx::query_scalar("SELECT count(*) FROM instagram.revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(source_count, 2);
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: account,
            profile_id: None,
            expected_profile_id: Some(p2),
        },
        None,
    )
    .await
    .unwrap();
    let source_count: i64 = sqlx::query_scalar("SELECT count(*) FROM instagram.revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(source_count, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn migrated_social_events_keep_public_ids_during_source_updates(pool: PgPool) {
    let owner = profile(&pool, "Owner").await;
    let author = profile(&pool, "Author").await;
    for (external, person) in [("fixture-account", owner), ("speaker", author)] {
        let source = Record {
            account: external.into(),
            ..record(
                "profile",
                "profile",
                "profile",
                json!({"display_name":external}),
            )
        };
        monica_core::ingest(&pool, &source).await.unwrap();
        drain(&pool).await;
        linking::set_link(
            &pool,
            linking::LinkChange {
                account_id: identity::account_id(Provider::Instagram, external).unwrap(),
                profile_id: Some(person),
                expected_profile_id: None,
            },
            None,
        )
        .await
        .unwrap();
    }
    let account = identity::account_id(Provider::Instagram, "fixture-account").unwrap();
    let post = Uuid::new_v4();
    let story = Uuid::new_v4();
    let legacy_interaction = Uuid::new_v4();
    for (kind, external, event) in [
        ("post", "legacy-post", post),
        ("story", "legacy-story", story),
    ] {
        let object = identity::object_id(account, kind, external);
        assert_ne!(event, object);
        sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,content,content_hash,version) VALUES($1,'instagram',$2,$3,$4,$5,$6,'{\"caption\":\"Legacy content\"}','legacy',1)")
            .bind(event).bind(object).bind(account).bind(owner).bind(kind).bind(external).execute(&pool).await.unwrap();
        if kind == "post" {
            sqlx::query("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_external_id,content) VALUES($1,$2,'instagram','comment','comment','speaker','{\"text\":\"Legacy comment\"}')")
                .bind(legacy_interaction).bind(post).execute(&pool).await.unwrap();
        }
        // Equal source versions must resolve the preserved ID without replacing
        // a migration's existing interaction with an older source payload.
        let first = record(
            kind,
            external,
            "first",
            json!({"caption":"Legacy content","interactions":[{"external_id":"comment","author_external_id":"speaker","content":{"text":"Old source comment"}}]}),
        );
        monica_core::ingest(&pool, &first).await.unwrap();
        drain(&pool).await;
        if kind == "post" {
            let text: String = sqlx::query_scalar(
                "SELECT content->>'text' FROM crm.social_interactions WHERE id=$1",
            )
            .bind(legacy_interaction)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(text, "Legacy comment");
        }
        let changed = record(
            kind,
            external,
            "changed",
            json!({"caption":"Updated content","coverage_status":"archived","interactions":[{"external_id":"comment","author_external_id":"speaker","content":{"text":"Updated comment"}}]}),
        );
        monica_core::ingest(&pool, &changed).await.unwrap();
        drain(&pool).await;
        monica_core::ingest(&pool, &changed).await.unwrap();
        drain(&pool).await;
        let saved: (Uuid, String, i64, String) = sqlx::query_as("SELECT id,content->>'caption',version,status FROM crm.timeline_events WHERE provider='instagram' AND object_id=$1")
            .bind(object).fetch_one(&pool).await.unwrap();
        assert_eq!(
            saved,
            (
                event,
                "Updated content".into(),
                2,
                if kind == "story" {
                    "archived"
                } else {
                    "active"
                }
                .into()
            )
        );
    }
    let interaction: (Uuid, Uuid, Option<Uuid>, String) = sqlx::query_as("SELECT id,post_id,author_profile_id,content->>'text' FROM crm.social_interactions WHERE post_id=$1 AND external_id='comment'")
        .bind(post).fetch_one(&pool).await.unwrap();
    assert_eq!(
        interaction,
        (
            legacy_interaction,
            post,
            Some(author),
            "Updated comment".into()
        )
    );
    let standalone = Record {
        account: "speaker".into(),
        ..record(
            "comment",
            "standalone",
            "standalone",
            json!({"post_account_external_id":"fixture-account","post_external_id":"legacy-post","text":"Standalone comment"}),
        )
    };
    monica_core::ingest(&pool, &standalone).await.unwrap();
    drain(&pool).await;
    let parent: Uuid = sqlx::query_scalar(
        "SELECT post_id FROM crm.social_interactions WHERE external_id='standalone'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(parent, post);
    let reply = Record {
        account: "speaker".into(),
        ..record(
            "reply",
            "story-reply",
            "story-reply",
            json!({"post_id":story,"text":"Reply to the public Story ID"}),
        )
    };
    monica_core::ingest(&pool, &reply).await.unwrap();
    drain(&pool).await;
    let parent: Uuid = sqlx::query_scalar(
        "SELECT post_id FROM crm.social_interactions WHERE external_id='story-reply'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(parent, story);
    let mut pending: Vec<Uuid> = sqlx::query_scalar("SELECT entity_id FROM crm.memory_legacy_pending_records WHERE kind='social' ORDER BY entity_id")
        .fetch_all(&pool).await.unwrap();
    let mut expected = vec![post, story];
    pending.sort();
    expected.sort();
    assert_eq!(
        pending, expected,
        "Memory must follow the public legacy event IDs"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn different_posts_for_same_person_project_concurrently(pool: PgPool) {
    monica_core::ingest(
        &pool,
        &record(
            "profile",
            "profile",
            "init",
            json!({"display_name":"Fixture"}),
        ),
    )
    .await
    .unwrap();
    drain(&pool).await;
    let p = profile(&pool, "Person").await;
    let a = identity::account_id(Provider::Instagram, "fixture-account").unwrap();
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: a,
            profile_id: Some(p),
            expected_profile_id: None,
        },
        None,
    )
    .await
    .unwrap();
    monica_core::ingest(&pool, &record("post", "one", "one", json!({"text":"One"})))
        .await
        .unwrap();
    monica_core::ingest(&pool, &record("post", "two", "two", json!({"text":"Two"})))
        .await
        .unwrap();
    let first = claim(&pool).await.unwrap().unwrap();
    let second = claim(&pool).await.unwrap().unwrap();
    let mut tx = pool.begin().await.unwrap();
    domain::project(&mut tx, &first).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), project(&pool, &second))
        .await
        .expect("same account post blocked by unrelated post")
        .unwrap();
    tx.rollback().await.unwrap();
    project(&pool, &first).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.timeline_events WHERE profile_id=$1")
        .bind(p)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2);
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.memory_legacy_pending_records")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 2);
    while monica_core::ai::memory::legacy::drain(&pool).await.unwrap() > 0 {}
    let inputs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind='social'",
    )
    .bind(p)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(inputs, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn durable_receipt_chunk_resume_replay_and_poison_isolation(pool: PgPool) {
    let records=(0..230).map(|i|serde_json::from_value(json!({"external_id":format!("post-{i}"),"source_account_external_id":"fixture-account","entity_type":"post","occurred_at":"2026-01-01T12:00:00Z","content":{"text":format!("Post {i}")}})).unwrap()).collect();
    let b = batch(records);
    let mut tx = pool.begin().await.unwrap();
    let id = imports::receive(&mut tx, &b, "fixture-hash").await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(imports::resume_chunk(&pool, id).await.unwrap(), Some(false));
    let receipt = imports::receipt(&pool, id).await.unwrap();
    assert_eq!(receipt["processed_count"], 100);
    let done = imports::resume(&pool, id).await.unwrap();
    assert_eq!(done["inserted_count"], 230);
    assert_eq!(done["status"], "complete");
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        imports::receive(&mut tx, &b, "fixture-hash").await.unwrap(),
        id
    );
    tx.commit().await.unwrap();
    assert_eq!(
        imports::resume(&pool, id).await.unwrap()["inserted_count"],
        230
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(
        imports::receive(&mut tx, &b, "different-hash")
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let bad=batch(vec![serde_json::from_value(json!({"external_id":"bad","source_account_external_id":"fixture-account","content":{"text":"x"},"occurred_at":"not-a-date"})).unwrap()]);
    let mut tx = pool.begin().await.unwrap();
    let bad_id = imports::receive(&mut tx, &bad, "bad-hash").await.unwrap();
    tx.commit().await.unwrap();
    imports::resume_pending(&pool).await.unwrap();
    let bad = imports::receipt(&pool, bad_id).await.unwrap();
    assert_eq!(bad["failure_count"], 1);
    assert_eq!(bad["processed_count"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn bulk_messages_replay_corrections_and_month_counts(pool: PgPool) {
    let records = (0..150)
        .map(|i| monica_core::messages::Message {
            account: "self".into(),
            conversation: "friend".into(),
            external_id: format!("message-{i:03}"),
            sent_at_ms: 1767268800000 + i,
            sender: if i % 2 == 0 { "self" } else { "friend" }.into(),
            body: format!("Body {i}"),
        })
        .collect::<Vec<_>>();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        imports::append_messages(&mut tx, Provider::Wechat, &records)
            .await
            .unwrap(),
        150
    );
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        imports::append_messages(&mut tx, Provider::Wechat, &records)
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    let conv = domain::conversation_id(Provider::Wechat, "self", "friend").unwrap();
    let counts: (i64, i64) =
        sqlx::query_as("SELECT sent,received FROM crm.message_months WHERE conversation_id=$1")
            .bind(conv)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(counts, (75, 75));
    imports::correct_message(
        &pool,
        Provider::Wechat,
        "self",
        "friend",
        "message-000",
        "delete-1",
        None,
    )
    .await
    .unwrap();
    imports::correct_message(
        &pool,
        Provider::Wechat,
        "self",
        "friend",
        "message-000",
        "delete-1",
        None,
    )
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT message_count FROM crm.conversations WHERE id=$1")
        .bind(conv)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 149);
    imports::correct_message(
        &pool,
        Provider::Wechat,
        "self",
        "friend",
        "message-000",
        "restore-1",
        Some("Corrected"),
    )
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT message_count FROM crm.conversations WHERE id=$1")
        .bind(conv)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 150);
    let mut changed = records[0].clone();
    changed.body = "Conflict".into();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        imports::append_messages(&mut tx, Provider::Wechat, &[changed])
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    imports::checkpoint(&pool, "wechat", "messages", "fixture-file", "hash", 150)
        .await
        .unwrap();
    assert!(
        imports::checkpoint(
            &pool,
            "wechat",
            "messages",
            "fixture-file",
            "different",
            151
        )
        .await
        .is_err()
    );
    assert!(
        imports::checkpoint(&pool, "wechat", "messages", "fixture-file", "hash", 149)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn append_holds_scoped_link_barrier_and_corrections_share_lock_order(pool: PgPool) {
    let records = vec![monica_core::messages::Message {
        account: "self".into(),
        conversation: "friend".into(),
        external_id: "first".into(),
        sent_at_ms: 1767268800000,
        sender: "friend".into(),
        body: "Original".into(),
    }];
    monica_core::messages::append(&pool, Provider::Wechat, &records)
        .await
        .unwrap();
    let account = identity::account_id(Provider::Wechat, "friend").unwrap();
    let first = profile(&pool, "Old").await;
    let second = profile(&pool, "New").await;
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: account,
            profile_id: Some(first),
            expected_profile_id: None,
        },
        None,
    )
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let mut next = records[0].clone();
    next.external_id = "second".into();
    next.body = "Second".into();
    imports::append_messages(&mut tx, Provider::Wechat, &[next.clone()])
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let copy = pool.clone();
    let task = tokio::spawn(async move {
        linking::set_link(
            &copy,
            linking::LinkChange {
                account_id: account,
                profile_id: Some(second),
                expected_profile_id: Some(first),
            },
            None,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND $1=ANY(pg_blocking_pids(pid)) AND query LIKE '%crm.source_accounts%')").bind(blocker_pid).fetch_one(&pool).await.unwrap();
            if waiting {break}
            assert!(!task.is_finished(),"relink returned before reaching the projection lock");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("relink never reached the PostgreSQL source-account lock");
    assert!(
        !task.is_finished(),
        "relink crossed uncommitted message projection"
    );
    tx.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mapped: Uuid =
        sqlx::query_scalar("SELECT profile_id FROM crm.conversations WHERE external_id='friend'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(mapped, second);
    let a = pool.clone();
    let b = pool.clone();
    let (mut next1, mut next2) = (next.clone(), next);
    next1.external_id = "third".into();
    next2.external_id = "fourth".into();
    let append = tokio::spawn(async move {
        monica_core::messages::append(&a, Provider::Wechat, &[next1, next2]).await
    });
    let correct = tokio::spawn(async move {
        imports::correct_message(
            &b,
            Provider::Wechat,
            "self",
            "friend",
            "first",
            "correction",
            Some("Updated"),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        append.await.unwrap().unwrap();
        correct.await.unwrap().unwrap();
    })
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT message_count FROM crm.conversations WHERE external_id='friend'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 4);
}

#[sqlx::test(migrations = "../../migrations")]
async fn immutable_call_batches_do_not_create_revision_jobs(pool: PgPool) {
    let records=(0..100).map(|i|serde_json::from_value(json!({"external_id":format!("call-{i}"),"source_account_external_id":"friend","entity_type":"call","occurred_at":"2026-01-01T12:00:00Z","content":{"direction":"in","duration_seconds":i,"status":"completed","media_type":"voice"}})).unwrap()).collect::<Vec<_>>();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        imports::append_events(&mut tx, Provider::Wechat, "calls", &records)
            .await
            .unwrap(),
        100
    );
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        imports::append_events(&mut tx, Provider::Wechat, "calls", &records)
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    let revisions: i64 = sqlx::query_scalar("SELECT count(*) FROM wechat.revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(revisions, 0);
    let mut changed = records[0].clone();
    changed.content["duration_seconds"] = json!(999);
    let mut tx = pool.begin().await.unwrap();
    assert!(
        imports::append_events(&mut tx, Provider::Wechat, "calls", &[changed])
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.contact_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 100);
}

#[sqlx::test(migrations = "../../migrations")]
async fn empty_origins_retire_reversibly_but_independent_notebook_facts_stay_visible(pool: PgPool) {
    let actor = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic@example.test','unused')").bind(actor).execute(&pool).await.unwrap();
    let target = profile(&pool, "Destination").await;
    let other = profile(&pool, "Independent person").await;
    let media = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) VALUES($1,repeat('a',64),'image/jpeg',1,'synthetic')").bind(media).execute(&pool).await.unwrap();
    for reason in [
        "empty",
        "summary-or-avatar",
        "manual-profile",
        "manual-history",
        "identity",
        "structured-data",
        "dates",
        "activities",
        "faces",
        "media",
        "relationships",
        "groups",
        "tags",
        "posts",
        "conversations",
        "independent-field",
    ] {
        let origin = profile(&pool, reason).await;
        let account = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram',$1::text,'person','Synthetic')").bind(account).execute(&pool).await.unwrap();
        linking::set_link(
            &pool,
            linking::LinkChange {
                account_id: account,
                profile_id: Some(origin),
                expected_profile_id: None,
            },
            None,
        )
        .await
        .unwrap();
        match reason {
            "summary-or-avatar" => {
                sqlx::query("UPDATE crm.profiles SET summary='Independent note' WHERE id=$1")
                    .bind(origin)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "manual-profile" => {
                sqlx::query("INSERT INTO crm.audit_events(action,entity_type,entity_id) VALUES('profile.created','profile',$1)").bind(origin).execute(&pool).await.unwrap();
            }
            "manual-history" | "structured-data" | "independent-field" => {
                let id = Uuid::new_v4();
                let key = if reason == "structured-data" {
                    "employment"
                } else {
                    "note"
                };
                sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation) VALUES($1,$2,$3,'\"Protected\"','synthetic',$4,'test')").bind(id).bind(origin).bind(key).bind(if reason=="manual-history"{"manual"}else{"monica"}).execute(&pool).await.unwrap();
                sqlx::query("INSERT INTO crm.field_current VALUES($1,$2,$3)")
                    .bind(origin)
                    .bind(key)
                    .bind(id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "identity" => {
                sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'email',$1::text,'active')").bind(Uuid::new_v4()).bind(origin).execute(&pool).await.unwrap();
            }
            "dates" => {
                sqlx::query("INSERT INTO crm.profile_dates(id,profile_id,kind,date_precision,month,day) VALUES($1,$2,'birthday','month_day',1,1)").bind(Uuid::new_v4()).bind(origin).execute(&pool).await.unwrap();
            }
            "activities" => {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO crm.activities(id,activity_type,occurred_at,title,source_type,content_hash) VALUES($1,'note',now(),'Protected','manual','synthetic')").bind(id).execute(&pool).await.unwrap();
                sqlx::query("INSERT INTO crm.activity_participants VALUES($1,$2)")
                    .bind(id)
                    .bind(origin)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "faces" => {
                sqlx::query("INSERT INTO crm.faces(id,media_id,box,quality,status,profile_id) VALUES($1,$2,ARRAY[0,0,1,1],1,'confirmed',$3)").bind(Uuid::new_v4()).bind(media).bind(origin).execute(&pool).await.unwrap();
            }
            "media" => {
                sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('profile',$1,$2,'photo')").bind(origin).bind(media).execute(&pool).await.unwrap();
            }
            "relationships" => {
                sqlx::query("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type) VALUES($1,$2,$3,'friend')").bind(Uuid::new_v4()).bind(origin).bind(other).execute(&pool).await.unwrap();
            }
            "groups" => {
                let group = profile(&pool, "Group").await;
                sqlx::query("UPDATE crm.profiles SET profile_type='group' WHERE id=$1")
                    .bind(group)
                    .execute(&pool)
                    .await
                    .unwrap();
                sqlx::query("INSERT INTO crm.group_memberships(group_profile_id,person_profile_id) VALUES($1,$2)").bind(group).bind(origin).execute(&pool).await.unwrap();
            }
            "tags" => {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO crm.tags(id,name) VALUES($1,$1::text)")
                    .bind(id)
                    .execute(&pool)
                    .await
                    .unwrap();
                sqlx::query("INSERT INTO crm.tag_memberships(tag_id,profile_id) VALUES($1,$2)")
                    .bind(id)
                    .bind(origin)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "posts" => {
                let independent = Uuid::new_v4();
                sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat',$1::text,'person','Independent')").bind(independent).execute(&pool).await.unwrap();
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,content,content_hash,version) VALUES($1,'wechat',$1,$2,$3,'post',$1::text,'{}','synthetic',1)").bind(id).bind(independent).bind(origin).execute(&pool).await.unwrap();
            }
            "conversations" => {
                sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,profile_id,conversation_type) VALUES($1,'wechat','self',$1::text,$2,'direct')").bind(Uuid::new_v4()).bind(origin).execute(&pool).await.unwrap();
            }
            _ => {}
        }
        linking::set_link(
            &pool,
            linking::LinkChange {
                account_id: account,
                profile_id: Some(target),
                expected_profile_id: Some(origin),
            },
            None,
        )
        .await
        .unwrap();
        let archived: bool =
            sqlx::query_scalar("SELECT archived_at IS NOT NULL FROM crm.profiles WHERE id=$1")
                .bind(origin)
                .fetch_one(&pool)
                .await
                .unwrap();
        if reason == "empty" {
            assert!(archived);
            assert_eq!(
                linking::resolve_profile(&pool, origin).await.unwrap(),
                target
            );
            assert!(linking::restore(&pool, origin, actor).await.unwrap());
            assert_eq!(
                linking::resolve_profile(&pool, origin).await.unwrap(),
                origin
            );
        } else {
            assert!(!archived, "{reason} was hidden");
            let mut tx = pool.begin().await.unwrap();
            let report = linking::retirement_candidate(&mut tx, origin)
                .await
                .unwrap();
            assert!(
                report["blockers"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(if reason == "structured-data" {
                        "independent-field"
                    } else {
                        reason
                    })),
                "{reason}: {report}"
            );
        }
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_batches_reserve_global_typed_content_in_one_order(pool: PgPool) {
    // Occupy the public hash with a manual string before projecting numeric zero.
    let mut tx = pool.begin().await.unwrap();
    let manual = monica_core::content::register(&mut tx, "field_value", &json!("0"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut records = Vec::new();
    for (account, fields) in [
        ("left", json!({"a":0,"b":"second","c":"0"})),
        ("right", json!({"a":"second","b":0,"c":"0"})),
    ] {
        let mut r = record("profile", "profile", "1", json!({"fields":fields}));
        r.account = account.into();
        monica_core::ingest(&pool, &r).await.unwrap();
        records.push(r);
    }
    let first = claim(&pool).await.unwrap().unwrap();
    let second = claim(&pool).await.unwrap().unwrap();
    let (a, b) = tokio::join!(project(&pool, &first), project(&pool, &second));
    a.unwrap();
    b.unwrap();
    let hashes: Vec<(Value,String,Uuid)> = sqlx::query_as("SELECT value,content_hash,content_object_id FROM crm.source_fields ORDER BY account_id,field_key")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(hashes.len(), 6);
    for (value, hash, id) in &hashes {
        if value == &json!("0") {
            assert_eq!(hash, &manual.hash);
            assert_eq!(id, &manual.id);
        }
        if value == &json!(0) {
            assert_ne!(hash, &manual.hash);
            assert_ne!(id, &manual.id);
        }
    }
    for r in records {
        let p = profile(&pool, &r.account).await;
        linking::set_link(
            &pool,
            linking::LinkChange {
                account_id: identity::account_id(r.provider, &r.account).unwrap(),
                profile_id: Some(p),
                expected_profile_id: None,
            },
            None,
        )
        .await
        .unwrap();
    }
    let inconsistent:i64=sqlx::query_scalar("SELECT count(*) FROM crm.field_revisions r JOIN crm.source_fields f ON f.account_id=r.source_account_id AND f.field_key=r.field_key WHERE r.content_object_id IS DISTINCT FROM f.content_object_id OR r.content_hash<>f.content_hash")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(inconsistent, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn typed_source_facts_reorder_edit_relink_and_generic_fields_stay_distinct(pool: PgPool) {
    let p1 = profile(&pool, "Typed facts").await;
    let p2 = profile(&pool, "Moved facts").await;
    let original = json!({"company":"Synthetic Company","start_date":"2020-01","title":"Engineer"});
    let other = json!({"company":"Synthetic Company","start_date":"2020-01","title":"Reviewer"});
    let mut r = record(
        "profile",
        "profile",
        "1",
        json!({"experience":[original.clone(),other.clone()],"education":[{"school":"Synthetic School","degree":"MS"}],"address":{"city":"Chicago"}}),
    );
    monica_core::ingest(&pool, &r).await.unwrap();
    drain(&pool).await;
    let account = identity::account_id(r.provider, &r.account).unwrap();
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: account,
            profile_id: Some(p1),
            expected_profile_id: None,
        },
        None,
    )
    .await
    .unwrap();
    let keys:Vec<(String,Value)>=sqlx::query_as("SELECT fact_key,value FROM crm.profile_structured_current WHERE profile_id=$1 AND fact_type='employment' ORDER BY fact_key").bind(p1).fetch_all(&pool).await.unwrap();
    assert_eq!(keys.len(), 2);
    r.event = "2".into();
    r.payload = json!({"experience":[other.clone(),original.clone()]});
    monica_core::ingest(&pool, &r).await.unwrap();
    drain(&pool).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.source_structured_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 4);
    let mut edited = original.clone();
    edited["description"] = json!("Expanded scope");
    r.event = "3".into();
    r.payload = json!({"experience":[other,edited.clone()]});
    monica_core::ingest(&pool, &r).await.unwrap();
    drain(&pool).await;
    let key: String =
        sqlx::query_scalar("SELECT fact_key FROM crm.profile_structured_current WHERE value=$1")
            .bind(&edited)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(key, keys.iter().find(|(_, v)| *v == original).unwrap().0);
    r.event = "4".into();
    r.payload = json!({"fields":{"linkedin.experience":[{"company":"Must not become entity"}]}});
    monica_core::ingest(&pool, &r).await.unwrap();
    drain(&pool).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.source_structured_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 5);
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: account,
            profile_id: Some(p2),
            expected_profile_id: Some(p1),
        },
        None,
    )
    .await
    .unwrap();
    let old: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.profile_structured_current WHERE profile_id=$1",
    )
    .bind(p1)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(old, 0);
    let new: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.profile_structured_current WHERE profile_id=$1",
    )
    .bind(p2)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(new, 4);
    let history: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.structured_revisions WHERE profile_id=$1")
            .bind(p2)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(history, 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unowned_platform_facts_block_moves_and_quarantine_blocks_detach(pool: PgPool) {
    let target = profile(&pool, "Target").await;
    for reason in [
        "quarantined",
        "unowned_media",
        "unowned_tags",
        "unowned_relationships",
    ] {
        let origin = profile(&pool, reason).await;
        let account = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram',$1::text,'person','Synthetic')").bind(account).execute(&pool).await.unwrap();
        linking::set_link(
            &pool,
            linking::LinkChange {
                account_id: account,
                profile_id: Some(origin),
                expected_profile_id: None,
            },
            None,
        )
        .await
        .unwrap();
        match reason {
            "quarantined" => {
                sqlx::query("INSERT INTO crm.identities(id,source_account_id,profile_id,provider,external_id,status) VALUES($1,$1,$2,'instagram',$1::text,'invalid') ON CONFLICT(id) DO UPDATE SET status='invalid'").bind(account).bind(origin).execute(&pool).await.unwrap();
            }
            "unowned_media" => {
                let media = Uuid::new_v4();
                sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) VALUES($1,md5($1::text)||md5($1::text),'image/jpeg',1,'synthetic')").bind(media).execute(&pool).await.unwrap();
                sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('profile',$1,$2,'instagram_avatar')").bind(origin).bind(media).execute(&pool).await.unwrap();
            }
            "unowned_tags" => {
                let tag = Uuid::new_v4();
                sqlx::query("INSERT INTO crm.tags(id,name) VALUES($1,$1::text)")
                    .bind(tag)
                    .execute(&pool)
                    .await
                    .unwrap();
                sqlx::query("INSERT INTO crm.tag_memberships(tag_id,profile_id,source_type) VALUES($1,$2,'instagram')").bind(tag).bind(origin).execute(&pool).await.unwrap();
            }
            _ => {
                sqlx::query("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type,source_type) VALUES($1,$2,$3,'friend','instagram')").bind(Uuid::new_v4()).bind(origin).bind(target).execute(&pool).await.unwrap();
            }
        }
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            linking::link_blockers(&mut tx, &[account]).await.unwrap()[&account],
            reason
        );
        tx.rollback().await.unwrap();
        assert!(
            linking::set_link(
                &pool,
                linking::LinkChange {
                    account_id: account,
                    profile_id: Some(target),
                    expected_profile_id: Some(origin)
                },
                None
            )
            .await
            .is_err()
        );
        let detached = linking::set_link(
            &pool,
            linking::LinkChange {
                account_id: account,
                profile_id: None,
                expected_profile_id: Some(origin),
            },
            None,
        )
        .await;
        assert_eq!(detached.is_err(), reason == "quarantined");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn organization_identity_normalizes_case_whitespace_and_keeps_latest_name(pool: PgPool) {
    for (account, name) in [
        ("first", "Synthetic Company"),
        ("second", "  SYNTHETIC   company "),
    ] {
        let mut r = record(
            "employment",
            "source-role",
            "1",
            json!({"company":name,"title":"Engineer"}),
        );
        r.account = account.into();
        monica_core::ingest(&pool, &r).await.unwrap();
        drain(&pool).await;
        let p = profile(&pool, account).await;
        linking::set_link(
            &pool,
            linking::LinkChange {
                account_id: identity::account_id(r.provider, &r.account).unwrap(),
                profile_id: Some(p),
                expected_profile_id: None,
            },
            None,
        )
        .await
        .unwrap();
    }
    let companies: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id,normalized_name,name FROM crm.organizations WHERE kind='company'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(companies.len(), 1);
    assert_eq!(companies[0].1, "synthetic company");
    assert_eq!(companies[0].2, "SYNTHETIC   company");
    let facts: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT company_id,company_name FROM crm.profile_structured_current")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(facts.len(), 2);
    assert!(
        facts
            .iter()
            .all(|(id, name)| *id == companies[0].0 && *name == companies[0].2)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn notebook_chunks_keep_native_revisions_cursor_and_memory_atomic(pool: PgPool) {
    let records:Vec<Value>=(0..101).map(|i|json!({"external_id":format!("native-{i}"),"content":{"full_name":format!("Native {i}"),"fields":{"bio":"Notebook field"},"experience":[{"company":"Shared company","title":"Engineer"}]}})).collect();
    let b:imports::Batch=serde_json::from_value(json!({"source":"monica","stream":"legacy","batch_id":"native","idempotency_key":"native","observed_at":"2026-01-01T00:00:00Z","cursor_after":"done","records":records})).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let id = imports::receive(&mut tx, &b, "native-hash").await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(imports::resume_chunk(&pool, id).await.unwrap(), Some(false));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.notebook_objects")
            .fetch_one(&pool)
            .await
            .unwrap(),
        100
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.source_cursors WHERE source='monica'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    let done = imports::resume(&pool, id).await.unwrap();
    assert_eq!(done["inserted_count"], 101);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT cursor FROM crm.source_cursors WHERE source='monica'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "done"
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.profile_structured_current WHERE current_source_type='monica' AND source_account_id IS NULL").fetch_one(&pool).await.unwrap(),101);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.memory_inputs WHERE source_kind='profile'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        101
    );
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='monica')"
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.memory_inputs WHERE source_kind='profile' AND payload->'fields'->>'bio'='Notebook field'").fetch_one(&pool).await.unwrap(),101);
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        imports::receive(&mut tx, &b, "native-hash").await.unwrap(),
        id
    );
    tx.commit().await.unwrap();
    imports::resume(&pool, id).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.notebook_revisions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        101
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.field_revisions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        101
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn notebook_owner_corrections_move_memory_and_keep_message_direction(pool: PgPool) {
    let first = profile(&pool, "First notebook owner").await;
    let second = profile(&pool, "Second notebook owner").await;
    for (iteration, owner) in [Some(first), None, Some(second)].into_iter().enumerate() {
        let records = json!([
            {"external_id":"post-owner","entity_type":"post","profile_id":owner,"content":{"caption":"Persistent post"}},
            {"external_id":"message-owner","entity_type":"message","profile_id":owner,"sender_profile_id":owner,"conversation_external_id":"native-thread","occurred_at":"2026-01-01T00:00:00Z","content":{"text":format!("Message {iteration}")}}
        ]);
        let b:imports::Batch=serde_json::from_value(json!({"source":"monica","stream":"custom","batch_id":format!("owner-{iteration}"),"idempotency_key":format!("owner-{iteration}"),"observed_at":"2026-01-01T00:00:00Z","records":records})).unwrap();
        let mut tx = pool.begin().await.unwrap();
        let id = imports::receive(&mut tx, &b, &format!("hash-{iteration}"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        imports::resume(&pool, id).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT direction FROM crm.message_search WHERE provider='monica'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "received",
            "omitted owners preserve the existing sender and direction"
        );
    }
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind IN ('chat','social')").bind(first).fetch_one(&pool).await.unwrap(),0);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind IN ('chat','social')").bind(second).fetch_one(&pool).await.unwrap(),2);
}

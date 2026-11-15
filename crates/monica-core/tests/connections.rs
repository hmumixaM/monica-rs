use monica_core::{Provider, connections::*};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
async fn connection(pool: &PgPool, provider: Provider, account: &str) -> Uuid {
    let id = connection_id(provider, account);
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,$2,$3,$3,'local',true,'connected')").bind(id).bind(provider.schema()).bind(account).execute(pool).await.unwrap();
    id
}
async fn page(pool: &PgPool, provider: Provider, cid: Uuid, records: &[Value]) -> u64 {
    let mut tx = pool.begin().await.unwrap();
    let n = commit_page(
        &mut tx,
        provider,
        cid,
        "messages",
        records,
        &json!({"position":records.len()}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    n
}
fn message() -> Value {
    json!({"external_id":"message-1","kind":"message","thread_id":"thread-1","occurred_at":"2026-01-02T03:00:00Z","body":"hello","participants":[{"id":"me","role":"sender","is_self":1},{"id":"friend","role":"recipient","emails":["friend@example.test"]}]})
}

#[sqlx::test(migrations = "../../migrations")]
async fn import_owner_presence_preserves_explicit_annotations_and_group_authors(pool: PgPool) {
    use monica_core::imports;
    use std::collections::BTreeMap;
    async fn import(pool: &PgPool, cid: Uuid, tag: &str, records: Vec<Value>) -> Uuid {
        let input = json!({"source":"linkedin","connection_id":cid,"stream":"messages","batch_id":tag,"idempotency_key":tag,"observed_at":"2026-01-01T00:00:00Z","records":records});
        let hash = monica_core::identity::observation_hash(&input);
        let batch: imports::Batch = serde_json::from_value(input).unwrap();
        let mut tx = pool.begin().await.unwrap();
        let receipt = imports::receive(&mut tx, &batch, &hash).await.unwrap();
        tx.commit().await.unwrap();
        imports::resume(pool, receipt).await.unwrap();
        deliver(pool, Provider::Linkedin, cid, 100).await.unwrap();
        while relink_pending(pool, 100).await.unwrap() > 0 {}
        receipt
    }
    async fn attribution(
        pool: &PgPool,
        cid: Uuid,
        external: &str,
    ) -> (Option<Uuid>, Option<Uuid>, String, String) {
        sqlx::query_as("SELECT c.profile_id,m.sender_profile_id,c.conversation_type,m.direction FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.connection_id=$1 AND m.external_id=$2").bind(cid).bind(external).fetch_one(pool).await.unwrap()
    }
    let cid = connection(&pool, Provider::Linkedin, "owner-presence").await;
    let known = Uuid::new_v4();
    let explicit = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Resolved contact'),($2,'person','Explicit owner')").bind(known).bind(explicit).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.connection_identity_links(connection_id,external_id,profile_id,confirmed,blocked) VALUES($1,'peer',$2,true,false),($1,'blocked',NULL,true,true)").bind(cid).bind(known).execute(&pool).await.unwrap();
    let cases = [
        ("missing", json!({}), Some(known), Some(known)),
        (
            "explicit",
            json!({"profile_id":explicit,"sender_profile_id":explicit}),
            Some(explicit),
            Some(explicit),
        ),
        (
            "unassigned",
            json!({"profile_id":null,"sender_profile_id":null}),
            None,
            None,
        ),
        (
            "owner-only",
            json!({"profile_id":explicit}),
            Some(explicit),
            Some(known),
        ),
        (
            "author-only",
            json!({"sender_profile_id":explicit}),
            Some(known),
            Some(explicit),
        ),
        (
            "unassigned-owner",
            json!({"profile_id":null}),
            None,
            Some(known),
        ),
        (
            "unassigned-author",
            json!({"sender_profile_id":null}),
            Some(known),
            None,
        ),
        ("group", json!({}), None, Some(known)),
        ("blocked", json!({}), None, None),
    ];
    let expected: BTreeMap<_, _> = cases
        .iter()
        .map(|(name, _, owner, author)| (*name, (*owner, *author)))
        .collect();
    let mut records:Vec<Value>=cases.iter().map(|(name,extra,_,_)|{
        let mut record=json!({"external_id":name,"entity_type":"message","conversation_external_id":format!("thread-{name}"),"conversation_type":if *name=="group"{"group"}else{"direct"},"occurred_at":"2026-01-01T00:00:00Z","content":{"body":format!("Original {name}"),"direction":"received","participants":[{"id":if *name=="blocked"{"blocked"}else{"peer"},"role":"sender"},{"id":"self","role":"participant","is_self":true}]}});
        record.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        if *name=="missing" { record["content"].as_object_mut().unwrap().remove("direction"); }
        record
    }).collect();
    // The first batch uses the bounded set projection, revised messages use the
    // ordered path. Both must apply the same independent owner/author policy.
    for revision in 1..=2 {
        if revision == 2 {
            for record in &mut records {
                record["content"]["body"] = json!(format!("Revised {}", record["external_id"]));
            }
        }
        import(&pool, cid, &format!("presence-{revision}"), records.clone()).await;
        for (name, (owner, author)) in &expected {
            let row = attribution(&pool, cid, name).await;
            assert_eq!(
                (row.0, row.1),
                (*owner, *author),
                "{name} revision{revision}"
            );
            assert_eq!(row.2, if *name == "group" { "group" } else { "direct" });
            assert_eq!(
                row.3, "received",
                "source direction wins; omitted direction follows resolved attribution"
            );
        }
    }
    let mut unassign = records[0].clone();
    unassign["profile_id"] = Value::Null;
    unassign["sender_profile_id"] = Value::Null;
    import(&pool, cid, "explicit-unassign", vec![unassign]).await;
    let row = attribution(&pool, cid, "missing").await;
    assert_eq!((row.0, row.1), (None, None));
    import(&pool, cid, "same-version-omitted", vec![records[0].clone()]).await;
    let row = attribution(&pool, cid, "missing").await;
    assert_eq!(
        (row.0, row.1),
        (None, None),
        "absence cannot erase an explicit unassignment on the same source revision"
    );
    let mut next = records[0].clone();
    next["content"]["body"] = json!("A new source revision");
    let receipt = import(&pool, cid, "new-revision", vec![next.clone()]).await;
    assert_eq!(
        import(&pool, cid, "new-revision", vec![next]).await,
        receipt
    );
    let row = attribution(&pool, cid, "missing").await;
    assert_eq!((row.0, row.1), (Some(known), Some(known)));
    let old:(Option<Uuid>,Option<Uuid>,bool,bool)=sqlx::query_as("SELECT profile_id,sender_profile_id,infer_profile,infer_sender FROM crm.connection_import_bindings WHERE connection_id=$1 AND external_id='missing' AND revision=2").bind(cid).fetch_one(&pool).await.unwrap();
    assert_eq!(
        old,
        (None, None, false, false),
        "old revision annotation remains intact"
    );
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM crm.connection_evidence),(SELECT count(*) FROM linkedin.connection_revision_refs),(SELECT count(*) FROM crm.profiles)").fetch_one(&pool).await.unwrap();
    assert_eq!(
        counts,
        (9, 19, 2),
        "replay keeps stable records and never creates duplicate people"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn journal_replay_correction_retraction_cursor_and_isolation(pool: PgPool) {
    let p = Provider::AppleMessages;
    let a = connection(&pool, p, "one").await;
    let b = connection(&pool, p, "two").await;
    let mut value = message();
    assert_eq!(page(&pool, p, a, &[value.clone()]).await, 1);
    assert_eq!(page(&pool, p, a, &[value.clone()]).await, 0);
    assert_eq!(page(&pool, p, b, &[value.clone()]).await, 1);
    assert_eq!(deliver(&pool, p, a, 100).await.unwrap(), 1);
    assert_eq!(deliver(&pool, p, a, 100).await.unwrap(), 0);
    deliver(&pool, p, b, 100).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.message_search")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    let own: Value = sqlx::query_scalar("SELECT metadata FROM crm.connection_evidence WHERE id=$1")
        .bind(evidence_id(a, "message-1"))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(own["direction"], "sent");
    assert_eq!(own["participants"][0]["is_self"], true);
    assert!(own.get("body").is_none());
    value["body"] = json!("corrected");
    value["occurred_at"] = json!("2026-02-01T05:00:00Z");
    value["thread_id"] = json!("new-thread");
    page(&pool, p, a, &[value.clone()]).await;
    deliver(&pool, p, a, 100).await.unwrap();
    let state: (i64, String) =
        sqlx::query_as("SELECT count(*),max(body) FROM crm.message_search WHERE account=$1")
            .bind(a.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, (1, "corrected".into()));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT sum(message_count)::bigint FROM crm.conversations WHERE connection_id=$1"
        )
        .bind(a)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    value["included"] = json!(0);
    page(&pool, p, a, &[value.clone()]).await;
    deliver(&pool, p, a, 100).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.message_search WHERE account=$1")
            .bind(a.to_string())
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    page(&pool, p, a, &[message()]).await;
    deliver(&pool, p, a, 100).await.unwrap();
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM apple_messages.connection_revision_refs WHERE connection_id=$1),(SELECT count(*) FROM apple_messages.connection_payloads)").bind(a).fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (4, 3));
    let mut tx = pool.begin().await.unwrap();
    commit_page(
        &mut tx,
        p,
        a,
        "rollback",
        &[json!({"external_id":"rollback"})],
        &json!({"page":2}),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(cursor(&mut tx, p, a, "rollback").await.unwrap(), json!({}));
    tx.rollback().await.unwrap();
}
#[sqlx::test(migrations = "../../migrations")]
async fn contact_shared_identifier_no_merge_deletion_and_relink(pool: PgPool) {
    let contacts = connection(&pool, Provider::AppleContacts, "contacts").await;
    let messages = connection(&pool, Provider::Mail, "me@example.test").await;
    let contact = |id: &str| json!({"external_id":id,"kind":"contact","emails":["shared@example.test"],"participants":[{"id":id,"name":id,"emails":["shared@example.test"]}]});
    page(
        &pool,
        Provider::AppleContacts,
        contacts,
        &[contact("a"), contact("b")],
    )
    .await;
    assert_eq!(
        deliver(&pool, Provider::AppleContacts, contacts, 100)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.profiles")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    let msg = json!({"external_id":"mail","occurred_at":"2026-01-01T00:00:00Z","participants":[{"id":"shared@example.test","role":"sender","emails":["SHARED@example.test"]},{"id":"me@example.test","role":"recipient","emails":["me@example.test"]}],"body":"hello"});
    page(&pool, Provider::Mail, messages, &[msg]).await;
    deliver(&pool, Provider::Mail, messages, 100).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.connection_identity_candidates WHERE connection_id=$1"
        )
        .bind(messages)
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.connection_evidence_people WHERE evidence_id=$1"
        )
        .bind(evidence_id(messages, "mail"))
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    let mut deleted = contact("b");
    deleted["deleted"] = json!(true);
    page(&pool, Provider::AppleContacts, contacts, &[deleted]).await;
    deliver(&pool, Provider::AppleContacts, contacts, 100)
        .await
        .unwrap();
    relink_pending(&pool, 100).await.unwrap();
    let resolved:Option<Uuid>=sqlx::query_scalar("SELECT resolved_profile_id FROM crm.connection_people WHERE connection_id=$1 AND external_id='shared@example.test'").bind(messages).fetch_one(&pool).await.unwrap();
    assert_eq!(resolved, Some(Uuid::new_v5(&contacts, b"contact:a")));
    let info:(String,String)=sqlx::query_as("SELECT c.conversation_type,m.direction FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE m.message_id=$1").bind(evidence_id(messages,"mail")).fetch_one(&pool).await.unwrap();
    assert_eq!(info, ("direct".into(), "received".into()));
}
#[sqlx::test(migrations = "../../migrations")]
async fn fenced_commit_rejects_stale_worker_and_projection_ack_is_atomic(pool: PgPool) {
    let p = Provider::Whatsapp;
    let cid = connection(&pool, p, "fixture").await;
    let owner = Uuid::new_v4();
    sqlx::query("UPDATE crm.source_connections SET lease_owner=$2,lease_until=now()+interval '1 minute' WHERE id=$1").bind(cid).bind(owner).execute(&pool).await.unwrap();
    let fence = Fence {
        connection_id: cid,
        owner,
        generation: 1,
    };
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        commit_page_fenced(&mut tx, p, &fence, "messages", &[message()], &json!({}))
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    sqlx::query("UPDATE crm.source_connections SET generation=generation+1 WHERE id=$1")
        .bind(cid)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        commit_page_fenced(&mut tx, p, &fence, "messages", &[], &json!({"stale":true}))
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let rows = pending(&mut tx, p, cid, 100).await.unwrap();
    project_revision(&mut tx, p, &rows[0], false).await.unwrap();
    acknowledge(&mut tx, p, cid, &[rows[0].sequence])
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(deliver(&pool, p, cid, 100).await.unwrap(), 1);
    assert_eq!(deliver(&pool, p, cid, 100).await.unwrap(), 0);
}
#[test]
fn normalization_preserves_aliases_and_unicode_casefold() {
    assert_eq!(
        normalize_identifier("email", "  Straße+Tag@EXAMPLE.test "),
        Some("strasse+tag@example.test".into())
    );
    assert_eq!(
        normalize_identifier("email", "ﬃ@bücher.example"),
        Some("ffi@xn--bcher-kva.example".into())
    );
    assert_eq!(normalize_identifier("phone", "(555) 123-4567"), None);
    assert_eq!(
        normalize_identifier("phone", "+1 (555) 123-4567"),
        Some("+15551234567".into())
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn native_snapshot_pages_replay_and_bounded_missing_contacts(pool: PgPool) {
    let p = Provider::AppleContacts;
    let cid = connection(&pool, p, "native").await;
    let contact =
        |id: &str| json!({"external_id":id,"kind":"contact","participants":[{"id":id,"name":id}]});
    page(&pool, p, cid, &[contact("a"), contact("b"), contact("c")]).await;
    deliver(&pool, p, cid, 100).await.unwrap();
    let sid = Uuid::new_v4();
    let first = NativeSnapshot {
        snapshot_id: sid,
        reset: true,
        snapshot_complete: false,
    };
    let last = NativeSnapshot {
        snapshot_id: sid,
        reset: false,
        snapshot_complete: true,
    };
    for _ in 0..2 {
        let mut tx = pool.begin().await.unwrap();
        commit_native_page(
            &mut tx,
            p,
            cid,
            "contacts",
            &[contact("a")],
            &json!({"chunk":1}),
            Some(&first),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(finish_native_snapshots(&pool, 1).await.unwrap(), 0);
    let mut tx = pool.begin().await.unwrap();
    commit_native_page(
        &mut tx,
        p,
        cid,
        "contacts",
        &[contact("d")],
        &json!({"token":"next"}),
        Some(&last),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(finish_native_snapshots(&pool, 1).await.unwrap(), 0);
    let mut tx = pool.begin().await.unwrap();
    commit_native_page(
        &mut tx,
        p,
        cid,
        "contacts",
        &[contact("d")],
        &json!({"token":"next"}),
        Some(&last),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(finish_native_snapshots(&pool, 1).await.unwrap(), 1);
    assert_eq!(finish_native_snapshots(&pool, 1).await.unwrap(), 1);
    assert_eq!(finish_native_snapshots(&pool, 1).await.unwrap(), 1);
    assert_eq!(finish_native_snapshots(&pool, 1).await.unwrap(), 0);
    deliver(&pool, p, cid, 100).await.unwrap();
    let visible:Vec<String>=sqlx::query_scalar("SELECT external_id FROM crm.connection_evidence WHERE connection_id=$1 AND NOT deleted ORDER BY external_id").bind(cid).fetch_all(&pool).await.unwrap();
    assert_eq!(visible, vec!["a", "d"]);
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        commit_native_page(
            &mut tx,
            p,
            cid,
            "contacts",
            &[contact("a")],
            &json!({"stale":"cursor"}),
            Some(&first)
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        cursor(&mut tx, p, cid, "contacts").await.unwrap(),
        json!({"token":"next"})
    );
    tx.rollback().await.unwrap();
    let replacement = NativeSnapshot {
        snapshot_id: Uuid::new_v4(),
        reset: true,
        snapshot_complete: true,
    };
    let mut tx = pool.begin().await.unwrap();
    commit_native_page(
        &mut tx,
        p,
        cid,
        "contacts",
        &[],
        &json!({"empty":true}),
        Some(&replacement),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    while finish_native_snapshots(&pool, 1).await.unwrap() > 0 {}
    deliver(&pool, p, cid, 100).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.connection_evidence WHERE connection_id=$1 AND NOT deleted"
        )
        .bind(cid)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn page_preserves_duplicate_external_id_order_and_bulk_counters(pool: PgPool) {
    let p = Provider::AppleMessages;
    let cid = connection(&pool, p, "bulk").await;
    let mut a = message();
    let mut b = a.clone();
    b["body"] = json!("changed");
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        commit_page(
            &mut tx,
            p,
            cid,
            "messages",
            &[a.clone(), a.clone(), b, a.clone()],
            &json!({})
        )
        .await
        .unwrap(),
        3
    );
    let rows = pending(&mut tx, p, cid, 100).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.revision).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    tx.commit().await.unwrap();
    deliver(&pool, p, cid, 100).await.unwrap();
    let records: Vec<_> = (0..1000)
        .map(|i| {
            a["external_id"] = json!(format!("bulk-{i}"));
            a["thread_id"] = json!(format!("group-{}", i % 10));
            a.clone()
        })
        .collect();
    assert_eq!(page(&pool, p, cid, &records).await, 1000);
    assert_eq!(deliver(&pool, p, cid, 1000).await.unwrap(), 1000);
    assert_eq!(page(&pool, p, cid, &records).await, 0);
    assert_eq!(deliver(&pool, p, cid, 1000).await.unwrap(), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT sum(message_count)::bigint FROM crm.conversations WHERE connection_id=$1"
        )
        .bind(cid)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1001
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT sum(sent)::bigint FROM crm.message_months_utc")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1001
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn byte_bounded_delivery_allows_one_oversized_record_without_skipping(pool: PgPool) {
    let p = Provider::Mail;
    let cid = connection(&pool, p, "bytes@example.test").await;
    let records: Vec<_> = (0..4)
        .map(|i| {
            let mut v = message();
            v["external_id"] = json!(format!("bytes-{i}"));
            v["body"] = json!("x".repeat(8192));
            v
        })
        .collect();
    page(&pool, p, cid, &records).await;
    let mut tx = pool.begin().await.unwrap();
    let rows = pending_bounded(&mut tx, p, cid, 1000, 1).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].external_id, "bytes-0");
    tx.rollback().await.unwrap();
    for _ in 0..4 {
        assert_eq!(deliver_bounded(&pool, p, cid, 1000, 8192).await.unwrap(), 1)
    }
    assert_eq!(deliver_bounded(&pool, p, cid, 1000, 8192).await.unwrap(), 0);
}

#[test]
fn identifier_normalization_matches_pinned_python_vectors() {
    let cases: Vec<(String, String, Option<String>)> =
        serde_json::from_str(include_str!("../data/connection-identifiers.json")).unwrap();
    for (kind, input, expected) in cases {
        assert_eq!(
            normalize_identifier(&kind, &input),
            expected,
            "{kind} {input:?}"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn native_snapshot_pause_generation_and_same_snapshot_resume(pool: PgPool) {
    let provider = Provider::AppleContacts;
    let cid = connection(&pool, provider, "pause-resume").await;
    let contact =
        |id: &str| json!({"external_id":id,"kind":"contact","participants":[{"id":id,"name":id}]});
    page(&pool, provider, cid, &[contact("keep"), contact("remove")]).await;
    let first = NativeSnapshot {
        snapshot_id: Uuid::new_v4(),
        reset: true,
        snapshot_complete: false,
    };
    let last = NativeSnapshot {
        reset: false,
        snapshot_complete: true,
        ..first
    };
    let mut tx = pool.begin().await.unwrap();
    commit_native_page(
        &mut tx,
        provider,
        cid,
        "contacts",
        &[contact("keep")],
        &json!({}),
        Some(&first),
    )
    .await
    .unwrap();
    commit_native_page(
        &mut tx,
        provider,
        cid,
        "contacts",
        &[],
        &json!({"token":"done"}),
        Some(&last),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE crm.source_connections SET enabled=false,generation=generation+1 WHERE id=$1",
    )
    .bind(cid)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(finish_native_snapshots(&pool, 100).await.unwrap(), 0);
    sqlx::query(
        "UPDATE crm.source_connections SET enabled=true,generation=generation+1 WHERE id=$1",
    )
    .bind(cid)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        finish_native_snapshots(&pool, 100).await.unwrap(),
        0,
        "an old finalizer must be fenced after resume"
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(
        commit_native_page(
            &mut tx,
            provider,
            cid,
            "contacts",
            &[],
            &json!({}),
            Some(&last)
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    commit_native_page(
        &mut tx,
        provider,
        cid,
        "contacts",
        &[],
        &json!({}),
        Some(&first),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(finish_native_snapshots(&pool, 100).await.unwrap(), 1);
    assert_eq!(finish_native_snapshots(&pool, 100).await.unwrap(), 1);
    deliver(&pool, provider, cid, 100).await.unwrap();
    let visible:Vec<String>=sqlx::query_scalar("SELECT external_id FROM crm.connection_evidence WHERE connection_id=$1 AND NOT deleted ORDER BY external_id").bind(cid).fetch_all(&pool).await.unwrap();
    assert_eq!(
        visible,
        vec!["keep"],
        "resume must preserve previously seen IDs even when first chunk replays empty"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn wechat_contact_fields_scoped_posts_and_explicit_unknown_direction(pool: PgPool) {
    let provider = Provider::Wechat;
    let a = connection(&pool, provider, "chatlog-a").await;
    let b = connection(&pool, provider, "chatlog-b").await;
    let contact = json!({"external_id":"contact:friend","kind":"contact","fields":{"alias":"friendly","remark":"Notebook remark","nick_name":"Nickname","description":"Contact biography","big_head_url":"https://example.invalid/avatar.jpg"},"participants":[{"id":"friend","name":"Friend","role":"contact"}]});
    for cid in [a, b] {
        page(&pool, provider, cid, std::slice::from_ref(&contact)).await;
        deliver(&pool, provider, cid, 100).await.unwrap();
    }
    let owner:Uuid=sqlx::query_scalar("SELECT resolved_profile_id FROM crm.connection_people WHERE connection_id=$1 AND external_id='friend'").bind(a).fetch_one(&pool).await.unwrap();
    let fields:Value=sqlx::query_scalar("SELECT jsonb_object_agg(c.field_key,r.value) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1").bind(owner).fetch_one(&pool).await.unwrap();
    assert_eq!(fields["wechat.nickname"], "Nickname");
    assert_eq!(fields["wechat.description"], "Contact biography");
    assert_eq!(
        fields["wechat.avatar_url"],
        "https://example.invalid/avatar.jpg"
    );
    let manual = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation) VALUES($1,$2,'wechat.description','\"Manual\"','manual','manual','edit')").bind(manual).bind(owner).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.field_current SET revision_id=$2 WHERE profile_id=$1 AND field_key='wechat.description'").bind(owner).bind(manual).execute(&pool).await.unwrap();
    let mut changed = contact.clone();
    changed["fields"]["description"] = json!("Changed biography");
    page(&pool, provider, a, &[changed]).await;
    deliver(&pool, provider, a, 100).await.unwrap();
    assert_eq!(sqlx::query_scalar::<_,Value>("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key='wechat.description'").bind(owner).fetch_one(&pool).await.unwrap(),json!("Manual"));
    let post = json!({"external_id":"post:same","kind":"post","body":"Moment text","raw_content":"<TimelineObject/>","occurred_at":"2026-01-01T00:00:00Z","participants":[{"id":"friend","role":"author"}]});
    for cid in [a, b] {
        page(&pool, provider, cid, std::slice::from_ref(&post)).await;
        deliver(&pool, provider, cid, 100).await.unwrap();
    }
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.timeline_events WHERE external_id='post:same' AND connection_id IN ($1,$2)").bind(a).bind(b).fetch_one(&pool).await.unwrap(),2);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind='connection' AND payload->>'_evidence_id'=$2").bind(owner).bind(evidence_id(a,"post:same").to_string()).fetch_one(&pool).await.unwrap(),1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.memory_legacy_current WHERE entity_id=$1"
        )
        .bind(evidence_id(a, "post:same"))
        .fetch_one(&pool)
        .await
        .unwrap(),
        0,
        "connection social evidence must have exactly one Memory representation"
    );
    let mut unknown = message();
    unknown["direction"] = json!("unknown");
    unknown["participants"][0]["is_self"] = json!(false);
    page(&pool, provider, a, &[unknown.clone()]).await;
    deliver(&pool, provider, a, 100).await.unwrap();
    unknown["body"] = json!("Corrected");
    page(&pool, provider, a, &[unknown]).await;
    deliver(&pool, provider, a, 100).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT direction FROM crm.message_search WHERE message_id=$1"
        )
        .bind(evidence_id(a, "message-1"))
        .fetch_one(&pool)
        .await
        .unwrap(),
        "unknown"
    );
}

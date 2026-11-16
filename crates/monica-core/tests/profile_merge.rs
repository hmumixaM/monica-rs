use monica_core::{linking, profile_merge};
use sqlx::PgPool;
use uuid::Uuid;

async fn person(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic person')").bind(id).execute(pool).await.unwrap();
    id
}
async fn request(pool: &PgPool, source: Uuid, target: Uuid) -> profile_merge::Request {
    let mut tx = pool.begin().await.unwrap();
    let p = profile_merge::preview(&mut tx, source, target)
        .await
        .unwrap();
    profile_merge::Request {
        operation_id: Uuid::new_v4(),
        target_id: target,
        source_generation: p["source"]["generation"].as_i64().unwrap(),
        target_generation: p["target"]["generation"].as_i64().unwrap(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn explicit_merge_moves_current_people_but_retains_history_and_is_replayable(pool: PgPool) {
    let source = person(&pool).await;
    let target = person(&pool).await;
    let account = Uuid::new_v4();
    let media = Uuid::new_v4();
    let confirmed = Uuid::new_v4();
    let pending = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram','merge-peer','person','Synthetic')").bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
        .bind(account)
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) VALUES($1,repeat('a',64),'image/jpeg',1,'synthetic')").bind(media).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role,source_account_id) VALUES('profile',$1,$2,'instagram_avatar',$3)").bind(source).bind(media).bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.faces(id,media_id,box,quality,status,profile_id) VALUES($1,$2,ARRAY[0.1,0.1,0.5,0.5],0.9,'confirmed',$3)").bind(confirmed).bind(media).bind(source).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.faces(id,media_id,box,quality,suggested_profile_id) VALUES($1,$2,ARRAY[0.1,0.1,0.5,0.5],0.9,$3)").bind(pending).bind(media).bind(source).execute(&pool).await.unwrap();
    let revision = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation) VALUES($1,$2,'manual.note','\"Keep this note\"',repeat('b',64),'manual','edit')").bind(revision).bind(source).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.field_current VALUES($1,'manual.note',$2)")
        .bind(source)
        .bind(revision)
        .execute(&pool)
        .await
        .unwrap();
    // Ordinary account movement cannot transfer all people detected in photos.
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: account,
            profile_id: Some(target),
            expected_profile_id: Some(source),
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT profile_id FROM crm.faces WHERE id=$1")
            .bind(confirmed)
            .fetch_one(&pool)
            .await
            .unwrap(),
        source
    );
    let second = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'linkedin','second-peer','person','Synthetic')").bind(second).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
        .bind(second)
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    let input = request(&pool, source, target).await;
    let result = profile_merge::apply(&pool, source, &input, None)
        .await
        .unwrap();
    assert_eq!(result["archived"], true);
    assert_eq!(
        profile_merge::apply(&pool, source, &input, None)
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        linking::resolve_profile(&pool, source).await.unwrap(),
        target
    );
    let owners:(Uuid,Uuid)=sqlx::query_as("SELECT (SELECT profile_id FROM crm.faces WHERE id=$1),(SELECT suggested_profile_id FROM crm.faces WHERE id=$2)").bind(confirmed).bind(pending).fetch_one(&pool).await.unwrap();
    assert_eq!(owners, (target, target));
    let field:(Uuid,Uuid)=sqlx::query_as("SELECT c.profile_id,r.profile_id FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE r.id=$1").bind(revision).fetch_one(&pool).await.unwrap();
    assert_eq!(field, (target, source));
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.media_links WHERE entity_type='profile' AND entity_id=$1 AND media_id=$2").bind(target).bind(media).fetch_one(&pool).await.unwrap(),1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.ai_jobs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    // A delayed face result using the old person ID is normalized to the survivor.
    sqlx::query("UPDATE crm.faces SET suggested_profile_id=$2 WHERE id=$1")
        .bind(pending)
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT suggested_profile_id FROM crm.faces WHERE id=$1")
            .bind(pending)
            .fetch_one(&pool)
            .await
            .unwrap(),
        target
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn merge_rejects_manual_conflicts_and_changed_versions_without_partial_writes(pool: PgPool) {
    let source = person(&pool).await;
    let target = person(&pool).await;
    for (id, value) in [(source, "source"), (target, "target")] {
        sqlx::query("UPDATE crm.profiles SET summary=$2 WHERE id=$1")
            .bind(id)
            .bind(value)
            .execute(&pool)
            .await
            .unwrap();
    }
    let input = request(&pool, source, target).await;
    assert!(
        profile_merge::apply(&pool, source, &input, None)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.profile_redirects")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("UPDATE crm.profiles SET summary=NULL,generation=generation+1 WHERE id=$1")
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        profile_merge::apply(&pool, source, &input, None)
            .await
            .is_err()
    );
    let current = request(&pool, source, target).await;
    assert!(
        profile_merge::apply(&pool, source, &current, None)
            .await
            .unwrap()["archived"]
            == true
    );
}

async fn import_notebook(pool: &PgPool, stream: &str, record: serde_json::Value) {
    use monica_core::{identity, imports};
    let batch:imports::Batch=serde_json::from_value(serde_json::json!({"source":"monica","stream":stream,"batch_id":Uuid::new_v4(),"idempotency_key":Uuid::new_v4(),"observed_at":"2026-11-15T12:00:00Z","records":[record]})).unwrap();
    let hash = identity::observation_hash(&serde_json::to_value(&batch).unwrap());
    let mut tx = pool.begin().await.unwrap();
    let id = imports::receive(&mut tx, &batch, &hash).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        imports::resume(pool, id).await.unwrap()["status"],
        "complete"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn merged_identity_survives_changed_imports_names_and_manual_suppression(pool: PgPool) {
    use serde_json::json;
    let source = person(&pool).await;
    let target = person(&pool).await;
    for (id, name) in [(source, "Earlier name"), (target, "Chosen name")] {
        let revision = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation) VALUES($1,$2,'display_name',$3,repeat('c',64),'manual','edit')").bind(revision).bind(id).bind(json!(name)).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.field_current VALUES($1,'display_name',$2)")
            .bind(id)
            .bind(revision)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE crm.profiles SET display_name=$2 WHERE id=$1")
            .bind(id)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
    }
    import_notebook(&pool,"profiles",json!({"external_id":"stable-notebook","profile_id":source,"content":{"fields":{"monica.note":"before"}}})).await;
    sqlx::query("INSERT INTO crm.ai_reconnect_decisions(profile_id,action,until_at) VALUES($1,'snooze',now()+interval '30 days')").bind(source).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profile_suggestion_decisions(profile_id,fingerprint,kind,decision) VALUES($1,'synthetic-fingerprint','job','ignored')").bind(source).execute(&pool).await.unwrap();
    let input = request(&pool, source, target).await;
    profile_merge::apply(&pool, source, &input, None)
        .await
        .unwrap();
    import_notebook(&pool,"profiles",json!({"external_id":"stable-notebook","profile_id":source,"content":{"fields":{"monica.note":"after"}}})).await;
    import_notebook(&pool,"messages",json!({"external_id":"after-merge-message","profile_id":source,"sender_profile_id":source,"occurred_at":"2026-11-15T13:00:00Z","content":{"body":"New message","conversation_id":"stable-thread"}})).await;
    let note:serde_json::Value=sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key='monica.note'").bind(target).fetch_one(&pool).await.unwrap();
    assert_eq!(note, "after");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.field_current WHERE profile_id=$1")
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT display_name FROM crm.profiles WHERE id=$1")
            .bind(target)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "Chosen name"
    );
    let message:(Uuid,Uuid,String)=sqlx::query_as("SELECT c.profile_id,m.sender_profile_id,m.direction FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id").fetch_one(&pool).await.unwrap();
    assert_eq!(message, (target, target, "received".into()));
    assert!(sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM crm.ai_reconnect_decisions WHERE profile_id=$1 AND until_at>now()) AND EXISTS(SELECT 1 FROM crm.profile_suggestion_decisions WHERE profile_id=$1 AND fingerprint='synthetic-fingerprint' AND decision='ignored')").bind(target).fetch_one(&pool).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.ai_jobs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn merge_blocks_different_manual_group_and_reconnect_decisions(pool: PgPool) {
    let source = person(&pool).await;
    let target = person(&pool).await;
    let group = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'group','Synthetic group')").bind(group).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.group_memberships(group_profile_id,person_profile_id,role,left_at) VALUES($1,$2,'owner',NULL),($1,$3,'member','2020-01-01')").bind(group).bind(source).bind(target).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.ai_reconnect_decisions(profile_id,action) VALUES($1,'dismiss'),($2,'contacted')").bind(source).bind(target).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let preview = profile_merge::preview(&mut tx, source, target)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(
        preview["conflicts"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("group_memberships"))
    );
    assert!(
        preview["conflicts"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("reconnect_decisions"))
    );
    let input = request(&pool, source, target).await;
    assert!(
        profile_merge::apply(&pool, source, &input, None)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.group_memberships")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.profile_redirects")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_relationship_reappears_after_account_correction_following_merge(pool: PgPool) {
    let source = person(&pool).await;
    let target = person(&pool).await;
    let corrected = person(&pool).await;
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let edge = Uuid::new_v4();
    for (id, person) in [(a, source), (b, target)] {
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram',$1::text,'person','Synthetic')").bind(id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
            .bind(id)
            .bind(person)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type,source_type,legacy_source_owned) VALUES($1,$2,$3,'friend','instagram',true)").bind(edge).bind(source).bind(target).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.legacy_relationship_owners VALUES($1,$2,$3)")
        .bind(edge)
        .bind(a)
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    let input = request(&pool, source, target).await;
    profile_merge::apply(&pool, source, &input, None)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.presented_relationship_edges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    linking::set_link(
        &pool,
        linking::LinkChange {
            account_id: a,
            expected_profile_id: Some(target),
            profile_id: Some(corrected),
        },
        None,
    )
    .await
    .unwrap();
    let pair: (Uuid, Uuid) = sqlx::query_as(
        "SELECT from_profile_id,to_profile_id FROM crm.presented_relationship_edges WHERE id=$1",
    )
    .bind(edge)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pair, (corrected, target));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT evidence_kind FROM crm.relationship_edges WHERE id=$1"
        )
        .bind(edge)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "relationship"
    );
}

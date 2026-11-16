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

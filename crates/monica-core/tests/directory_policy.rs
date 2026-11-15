use sqlx::PgPool;
use uuid::Uuid;

async fn official(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic directory person')")
        .bind(id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'wechat','gh_synthetic_directory','active')")
        .bind(Uuid::new_v4()).bind(id).execute(pool).await.unwrap();
    id
}
async fn visible(pool: &PgPool, id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.directory_profiles WHERE id=$1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn directory_projection_reads_own_changes_and_rolls_back_with_facts(pool: PgPool) {
    let id = official(&pool).await;
    assert!(!visible(&pool, id).await);
    let version: Uuid = sqlx::query_scalar(
        "SELECT version FROM crm.cache_versions WHERE domain='profile' AND subject=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'linkedin','synthetic-legitimate','active')")
        .bind(Uuid::new_v4()).bind(id).execute(&mut *tx).await.unwrap();
    let own: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.directory_profiles WHERE id=$1)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert!(
        own,
        "a writer must see its new legitimate source before deferred publication"
    );
    tx.rollback().await.unwrap();
    assert!(!visible(&pool, id).await);
    let after: Uuid = sqlx::query_scalar(
        "SELECT version FROM crm.cache_versions WHERE domain='profile' AND subject=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        version, after,
        "rolled back facts must not publish a new cache generation"
    );

    sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'linkedin','synthetic-legitimate','active')")
        .bind(Uuid::new_v4()).bind(id).execute(&pool).await.unwrap();
    assert!(visible(&pool, id).await);
    sqlx::query("UPDATE crm.identities SET status='invalid' WHERE provider='linkedin'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(!visible(&pool, id).await);
    sqlx::query("UPDATE crm.profiles SET archived_at=now() WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE crm.profiles SET archived_at=NULL WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !visible(&pool, id).await,
        "unarchiving must recompute the official-only decision"
    );
    // Source kind and native provider changes also affect eligibility, even
    // though they do not edit a notebook profile or identity row.
    let account = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'linkedin','synthetic-kind','person','Synthetic source')").bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
        .bind(account)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(visible(&pool, id).await);
    sqlx::query("UPDATE crm.source_accounts SET object_kind='group' WHERE id=$1")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!visible(&pool, id).await);
    sqlx::query("DELETE FROM crm.source_account_links WHERE account_id=$1")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    let connection = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES($1,'linkedin','synthetic-policy','Synthetic native','manual')").bind(connection).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.connection_people(connection_id,external_id,display_name,resolved_profile_id) VALUES($1,'gh_synthetic_native','Synthetic native',$2)").bind(connection).bind(id).execute(&pool).await.unwrap();
    assert!(visible(&pool, id).await);
    sqlx::query("UPDATE crm.source_connections SET provider='wechat' WHERE id=$1")
        .bind(connection)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!visible(&pool, id).await);
    sqlx::query("DELETE FROM crm.connection_people WHERE connection_id=$1")
        .bind(connection)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query(
        "UPDATE crm.identities SET external_id='wxid_synthetic_policy' WHERE provider='wechat'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(visible(&pool, id).await);
    sqlx::query("SELECT crm.observe_wechat_contact('wxid_synthetic_policy','{\"verify_flag\":8}',now(),'synthetic-policy')").execute(&pool).await.unwrap();
    assert!(!visible(&pool, id).await);
    sqlx::query(
        "DELETE FROM crm.wechat_directory_accounts WHERE external_id='wxid_synthetic_policy'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(visible(&pool, id).await);
    sqlx::query(
        "UPDATE crm.identities SET external_id='gh_synthetic_directory' WHERE provider='wechat'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(!visible(&pool, id).await);
    sqlx::query("INSERT INTO crm.audit_events(action,entity_type,entity_id) VALUES('profile.created','profile',$1)").bind(id).execute(&pool).await.unwrap();
    assert!(
        visible(&pool, id).await,
        "manual creation overrides source-only classification immediately"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn directory_projection_serializes_publication_after_existing_epoch_locks(pool: PgPool) {
    let id = official(&pool).await;
    let mut accounts = Vec::new();
    for provider in ["linkedin", "instagram"] {
        let account = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,$2,'synthetic-legitimate','person','Synthetic source')")
            .bind(account).bind(provider).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
            .bind(account)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        accounts.push(account);
    }
    assert!(visible(&pool, id).await);
    let mut first = pool.begin().await.unwrap();
    let mut second = pool.begin().await.unwrap();
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *second)
        .await
        .unwrap();
    sqlx::query("DELETE FROM crm.source_account_links WHERE account_id=$1")
        .bind(accounts[0])
        .execute(&mut *first)
        .await
        .unwrap();
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *first)
        .await
        .unwrap();
    sqlx::query("DELETE FROM crm.source_account_links WHERE account_id=$1")
        .bind(accounts[1])
        .execute(&mut *second)
        .await
        .unwrap();
    let committing = tokio::spawn(async move { second.commit().await.unwrap() });
    // Observe the real commit lock, rather than relying on task timing. This
    // ensures the second refresh must see the first removal after it waits.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1))>0")
                .bind(second_pid)
                .fetch_one(&pool)
                .await
                .unwrap();
            if blocked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    first.commit().await.unwrap();
    committing.await.unwrap();
    assert!(
        !visible(&pool, id).await,
        "both legitimate sources are gone; the later publisher must not retain a stale visible result"
    );
}

use monica_core::*;
use serde_json::json;
use sqlx::PgPool;

fn record(provider: Provider, object: &str, event: &str, value: &str) -> Record {
    Record {
        provider,
        account: "fixture-account".into(),
        object: object.into(),
        event: event.into(),
        payload: json!({"text":value}),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn replay_and_revisions(pool: PgPool) {
    for p in [Provider::Wechat, Provider::Instagram, Provider::Linkedin] {
        assert_eq!(
            ingest(&pool, &record(p, "post", "1", "A")).await.unwrap(),
            1
        );
        assert_eq!(
            ingest(&pool, &record(p, "post", "1", "A")).await.unwrap(),
            1
        );
        assert_eq!(
            ingest(&pool, &record(p, "post", "2", "A")).await.unwrap(),
            1
        );
        assert!(
            ingest(&pool, &record(p, "post", "1", "different"))
                .await
                .is_err()
        );
        assert_eq!(
            ingest(&pool, &record(p, "post", "3", "B")).await.unwrap(),
            2
        );
        assert_eq!(
            ingest(&pool, &record(p, "post", "4", "A")).await.unwrap(),
            3
        );
    }
    let mut count = 0;
    while let Some(l) = claim(&pool).await.unwrap() {
        project(&pool, &l).await.unwrap();
        count += 1;
    }
    assert_eq!(count, 9);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.projections WHERE version=3 AND payload->>'text'='A'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn takeover_fences_old_worker(pool: PgPool) {
    ingest(&pool, &record(Provider::Wechat, "p", "1", "A"))
        .await
        .unwrap();
    let old = claim(&pool).await.unwrap().unwrap();
    sqlx::query("UPDATE crm.jobs SET lease_until=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let new = claim(&pool).await.unwrap().unwrap();
    assert_ne!(old.token, new.token);
    assert!(!heartbeat(&pool, &old).await.unwrap());
    assert!(project(&pool, &old).await.is_err());
    assert!(heartbeat(&pool, &new).await.unwrap());
    project(&pool, &new).await.unwrap();
    assert!(claim(&pool).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn unrelated_objects_progress_and_same_object_waits(pool: PgPool) {
    ingest(&pool, &record(Provider::Wechat, "a", "1", "A"))
        .await
        .unwrap();
    ingest(&pool, &record(Provider::Wechat, "a", "2", "B"))
        .await
        .unwrap();
    ingest(&pool, &record(Provider::Wechat, "b", "1", "C"))
        .await
        .unwrap();
    let first = claim(&pool).await.unwrap().unwrap();
    let independent = claim(&pool).await.unwrap().unwrap();
    assert_ne!(first.object, independent.object);
    assert!(claim(&pool).await.unwrap().is_none());
    project(&pool, &independent).await.unwrap();
    project(&pool, &first).await.unwrap();
    let second = claim(&pool).await.unwrap().unwrap();
    assert_eq!(second.object, first.object);
    assert_eq!(second.version, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_duplicate_has_one_job(pool: PgPool) {
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let pool = pool.clone();
        tasks.spawn(async move {
            ingest(&pool, &record(Provider::Instagram, "same", "same", "A"))
                .await
                .unwrap()
        });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap(), 1);
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn enqueue_failure_rolls_back_source(pool: PgPool) {
    sqlx::query("ALTER TABLE crm.jobs ADD CONSTRAINT reject_fixture CHECK(version < 0)")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        ingest(&pool, &record(Provider::Wechat, "p", "1", "A"))
            .await
            .is_err()
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM wechat.objects")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_commit_rolls_back_projection(pool: PgPool) {
    ingest(&pool, &record(Provider::Wechat, "p", "1", "A"))
        .await
        .unwrap();
    let lease = claim(&pool).await.unwrap().unwrap();
    sqlx::query("UPDATE crm.jobs SET lease_until=now()+interval '300 milliseconds'")
        .execute(&pool)
        .await
        .unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE crm.projections IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let worker_pool = pool.clone();
    let task = tokio::spawn(async move { project(&worker_pool, &lease).await });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    blocker.commit().await.unwrap();
    assert!(task.await.unwrap().is_err());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.projections")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert!(claim(&pool).await.unwrap().is_some());
}

use monica_core::{Provider, messages::*};
use sqlx::PgPool;

#[test]
fn feedback_stays_bounded() {
    let mut b = BatchBudget::default();
    for _ in 0..100 {
        b.observe(std::time::Duration::from_millis(1));
    }
    assert_eq!(b.records, MAX_RECORDS);
    for _ in 0..100 {
        b.observe(std::time::Duration::from_millis(500));
    }
    assert_eq!(b.records, 25);
}

fn batch(start: usize, n: usize) -> Vec<Message> {
    (start..start + n)
        .map(|i| Message {
            account: "owner".into(),
            conversation: "chat".into(),
            external_id: format!("m{i:08}"),
            sent_at_ms: i as i64,
            sender: "sender".into(),
            body: format!("Message {i}"),
        })
        .collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn thousand_messages_one_work_item_and_no_revision_jobs(pool: PgPool) {
    assert_eq!(
        append(&pool, Provider::Wechat, &batch(0, 1000))
            .await
            .unwrap(),
        1000
    );
    assert_eq!(
        append(&pool, Provider::Wechat, &batch(0, 1000))
            .await
            .unwrap(),
        0
    );
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM crm.conversation_work),(SELECT count(*) FROM crm.jobs),(SELECT count(*) FROM wechat.revisions)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 0, 0));
    let l = claim(&pool).await.unwrap().unwrap();
    let s = snapshot(&pool, &l).await.unwrap();
    assert_eq!(s.count, 1000);
    assert_eq!(s.generation, 1);
    finish(&pool, &l, &s).await.unwrap();
    assert!(claim(&pool).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn arrival_during_run_preserves_next_generation(pool: PgPool) {
    append(&pool, Provider::Wechat, &batch(0, 20))
        .await
        .unwrap();
    let l = claim(&pool).await.unwrap().unwrap();
    let old = snapshot(&pool, &l).await.unwrap();
    append(&pool, Provider::Wechat, &batch(20, 20))
        .await
        .unwrap();
    assert!(claim(&pool).await.unwrap().is_none());
    finish(&pool, &l, &old).await.unwrap();
    let next = claim(&pool).await.unwrap().unwrap();
    let new = snapshot(&pool, &next).await.unwrap();
    assert_eq!(new.count, 40);
    assert_eq!(new.generation, 2);
    finish(&pool, &next, &new).await.unwrap();
    assert!(finish(&pool, &l, &old).await.is_err());
    assert!(claim(&pool).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn conflicting_replay_rolls_back_entire_batch(pool: PgPool) {
    append(&pool, Provider::Wechat, &batch(0, 1)).await.unwrap();
    let mut input = batch(0, 2);
    input[0].body = "edited".into();
    assert!(append(&pool, Provider::Wechat, &input).await.is_err());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM wechat.messages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let n: i64 = sqlx::query_scalar("SELECT message_count FROM crm.conversation_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn bounds_and_internal_duplicates(pool: PgPool) {
    assert!(
        append(&pool, Provider::Wechat, &batch(0, 1001))
            .await
            .is_err()
    );
    let mut big = batch(0, 1);
    big[0].body = "x".repeat(MAX_BYTES + 1);
    assert!(append(&pool, Provider::Wechat, &big).await.is_err());
    let mut duplicate = batch(0, 1);
    duplicate.push(duplicate[0].clone());
    assert_eq!(
        append(&pool, Provider::Wechat, &duplicate).await.unwrap(),
        1
    );
    duplicate[1].body = "different".into();
    assert!(append(&pool, Provider::Wechat, &duplicate).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_overlap_counts_only_new_facts(pool: PgPool) {
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..8 {
        let p = pool.clone();
        tasks.spawn(async move {
            append(&p, Provider::Wechat, &batch(i * 50, 100))
                .await
                .unwrap()
        });
    }
    let mut added = 0;
    while let Some(r) = tasks.join_next().await {
        added += r.unwrap();
    }
    assert_eq!(added, 450);
    let l = claim(&pool).await.unwrap().unwrap();
    let s = snapshot(&pool, &l).await.unwrap();
    assert_eq!(s.count, 450);
    assert_eq!(s.last_at_ms, 449);
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_isolation_and_late_history(pool: PgPool) {
    for provider in [Provider::Wechat, Provider::Instagram, Provider::Linkedin] {
        append(&pool, provider, &batch(100, 10)).await.unwrap();
        append(&pool, provider, &batch(0, 10)).await.unwrap();
    }
    let mut n = 0;
    while let Some(l) = claim(&pool).await.unwrap() {
        let s = snapshot(&pool, &l).await.unwrap();
        assert_eq!(s.count, 20);
        assert_eq!(s.last_at_ms, 109);
        finish(&pool, &l, &s).await.unwrap();
        n += 1;
    }
    assert_eq!(n, 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn summary_lease_takeover_rejects_old_snapshot(pool: PgPool) {
    append(&pool, Provider::Wechat, &batch(0, 1)).await.unwrap();
    let old = claim(&pool).await.unwrap().unwrap();
    let s = snapshot(&pool, &old).await.unwrap();
    sqlx::query("UPDATE crm.conversation_work SET lease_until=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let new = claim(&pool).await.unwrap().unwrap();
    assert!(finish(&pool, &old, &s).await.is_err());
    finish(&pool, &new, &s).await.unwrap();
}

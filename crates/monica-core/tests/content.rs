use monica_core::{content, identity};
use serde_json::{Value, json};
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn legacy_typed_content_collisions_and_immutable_reuse(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    let text = content::register(&mut tx, "field_value", &json!("0"))
        .await
        .unwrap();
    let number = content::register(&mut tx, "field_value", &json!(0))
        .await
        .unwrap();
    assert_eq!(text.hash, identity::bytes_hash(b"0"));
    assert_eq!(number.hash, identity::bytes_hash(b"json\x000"));
    assert_ne!(text.id, number.id);
    assert_eq!(
        content::register(&mut tx, "field_value", &json!("0"))
            .await
            .unwrap()
            .id,
        text.id
    );
    assert_eq!(
        content::register(&mut tx, "field_value", &json!(0))
            .await
            .unwrap()
            .id,
        number.id
    );
    let number = content::register(&mut tx, "field_value", &json!(42))
        .await
        .unwrap();
    let text = content::register(&mut tx, "field_value", &json!("42"))
        .await
        .unwrap();
    assert_eq!(number.hash, identity::bytes_hash(b"42"));
    assert_eq!(text.hash, identity::bytes_hash(b"json\x00\"42\""));
    let original = content::register(&mut tx, "field_value", &json!({"b":"a\0b","a":1e-7}))
        .await
        .unwrap();
    let repeated = content::register(&mut tx, "field_value", &json!({"a":1e-7,"b":"ab"}))
        .await
        .unwrap();
    assert_eq!(original.id, repeated.id);
    tx.commit().await.unwrap();
    let values: Vec<Value> =
        sqlx::query_scalar("SELECT payload_json FROM crm.content_objects WHERE sha256=$1")
            .bind(original.hash)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["b"], "ab");
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_content_registration_has_one_immutable_object(pool: PgPool) {
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let pool = pool.clone();
        tasks.spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            let object = content::register(&mut tx, "field_value", &json!("synthetic concurrent"))
                .await
                .unwrap();
            tx.commit().await.unwrap();
            object.id
        });
    }
    let mut ids = std::collections::BTreeSet::new();
    while let Some(id) = tasks.join_next().await {
        ids.insert(id.unwrap());
    }
    assert_eq!(ids.len(), 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.content_objects")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reversed_multi_value_batches_lock_addresses_in_the_same_order(pool: PgPool) {
    let mut tasks = tokio::task::JoinSet::new();
    for values in [
        vec![json!("shared A"), json!("shared B")],
        vec![json!("shared B"), json!("shared A")],
    ] {
        let pool = pool.clone();
        tasks.spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            let objects = content::register_many(&mut tx, "field_value", &values)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            objects
                .into_iter()
                .map(|o| o.id)
                .collect::<std::collections::BTreeSet<_>>()
        });
    }
    let first = tasks.join_next().await.unwrap().unwrap();
    let second = tasks.join_next().await.unwrap().unwrap();
    assert_eq!(first, second);
}

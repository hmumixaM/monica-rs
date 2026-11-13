use monica_core::{content, identity, imports};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::time::Duration;
use uuid::Uuid;

async fn notebook(pool: &PgPool, records: Vec<Value>) {
    notebook_stream(pool, "posts", records).await;
}
async fn notebook_stream(pool: &PgPool, stream: &str, records: Vec<Value>) {
    let key = Uuid::new_v4().to_string();
    let batch: imports::Batch = serde_json::from_value(json!({
        "source":"monica", "stream":stream, "batch_id":key,
        "idempotency_key":key, "observed_at":"2026-01-01T00:00:00Z", "records":records,
    }))
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let id = imports::receive(&mut tx, &batch, &key).await.unwrap();
    tx.commit().await.unwrap();
    let result = imports::resume(pool, id).await.unwrap();
    assert_eq!(result["status"], "complete", "{result}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_facts_reject_rewrites_but_accept_new_revisions(pool: PgPool) {
    let tables: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT n.nspname,c.relname,a.attname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace JOIN pg_attribute a ON a.attrelid=c.oid AND a.attnum=1 WHERE t.tgname='append_only_facts' ORDER BY 1,2",
    ).fetch_all(&pool).await.unwrap();
    assert_eq!(tables.len(), 70);
    // Test the database boundary, including accidental writes matching no row.
    for (schema, table, column) in tables {
        for statement in [
            format!("UPDATE {schema}.{table} SET {column}={column} WHERE false"),
            format!("DELETE FROM {schema}.{table} WHERE false"),
        ] {
            let error = sqlx::query(&statement).execute(&pool).await.unwrap_err();
            assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("55000"),
                "{statement}"
            );
        }
    }
    let mut tx = pool.begin().await.unwrap();
    let a = content::register(&mut tx, "post_body", &json!({"text":"Original"}))
        .await
        .unwrap();
    let b = content::register(&mut tx, "post_body", &json!({"text":"Corrected"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_ne!(a.id, b.id);
    let error = sqlx::query("UPDATE crm.content_objects SET payload_json='{}' WHERE id=$1")
        .bind(a.id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("55000")
    );
    let old: Value = sqlx::query_scalar("SELECT payload_json FROM crm.content_objects WHERE id=$1")
        .bind(a.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(old, json!({"text":"Original"}));
}

#[sqlx::test(migrations = "../../migrations")]
async fn story_replay_owner_change_and_retraction_preserve_originals(pool: PgPool) {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    for id in [first, second] {
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic story owner')")
            .bind(id).execute(&pool).await.unwrap();
    }
    let mut record = json!({"external_id":"stable-story","entity_type":"story","profile_id":first,"content":{"text":"Original frame","coverage_status":"partial"}});
    notebook_stream(&pool, "stories", vec![record.clone()]).await;
    let before: (String, String) = sqlx::query_as("SELECT e.xmin::text,h.xmin::text FROM crm.timeline_events e JOIN crm.notebook_objects h ON h.external_id=e.external_id WHERE e.external_id='stable-story'")
        .fetch_one(&pool).await.unwrap();
    notebook_stream(&pool, "stories", vec![record.clone()]).await;
    let after: (String, String) = sqlx::query_as("SELECT e.xmin::text,h.xmin::text FROM crm.timeline_events e JOIN crm.notebook_objects h ON h.external_id=e.external_id WHERE e.external_id='stable-story'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(after, before);
    record["profile_id"] = json!(second);
    notebook_stream(&pool, "stories", vec![record.clone()]).await;
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>(
            "SELECT profile_id FROM crm.timeline_events WHERE external_id='stable-story'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        second
    );
    record["content"]["coverage_status"] = json!("deleted");
    notebook_stream(&pool, "stories", vec![record.clone()]).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM crm.timeline_events WHERE external_id='stable-story'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "deleted"
    );
    record["content"]["coverage_status"] = json!("partial");
    notebook_stream(&pool, "stories", vec![record]).await;
    let status: String = sqlx::query_scalar(
        "SELECT status FROM crm.timeline_events WHERE external_id='stable-story'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "partial");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.notebook_social_revisions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.content_objects WHERE content_kind='story_body'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn published_content_reuse_is_read_only_and_does_not_wait_for_writer(pool: PgPool) {
    let values = vec![json!("0"), json!(0), json!({"body":"Published"})];
    let mut tx = pool.begin().await.unwrap();
    let original = content::register_many(&mut tx, "post_body", &values)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut writer = pool.begin().await.unwrap();
    sqlx::query("SELECT crm.lock_content_addresses($1)")
        .bind(vec![format!(
            "post_body:{}",
            identity::content_hash(&values[0])
        )])
        .execute(&mut *writer)
        .await
        .unwrap();
    let mut reader = pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *reader)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        content::register_many(&mut reader, "post_body", &values),
    )
    .await;
    writer.rollback().await.unwrap();
    let reused = result
        .expect("published bodies waited for writer reservation")
        .unwrap();
    assert_eq!(
        reused.iter().map(|r| r.id).collect::<Vec<_>>(),
        original.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    reader.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.content_objects")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
}

async fn timeline_state(pool: &PgPool) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('timeline',to_jsonb(e),'timeline_xmin',e.xmin::text,'head_xmin',h.xmin::text,'profile_generation',p.generation,'content_count',(SELECT count(*) FROM crm.content_objects),'interactions',(SELECT jsonb_agg(to_jsonb(i)||jsonb_build_object('xmin',i.xmin::text) ORDER BY i.id) FROM crm.social_interactions i WHERE i.post_id=e.id),'cache',(SELECT jsonb_agg(to_jsonb(v) ORDER BY domain,subject) FROM crm.cache_versions v WHERE domain='posts' OR subject=e.profile_id)) FROM crm.timeline_events e JOIN crm.notebook_objects h ON h.external_id=e.external_id AND h.stream='posts' JOIN crm.profiles p ON p.id=e.profile_id WHERE e.external_id='stable-post'")
        .fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn timeline_replay_skips_writes_and_locks_but_metadata_and_reversions_survive(pool: PgPool) {
    let person = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic author')")
        .bind(person).execute(&pool).await.unwrap();
    let a = json!({"external_id":"stable-post","profile_id":person,"entity_type":"post","occurred_at":"2026-01-01T00:00:00Z","content":{"text":"Original","interactions":[{"external_id":"comment-1","interaction_type":"comment","text":"A comment"}]}});
    notebook(&pool, vec![a.clone()]).await;
    let before = timeline_state(&pool).await;
    let mut writer = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 FOR UPDATE")
        .bind(person)
        .execute(&mut *writer)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('notebook:stable-post',0))")
        .execute(&mut *writer)
        .await
        .unwrap();
    let repeated =
        tokio::time::timeout(Duration::from_secs(1), notebook(&pool, vec![a.clone()])).await;
    writer.rollback().await.unwrap();
    repeated.expect("unchanged timeline tried to lock its object/person");
    assert_eq!(
        timeline_state(&pool).await,
        before,
        "replay rewrote content/head/profile/cache/interactions"
    );

    let mut metadata = a.clone();
    metadata["occurred_at"] = json!("2026-02-01T00:00:00Z");
    notebook(&pool, vec![metadata]).await;
    let at: String = sqlx::query_scalar(
        "SELECT occurred_at::text FROM crm.timeline_events WHERE external_id='stable-post'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(at.starts_with("2026-02-01"));

    let mut b = a.clone();
    b["content"]["text"] = json!("Corrected");
    notebook(&pool, vec![b, a]).await;
    let event = sqlx::query(
        "SELECT content,version FROM crm.timeline_events WHERE external_id='stable-post'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event.get::<Value, _>("content")["text"], "Original");
    assert_eq!(event.get::<i64, _>("version"), 3);
    let history: Vec<String> = sqlx::query_scalar("SELECT c.payload_json->>'text' FROM crm.notebook_social_revisions r JOIN crm.content_objects c ON c.id=r.content_object_id ORDER BY r.revision_seq").fetch_all(&pool).await.unwrap();
    assert_eq!(history, ["Original", "Corrected", "Original"]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.content_objects WHERE content_kind='post_body'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
}

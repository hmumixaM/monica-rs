use monica_core::ai;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
async fn connection(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,'mail','synthetic@example.invalid','Synthetic mail','imap',true)").bind(id).execute(pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(pool)
        .await
        .unwrap();
    id
}
async fn evidence(
    pool: &PgPool,
    connection: Uuid,
    external: &str,
    category: &str,
    body: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let headers = json!({"from":"fixture@example.invalid","to":"synthetic@example.invalid","subject":"Synthetic message","authorization":"MUST-NOT-LEAVE","x-private-token":"MUST-NOT-LEAVE"});
    let metadata = json!({"headers":headers,"preview":"PRIVATE-PREVIEW","classification_reason":"needs_classification"});
    sqlx::query("INSERT INTO crm.connection_evidence(id,connection_id,external_id,revision,content_hash,kind,title,body,category,included,metadata) VALUES($1,$2,$3,1,$4,'message','Synthetic message',$5,$6,$7,$8)").bind(id).bind(connection).bind(external).bind(format!("synthetic-{external}")).bind(body).bind(category).bind(category=="transactional").bind(metadata).execute(pool).await.unwrap();
    id
}
#[sqlx::test(migrations = "../../migrations")]
async fn mail_header_batch_privacy_validation_and_atomic_reprocess(pool: PgPool) {
    let connection = connection(&pool).await;
    evidence(&pool, connection, "mail-1", "pending", "SENSITIVE-RAW-BODY").await;
    ai::mail::enqueue_pending(&pool).await.unwrap();
    let lease = ai::claim(&pool).await.unwrap().unwrap();
    assert_eq!(lease.kind, "ai.mail_classify");
    assert!(!lease.input.to_string().contains("MUST-NOT-LEAVE"));
    assert!(!lease.input.to_string().contains("PRIVATE-PREVIEW"));
    assert!(!lease.input.to_string().contains("SENSITIVE-RAW-BODY"));
    let answer = json!({"items":[{"external_id":"invented-id","category":"personal"},{"external_id":"mail-1","category":"marketing"}]});
    assert!(ai::finish(&pool, &lease, answer).await.unwrap());
    let category:String=sqlx::query_scalar("SELECT category FROM crm.mail_classifications WHERE connection_id=$1 AND external_id='mail-1'").bind(connection).fetch_one(&pool).await.unwrap();
    assert_eq!(category, "marketing");
    let reprocess: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.connection_reprocess WHERE connection_id=$1")
            .bind(connection)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reprocess, 1);
    let source: String =
        sqlx::query_scalar("SELECT category FROM crm.connection_evidence WHERE connection_id=$1")
            .bind(connection)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        source, "pending",
        "model must not overwrite a retained source fact"
    );
    ai::mail::enqueue_pending(&pool).await.unwrap();
    assert!(ai::claim(&pool).await.unwrap().is_none());
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.ai_jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE crm.connection_evidence SET content_hash='body-refreshed',revision=2,body='Changed body only' WHERE connection_id=$1").bind(connection).execute(&pool).await.unwrap();
    ai::mail::enqueue_pending(&pool).await.unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.ai_jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "unchanged headers must not cost another model call"
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn transactional_event_quotes_dates_and_stale_revision_fence(pool: PgPool) {
    let connection = connection(&pool).await;
    let id = evidence(
        &pool,
        connection,
        "event-mail",
        "transactional",
        "Your flight departs May 7 at 09:00 from Chicago. Reference ABC123.",
    )
    .await;
    assert!(
        ai::enqueue(&pool, "ai.mail_events", &id.to_string())
            .await
            .is_err()
    );
    sqlx::query("UPDATE crm.ai_settings SET include_messages=true")
        .execute(&pool)
        .await
        .unwrap();
    ai::mail::enqueue_pending(&pool).await.unwrap();
    let lease = ai::claim(&pool).await.unwrap().unwrap();
    assert_eq!(lease.kind, "ai.mail_events");
    let output = json!({"events":[{"kind":"travel","title":"Synthetic flight","quote":"Your flight departs May 7 at 09:00 from Chicago.","date_text":"May 7 at 09:00","location":"Chicago","reference":"ABC123"},{"kind":"order","title":"Invented","quote":"This quote is absent"}]});
    assert!(ai::finish(&pool, &lease, output).await.unwrap());
    let saved: Value =
        sqlx::query_scalar("SELECT events FROM crm.mail_event_extractions WHERE evidence_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(saved.as_array().unwrap().len(), 1);
    assert_eq!(saved[0]["date_text"], "May 7 at 09:00");
    assert!(!saved.to_string().contains("UTC"));
    sqlx::query("UPDATE crm.connection_evidence SET revision=2,content_hash='changed',body='The flight was cancelled.' WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    ai::mail::enqueue_pending(&pool).await.unwrap();
    let stale = ai::claim(&pool).await.unwrap().unwrap();
    sqlx::query("UPDATE crm.connection_evidence SET included=false,revision=3 WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !ai::finish(&pool, &stale, json!({"events":[]}))
            .await
            .unwrap()
    );
    let revision: i64 =
        sqlx::query_scalar("SELECT revision FROM crm.mail_event_extractions WHERE evidence_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        revision, 1,
        "stale result must not publish over previous evidence"
    );
    let people: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.profiles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(people, 0, "mail event extraction must not create people");
}

use monica_core::faces::{self, Batch, Decision};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn media(pool: &PgPool, entity: &str, owner: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let hash = format!("{:064x}", id.as_u128());
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,width,height) VALUES($1,$2,'image/png',100,'synthetic-only',100,100)").bind(id).bind(hash).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES($1,$2,$3,$4)",
    )
    .bind(entity)
    .bind(owner)
    .bind(id)
    .bind(if entity == "profile" {
        "manual_photo"
    } else {
        "media-0"
    })
    .execute(pool)
    .await
    .unwrap();
    id
}
async fn person(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic face target')").bind(id).execute(pool).await.unwrap();
    id
}
fn parse(value: Value) -> Batch {
    serde_json::from_value(value).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn device_boundary_excludes_chats_and_successful_zero_is_final(pool: PgPool) {
    let p = person(&pool).await;
    let avatar = media(&pool, "profile", p).await;
    let chat = media(&pool, "message_event", Uuid::new_v4()).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM crm.face_device_runtime")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "needs_models"
    );
    let work = faces::work(&pool, 200).await.unwrap();
    assert_eq!(work["items"].as_array().unwrap().len(), 1);
    assert_eq!(work["items"][0]["media_id"], avatar.to_string());
    assert!(
        faces::batch(
            &pool,
            &parse(
                json!({"model":"synthetic-fake-boundary","scans":[{"media_id":chat,"faces":[]}]})
            )
        )
        .await
        .is_err()
    );
    faces::batch(
        &pool,
        &parse(json!({"model":"synthetic-fake-boundary","scans":[{"media_id":avatar,"faces":[]}]})),
    )
    .await
    .unwrap();
    assert!(
        faces::work(&pool, 200).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    faces::batch(&pool,&parse(json!({"model":"synthetic-fake-boundary","scans":[{"media_id":avatar,"faces":[],"status":"retryable_error","error":"fixture timeout"}]}))).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM crm.face_scans")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "success"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn human_decisions_win_over_device_replay_and_can_be_restored(pool: PgPool) {
    let p = person(&pool).await;
    let m = media(&pool, "profile", p).await;
    let face = Uuid::new_v4();
    let input = parse(
        json!({"model":"synthetic-model","scans":[{"media_id":m,"faces":[{"id":face,"box":[0.1,0.2,0.3,0.4],"quality":0.9}]}],"suggestions":[{"face_id":face,"cluster_key":"fake-group","profile_id":p,"score":0.95}]}),
    );
    faces::batch(&pool, &input).await.unwrap();
    assert_eq!(
        faces::anchors(&pool).await.unwrap()["anchors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let previous = faces::decide(
        &pool,
        &Decision {
            face_ids: vec![face],
            action: "confirm".into(),
            profile_id: Some(p),
        },
    )
    .await
    .unwrap();
    faces::batch(&pool, &input).await.unwrap();
    assert_eq!(
        faces::avatar_matches(&pool, &[m]).await.unwrap()[&(m, p)],
        1.0
    );
    let states: Vec<faces::FaceState> =
        serde_json::from_value(previous["previous"].clone()).unwrap();
    faces::restore(&pool, &states).await.unwrap();
    faces::decide(
        &pool,
        &Decision {
            face_ids: vec![face],
            action: "not".into(),
            profile_id: Some(p),
        },
    )
    .await
    .unwrap();
    faces::batch(&pool, &input).await.unwrap();
    assert!(faces::avatar_matches(&pool, &[m]).await.unwrap().is_empty());
    assert_eq!(
        faces::anchors(&pool).await.unwrap()["rejected"][0]["profile_id"],
        p.to_string()
    );
    faces::decide(
        &pool,
        &Decision {
            face_ids: vec![face],
            action: "ignore".into(),
            profile_id: None,
        },
    )
    .await
    .unwrap();
    faces::batch(&pool, &input).await.unwrap();
    assert_eq!(
        faces::anchors(&pool).await.unwrap()["ignored"][0],
        face.to_string()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_batches_rollback_and_retryable_scans_back_off(pool: PgPool) {
    let p = person(&pool).await;
    let m = media(&pool, "profile", p).await;
    let face = Uuid::new_v4();
    assert!(faces::batch(&pool,&parse(json!({"model":"synthetic","scans":[{"media_id":m,"faces":[{"id":face,"box":[0.0,0.0,-0.1,0.4],"quality":0.9}]}]}))).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.face_scans")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    faces::batch(
        &pool,
        &parse(json!({"model":"synthetic:timeout","scans":[{"media_id":m,"faces":[]}]})),
    )
    .await
    .unwrap();
    assert!(
        faces::work(&pool, 10).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE crm.face_scans SET next_attempt_at=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        faces::work(&pool, 10).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let same = Uuid::new_v4();
    let other = media(&pool, "profile", p).await;
    faces::batch(&pool,&parse(json!({"model":"synthetic","scans":[{"media_id":m,"faces":[{"id":same,"box":[0.0,0.0,0.2,0.3],"quality":0.7}]}]}))).await.unwrap();
    assert!(faces::batch(&pool,&parse(json!({"model":"synthetic","scans":[{"media_id":other,"faces":[{"id":same,"box":[0.0,0.0,0.2,0.3],"quality":0.7}]}]}))).await.is_err());
}

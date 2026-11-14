use monica_core::ai;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

async fn person(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2)")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}
async fn drain(pool: &PgPool) {
    while ai::visual::refresh_inputs(pool).await.unwrap() > 0 {}
}
async fn input(pool: &PgPool, profile: Uuid) -> Option<Value> {
    sqlx::query_scalar(
        "SELECT payload FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind='visual'",
    )
    .bind(profile)
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn saved_visual_context_tracks_confirmed_identity_and_source_lifecycle(pool: PgPool) {
    let owner = person(&pool, "Post author").await;
    let other = person(&pool, "Other author").await;
    let confirmed = person(&pool, "Confirmed person").await;
    let suggested = person(&pool, "UNCONFIRMED-CANDIDATE").await;
    let account = Uuid::new_v4();
    let post = Uuid::new_v4();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=false")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE crm.image_ocr_settings SET enabled=false,include_chat=true")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'linkedin','synthetic-visual','person','Synthetic')")
        .bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) VALUES($1,'linkedin',$1,$2,$3,'post','post','2026-11-10T18:00:00Z','{\"text\":\"Caption\"}','post-v1',1)")
        .bind(post).bind(account).bind(owner).execute(&pool).await.unwrap();
    let mut images = Vec::new();
    for (n, kind, role, text) in [
        (1, "social_post", "thumbnail-video-1", "SAVED-PUBLIC-OCR"),
        (2, "social_post", "avatar", "PRIVATE-AVATAR"),
        (3, "message_event", "image", "PRIVATE-CHAT-OCR"),
    ] {
        let media = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) VALUES($1,$2,'image/png',0,'synthetic-unused')")
            .bind(media).bind(format!("{n:064x}")).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES($1,$2,$3,$4)",
        )
        .bind(kind)
        .bind(post)
        .bind(media)
        .bind(role)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO crm.image_ocr_readings(media_id,status,summary,text,model,generated_at,input_hash) VALUES($1,'complete','Saved observation',$2,'historical-model','2026-11-11T12:00:00Z','legacy-input')")
            .bind(media).bind(text).execute(&pool).await.unwrap();
        images.push(media);
    }
    let face = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.faces(id,media_id,box,quality,status,profile_id,decided_at) VALUES($1,$2,ARRAY[0.1,0.2,0.3,0.4]::real[],0.9,'confirmed',$3,now())")
        .bind(face).bind(images[0]).bind(confirmed).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.faces(id,media_id,box,quality,status,suggested_profile_id) VALUES($1,$2,ARRAY[0.1,0.2,0.3,0.4]::real[],0.9,'pending',$3)")
        .bind(Uuid::new_v4()).bind(images[0]).bind(suggested).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_readings(entity_id,entity_type,status,summary,model,generated_at) VALUES($1,'social_post','complete','SAVED-CAPTION','historical-model','2026-11-11T12:00:00Z')")
        .bind(post).execute(&pool).await.unwrap();
    drain(&pool).await;
    let visual = input(&pool, owner).await.unwrap();
    let text = visual.to_string();
    assert!(text.contains("SAVED-PUBLIC-OCR") && text.contains("SAVED-CAPTION"));
    assert!(text.contains("Confirmed person") && text.contains("historical-model"));
    assert!(!text.contains("UNCONFIRMED-CANDIDATE") && !text.contains("PRIVATE-"));
    assert_eq!(visual["at"], "2026-11-10T18:00:00+00:00");
    assert_eq!(visual["citation"], format!("/timeline?event={post}"));
    // Saved observations enter both model snapshots without enabling execution.
    for (kind, subject) in [
        ("ai.profile", owner.to_string()),
        ("ai.person_week", format!("{owner}:2026-11-09")),
        ("ai.digest", "2026-11-09".into()),
    ] {
        let mut tx = pool.begin().await.unwrap();
        let snapshot = ai::snapshot(&mut tx, kind, &subject).await.unwrap();
        assert!(snapshot.input.to_string().contains("SAVED-PUBLIC-OCR"));
        tx.rollback().await.unwrap();
    }
    let before: i64 =
        sqlx::query_scalar("SELECT generation FROM crm.memory_heads WHERE profile_id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ai::visual::refresh_inputs(&pool).await.unwrap(), 0);
    sqlx::query("UPDATE crm.image_ocr_readings SET text='CORRECTED-SAVED-OCR' WHERE media_id=$1")
        .bind(images[0])
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_pending WHERE profile_id=$1)"
        )
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap()
    );
    drain(&pool).await;
    assert!(
        input(&pool, owner)
            .await
            .unwrap()
            .to_string()
            .contains("CORRECTED-SAVED-OCR")
    );
    let after: i64 =
        sqlx::query_scalar("SELECT generation FROM crm.memory_heads WHERE profile_id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(after > before);
    sqlx::query("UPDATE crm.profiles SET display_name='Renamed confirmation' WHERE id=$1")
        .bind(confirmed)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(
        input(&pool, owner)
            .await
            .unwrap()
            .to_string()
            .contains("Renamed confirmation")
    );
    sqlx::query("UPDATE crm.faces SET status='pending',profile_id=NULL WHERE id=$1")
        .bind(face)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(
        !input(&pool, owner)
            .await
            .unwrap()
            .to_string()
            .contains("Renamed confirmation")
    );
    sqlx::query("UPDATE crm.timeline_events SET profile_id=$2 WHERE id=$1")
        .bind(post)
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(input(&pool, owner).await.is_none());
    assert!(input(&pool, other).await.is_some());
    sqlx::query("UPDATE crm.timeline_events SET status='deleted' WHERE id=$1")
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(input(&pool, other).await.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.ai_jobs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn connection_filter_and_disable_remove_saved_visual_inputs(pool: PgPool) {
    let owner = person(&pool, "Native author").await;
    let connection = Uuid::new_v4();
    let account = Uuid::new_v4();
    let event = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,'linkedin','synthetic','Synthetic','archive',true)")
        .bind(connection).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'linkedin','synthetic-native','person','Synthetic')")
        .bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.connection_evidence(id,connection_id,external_id,revision,content_hash,kind,included) VALUES($1,$2,'post',1,'native-v1','post',true)")
        .bind(event).bind(connection).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,content,content_hash,version,connection_id) VALUES($1,'linkedin',$1,$2,$3,'post','post','{}','native-v1',1,$4)")
        .bind(event).bind(account).bind(owner).bind(connection).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_readings(entity_id,entity_type,status,summary,model,generated_at) VALUES($1,'social_post','complete','Saved native reading','historical-model',now())")
        .bind(event).execute(&pool).await.unwrap();
    drain(&pool).await;
    assert!(input(&pool, owner).await.is_some());
    sqlx::query("UPDATE crm.source_connections SET enabled=false WHERE id=$1")
        .bind(connection)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(input(&pool, owner).await.is_none());
    sqlx::query("UPDATE crm.source_connections SET enabled=true WHERE id=$1")
        .bind(connection)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(input(&pool, owner).await.is_some());
    sqlx::query("UPDATE crm.connection_evidence SET included=false WHERE id=$1")
        .bind(event)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(input(&pool, owner).await.is_none());
    sqlx::query("UPDATE crm.connection_evidence SET included=true WHERE id=$1")
        .bind(event)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(input(&pool, owner).await.is_some());
    sqlx::query("UPDATE crm.connection_evidence SET deleted=true WHERE id=$1")
        .bind(event)
        .execute(&pool)
        .await
        .unwrap();
    drain(&pool).await;
    assert!(input(&pool, owner).await.is_none());
}

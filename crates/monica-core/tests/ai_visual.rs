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
    while ai::week_window::refresh_observations(pool).await.unwrap() > 0 {}
}
async fn wait_for_head_waiters(pool: &PgPool, wanted: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        loop {
            let waiting: i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query LIKE '%crm.memory_heads%'")
                .fetch_one(pool).await.unwrap();
            if waiting>=wanted { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("expected actors did not reach the head lock");
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
    let story = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,content,content_hash,version,status) VALUES($1,'linkedin',$1,$2,$3,'story','partial-story','{}','story-v1',1,'partial')")
        .bind(story).bind(account).bind(other).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_readings(entity_id,entity_type,status,summary,model,generated_at) VALUES($1,'social_story','complete','SAVED-PARTIAL-STORY','historical-model',now())")
        .bind(story).execute(&pool).await.unwrap();
    drain(&pool).await;
    assert!(
        input(&pool, other)
            .await
            .unwrap()
            .to_string()
            .contains("SAVED-PARTIAL-STORY")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.ai_jobs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn large_saved_ocr_remains_complete_without_expanding_model_snapshots(pool: PgPool) {
    let owner = person(&pool, "Visual volume").await;
    let account = Uuid::new_v4();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=false")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'linkedin','volume','person','Synthetic volume')")
        .bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) SELECT gen_random_uuid(),'linkedin',gen_random_uuid(),$1,$2,'post',n::text,'2026-11-10T18:00:00Z','{}','synthetic',1 FROM generate_series(1,20) n")
        .bind(account).bind(owner).execute(&pool).await.unwrap();
    sqlx::query("WITH added AS (INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) SELECT gen_random_uuid(),lpad(n::text,64,'0'),'image/png',0,'synthetic-unused' FROM generate_series(1,80) n RETURNING id), ranked_media AS (SELECT id,row_number() OVER(ORDER BY id) n FROM added), ranked_posts AS (SELECT id,row_number() OVER(ORDER BY id) n FROM crm.timeline_events WHERE profile_id=$1) INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) SELECT 'social_post',p.id,m.id,'media-'||((m.n-1)%4) FROM ranked_media m JOIN ranked_posts p ON p.n=(m.n-1)/4+1")
        .bind(owner).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.image_ocr_readings(media_id,status,text,model,generated_at) SELECT id,'complete',repeat('x',7990)||'END-MARKER','historical-model',now() FROM crm.media_assets")
        .execute(&pool).await.unwrap();
    // The committed claim alone remains a publication fence after a restart.
    assert_eq!(ai::visual::claim_pending(&pool).await.unwrap(), 20);
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_pending WHERE profile_id=$1)"
        )
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.memory_visual_pending")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    // New work can arrive after claim commit. Processing the old claim must
    // leave the source's pending reference available for the following turn.
    sqlx::query("UPDATE crm.image_ocr_readings SET text='Y'||substr(text,2) WHERE media_id=(SELECT min(media_id::text)::uuid FROM crm.image_ocr_readings)")
        .execute(&pool).await.unwrap();
    drain(&pool).await;
    let bytes: i64=sqlx::query_scalar("SELECT sum(octet_length(payload::text))::bigint FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind='visual'")
        .bind(owner).fetch_one(&pool).await.unwrap();
    assert!(bytes > 512 * 1024);
    let lengths: Vec<i32>=sqlx::query_scalar("SELECT length(m->'reading'->>'text') FROM crm.memory_inputs i CROSS JOIN LATERAL jsonb_array_elements(i.payload->'media') m WHERE i.profile_id=$1 AND i.source_kind='visual'")
        .bind(owner).fetch_all(&pool).await.unwrap();
    assert_eq!(lengths, vec![8000; 80]);
    for (kind, subject) in [
        ("ai.profile", owner.to_string()),
        ("ai.person_week", format!("{owner}:2026-11-09")),
        ("ai.digest", "2026-11-09".into()),
    ] {
        let mut tx = pool.begin().await.unwrap();
        let snapshot = ai::snapshot(&mut tx, kind, &subject).await.unwrap();
        assert!(snapshot.input.to_string().len() < 512 * 1024);
        tx.rollback().await.unwrap();
    }
    ai::memory::enqueue_pending(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.ai_jobs WHERE status='queued'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
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
    // Reproduce the real native-writer/head order. The reducer must finish
    // without waiting for the pending row held by that writer.
    sqlx::query("INSERT INTO crm.connection_evidence_people(evidence_id,profile_id,role) VALUES($1,$2,'author')")
        .bind(event).bind(owner).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.media_readings SET summary='Updated saved reading' WHERE entity_id=$1")
        .bind(event)
        .execute(&pool)
        .await
        .unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT profile_id FROM crm.memory_heads WHERE profile_id=$1 FOR UPDATE")
        .bind(owner)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let reducer_pool = pool.clone();
    let reducer = tokio::spawn(async move { ai::visual::refresh_inputs(&reducer_pool).await });
    wait_for_head_waiters(&pool, 1).await;
    let mut writer = pool.begin().await.unwrap();
    sqlx::query("UPDATE crm.timeline_events SET version=2 WHERE id=$1")
        .bind(event)
        .execute(&mut *writer)
        .await
        .unwrap();
    let writer = tokio::spawn(async move {
        ai::memory::connection_evidence_many(&mut writer, &[event]).await?;
        writer.commit().await?;
        Ok::<_, anyhow::Error>(())
    });
    wait_for_head_waiters(&pool, 2).await;
    blocker.commit().await.unwrap();
    let (reduced, written) = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        tokio::join!(reducer, writer)
    })
    .await
    .expect("visual/native writer failed to make progress");
    assert_eq!(reduced.unwrap().unwrap(), 1);
    written.unwrap().unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM crm.memory_visual_pending WHERE event_id=$1)"
        )
        .bind(event)
        .fetch_one(&pool)
        .await
        .unwrap()
    );
    drain(&pool).await;
    assert_eq!(input(&pool, owner).await.unwrap()["post_version"], 2);
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

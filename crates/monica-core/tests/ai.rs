use monica_core::ai::{
    self,
    provider::{FakeProvider, OpenAiProvider, Provider},
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn lease(kind: &str) -> ai::Lease {
    ai::Lease {
        id: Uuid::new_v4(),
        token: Uuid::new_v4(),
        kind: kind.into(),
        subject: Uuid::new_v4().to_string(),
        model: "synthetic-offline".into(),
        input: json!({"evidence":[{"id":"e1","kind":"post","text":"Synthetic public post","profile_id":"p1"}]}),
        input_hash: "synthetic".into(),
        privacy: json!({"provider":"fake"}),
    }
}
#[test]
fn evidence_and_output_boundaries() {
    let job = lease("ai.profile");
    let output=ai::validate_output(&job,json!({"summary":"Test","items":[{"kind":"company","value":"Fixture Corp","evidence":["invented"],"confidence":"high"},{"kind":"company","value":"Fixture Corp","evidence":["e1","e1","invented"],"confidence":"high","quote":"Synthetic"},{"kind":"password","value":"bad","evidence":["e1"],"confidence":"high"}]})).unwrap();
    assert_eq!(output["items"].as_array().unwrap().len(), 1);
    assert_eq!(output["items"][0]["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(
        output["items"][0]["evidence"][0]["text"],
        "Synthetic public post"
    );
    let digest=ai::validate_output(&lease("ai.digest"),json!({"summary":"Week","life_events":[{"profile_id":"p1","post_id":"e1","kind":"travel","text":"Synthetic trip"},{"profile_id":"wrong","post_id":"e1","kind":"travel","text":"Unshown"}],"suggestions":[{"profile_id":"p1","reason":"Synthetic reason"},{"profile_id":"unknown","reason":"Unshown"}]})).unwrap();
    assert_eq!(digest["life_events"].as_array().unwrap().len(), 1);
    assert_eq!(digest["suggestions"].as_array().unwrap().len(), 1);
    assert!(ai::validate_output(&job, json!([])).is_err());
    assert!(ai::birthday("02-29").is_some());
    assert!(ai::birthday("2025-02-29").is_none());
    assert!(ai::birthday("2999-01-01").is_none());
    assert_eq!(
        ai::fingerprint("company", " Fixture  Corp ", None),
        ai::fingerprint("company", "fixturecorp", None)
    );
    assert!(OpenAiProvider::new("http://example.com/v1", "synthetic-key".into()).is_err());
    assert!(
        OpenAiProvider::new(
            "http://host.docker.internal:4000/v1",
            "synthetic-key".into()
        )
        .is_ok()
    );
    assert!(
        OpenAiProvider::new("https://user:secret@example.com/v1", "synthetic-key".into()).is_err()
    );
}
async fn person(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic AI Contact')").bind(id).execute(pool).await.unwrap();
    id
}

#[sqlx::test(migrations = "../../migrations")]
async fn post_media_requires_real_image_evidence_and_preserves_the_image_role(pool: PgPool) {
    person(&pool).await;
    let post = Uuid::new_v4();
    let image = Uuid::new_v4();
    let old_image = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,event_type,external_id,content,content_hash,version)VALUES($1,'wechat',$1,'post','no-photo','{\"text\":\"This caption is not image OCR\"}','fixture',1)").bind(post).execute(&pool).await.unwrap();
    let error = ai::enqueue(&pool, "ai.post_media", &post.to_string())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "post has no image evidence");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*)FROM crm.ai_jobs WHERE kind='ai.post_media'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path)VALUES($1,repeat('a',64),'image/png',4,'unused-fixture')").bind(image).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role)VALUES('social_post',$1,$2,'media-3')").bind(post).bind(image).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.image_ocr_readings(media_id,status,description,text,model,generated_at)VALUES($1,'complete','Teapots','','fixture',now())").bind(image).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path)VALUES($1,repeat('b',64),'image/png',4,'unused-fixture')").bind(old_image).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role)VALUES('social_post',$1,$2,'media-1')").bind(post).bind(old_image).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_readings(entity_id,entity_type,status,items)VALUES($1,'social_post','complete',$2)").bind(post).bind(json!([{"index":1,"media_id":old_image,"description":"Saved Next city image"},{"index":3,"media_id":image,"description":"Saved Next teapots","text":"Menu words"}])).execute(&pool).await.unwrap();
    let job = ai::enqueue(&pool, "ai.post_media", &post.to_string())
        .await
        .unwrap();
    let input: serde_json::Value = sqlx::query_scalar("SELECT input FROM crm.ai_jobs WHERE id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(input["images"].as_array().unwrap().len(), 2);
    assert_eq!(input["images"][1]["media_id"], image.to_string());
    assert_eq!(input["images"][1]["slot"], 3);
    assert!(
        ai::run_one(
            &pool,
            &FakeProvider {
                output: json!({"summary":"两张图片", "items":[
                    {"index":1,"description":"Saved Next city image","text":"","category":"城市街景"},
                    {"index":2,"description":"Teapots","text":"","category":"食物"}
                ]})
            }
        )
        .await
        .unwrap()
    );
    let saved: serde_json::Value = sqlx::query_scalar(
        "SELECT items FROM crm.media_readings WHERE entity_id=$1 AND entity_type='social_post'",
    )
    .bind(post)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(saved.as_array().unwrap().len(), 2);
    assert!(
        saved
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["media_id"] == old_image.to_string()
                && item["description"] == "Saved Next city image")
    );
    assert!(
        saved
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["media_id"] == image.to_string()
                && item["text"] == "Menu words"
                && item["description"] == "Teapots")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn archived_story_reads_pixels_without_waiting_for_single_image_history(pool: PgPool) {
    use image::ImageEncoder;
    use std::io::{Read, Write};
    person(&pool).await;
    sqlx::query("UPDATE crm.ai_settings SET provider='openai',execution_enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE crm.image_ocr_settings SET enabled=true,daily_limit=1,model='fixture-vision'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let image = Uuid::new_v4();
    let post = Uuid::new_v4();
    let root = tempfile::tempdir().unwrap();
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(
            &[255, 0, 0, 255, 0, 0],
            1,
            2,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    let sha = monica_core::identity::bytes_hash(&bytes);
    let relative = monica_core::media::object_path(&sha).unwrap();
    let path = root.path().join(&relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &bytes).unwrap();
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path)VALUES($1,$2,'image/png',$3,$4)")
        .bind(image).bind(&sha).bind(bytes.len()as i64).bind(&relative).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,event_type,external_id,occurred_at,content,content_hash,version,status)VALUES($1,'instagram',$1,'story','fixture-story',now(),'{\"text\":\"配文\"}','fixture',1,'archived')")
        .bind(post).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role)VALUES('social_story',$1,$2,'media-1')")
        .bind(post).bind(image).execute(&pool).await.unwrap();
    let single_image_job = ai::enqueue(&pool, "ai.image_ocr", &image.to_string())
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.ai_call_reservations(job_id,day)VALUES($1,(now() AT TIME ZONE 'America/Chicago')::date)")
        .bind(single_image_job).execute(&pool).await.unwrap();
    assert_eq!(
        ai::post_media::pending_subjects(&pool, None).await.unwrap(),
        vec![post.to_string()]
    );
    let mut tx = pool.begin().await.unwrap();
    let ingested = ai::post_media::enqueue_import(&mut tx, post, "media-1")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert!(ai::claim(&pool).await.unwrap().is_none());
    tokio::time::sleep(std::time::Duration::from_millis(12_050)).await;
    let failed = ai::claim(&pool).await.unwrap().unwrap();
    assert_eq!(failed.id, ingested);
    ai::fail(&pool, &failed, "network unavailable")
        .await
        .unwrap();
    assert_eq!(
        ai::post_media::pending_subjects(&pool, None).await.unwrap(),
        vec![post.to_string()]
    );
    sqlx::query("UPDATE crm.ai_jobs SET error='image or video frame could not be read'WHERE id=$1")
        .bind(failed.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        ai::post_media::pending_subjects(&pool, None)
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE crm.timeline_events SET version=version+1 WHERE id=$1")
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        ai::post_media::pending_subjects(&pool, None).await.unwrap(),
        vec![post.to_string()]
    );
    ai::enqueue(&pool, "ai.post_media", &post.to_string())
        .await
        .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0; 4096];
        let (head_end, length) = loop {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            raw.extend_from_slice(&chunk[..n]);
            if let Some(n) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&raw[..n]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|s| s.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                break (n + 4, length);
            }
        };
        while raw.len() < head_end + length {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            raw.extend_from_slice(&chunk[..n]);
        }
        let request: serde_json::Value =
            serde_json::from_slice(&raw[head_end..head_end + length]).unwrap();
        let output = json!({"summary":"红色照片","items":[{"index":1,"description":"红色图片","text":"","category":"其他"}]});
        let body = json!({"choices":[{"message":{"content":output.to_string()}}]}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        request
    });
    let provider =
        OpenAiProvider::new(&format!("http://{address}/v1"), "fixture-key".into()).unwrap();
    assert!(
        ai::run_with_media(&pool, &provider, Some(root.path()), None)
            .await
            .unwrap()
    );
    let request = server.join().unwrap();
    assert_eq!(request["messages"][0]["content"], ai::prompts::POST_MEDIA);
    assert_eq!(request["response_format"]["type"], "json_schema");
    assert_eq!(request["messages"][1]["content"][1]["text"], "图 1（照片）");
    assert!(
        request["messages"][1]["content"][2]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    let reading: serde_json::Value =
        sqlx::query_scalar("SELECT items FROM crm.media_readings WHERE entity_id=$1")
            .bind(post)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reading[0]["media_id"], image.to_string());
    assert_eq!(reading[0]["description"], "红色图片");
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM crm.image_ocr_readings WHERE media_id=$1"
        )
        .bind(image)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "complete"
    );
    assert!(
        ai::post_media::pending_subjects(&pool, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*)FROM crm.ai_call_reservations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM crm.ai_jobs WHERE id=$1")
            .bind(single_image_job)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "cancelled"
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(
        ai::post_media::enqueue_import(&mut tx, post, "media-1")
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
}
#[sqlx::test(migrations = "../../migrations")]
async fn input_dedupe_privacy_and_stale_output_fencing(pool: PgPool) {
    let id = person(&pool).await;
    let note = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.activities(id,activity_type,occurred_at,title,body,source_type,content_hash) VALUES($1,'note',now(),'Private synthetic note','PRIVATE-MARKER','manual','synthetic')").bind(note).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.activity_participants VALUES($1,$2)")
        .bind(note)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let first = ai::enqueue(&pool, "ai.profile", &id.to_string())
        .await
        .unwrap();
    assert_eq!(
        first,
        ai::enqueue(&pool, "ai.profile", &id.to_string())
            .await
            .unwrap()
    );
    let old = ai::claim(&pool).await.unwrap().unwrap();
    assert!(!old.input.to_string().contains("PRIVATE-MARKER"));
    sqlx::query("UPDATE crm.profiles SET generation=generation+1,summary='New committed content' WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    let second = ai::enqueue(&pool, "ai.profile", &id.to_string())
        .await
        .unwrap();
    assert_ne!(first, second);
    let current = ai::claim(&pool).await.unwrap().unwrap();
    assert!(
        ai::finish(&pool, &current, json!({"summary":"Current"}))
            .await
            .unwrap()
    );
    assert!(
        !ai::finish(&pool, &old, json!({"summary":"Stale"}))
            .await
            .unwrap()
    );
    let summary: String =
        sqlx::query_scalar("SELECT output->>'summary' FROM crm.ai_outputs WHERE subject=$1")
            .bind(id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(summary, "Current");
    sqlx::query("UPDATE crm.ai_settings SET include_private_notes=true")
        .execute(&pool)
        .await
        .unwrap();
    ai::enqueue(&pool, "ai.profile", &id.to_string())
        .await
        .unwrap();
    let private = ai::claim(&pool).await.unwrap().unwrap();
    assert!(private.input.to_string().contains("PRIVATE-MARKER"));
    sqlx::query("UPDATE crm.ai_settings SET include_private_notes=false")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !ai::finish(&pool, &private, json!({"summary":"Must not publish"}))
            .await
            .unwrap()
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn lease_takeover_cancel_and_fake_provider(pool: PgPool) {
    let id = person(&pool).await;
    let jid = ai::enqueue(&pool, "ai.profile", &id.to_string())
        .await
        .unwrap();
    let old = ai::claim(&pool).await.unwrap().unwrap();
    sqlx::query("UPDATE crm.ai_jobs SET lease_until=now()-interval '1 second' WHERE id=$1")
        .bind(jid)
        .execute(&pool)
        .await
        .unwrap();
    let current = ai::claim(&pool).await.unwrap().unwrap();
    assert_ne!(old.token, current.token);
    assert!(!ai::heartbeat(&pool, &old).await.unwrap());
    assert!(
        !ai::finish(&pool, &old, json!({"summary":"old"}))
            .await
            .unwrap()
    );
    assert!(ai::cancel(&pool, jid).await.unwrap());
    assert!(
        !ai::finish(&pool, &current, json!({"summary":"cancelled"}))
            .await
            .unwrap()
    );
    assert_eq!(
        jid,
        ai::enqueue(&pool, "ai.profile", &id.to_string())
            .await
            .unwrap()
    );
    for _ in 0..32 {
        if !ai::run_one(
            &pool,
            &FakeProvider {
                output: json!({"summary":"Offline complete","items":[]}),
            },
        )
        .await
        .unwrap()
        {
            break;
        }
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.ai_outputs")
        .fetch_one(&pool)
        .await
        .unwrap();
    let states:Vec<serde_json::Value>=sqlx::query_scalar("SELECT jsonb_build_object('kind',kind,'status',status,'error',error,'attempts',attempts) FROM crm.ai_jobs ORDER BY created_at").fetch_all(&pool).await.unwrap();
    assert_eq!(count, 1, "job states: {states:?}");
    assert!(
        !ai::run_one(&pool, &FakeProvider { output: json!({}) })
            .await
            .unwrap()
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn ocr_privacy_and_daily_admission(pool: PgPool) {
    let id = person(&pool).await;
    let account = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat','synthetic-images','person','Synthetic')").bind(account).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.image_ocr_settings SET enabled=true,daily_limit=1")
        .execute(&pool)
        .await
        .unwrap();
    let mut images = Vec::new();
    for n in 0..2 {
        let image = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) VALUES($1,$2,'image/png',0,'synthetic-unused')").bind(image).bind(format!("{n:064x}")).execute(&pool).await.unwrap();
        assert!(
            ai::enqueue(&pool, "ai.image_ocr", &image.to_string())
                .await
                .is_err()
        );
        sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('profile',$1,$2,'photo')").bind(id).bind(image).execute(&pool).await.unwrap();
        assert!(
            ai::enqueue(&pool, "ai.image_ocr", &image.to_string())
                .await
                .is_err()
        );
        let post = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,content,content_hash,version) VALUES($1,'wechat',$1,$2,$3,'post',$4,'{}','synthetic',1)").bind(post).bind(account).bind(id).bind(post.to_string()).execute(&pool).await.unwrap();
        let role = if n == 0 {
            "thumbnail-video-1"
        } else {
            "media-0"
        };
        sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('social_post',$1,$2,$3)").bind(post).bind(image).bind(role).execute(&pool).await.unwrap();
        images.push(
            ai::enqueue(&pool, "ai.image_ocr", &image.to_string())
                .await
                .unwrap(),
        );
    }
    let provider = FakeProvider {
        output: json!({"summary":"Synthetic image","text":"TEST","category":"其他","description":"Offline"}),
    };
    assert!(ai::run_one(&pool, &provider).await.unwrap());
    assert!(!ai::run_one(&pool, &provider).await.unwrap());
    let complete: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.image_ocr_readings WHERE status='complete'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(complete, 1);
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.ai_jobs WHERE status='queued'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queued, 1);
    let chat = Uuid::new_v4();
    let conversation = Uuid::new_v4();
    let message = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) VALUES($1,$2,'image/png',0,'synthetic-chat')").bind(chat).bind(format!("{:064x}",99)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type) VALUES($1,'wechat','synthetic-self','synthetic-group','group')").bind(conversation).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body,message_type) VALUES($1,$2,'wechat','synthetic-self','photo','unknown',now(),'[图片]','3')").bind(message).bind(conversation).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('message_event',$1,$2,'image')").bind(message).bind(chat).execute(&pool).await.unwrap();
    assert!(
        ai::enqueue(&pool, "ai.image_ocr", &chat.to_string())
            .await
            .is_err()
    );
    sqlx::query("UPDATE crm.image_ocr_settings SET include_chat=true")
        .execute(&pool)
        .await
        .unwrap();
    let chat_job = ai::enqueue(&pool, "ai.image_ocr", &chat.to_string())
        .await
        .unwrap();
    sqlx::query("UPDATE crm.image_ocr_settings SET include_chat=false,daily_limit=2")
        .execute(&pool)
        .await
        .unwrap();
    ai::cancel(&pool, images[1]).await.unwrap();
    assert!(!ai::run_one(&pool, &provider).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM crm.ai_jobs WHERE id=$1")
            .bind(chat_job)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "cancelled"
    );
    // Automatic backfill retains complete same-model legacy OCR without
    // requiring its old input hash to match the new implementation's hash.
    sqlx::query(
        "UPDATE crm.image_ocr_readings SET input_hash='legacy-ocr-hash' WHERE status='complete'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let dispatch = ai::enqueue_dispatch(&pool, "ai.image_ocr", "2026-01-01T00", true)
        .await
        .unwrap();
    assert!(ai::dispatch_one(&pool).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT processed_count FROM crm.ai_dispatches WHERE id=$1")
            .bind(dispatch)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn configured_roles_drive_memory_and_preserve_profile_field_evidence(pool: PgPool) {
    use std::{future::Future, pin::Pin, sync::Mutex};
    struct Models(Mutex<Vec<(String, String)>>);
    impl Provider for Models {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn complete<'a>(
            &'a self,
            job: &'a ai::Lease,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<serde_json::Value>> + Send + 'a>> {
            Box::pin(async move {
                self.0
                    .lock()
                    .unwrap()
                    .push((job.kind.clone(), job.model.clone()));
                Ok(json!({"summary":"Synthetic routed summary","facts":[],"gaps":[],"items":[]}))
            })
        }
    }
    let id = person(&pool).await;
    sqlx::query("UPDATE crm.ai_settings SET model='default-model',model_routes=$1").bind(json!({"batch":"saved-batch","person":"saved-person","writer":"saved-writer","qa":"saved-qa"})).execute(&pool).await.unwrap();
    for (key, value, source) in [
        ("linkedin.about", "Synthetic biography", "linkedin"),
        ("wechat.signature", "Synthetic signature", "wechat"),
    ] {
        let revision = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation) VALUES($1,$2,$3,$4,$5,$6,'import')").bind(revision).bind(id).bind(key).bind(json!(value)).bind(revision.to_string()).bind(source).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO crm.field_current(profile_id,field_key,revision_id) VALUES($1,$2,$3)",
        )
        .bind(id)
        .bind(key)
        .bind(revision)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO crm.memory_profile_refresh(profile_id) VALUES($1)")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(ai::memory::refresh_profile_facts(&pool).await.unwrap(), 1);
    assert_eq!(ai::memory::refresh_profile_facts(&pool).await.unwrap(), 0);
    let old = ai::enqueue(&pool, "ai.profile", &id.to_string())
        .await
        .unwrap();
    let model: String = sqlx::query_scalar("SELECT model FROM crm.ai_jobs WHERE id=$1")
        .bind(old)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(model, "saved-person");
    sqlx::query("UPDATE crm.ai_settings SET model_routes=jsonb_set(model_routes,'{person}','\"updated-person\"')").execute(&pool).await.unwrap();
    let provider = Models(Mutex::new(Vec::new()));
    assert!(ai::run_one(&pool, &provider).await.unwrap());
    assert!(provider.0.lock().unwrap().is_empty()); // Stale model was not called.
    for _ in 0..20 {
        ai::memory::legacy::drain(&pool).await.unwrap();
        if !ai::run_one(&pool, &provider).await.unwrap() {
            break;
        }
    }
    {
        let calls = provider.0.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|(k, m)| k == "ai.memory_chunk" && m == "saved-batch"),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|(k, m)| k == "ai.memory_final" && m == "updated-person"),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .all(|(_, m)| m != "saved-person" && m != "default-model")
        );
    }
    // A batch-only settings change must enter normal pending processing even
    // when source generation, privacy and the final/person model are unchanged.
    provider.0.lock().unwrap().clear();
    sqlx::query("UPDATE crm.ai_settings SET model_routes=jsonb_set(model_routes,'{batch}','\"updated-batch\"')").execute(&pool).await.unwrap();
    ai::memory::enqueue_pending(&pool).await.unwrap();
    for _ in 0..20 {
        if !ai::run_one(&pool, &provider).await.unwrap() {
            break;
        }
    }
    assert!(
        provider
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(k, m)| k == "ai.memory_chunk" && m == "updated-batch")
    );
    let chunk: String =
        sqlx::query_scalar("SELECT chunk_model FROM crm.memory_current WHERE profile_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(chunk, "updated-batch");
    ai::memory::enqueue_pending(&pool).await.unwrap();
    assert!(!ai::run_one(&pool, &provider).await.unwrap());
    // A person-only change reuses the completed batch cache.
    provider.0.lock().unwrap().clear();
    sqlx::query("UPDATE crm.ai_settings SET model_routes=jsonb_set(model_routes,'{person}','\"final-person\"')").execute(&pool).await.unwrap();
    ai::memory::enqueue_pending(&pool).await.unwrap();
    for _ in 0..20 {
        if !ai::run_one(&pool, &provider).await.unwrap() {
            break;
        }
    }
    {
        let calls = provider.0.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|(k, m)| k == "ai.memory_final" && m == "final-person")
        );
        assert!(
            calls
                .iter()
                .all(|(k, m)| k != "ai.memory_chunk" && m == "final-person")
        );
    }
    let facts:serde_json::Value=sqlx::query_scalar("SELECT payload FROM crm.memory_inputs WHERE profile_id=$1 AND source_kind='profile' AND source_id='current'").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(facts["fields"]["linkedin.about"], "Synthetic biography");
    assert_eq!(facts["fields"]["wechat.signature"], "Synthetic signature");
    assert_eq!(
        facts["field_evidence"]["linkedin.about"]["source_type"],
        "linkedin"
    );
    assert!(facts["field_evidence"]["wechat.signature"]["observed_at"].is_string());
    assert_eq!(
        ai::models::resolve("default", &json!({"writer":"saved-writer"}), "ai.qa"),
        "saved-writer"
    );
}
async fn loopback_model_response(
    output: serde_json::Value,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let n = stream.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..pos]);
                let length = headers
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|n| n.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= pos + 4 + length {
                    break;
                }
            }
        }
        let request = String::from_utf8(bytes).unwrap();
        assert!(request.starts_with("POST /v1/chat/completions"));
        assert!(request.contains("Bearer synthetic-only-key"));
        assert!(request.contains("json_object"));
        let body = json!({"choices":[{"message":{"content":output.to_string()}}]}).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (format!("http://{address}/v1"), service)
}
#[tokio::test]
async fn openai_transport_against_loopback_fake() {
    let (base, service) = loopback_model_response(json!({"summary":"Loopback only"})).await;
    let client = OpenAiProvider::new(&base, "synthetic-only-key".into()).unwrap();
    let output = client.complete(&lease("ai.profile")).await.unwrap();
    assert_eq!(output["summary"], "Loopback only");
    service.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn configured_ready_work_progresses_while_dispatch_is_blocked(pool: PgPool) {
    let (base, service) =
        loopback_model_response(json!({"label":"Synthetic community","reason":"Fixture"})).await;
    let cipher = monica_core::credentials::CredentialCipher::new(
        "synthetic-installation-secret-at-least-32",
    )
    .unwrap();
    let encrypted = cipher
        .encrypt(&json!({"api_key":"synthetic-only-key"}))
        .unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=true,provider='openai',base_url=$1,credential_ciphertext=$2").bind(base).bind(encrypted).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.ai_community_inputs(community_id,input,version_hash)VALUES('fixture-community','{}','v1')").execute(&pool).await.unwrap();
    let job = ai::enqueue(&pool, "ai.community", "fixture-community")
        .await
        .unwrap();
    let mut planning = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE crm.ai_dispatches IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *planning)
        .await
        .unwrap();
    let media = tempfile::tempdir().unwrap();
    let budget = monica_core::resources::ResourceBudget::new(Default::default()).unwrap();
    let progressed = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        ai::run_configured(&pool, media.path(), &budget, Some(&cipher)),
    )
    .await;
    planning.rollback().await.unwrap();
    assert!(
        progressed
            .expect("ready model work must not wait for dispatch preparation")
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM crm.ai_jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "complete"
    );
    service.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn weekly_facts_and_person_evidence_keep_dates_and_authors(pool: PgPool) {
    let id = person(&pool).await;
    let other = person(&pool).await;
    sqlx::query("UPDATE crm.ai_settings SET include_messages=true")
        .execute(&pool)
        .await
        .unwrap();
    let direct = Uuid::new_v4();
    let group = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,profile_id,conversation_type) VALUES($1,'wechat','self','friend',$3,'direct'),($2,'wechat','self','room',NULL,'group')").bind(direct).bind(group).bind(id).execute(&pool).await.unwrap();
    for (i, conversation, sender, author, direction, at, text) in [
        (
            0,
            direct,
            "self",
            None,
            "sent",
            "2026-01-05T06:00:00Z",
            "OWN_WORDS",
        ),
        (
            1,
            direct,
            "friend",
            Some(id),
            "received",
            "2026-01-07T12:00:00Z",
            "CONTACT_WORDS",
        ),
        (
            2,
            group,
            "friend",
            Some(id),
            "received",
            "2026-01-08T12:00:00Z",
            "CONTACT_GROUP_WORDS",
        ),
        (
            3,
            group,
            "peer",
            Some(other),
            "received",
            "2026-01-08T13:00:00Z",
            "UNRELATED_GROUP_WORDS",
        ),
        (
            4,
            direct,
            "friend",
            Some(id),
            "received",
            "2026-01-12T06:00:00Z",
            "NEXT_WEEK_WORDS",
        ),
    ] {
        sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_profile_id,direction,occurred_at,body) VALUES($1,$2,'wechat','self',$3,$4,$5,$6,$7::timestamptz,$8)").bind(Uuid::new_v4()).bind(conversation).bind(i.to_string()).bind(sender).bind(author).bind(direction).bind(at).bind(text).execute(&pool).await.unwrap();
    }
    let revision = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,observed_at) VALUES($1,$2,'monica.company','\"OLD_BACKGROUND\"','old','manual','set','2020-01-01')").bind(revision).bind(id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.field_current VALUES($1,'monica.company',$2)")
        .bind(id)
        .bind(revision)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.profile_dates(id,profile_id,kind,date_precision,month,day) VALUES($1,$2,'birthday','month_day',1,12)").bind(Uuid::new_v4()).bind(id).execute(&pool).await.unwrap();
    let post = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) VALUES($1,'wechat',$1,$2,'post','weekly-post','2026-01-06T12:00:00Z','{\"text\":\"WEEK_POST\"}','post',1)").bind(post).bind(id).execute(&pool).await.unwrap();
    while ai::week_window::refresh_observations(&pool).await.unwrap() > 0 {}
    let mut tx = pool.begin().await.unwrap();
    let data = ai::snapshot(&mut tx, "ai.person_week", &format!("{id}:2026-01-05"))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM crm.ai_week_sources('2026-01-05',$1,true,false) ORDER BY source_key",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(data.input["window"]["source_count"], rows.len());
    let text = serde_json::to_string(&rows).unwrap();
    assert!(text.contains("CONTACT_GROUP_WORDS"));
    assert!(!text.contains("UNRELATED_GROUP_WORDS"));
    assert!(!text.contains("NEXT_WEEK_WORDS"));
    assert!(!text.contains("OLD_BACKGROUND"));
    assert_eq!(
        rows.iter().find(|v| v["text"] == "OWN_WORDS").unwrap()["speaker"],
        "我"
    );
    assert_eq!(
        rows.iter().find(|v| v["text"] == "CONTACT_WORDS").unwrap()["speaker"],
        "TA"
    );
    let source = rows
        .iter()
        .find(|v| v["text"] == "CONTACT_GROUP_WORDS")
        .unwrap();
    let resolved = ai::resolve_evidence(&json!({"evidence":rows}), &json!([source["id"]]));
    assert_eq!(resolved[0]["kind"], "group");
    assert!(
        resolved[0]["citation"]
            .as_str()
            .unwrap()
            .contains(&group.to_string())
    );
    let facts = ai::weekly::facts(&mut tx, "2026-01-05".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(facts["totals"], json!({"messages":4,"people":1,"posts":1}));
    assert_eq!(facts["top_contacts"][0]["messages"], 2);
    assert_eq!(facts["upcoming_dates"][0]["date"], "2026-01-12");
    let digest = ai::snapshot(&mut tx, "ai.digest", "2026-01-05")
        .await
        .unwrap();
    assert_eq!(digest.input["facts"], facts);
    tx.rollback().await.unwrap();
    ai::enqueue(&pool, "ai.digest", "2026-01-05").await.unwrap();
    let provider = FakeProvider {
        output: json!({"summary":"Synthetic digest","life_events":[],"suggestions":[{"profile_id":id,"reason":"Supported by actual contact counts"}],"facts":{"totals":{"messages":999999}}}),
    };
    for _ in 0..100 {
        if !ai::run_one(&pool, &provider).await.unwrap() {
            break;
        }
    }
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT output FROM crm.weekly_digests WHERE week_start='2026-01-05'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored["facts"], facts);
    assert_eq!(stored["suggestions"].as_array().unwrap().len(), 1);
}

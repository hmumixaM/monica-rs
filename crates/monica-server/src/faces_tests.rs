use super::*;
use axum::{body::to_bytes, http::Request};
use tower::ServiceExt;
const SECRET: &str = "synthetic-face-device-signing-secret-123";
fn signed(method: &str, path: &str, body: Value, nonce: &str) -> Request<Body> {
    let bytes = if method == "GET" {
        Vec::new()
    } else {
        serde_json::to_vec(&body).unwrap()
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(format!("{stamp}.{nonce}.{method} {path}\n").as_bytes());
    mac.update(&bytes);
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "testserver")
        .header("content-type", "application/json")
        .header("x-import-timestamp", stamp)
        .header("x-import-nonce", nonce)
        .header(
            "x-import-signature",
            format!("{:x}", mac.finalize().into_bytes()),
        )
        .header("x-import-token", "synthetic-face-token")
        .body(Body::from(bytes))
        .unwrap()
}
fn session(method: &str, path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "testserver")
        .header("cookie", "crm_session=synthetic-session; crm_csrf=fixture")
        .header("x-csrf-token", "fixture")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}
async fn value(app: &Router, req: Request<Body>, expected: StatusCode) -> Value {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn signed_fake_device_roundtrip_and_review_reads(pool: sqlx::PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(
        pool.clone(),
        "synthetic-session-signing-1234567890",
        "http://testserver".into(),
        dir.path().into(),
    )
    .await
    .unwrap();
    state.import_hmac_secret = Some(SECRET.into());
    let actor = Uuid::new_v4();
    let profile = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'fixture@example.invalid','synthetic')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"synthetic-session"))).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic face profile')").bind(profile).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.import_service_tokens(id,source,label,token_hash) VALUES($1,'faces','synthetic',$2)").bind(Uuid::new_v4()).bind(format!("{:x}",Sha256::digest(b"synthetic-face-token"))).execute(&pool).await.unwrap();
    let media = Uuid::new_v4();
    let hidden = Uuid::new_v4();
    let bytes = b"synthetic-tar-bytes";
    let hash = format!("{:x}", Sha256::digest(bytes));
    let path = monica_core::media::object_path(&hash).unwrap();
    tokio::fs::create_dir_all(dir.path().join(&path).parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(dir.path().join(&path), bytes)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,width,height) VALUES($1,$2,'image/png',18,$3,100,100),($4,$5,'image/png',5,'hidden',1,1)").bind(media).bind(hash).bind(path).bind(hidden).bind("a".repeat(64)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_links VALUES('profile',$1,$2,'manual_photo',now(),NULL),('message_event',$3,$4,'media-0',now(),NULL)").bind(profile).bind(media).bind(Uuid::new_v4()).bind(hidden).execute(&pool).await.unwrap();
    let app = crate::auth::routes(state);
    let summary = value(
        &app,
        session("GET", "/api/v1/faces/summary", Value::Null),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        summary,
        json!({"scanned":0,"last_scan_at":null,"faces":0,"to_review":0,"confirmed":0,"people":0})
    );
    let initial = value(
        &app,
        signed("GET", "/api/v1/faces/device/status", Value::Null, "status"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(initial["state"], "needs_models");
    let work = value(
        &app,
        signed(
            "GET",
            "/api/v1/faces/device/work?limit=10",
            Value::Null,
            "work",
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(work["items"].as_array().unwrap().len(), 1);
    assert_eq!(work["items"][0]["media_id"], media.to_string());
    value(
        &app,
        signed(
            "GET",
            "/api/v1/faces/device/work?limit=10",
            Value::Null,
            "work",
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let mut tampered = signed(
        "GET",
        "/api/v1/faces/device/work?limit=10",
        Value::Null,
        "tampered",
    );
    *tampered.uri_mut() = "/api/v1/faces/device/work?limit=11".parse().unwrap();
    value(&app, tampered, StatusCode::UNAUTHORIZED).await;
    let response = app
        .clone()
        .oneshot(signed(
            "POST",
            "/api/v1/faces/device/media",
            json!({"ids":[media,hidden]}),
            "tar",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let tar = to_bytes(response.into_body(), 10000).await.unwrap();
    assert_eq!(&tar[..36], media.to_string().as_bytes());
    assert_eq!(&tar[512..512 + bytes.len()], bytes);
    assert_eq!(tar.len(), 2048);
    let face = Uuid::new_v4();
    let batch = json!({"model":"synthetic-fake-boundary","scans":[{"media_id":media,"faces":[{"id":face,"box":[0.1,0.2,0.3,0.4],"quality":0.9}]}],"suggestions":[{"face_id":face,"cluster_key":"fixture-cluster","profile_id":profile,"score":0.9}]});
    value(
        &app,
        signed("POST", "/api/v1/faces/device/batch", batch.clone(), "batch"),
        StatusCode::OK,
    )
    .await;
    let undo = value(
        &app,
        session(
            "POST",
            "/api/v1/faces/decide",
            json!({"face_ids":[face],"action":"confirm","profile_id":profile}),
        ),
        StatusCode::OK,
    )
    .await;
    value(
        &app,
        signed("POST", "/api/v1/faces/device/batch", batch, "replay"),
        StatusCode::OK,
    )
    .await;
    for path in [
        "/api/v1/faces/summary".to_string(),
        "/api/v1/faces/groups".into(),
        format!("/api/v1/faces/in-media?ids={media}"),
        format!("/api/v1/faces/people/{profile}"),
        format!("/api/v1/faces/people/{profile}/photos"),
        format!("/api/v1/faces/people/{profile}/photos?sort=score"),
        format!("/api/v1/faces/people/{profile}/photos/status"),
        format!("/api/v1/faces/media/{media}/reading"),
    ] {
        value(&app, session("GET", &path, Value::Null), StatusCode::OK).await;
    }
    assert_eq!(
        value(
            &app,
            session("GET", "/api/v1/faces/summary", Value::Null),
            StatusCode::OK
        )
        .await["confirmed"],
        1
    );
    value(
        &app,
        session("POST", "/api/v1/faces/restore", undo["previous"].clone()),
        StatusCode::OK,
    )
    .await;
    value(
        &app,
        signed(
            "GET",
            "/api/v1/faces/device/anchors",
            Value::Null,
            "anchors",
        ),
        StatusCode::OK,
    )
    .await;
    // A null score sorts after scored suggestions, and continuation must not
    // duplicate either image when the page boundary crosses those groups.
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('profile',$1,$2,'manual_photo')").bind(profile).bind(hidden).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.faces(id,media_id,box,quality,status,profile_id) VALUES($1,$2,ARRAY[0.1,0.2,0.3,0.4]::real[],0.9,'confirmed',$3)").bind(Uuid::new_v4()).bind(hidden).bind(profile).execute(&pool).await.unwrap();
    let page = value(
        &app,
        session(
            "GET",
            &format!("/api/v1/faces/people/{profile}/photos?sort=score&limit=1"),
            Value::Null,
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(page["counts"]["all"], 2);
    assert_eq!(page["items"][0]["media_id"], media.to_string());
    let cursor = page["next_cursor"].as_str().unwrap();
    let next = value(
        &app,
        session(
            "GET",
            &format!("/api/v1/faces/people/{profile}/photos?sort=score&limit=1&cursor={cursor}"),
            Value::Null,
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(next["items"][0]["media_id"], hidden.to_string());
    assert!(next["next_cursor"].is_null());
    // Saved OCR/social readings are exact read-only projections. No provider
    // job is enqueued when the gallery asks for these annotations.
    sqlx::query("INSERT INTO crm.image_ocr_readings(media_id,status,summary,text,model,input_hash) VALUES($1,'complete','Synthetic OCR','fixture words','offline-fake','private-version')").bind(media).execute(&pool).await.unwrap();
    let ocr = value(
        &app,
        session("GET", &format!("/api/v1/media/{media}/ocr"), Value::Null),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ocr["text"], "fixture words");
    assert_eq!(ocr.as_object().unwrap().len(), 8);
    assert!(ocr.get("input_hash").is_none());
    let account = Uuid::new_v4();
    let post = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram','synthetic-reading','person','Synthetic reading')").bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,event_type,external_id,content,content_hash,version) VALUES($1,'instagram',$1,$2,'post','synthetic-reading','{}','fixture',1)").bind(post).bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('social_post',$1,$2,'media-0')").bind(post).bind(media).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_readings(entity_id,entity_type,status,summary,items) VALUES($1,'social_post','complete','Synthetic saved reading','[{\"kind\":\"scene\"}]')").bind(post).execute(&pool).await.unwrap();
    let reading = value(
        &app,
        session(
            "GET",
            &format!("/api/v1/faces/media/{media}/reading"),
            Value::Null,
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        reading,
        json!({"readings":[{"kind":"social_post","entity_id":post,"summary":"Synthetic saved reading","items":[{"kind":"scene"}]}]})
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.ai_jobs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let bad_cursor=URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"profile":profile,"state":"all","sort":"score","id":media,"time":"2026-01-01T00:00:00Z","score":"invalid"})).unwrap());
    value(
        &app,
        session(
            "GET",
            &format!("/api/v1/faces/people/{profile}/photos?sort=score&cursor={bad_cursor}"),
            Value::Null,
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    sqlx::query("UPDATE crm.import_service_tokens SET revoked_at=now()")
        .execute(&pool)
        .await
        .unwrap();
    value(
        &app,
        signed("GET", "/api/v1/faces/device/work", Value::Null, "revoked"),
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

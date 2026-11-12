use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;
async fn call(app: &Router, method: &str, path: &str, value: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:17494")
        .header(
            "cookie",
            "crm_session=synthetic-connections; crm_csrf=fixture",
        )
        .header("x-csrf-token", "fixture")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    (status, value)
}
async fn setup(pool: &sqlx::PgPool, root: &std::path::Path) -> Router {
    let user = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic-connections@example.invalid','unused')").bind(user).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(user).bind(monica_core::identity::bytes_hash(b"synthetic-connections")).execute(pool).await.unwrap();
    crate::auth::routes(
        AppState::new(
            pool.clone(),
            "synthetic-signing-key-at-least-32-bytes",
            "http://127.0.0.1:17494".into(),
            root.into(),
        )
        .await
        .unwrap(),
    )
}
#[sqlx::test(migrations = "../../migrations")]
async fn encrypted_connections_actions_oauth_and_identity(pool: sqlx::PgPool) {
    let root = tempfile::tempdir().unwrap();
    let app = setup(&pool, root.path()).await;
    let (status, catalogue) = call(&app, "GET", "/api/v1/sources", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalogue.as_array().unwrap().len(), 7);
    let config = json!({"provider":"mail","external_account":"SELF@example.invalid","display_name":"Synthetic inbox","connection_method":"imap","host":"imap.example.invalid","password":"synthetic-mail-password"});
    let (status, value) = call(&app, "POST", "/api/v1/source-connections", config).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let id = value["id"].as_str().unwrap();
    let (_, list) = call(&app, "GET", "/api/v1/source-connections", Value::Null).await;
    assert_eq!(
        list.as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"].as_str() == Some(id))
            .unwrap()["external_account"],
        "self@example.invalid"
    );
    assert!(!list.to_string().contains("password"));
    let cipher: String = sqlx::query_scalar(
        "SELECT credential_ciphertext FROM crm.source_connections WHERE provider='mail'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!cipher.contains("synthetic"));
    let lease = Uuid::new_v4();
    sqlx::query(
        "UPDATE crm.source_connections SET lease_token=$1,lease_until=now()+interval '1 minute' WHERE provider='mail'",
    )
    .bind(lease)
    .execute(&pool)
    .await
    .unwrap();
    let (status, value) = call(
        &app,
        "POST",
        &format!("/api/v1/source-connections/{id}/action"),
        json!({"action":"pause"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let (enabled, lease): (bool, Option<Uuid>) = sqlx::query_as(
        "SELECT enabled,lease_token FROM crm.source_connections WHERE provider='mail'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!enabled);
    assert!(lease.is_none());
    let (status, value) = call(
        &app,
        "POST",
        "/api/v1/mail-oauth/start",
        json!({"provider":"gmail","client_id":"synthetic-client"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(
        value["authorization_url"]
            .as_str()
            .unwrap()
            .contains("code_challenge_method=S256")
    );
    assert!(!value.to_string().contains("verifier"));
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/source-connections/{id}/action"),
        json!({"action":"disconnect"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cipher: Option<String> = sqlx::query_scalar(
        "SELECT credential_ciphertext FROM crm.source_connections WHERE provider='mail'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(cipher.is_none());
}
#[sqlx::test(migrations = "../../migrations")]
async fn evidence_revisions_unicode_filtering_and_relink(pool: sqlx::PgPool) {
    let root = tempfile::tempdir().unwrap();
    let app = setup(&pool, root.path()).await;
    let cid = core::connection_id(Provider::Mail, "synthetic@example.invalid");
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES($1,'mail','synthetic@example.invalid','Synthetic','imap')").bind(cid).execute(&pool).await.unwrap();
    let participant = json!({"id":"sender","name":"Synthetic Sender","emails":["sender@example.invalid"],"role":"sender"});
    for body in ["一🦆三四", "corrected"] {
        let mut tx = pool.begin().await.unwrap();
        core::commit_page(&mut tx,Provider::Mail,cid,"inbox",&[json!({"external_id":"one","kind":"message","body":body,"title":"Synthetic","occurred_at":"2026-01-01T00:00:00Z","participants":[participant],"included":true,"thread_id":"thread-one"})],&json!({"next":body})).await.unwrap();
        tx.commit().await.unwrap();
        core::deliver(&pool, Provider::Mail, cid, 100)
            .await
            .unwrap();
    }
    let id = core::evidence_id(cid, "one");
    let (status, value) = call(&app, "GET", "/api/v1/evidence", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value.as_array().unwrap().len(), 1);
    assert_eq!(value[0]["body"], "corrected");
    let (status, value) = call(
        &app,
        "GET",
        &format!("/api/v1/evidence/{id}?revision=1&body_offset=1&body_limit=2"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["body"], "🦆三");
    assert_eq!(value["next_body_offset"], 3);
    assert_eq!(value["total_characters"], 4);
    assert_eq!(value["revision"], 1);
    let profile = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Confirmed')",
    )
    .bind(profile)
    .execute(&pool)
    .await
    .unwrap();
    let (status, value) = call(
        &app,
        "POST",
        &format!("/api/v1/source-connections/{cid}/identity"),
        json!({"external_id":"sender","action":"link","profile_id":profile}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let (status, value) = call(
        &app,
        "GET",
        &format!("/api/v1/source-connections/{cid}/people"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value[0]["profile_id"], json!(profile));
    let (status, value) = call(
        &app,
        "POST",
        &format!("/api/v1/source-connections/{cid}/mail-rules"),
        json!({"scope":"thread","target":"thread-one","category":"marketing"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["queued"], 1);
    let (status, value) = call(
        &app,
        "POST",
        &format!("/api/v1/source-connections/{cid}/mail-rules"),
        json!({"scope":"sender","target":"sender","category":"personal"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["queued"], 1);
    let (status, value) = call(
        &app,
        "GET",
        &format!("/api/v1/profiles/{profile}/source-coverage"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn phone_export_replay_and_revision_owned_original_download(pool: sqlx::PgPool) {
    let root = tempfile::tempdir().unwrap();
    let app = setup(&pool, root.path()).await;
    let boundary = "synthetic-export-boundary";
    let mut data = String::new();
    for (key, value) in [
        ("conversation", "Synthetic chat"),
        ("owner", "Me"),
        ("timezone", "America/Chicago"),
        ("date_order", "mdy"),
        ("conversation_kind", "direct"),
    ] {
        data.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n"
        ));
    }
    data.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"synthetic.txt\"\r\nContent-Type: text/plain\r\n\r\n11/11/2026, 09:01 - Me: Hello\n11/11/2026, 09:02 - Peer: Hi\r\n--{boundary}--\r\n"));
    data = data.replace("Me: Hello", &format!("Me: Hello {}", "x".repeat(70_000)));
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/whatsapp/phone-export")
            .header("host", "127.0.0.1:17494")
            .header(
                "cookie",
                "crm_session=synthetic-connections; crm_csrf=fixture",
            )
            .header("x-csrf-token", "fixture")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(data.clone()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["records_read"], 2);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM whatsapp.connection_revision_refs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    let (_, evidence) = call(&app, "GET", "/api/v1/evidence", Value::Null).await;
    let id = evidence[0]["id"].as_str().unwrap();
    let (status, detail) = call(&app, "GET", &format!("/api/v1/evidence/{id}"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    let digest = detail["metadata"]["original"]["blob"].as_str().unwrap();
    let request = Request::builder()
        .uri(format!("/api/v1/evidence/{id}/files/{digest}?revision=1"))
        .header("host", "127.0.0.1:17494")
        .header("cookie", "crm_session=synthetic-connections")
        .header("range", "bytes=0-9")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(response.headers()["etag"], format!("\"{digest}\"").as_str());
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().len(),
        10
    );
    let request = Request::builder()
        .uri(format!("/api/v1/evidence/{id}/files/{digest}?revision=1"))
        .header("host", "127.0.0.1:17494")
        .header("cookie", "crm_session=synthetic-connections")
        .header("if-none-match", format!("\"{digest}\""))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::NOT_MODIFIED
    );
    let request = Request::builder()
        .uri(format!("/api/v1/evidence/{id}/files/{digest}?revision=1"))
        .header("host", "127.0.0.1:17494")
        .header("cookie", "crm_session=synthetic-connections")
        .header("range", "bytes=0-9")
        .header("if-range", "\"different\"")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(to_bytes(response.into_body(), 100000).await.unwrap().len() > 70000);

    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/evidence/{id}/files/{}", "a".repeat(64)),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn undated_evidence_pages_and_restored_contacts_keep_fields(pool: sqlx::PgPool) {
    let root = tempfile::tempdir().unwrap();
    let app = setup(&pool, root.path()).await;
    let cid = core::connection_id(Provider::AppleContacts, "local");
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES($1,'apple_contacts','local','Synthetic Contacts','local')").bind(cid).execute(&pool).await.unwrap();
    let record = json!({"external_id":"contact-one","kind":"contact","emails":["test@example.invalid"],"phones":[],"participants":[{"id":"person-one","name":"Synthetic Person","role":"contact"}]});
    let others = (0..4).map(|n|json!({"external_id":format!("other-{n}"),"kind":"message","body":"undated","participants":[]})).collect::<Vec<_>>();
    let mut records = vec![record.clone()];
    records.extend(others);
    let mut tx = pool.begin().await.unwrap();
    core::commit_page(
        &mut tx,
        Provider::AppleContacts,
        cid,
        "contacts",
        &records,
        &json!({"position":1}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    core::deliver(&pool, Provider::AppleContacts, cid, 100)
        .await
        .unwrap();
    let (_, catalogue) = call(&app, "GET", "/api/v1/sources", Value::Null).await;
    assert_eq!(
        catalogue
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "apple_contacts")
            .unwrap()["collection_available"],
        false
    );
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/source-connections/{cid}/action"),
        json!({"action":"resume"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let mut ids = std::collections::BTreeSet::new();
    let mut path = format!("/api/v1/evidence?connection_id={cid}&limit=2");
    loop {
        let (status, page) = call(&app, "GET", &path, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        let page = page.as_array().unwrap();
        if page.is_empty() {
            break;
        }
        for entry in page {
            assert!(
                ids.insert(entry["id"].as_str().unwrap().to_owned()),
                "pagination repeated an undated row"
            );
        }
        path = format!(
            "/api/v1/evidence?connection_id={cid}&limit=2&before_id={}",
            page.last().unwrap()["id"].as_str().unwrap()
        );
    }
    assert_eq!(ids.len(), 5);
    let profile: Uuid = sqlx::query_scalar("SELECT profile_id FROM crm.connection_identity_links WHERE connection_id=$1 AND external_id='person-one'").bind(cid).fetch_one(&pool).await.unwrap();
    for deleted in [true, false] {
        let mut next = record.clone();
        next["deleted"] = json!(deleted);
        let mut tx = pool.begin().await.unwrap();
        core::commit_page(
            &mut tx,
            Provider::AppleContacts,
            cid,
            "contacts",
            &[next],
            &json!({"deleted":deleted}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        core::deliver(&pool, Provider::AppleContacts, cid, 100)
            .await
            .unwrap();
        let current: Option<Value> = sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key='apple_contacts.emails'").bind(profile).fetch_optional(&pool).await.unwrap();
        assert_eq!(
            current,
            if deleted {
                None
            } else {
                Some(json!(["test@example.invalid"]))
            }
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn connection_posts_keep_media_and_interactions_without_wrong_memory_owner(
    pool: sqlx::PgPool,
) {
    let root = tempfile::tempdir().unwrap();
    let app = setup(&pool, root.path()).await;
    let cid = core::connection_id(Provider::Wechat, "synthetic-wechat");
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,'wechat','synthetic-wechat','Synthetic WeChat','local',true,'connected')").bind(cid).execute(&pool).await.unwrap();
    let contacts = [
        json!({"external_id":"contact:author","kind":"contact","participants":[{"id":"author","name":"Synthetic Author","role":"contact"}]}),
        json!({"external_id":"contact:peer","kind":"contact","participants":[{"id":"peer","name":"Synthetic Peer","role":"contact"}]}),
    ];
    let mut tx = pool.begin().await.unwrap();
    core::commit_page(
        &mut tx,
        Provider::Wechat,
        cid,
        "contacts",
        &contacts,
        &json!({"page":1}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    core::deliver(&pool, Provider::Wechat, cid, 100)
        .await
        .unwrap();
    let author:Uuid=sqlx::query_scalar("SELECT profile_id FROM crm.connection_identity_links WHERE connection_id=$1 AND external_id='author'").bind(cid).fetch_one(&pool).await.unwrap();
    let peer:Uuid=sqlx::query_scalar("SELECT profile_id FROM crm.connection_identity_links WHERE connection_id=$1 AND external_id='peer'").bind(cid).fetch_one(&pool).await.unwrap();
    let post = json!({"external_id":"post:12","kind":"post","body":"Synthetic Moments","occurred_at":"2026-01-01T00:00:00Z","coordinate_format":"chatlog_sns_xml","location":{"latitude":"48.85","longitude":"2.35"},"media":[{"media_type":"image","source_url":"https://example.invalid/photo.jpg"}],"participants":[{"id":"author","role":"author"},{"id":"peer","role":"interactor"}],"interactions":[{"external_id":"12:comment:99","interaction_type":"comment","author_external_id":"peer","author_name":"Synthetic Peer","occurred_at":"2026-01-01T00:00:02Z","text":"Synthetic comment","media":[{"source_url":"https://example.invalid/comment.jpg"}]}]});
    let mut tx = pool.begin().await.unwrap();
    core::commit_page(
        &mut tx,
        Provider::Wechat,
        cid,
        "moments",
        &[post],
        &json!({"page":1}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    core::deliver(&pool, Provider::Wechat, cid, 100)
        .await
        .unwrap();
    let eid = core::evidence_id(cid, "post:12");
    let (status, detail) = call(&app, "GET", &format!("/api/v1/timeline/{eid}"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["media"][0]["source_url"],
        "https://example.invalid/photo.jpg"
    );
    assert_eq!(
        detail["interactions"][0]["content"]["text"],
        "Synthetic comment"
    );
    assert_eq!(detail["interactions"][0]["author_profile_id"], json!(peer));
    assert_eq!(
        detail["interactions"][0]["media"][0]["source_url"],
        "https://example.invalid/comment.jpg"
    );
    let owners:Vec<(Uuid,bool)>=sqlx::query_as("SELECT profile_id,private FROM crm.memory_inputs WHERE source_kind='connection' AND source_id=$1 ORDER BY profile_id").bind(eid.to_string()).fetch_all(&pool).await.unwrap();
    assert_eq!(owners, vec![(author, false)]);
    let mut corrected = sqlx::query_scalar::<_, Value>("SELECT payload FROM wechat.connection_records WHERE connection_id=$1 AND external_id='post:12'").bind(cid).fetch_one(&pool).await.unwrap();
    corrected["interactions"][0]["interaction_type"] = json!("reply");
    let mut tx = pool.begin().await.unwrap();
    core::commit_page(
        &mut tx,
        Provider::Wechat,
        cid,
        "moments",
        &[corrected],
        &json!({"page":2}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    core::deliver(&pool, Provider::Wechat, cid, 100)
        .await
        .unwrap();
    let types: Vec<String> =
        sqlx::query_scalar("SELECT interaction_type FROM crm.social_interactions WHERE post_id=$1")
            .bind(eid)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(types, vec!["reply".to_owned()]);
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/source-connections/{cid}/identity"),
        json!({"external_id":"peer","action":"unlink"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    core::relink_pending(&pool, 100).await.unwrap();
    let linked: Option<Uuid> = sqlx::query_scalar(
        "SELECT author_profile_id FROM crm.social_interactions WHERE post_id=$1",
    )
    .bind(eid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(linked.is_none());
}

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;
fn request(method: &str, path: &str, value: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "testserver")
        .header(
            "cookie",
            "crm_session=synthetic-ai-session; crm_csrf=fixture",
        )
        .header("x-csrf-token", "fixture")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap()
}
async fn response(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    status: StatusCode,
) -> Value {
    let response = app
        .clone()
        .oneshot(request(method, path, body))
        .await
        .unwrap();
    let actual = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(actual, status, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn authenticated_fake_ai_decisions_are_atomic_and_private_outputs_hide(pool: sqlx::PgPool) {
    let root = tempfile::tempdir().unwrap();
    let state = AppState::new(
        pool.clone(),
        "synthetic-session-signing-key-1234567890",
        "http://testserver".into(),
        root.path().into(),
    )
    .await
    .unwrap();
    let actor = Uuid::new_v4();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic-ai@example.invalid','unused')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(actor).bind(monica_core::identity::bytes_hash(b"synthetic-ai-session")).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic AI Profile')").bind(id).execute(&pool).await.unwrap();
    let app = crate::auth::routes(state);
    assert_eq!(
        response(
            &app,
            "GET",
            "/api/v1/ai/status",
            Value::Null,
            StatusCode::OK
        )
        .await,
        json!({"enabled":false})
    );
    response(
        &app,
        "PATCH",
        "/api/v1/ai/image-ocr",
        json!({"enabled":true}),
        StatusCode::NOT_FOUND,
    )
    .await;
    let browser = response(
        &app,
        "GET",
        "/api/v1/ai/browser-model",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(browser["enabled"], false);
    assert_eq!(browser["context_window"], 32768);
    assert_eq!(browser["max_tokens"], 8192);
    response(
        &app,
        "POST",
        "/api/v1/ai/chat/completions",
        json!({"messages":[{"role":"user","content":"Synthetic request"}]}),
        StatusCode::CONFLICT,
    )
    .await;
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    response(
        &app,
        "PATCH",
        &format!("/api/v1/profiles/{id}/fields"),
        json!({"field_key":"monica.company","value":"Synthetic Original"}),
        StatusCode::OK,
    )
    .await;
    ai::enqueue(&pool, "ai.profile", &id.to_string())
        .await
        .unwrap();
    let lease = ai::claim(&pool).await.unwrap().unwrap();
    let evidence = lease.input["evidence"][0]["id"].clone();
    ai::finish(&pool,&lease,json!({"summary":"Synthetic memory","items":[{"kind":"company","value":"Synthetic New","evidence":[evidence],"quote":"Fixture only","confidence":"high"}]})).await.unwrap();
    let visible = response(
        &app,
        "GET",
        &format!("/api/v1/ai/profiles/{id}"),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(visible["memory"]["summary"], "Synthetic memory");
    assert_eq!(visible["suggestions"].as_array().unwrap().len(), 1);
    let key = visible["suggestions"][0]["fingerprint"].as_str().unwrap();
    let path = format!("/api/v1/ai/suggestions/{id}/{key}");
    response(
        &app,
        "POST",
        &path,
        json!({"action":"ignore"}),
        StatusCode::OK,
    )
    .await;
    response(&app, "DELETE", &path, Value::Null, StatusCode::OK).await;
    let a = app
        .clone()
        .oneshot(request("POST", &path, json!({"action":"accept"})));
    let b = app
        .clone()
        .oneshot(request("POST", &path, json!({"action":"accept"})));
    let (a, b) = tokio::join!(a, b);
    let statuses = [a.unwrap().status(), b.unwrap().status()];
    assert!(statuses.contains(&StatusCode::OK) && statuses.contains(&StatusCode::NOT_FOUND));
    let value:Value=sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key='monica.company'").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(value, "Synthetic New");
    let decisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.profile_suggestion_decisions WHERE profile_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(decisions, 1);
    response(&app, "DELETE", &path, Value::Null, StatusCode::NOT_FOUND).await;
    sqlx::query("UPDATE crm.ai_settings SET include_messages=true")
        .execute(&pool)
        .await
        .unwrap();
    let hidden = response(
        &app,
        "GET",
        &format!("/api/v1/ai/profiles/{id}"),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert!(hidden["memory"].is_null());
    assert!(hidden["suggestions"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn model_settings_encrypt_keys_and_accept_large_agent_context(pool: sqlx::PgPool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = tempfile::tempdir().unwrap();
    let state = AppState::new(
        pool.clone(),
        "synthetic-model-settings-installation-secret",
        "http://testserver".into(),
        root.path().into(),
    )
    .await
    .unwrap();
    let cipher = state.credentials.clone();
    let actor = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic-model@example.invalid','unused')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(actor).bind(monica_core::identity::bytes_hash(b"synthetic-ai-session")).execute(&pool).await.unwrap();
    let app = crate::auth::routes(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let value=response(&app,"PATCH","/api/v1/ai/settings",json!({"enabled":true,"base_url":base,"model":"synthetic-model","api_key":"synthetic-model-key","include_messages":true}),StatusCode::OK).await;
    assert_eq!(value["has_key"], true);
    assert_eq!(value["qa_enabled"], true);
    assert!(!value.to_string().contains("synthetic-model-key"));
    let encrypted: String =
        sqlx::query_scalar("SELECT credential_ciphertext FROM crm.ai_settings WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!encrypted.contains("synthetic-model-key"));
    assert_eq!(
        cipher.decrypt(&encrypted).unwrap()["api_key"],
        "synthetic-model-key"
    );
    let audit: String = sqlx::query_scalar(
        "SELECT string_agg(metadata::text,'') FROM crm.audit_events WHERE action='ai.settings'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!audit.contains("synthetic-model-key"));
    let value = response(
        &app,
        "PATCH",
        "/api/v1/ai/settings",
        json!({"qa_enabled":false}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(value["enabled"], true);
    assert_eq!(value["include_messages"], true);
    assert_eq!(value["has_key"], true);
    response(
        &app,
        "PATCH",
        "/api/v1/ai/settings",
        json!({"qa_enabled":true}),
        StatusCode::OK,
    )
    .await;
    let status = response(
        &app,
        "GET",
        "/api/v1/ai/status",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(status["configured"], true);
    assert!(status["image_ocr"]["counts"].is_object());
    assert!(status["image_ocr"]["attempted_today"].is_number());
    assert!(status["image_ocr"]["include_chat"].is_boolean());
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0; 8192];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|s| s.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    assert!(headers.contains("authorization: bearer synthetic-model-key"));
                    let body: Value =
                        serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                    assert_eq!(
                        body["messages"][0]["content"].as_str().unwrap().len(),
                        70000
                    );
                    break;
                }
            }
        }
        let body = b"data: [DONE]\n\n";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",body.len()).as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
    });
    let result = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/ai/chat/completions",
            json!({"messages":[{"role":"user","content":"x".repeat(70000)}],"stream":true}),
        ))
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(result.into_body(), 10000).await.unwrap().as_ref(),
        b"data: [DONE]\n\n"
    );
    upstream.await.unwrap();
    response(
        &app,
        "PATCH",
        "/api/v1/ai/settings",
        json!({"enabled":false,"clear_key":true}),
        StatusCode::OK,
    )
    .await;
    let key: Option<String> =
        sqlx::query_scalar("SELECT credential_ciphertext FROM crm.ai_settings WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(key.is_none());
    let result = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/ai/settings")
                .header("host", "testserver")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn generated_digest_has_complete_web_contract_and_privacy_fence(pool: sqlx::PgPool) {
    let root = tempfile::tempdir().unwrap();
    let state = AppState::new(
        pool.clone(),
        "synthetic-digest-installation-secret",
        "http://testserver".into(),
        root.path().into(),
    )
    .await
    .unwrap();
    let actor = Uuid::new_v4();
    let person = Uuid::new_v4();
    let post = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'digest@example.invalid','unused')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(actor).bind(monica_core::identity::bytes_hash(b"synthetic-ai-session")).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic Digest Person')").bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) VALUES($1,'wechat',$1,$2,'post','digest-post','2026-01-06T12:00:00Z','{\"text\":\"Synthetic event\"}','event',1)").bind(post).bind(person).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    ai::enqueue(&pool, "ai.digest", "2026-01-05").await.unwrap();
    let lease = ai::claim(&pool).await.unwrap().unwrap();
    assert!(ai::finish(&pool,&lease,json!({"summary":"Synthetic generated recap","life_events":[{"profile_id":person,"post_id":post,"kind":"travel","text":"Synthetic trip"},{"profile_id":person,"post_id":post,"kind":"invalid-model-kind","text":"Must be dropped"}],"suggestions":[]})).await.unwrap());
    let app = crate::auth::routes(state);
    let row = response(
        &app,
        "GET",
        "/api/v1/digests/2026-01-05",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(row["facts"]["totals"]["posts"], 1);
    for key in [
        "top_contacts",
        "new_people",
        "new_links",
        "upcoming_dates",
        "newly_dormant",
    ] {
        assert!(row["facts"][key].is_array(), "{key}");
    }
    assert_eq!(
        row["people"][person.to_string()]["name"],
        "Synthetic Digest Person"
    );
    assert!(row["post_dates"][post.to_string()].is_string());
    assert_eq!(row["narrative"]["life_events"].as_array().unwrap().len(), 1);
    sqlx::query("UPDATE crm.ai_settings SET include_messages=true")
        .execute(&pool)
        .await
        .unwrap();
    let hidden = response(
        &app,
        "GET",
        "/api/v1/digests/2026-01-05",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert!(hidden["narrative"].is_null());
    assert_eq!(hidden["facts"], row["facts"]);
    let list = response(&app, "GET", "/api/v1/digests", Value::Null, StatusCode::OK).await;
    assert!(list["digests"][0]["summary"].is_null());
}

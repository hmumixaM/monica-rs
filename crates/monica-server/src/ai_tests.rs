use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

#[sqlx::test(migrations = "../../migrations")]
async fn home_highlights_read_latest_saved_week_and_respect_visibility(pool: sqlx::PgPool) {
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
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'highlights@example.invalid','unused')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(actor).bind(monica_core::identity::bytes_hash(b"synthetic-ai-session")).execute(&pool).await.unwrap();
    let app = crate::auth::routes(state);
    let route = "/api/v1/ai/weekly-highlights";
    let week = ai::previous_week_for(&pool).await.unwrap() - chrono::Duration::days(7);
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    for n in 0..10 {
        let person = Uuid::new_v4();
        let job = Uuid::new_v4();
        let subject = format!("{person}:{week}");
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2)",
        )
        .bind(person)
        .bind(format!("Saved person {n}"))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO crm.ai_jobs(id,kind,subject,model,input_hash,input,source_versions,privacy,status) VALUES($1,'ai.person_week',$2,'fake','fixture','{}','{}','{\"policy\":{\"include_private_notes\":false,\"include_messages\":false}}','complete')").bind(job).bind(&subject).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.ai_outputs(kind,subject,output,model,input_hash,source_versions,job_id) VALUES('ai.person_week',$1,'{}','fake','fixture','{}',$2)").bind(subject).bind(job).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.person_weekly_updates(profile_id,week_start,output,input_hash) VALUES($1,$2,'{\"summary\":\"Saved summary\",\"sources\":[\"large evidence\"]}','fixture')").bind(person).bind(week).execute(&pool).await.unwrap();
    }
    let saved = response(&app, "GET", route, Value::Null, StatusCode::OK).await;
    assert_eq!(saved["week_start"], week.to_string());
    assert_eq!(saved["people"].as_array().unwrap().len(), 8);
    assert!(
        saved["people"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["summary"] == "Saved summary" && p.get("sources").is_none())
    );
    assert!(saved.get("weeks").is_none());
    // Ten people have a note and eight are shown: visits draw different people.
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 {
        let draw = response(&app, "GET", route, Value::Null, StatusCode::OK).await;
        seen.extend(
            draw["people"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["profile_id"].as_str().unwrap().to_owned()),
        );
    }
    assert!(seen.len() > 8, "every visit showed the same eight people");
    sqlx::query("UPDATE crm.ai_settings SET include_messages=true")
        .execute(&pool)
        .await
        .unwrap();
    let hidden = response(&app, "GET", route, Value::Null, StatusCode::OK).await;
    assert!(hidden["people"].as_array().unwrap().is_empty());
    assert!(hidden["week_start"].is_null());
    let unauthenticated = app
        .oneshot(
            Request::builder()
                .uri(route)
                .header("host", "testserver")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
}
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
            "/api/v1/ai/suggestions",
            Value::Null,
            StatusCode::OK
        )
        .await,
        json!({"enabled":false,"total":0,"items":0,"kinds":{},"people":[]})
    );
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
    // Existing imported suggestions are hidden against current structured facts
    // without deleting their original result or needing another inference.
    let source = monica_core::Record {
        provider: monica_core::Provider::Linkedin,
        account: "synthetic-structured".into(),
        object: "profile".into(),
        kind: "profile".into(),
        event: "1".into(),
        payload: json!({"education":[{"school":"Synthetic University","degree":"MS"}],"experience":[{"company":"Former Company","title":"Engineer","end_date":"2022-01"}]}),
    };
    monica_core::ingest(&pool, &source).await.unwrap();
    while let Some(job) = monica_core::claim(&pool).await.unwrap() {
        monica_core::project(&pool, &job).await.unwrap();
    }
    monica_core::linking::set_link(
        &pool,
        monica_core::linking::LinkChange {
            account_id: monica_core::identity::account_id(source.provider, &source.account)
                .unwrap(),
            profile_id: Some(id),
            expected_profile_id: None,
        },
        None,
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.profile_suggestions(profile_id,fingerprint,kind,value,evidence,quote,confidence,input_hash) VALUES($1,'synthetic-existing-structured','school','合成大学','[{\"kind\":\"profile\"}]','Master of Science at Synthetic University','high',$2)")
        .bind(id).bind(&lease.input_hash).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profile_suggestions(profile_id,fingerprint,kind,value,evidence,quote,confidence,input_hash) VALUES($1,'synthetic-returned-employer','company','Former Company','[{\"kind\":\"post\",\"text\":\"I rejoined Former Company today.\",\"at\":\"2026-11-14T12:00:00Z\"}]','I rejoined Former Company today.','high',$2)")
        .bind(id).bind(&lease.input_hash).execute(&pool).await.unwrap();
    let structured = response(
        &app,
        "GET",
        "/api/v1/ai/suggestions",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(structured["items"], 1);
    assert_eq!(
        structured["people"][0]["items"][0]["value"],
        "Former Company"
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM crm.profile_suggestions WHERE profile_id=$1 AND fingerprint='synthetic-existing-structured'")
        .bind(id).fetch_one(&pool).await.unwrap(),1);
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
async fn three_browser_completions_open_while_previous_streams_are_alive(pool: sqlx::PgPool) {
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
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'parallel-ai@example.invalid','unused')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(actor).bind(monica_core::identity::bytes_hash(b"synthetic-ai-session")).execute(&pool).await.unwrap();
    let app = crate::auth::routes(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let provider = Router::new().route("/v1/chat/completions", post(|| async {
        let body = async_stream::stream! {
            yield Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b"data: synthetic\n\n"));
            std::future::pending::<()>().await;
        };
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(body))
            .unwrap()
    }));
    let upstream = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    response(
        &app, "PATCH", "/api/v1/ai/settings",
        json!({"enabled":true,"base_url":base_url,"model":"synthetic-model","api_key":"synthetic-key"}),
        StatusCode::OK,
    ).await;
    let send = || {
        app.clone().oneshot(request(
            "POST",
            "/api/v1/ai/chat/completions",
            json!({"messages":[{"role":"user","content":"parallel fixture"}],"stream":true}),
        ))
    };
    let (a, b, c) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(send(), send(), send())
    })
    .await
    .expect("new conversations must not wait for the first stream to close");
    let opened = [a.unwrap(), b.unwrap(), c.unwrap()];
    assert!(
        opened
            .iter()
            .all(|response| response.status() == StatusCode::OK)
    );
    drop(opened);
    upstream.abort();
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
    let value=response(&app,"PATCH","/api/v1/ai/settings",json!({"enabled":true,"base_url":base,"model":"synthetic-model","model_routes":{"batch":"saved-batch","person":"saved-person","writer":"saved-writer","qa":"saved-qa"},"vision_model":"saved-vision","api_key":"synthetic-model-key","include_messages":true}),StatusCode::OK).await;
    assert_eq!(value["has_key"], true);
    assert_eq!(value["qa_enabled"], true);
    assert_eq!(value["model_routes"]["person"], "saved-person");
    assert_eq!(value["vision_model"], "saved-vision");
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
    assert_eq!(
        status["models"],
        json!({"batch":"saved-batch","person":"saved-person","writer":"saved-writer","qa":"saved-qa","vision":"saved-vision"})
    );
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
                    assert_eq!(body["model"], "saved-qa");
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
    ai::week_window::refresh_observations(&pool).await.unwrap();
    ai::enqueue(&pool, "ai.digest", "2026-01-05").await.unwrap();
    let lease = ai::claim(&pool).await.unwrap().unwrap();
    let provider = ai::provider::FakeProvider {
        output: json!({"summary":"Synthetic context","sources":[post]}),
    };
    let mut prepared = false;
    for _ in 0..10 {
        if ai::week_window::prepare(&pool, &provider, &lease)
            .await
            .unwrap()
            .is_some()
        {
            prepared = true;
            break;
        }
    }
    assert!(prepared, "offline context must be ready before publishing");
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

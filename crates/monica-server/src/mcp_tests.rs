use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;
async fn setup(pool: &sqlx::PgPool) -> (AppState, Router, Uuid) {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'mcp@example.invalid','unused')").bind(id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(id).bind(hash("fixture-session")).execute(pool).await.unwrap();
    let state = AppState::new(
        pool.clone(),
        "synthetic-secret-for-mcp-tests-32bytes",
        "http://127.0.0.1:17494".into(),
        tempfile::tempdir().unwrap().keep(),
    )
    .await
    .unwrap();
    let app = auth::routes(state.clone()).merge(routes(state.clone()));
    (state, app, id)
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:17494")
        .header("content-type", "application/json");
    if let Some(key) = bearer {
        req = req
            .header("authorization", format!("Bearer {key}"))
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-06-18");
    } else {
        req = req
            .header("cookie", "crm_session=fixture-session; crm_csrf=fixture")
            .header("x-csrf-token", "fixture");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 2 * 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
    )
}
#[sqlx::test(migrations = "../../migrations")]
async fn transport_business_tools_revocation_and_boundary(pool: sqlx::PgPool) {
    let (_, app, _) = setup(&pool).await;
    assert_eq!(
        call(&app, "POST", "/mcp", json!({}), Some("unknown"))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, key) = call(
        &app,
        "POST",
        "/api/v1/mcp/keys",
        json!({"name":"fixture"}),
        None,
    )
    .await;
    let token = key["key"].as_str().unwrap();
    assert_eq!(
        call(&app, "POST", "/mcp", json!({}), Some(token)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/mcp/settings",
            json!({"enabled":true,"public_url":"http://remote.example/mcp"}),
            None
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/mcp/settings",
            json!({"enabled":true,"public_url":"https://remote.example/mcp"}),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let rpc = |method: &str, params: Value| json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    let (status,init)=call(&app,"POST","/mcp",rpc("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}})),Some(token)).await;
    assert_eq!(status, StatusCode::OK, "{init}");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
    let (_, list) = call(
        &app,
        "POST",
        "/mcp",
        rpc("tools/list", json!({})),
        Some(token),
    )
    .await;
    assert_eq!(
        list["result"]["tools"].as_array().unwrap().len(),
        specs().len(),
        "{list}"
    );
    let (_, created) = call(
        &app,
        "POST",
        "/mcp",
        rpc(
            "tools/call",
            json!({"name":"create_profile","arguments":{"body":{"display_name":"MCP person"}}}),
        ),
        Some(token),
    )
    .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let text = created["result"]["content"][0]["text"].as_str().unwrap();
    let created: Value = serde_json::from_str(text).unwrap();
    let id = created["data"]["id"].as_str().unwrap();
    let (_, read) = call(
        &app,
        "GET",
        &format!("/api/v1/profiles/{id}"),
        Value::Null,
        None,
    )
    .await;
    assert_eq!(read["display_name"], "MCP person");
    let (_,edited)=call(&app,"POST","/mcp",rpc("tools/call",json!({"name":"edit_field","arguments":{"id":id,"body":{"field_key":"notes","value":"MCP note"}}})),Some(token)).await;
    assert_ne!(edited["result"]["isError"], true, "{edited}");
    let (_, bad) = call(
        &app,
        "POST",
        "/mcp",
        rpc("tools/call", json!({"name":"reset_account","arguments":{}})),
        Some(token),
    )
    .await;
    assert!(bad["error"].is_object(), "{bad}");
    for (name, value) in [
        ("host", "attacker.example"),
        ("origin", "https://attacker.example"),
    ] {
        let mut req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", "127.0.0.1:17494")
            .header("authorization", format!("Bearer {token}"))
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        req.headers_mut()
            .unwrap()
            .insert(name, value.parse().unwrap());
        let res = app
            .clone()
            .oneshot(
                req.body(Body::from(rpc("tools/list", json!({})).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!res.status().is_success());
    }
    let (_, keys) = call(&app, "GET", "/api/v1/mcp/keys", Value::Null, None).await;
    assert!(!keys.to_string().contains(token));
    assert!(!keys.to_string().contains("token_hash"));
    call(
        &app,
        "POST",
        &format!("/api/v1/mcp/keys/{}/revoke", key["id"].as_str().unwrap()),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/mcp",
            rpc("tools/list", json!({})),
            Some(token)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn reset_retains_identity_and_revokes_sessions(pool: sqlx::PgPool) {
    let (s, app, id) = setup(&pool).await;
    sqlx::query(
        "UPDATE crm.app_users SET totp_secret='retained',pending_totp_secret='pending' WHERE id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.auth_recovery_codes(user_id,code_hash) VALUES($1,'fixture')")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let profile = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO crm.profiles(id,display_name,profile_type) VALUES($1,'retained','person')",
    )
    .bind(profile)
    .execute(&pool)
    .await
    .unwrap();
    for reset_totp in [false, true] {
        let _ = auth::reset_password(
            &s,
            auth::ResetPassword {
                email: "mcp@example.invalid".into(),
                password: "new-fixture-password".into(),
                reset_totp,
            },
            None,
        )
        .await
        .unwrap();
        let retained: (Uuid, bool) =
            sqlx::query_as("SELECT id,totp_secret IS NOT NULL FROM crm.app_users")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(retained, (id, !reset_totp));
    }
    assert_eq!(
        call(&app, "GET", "/api/v1/me", Value::Null, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, login) = call(
        &app,
        "POST",
        "/api/v1/auth/login",
        json!({"email":"mcp@example.invalid","password":"new-fixture-password"}),
        None,
    )
    .await;
    assert!(login["detail"].is_null(), "{login}");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.profiles WHERE id=$1")
            .bind(profile)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.auth_recovery_codes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn progress_rollup_replay_projection_and_timezones(pool: sqlx::PgPool) {
    let (_, app, _) = setup(&pool).await;
    let cid = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES($1,'apple_contacts','fixture','Fixture','local')").bind(cid).execute(&pool).await.unwrap();
    let record = json!({"external_id":"contact-1","kind":"contact","name":"Fixture","emails":[]});
    for _ in 0..2 {
        let mut tx = pool.begin().await.unwrap();
        monica_core::connections::commit_page(
            &mut tx,
            monica_core::Provider::AppleContacts,
            cid,
            "contacts",
            std::slice::from_ref(&record),
            &json!({"after":1}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_activity WHERE kind='page'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT records FROM crm.import_activity_totals WHERE provider='apple_contacts'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    monica_core::connections::deliver(&pool, monica_core::Provider::AppleContacts, cid, 100)
        .await
        .unwrap();
    for tz in ["UTC", "America/Chicago", "Asia/Kathmandu"] {
        let (status, body) = call(
            &app,
            "GET",
            &format!("/api/v1/imports/daily?timezone={tz}"),
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["records"][0]["records"], 1);
        assert_eq!(body["records"][0]["processed"], 1);
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/imports/daily?timezone=invalid",
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, runs) = call(
        &app,
        "GET",
        &format!("/api/v1/imports/runs?connection_id={cid}&limit=1"),
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{runs}");
    assert_eq!(runs["records"].as_array().unwrap().len(), 1);
    let next = &runs["next"];
    let path = format!(
        "/api/v1/imports/runs?before={}&before_id={}",
        percent_encoding::utf8_percent_encode(
            next["before"].as_str().unwrap(),
            percent_encoding::NON_ALPHANUMERIC
        ),
        percent_encoding::utf8_percent_encode(
            next["before_id"].as_str().unwrap(),
            percent_encoding::NON_ALPHANUMERIC
        )
    );
    assert_eq!(
        call(&app, "GET", &path, Value::Null, None).await.1["records"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let (status, state) = call(&app, "GET", "/api/v1/imports/status", Value::Null, None).await;
    assert_eq!(status, StatusCode::OK, "{state}");
    assert_eq!(state["connections"][0]["provider"], "apple_contacts");
    assert_eq!(state["analysis_execution_enabled"], false);
    assert_eq!(state["stages"]["observation_records_pending"], 0);
    assert_eq!(state["stages"]["week_refresh_pending"], 0);
    assert_eq!(state["week_analysis"], json!([]));
    // Failure and recovery retain one failure while updating the same run.
    let lease = Uuid::new_v4();
    sqlx::query("UPDATE crm.source_connections SET lease_token=$2,enabled=true WHERE id=$1")
        .bind(cid)
        .bind(lease)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE crm.source_connections SET lease_token=NULL,error='do not expose raw credentials' WHERE id=$1").bind(cid).execute(&pool).await.unwrap();
    let (_, runs) = call(&app, "GET", "/api/v1/imports/runs", Value::Null, None).await;
    assert!(!runs.to_string().contains("credentials"));
    assert_eq!(runs["records"][0]["state"], "failed");
}

use super::*;
use crate::auth;
use axum::{Json, Router, body::to_bytes, http::Request, routing::post};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

async fn request(
    app: &Router,
    actor: &str,
    method: &str,
    path: &str,
    body: Value,
    status: StatusCode,
) -> Value {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "testserver")
        .header("content-type", "application/json")
        .header("cookie", format!("crm_session={actor}; crm_csrf=ask-test"))
        .header("x-csrf-token", "ask-test")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let code = response.status();
    let bytes = to_bytes(response.into_body(), 3 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        code,
        status,
        "{method} {path} returned {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn setup(pool: &PgPool) -> (AppState, Router) {
    let root = tempfile::tempdir().unwrap();
    let state = AppState::new(
        pool.clone(),
        "synthetic-ask-session-secret-32-bytes",
        "http://testserver".into(),
        root.path().into(),
    )
    .await
    .unwrap();
    // Production currently provisions a single account. Exercise the owner
    // boundary against a second account without changing the product schema.
    sqlx::query("ALTER TABLE crm.app_users DROP CONSTRAINT app_users_singleton_key")
        .execute(pool)
        .await
        .unwrap();
    for name in ["owner", "other"] {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,$2,'unused')")
            .bind(id)
            .bind(format!("{name}@example.invalid"))
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')")
            .bind(Uuid::new_v4()).bind(id).bind(monica_core::identity::bytes_hash(name.as_bytes())).execute(pool).await.unwrap();
    }
    let app = auth::routes(state.clone());
    (state, app)
}

async fn new_chat(app: &Router) -> String {
    let id = Uuid::new_v4();
    for _ in 0..2 {
        let chat = request(
            app,
            "owner",
            "POST",
            "/api/v1/ask/chats",
            json!({"id":id}),
            StatusCode::OK,
        )
        .await;
        assert_eq!(chat["id"], json!(id));
    }
    format!("/api/v1/ask/chats/{id}")
}
fn question(run: Uuid, text: &str) -> Value {
    json!({"request_id":run, "question":text, "model":"deepseek-v4-pro", "timezone":"America/Chicago"})
}
fn events(items: &[(Value, Value)], done: bool) -> Response {
    let mut body = items
        .iter()
        .map(|(delta, finish)| {
            format!(
                "data: {}\r\n\r\n",
                json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
            )
        })
        .collect::<String>();
    if done {
        body.push_str("data: [DONE]\r\n\r\n");
    }
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

#[sqlx::test(migrations = "../../migrations")]
async fn ask_runs_without_a_browser_and_recovers_a_stream_after_real_tag_lookup(pool: PgPool) {
    let (state, app) = setup(&pool).await;
    let tag = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.tags(id,name) VALUES($1,'QD')")
        .bind(tag)
        .execute(&pool)
        .await
        .unwrap();
    let mut people = Vec::new();
    for (index, name) in ["张三", "乙Q", "标签以外"].iter().enumerate() {
        let profile = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2)",
        )
        .bind(profile)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO crm.audit_events(action,entity_type,entity_id) VALUES('profile.created','profile',$1)")
            .bind(profile).execute(&pool).await.unwrap();
        if index < 2 {
            sqlx::query("INSERT INTO crm.tag_memberships(tag_id,profile_id) VALUES($1,$2)")
                .bind(tag)
                .bind(profile)
                .execute(&pool)
                .await
                .unwrap();
            people.push(profile);
        }
    }
    let attempts = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let script = r#"const a = cli("directory",{tags:["qd"],limit:1,fields:["link","name"]}); const b = cli("directory",{tags:["QD"],offset:a.next_offset,fields:["link","name"]}); return {total:a.total, rows:[...a.rows,...b.rows], initials:cli("directory",{query:"zs"}).rows.map(p=>p.name), security:[typeof require,typeof fetch,typeof process]};"#;
    let fake = Router::new().route("/v1/chat/completions",post({
        let attempts = attempts.clone();
        move |Json(body): Json<Value>| {
            let attempt = attempts.fetch_add(1,Ordering::SeqCst);
            let people = people.clone();
            async move {
                assert_eq!(body["model"],"deepseek-v4-pro");
                assert_eq!(body["reasoning_effort"],"high");
                assert_eq!(body["allowed_openai_params"],json!(["reasoning_effort"]));
                let messages = body["messages"].as_array().unwrap();
                if attempt==0 {
                    let call = json!({"index":0,"id":"tag-fixture","function":{"name":"monica_cli","arguments":json!({"title":"按 QD 标签筛选联系人","script":script}).to_string()}});
                    return events(&[(json!({"reasoning_content":"先查标签","tool_calls":[call]}),json!("tool_calls"))],true);
                }
                let saved = messages.iter().find(|m| m["role"]=="tool").expect("saved query is carried into each retry");
                assert_eq!(saved["tool_call_id"],"tag-fixture");
                let output: Value = serde_json::from_str(saved["content"].as_str().unwrap()).unwrap();
                assert_eq!(output["total"],2);
                assert!(output["rows"].as_array().unwrap().iter().all(|row| row.as_object().is_some_and(|row| row.len() == 2 && row.contains_key("link") && row.contains_key("name"))));
                assert_eq!(output["initials"],json!(["张三"]));
                assert_eq!(output["security"],json!(["undefined","undefined","undefined"]));
                for person in people { assert!(output["rows"].as_array().unwrap().iter().any(|row| row["link"]==format!("/profiles/{person}"))); }
                assert_eq!(messages.iter().find(|m| m["role"]=="assistant").unwrap()["reasoning_content"],"先查标签");
                match attempt {
                    1 | 3..=10 => (StatusCode::SERVICE_UNAVAILABLE,[(header::RETRY_AFTER,"0")]).into_response(),
                    2 => events(&[(json!({"content":"断线的草稿"}),Value::Null)],false),
                    11 => events(&[(json!({"reasoning_content":"已有两条证据，准备回答"}),json!("length"))],true),
                    _ => {
                        assert!(messages.last().unwrap()["content"].as_str().unwrap().starts_with("继续"));
                        events(&[(json!({"content":"已找到 QD 标签下的两位联系人。"}),json!("stop"))],true)
                    }
                }
            }
        }
    }));
    let upstream = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
    request(&app,"owner","PATCH","/api/v1/ai/settings",
        json!({"enabled":true,"base_url":endpoint,"model":"deepseek-v4-pro","api_key":"synthetic-key"}),StatusCode::OK).await;
    let chat = new_chat(&app).await;
    let run = Uuid::new_v4();
    let choices = request(
        &app,
        "owner",
        "GET",
        "/api/v1/ai/browser-model",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        choices["models"],
        json!(["deepseek-v4-pro", "deepseek-v4-pro-medium"])
    );
    let mut body = question(run, "所有 QD 标签的人有哪些？");
    body["model"] = json!("deepseek-v4-pro-medium");
    request(
        &app,
        "owner",
        "POST",
        &format!("{chat}/runs"),
        body.clone(),
        StatusCode::ACCEPTED,
    )
    .await;
    request(
        &app,
        "owner",
        "POST",
        &format!("{chat}/runs"),
        body,
        StatusCode::ACCEPTED,
    )
    .await;
    let worker = crate::ask::launch(state, pool.clone());
    // No browser or transcript request is open while the worker runs.
    timeout(Duration::from_secs(25), async {
        loop {
            let status: String = sqlx::query_scalar("SELECT status FROM crm.ask_runs WHERE id=$1")
                .bind(run)
                .fetch_one(&pool)
                .await
                .unwrap();
            if status == "done" {
                break;
            }
            assert_ne!(status, "failed");
            sleep(Duration::from_millis(75)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 13);
    let snapshot = request(&app, "owner", "GET", &chat, Value::Null, StatusCode::OK).await;
    let messages = snapshot["rows"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(
        messages[1]["message"]["content"][1]["arguments"]["script"],
        script
    );
    assert_eq!(
        messages[1]["message"]["content"][1]["arguments"]["title"],
        "按 QD 标签筛选联系人"
    );
    assert_eq!(
        messages[2]["message"]["details"]["queries"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        messages[3]["message"]["content"][0]["thinking"],
        "已有两条证据，准备回答"
    );
    assert_eq!(
        messages[4]["message"]["content"][0]["text"],
        "已找到 QD 标签下的两位联系人。"
    );
    for method in ["GET", "DELETE"] {
        request(
            &app,
            "other",
            method,
            &chat,
            Value::Null,
            StatusCode::NOT_FOUND,
        )
        .await;
    }
    assert_eq!(
        request(
            &app,
            "other",
            "GET",
            "/api/v1/ask/chats",
            Value::Null,
            StatusCode::OK
        )
        .await,
        json!([])
    );
    worker.abort();
    upstream.abort();
}

#[sqlx::test(migrations = "../../migrations")]
async fn ask_leases_fence_restarts_cancellation_and_deletion(pool: PgPool) {
    let (_, app) = setup(&pool).await;
    sqlx::query(
        "UPDATE crm.ai_settings SET enabled=true,execution_enabled=true,qa_enabled=true WHERE id",
    )
    .execute(&pool)
    .await
    .unwrap();
    let first = new_chat(&app).await;
    let second = new_chat(&app).await;
    let first_run = Uuid::new_v4();
    let second_run = Uuid::new_v4();
    for (chat, run) in [(&first, first_run), (&second, second_run)] {
        request(
            &app,
            "owner",
            "POST",
            &format!("{chat}/runs"),
            question(run, "Synthetic question"),
            StatusCode::ACCEPTED,
        )
        .await;
    }
    request(
        &app,
        "owner",
        "POST",
        &format!("{first}/runs"),
        question(Uuid::new_v4(), "duplicate"),
        StatusCode::CONFLICT,
    )
    .await;
    let (a, b) = tokio::join!(claim(&pool), claim(&pool));
    let (a, b) = (a.unwrap().unwrap(), b.unwrap().unwrap());
    let (a, b) = if a.id == first_run { (a, b) } else { (b, a) };
    assert_eq!((a.id, b.id), (first_run, second_run));
    let saved_call = json!({"role":"assistant","content":[{"type":"toolCall","id":"saved-call","name":"monica_cli","arguments":{"title":"已排队的操作","script":"return 123;"}}]});
    let cp = Checkpoint {
        rounds: 1,
        ..Default::default()
    };
    save(&pool, &a, &cp, Some(&saved_call), None).await.unwrap();
    sqlx::query("UPDATE crm.ask_runs SET lease_until=now()-interval '1 second' WHERE id=$1")
        .bind(a.id)
        .execute(&pool)
        .await
        .unwrap();
    let recovered = claim(&pool).await.unwrap().unwrap();
    assert_eq!(recovered.id, a.id);
    assert_ne!(recovered.lease_id, a.lease_id);
    assert_eq!(recovered.checkpoint.0.rounds, 1);
    let result = json!({"role":"toolResult","toolName":"monica_cli","toolCallId":"saved-call","content":[{"type":"text","text":"123"}]});
    assert!(save(&pool, &a, &cp, Some(&result), None).await.is_err());
    save(&pool, &recovered, &cp, Some(&result), None)
        .await
        .unwrap();
    let draft = json!({"role":"assistant","content":[{"type":"text","text":"部分回答"}],"stopReason":"stop"});
    progress(&pool, &recovered, "正在回答…", Some(&draft))
        .await
        .unwrap();
    request(
        &app,
        "owner",
        "DELETE",
        &format!("{first}/runs/{first_run}"),
        Value::Null,
        StatusCode::NO_CONTENT,
    )
    .await;
    assert!(
        progress(&pool, &recovered, "old worker", None)
            .await
            .is_err()
    );
    let view = request(&app, "owner", "GET", &first, Value::Null, StatusCode::OK).await;
    assert_eq!(view["run"]["status"], "cancelled");
    assert_eq!(view["rows"][3]["message"]["content"][0]["text"], "部分回答");
    assert_eq!(view["rows"][3]["message"]["stopReason"], "aborted");
    request(
        &app,
        "owner",
        "DELETE",
        &second,
        Value::Null,
        StatusCode::NO_CONTENT,
    )
    .await;
    assert!(
        save(&pool, &b, &cp, Some(&draft), Some(("done", "")))
            .await
            .is_err()
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.ask_messages WHERE run_id=$1")
            .bind(second_run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn gemini_tool_signatures_survive_same_model_turns_in_litellm() {
    let signature = "A".repeat(2048);
    let id = format!("call_fixture__thought__{signature}");
    let mut reply = Reply::default();
    let event = json!({"choices":[{"delta":{"tool_calls":[{
        "index":0,"id":id,"function":{"name":"monica_cli","arguments":"{}"},
        "provider_specific_fields":{"thought_signature":signature}
    }]},"finish_reason":"tool_calls"}]});
    add_event(&mut reply, event.to_string().as_bytes()).unwrap();
    reply.reasoning_details = vec![json!({"type":"reasoning.encrypted","data":"opaque-replay"})];
    let restored = imported_message(reply.message(true, "gemini-4-neon")).unwrap();
    let history = vec![
        json!({"role":"user","content":"sample"}),
        restored,
        json!({"role":"toolResult","toolCallId":id,"content":[{"type":"text","text":"example data"}]}),
    ];
    let same = model_context(&history, "system", "", "gemini-4-neon", 50_000);
    assert_eq!(same[2]["tool_calls"][0]["id"], id);
    assert_eq!(
        same[2]["tool_calls"][0]["provider_specific_fields"]["thought_signature"],
        signature
    );
    assert_eq!(same[3]["tool_call_id"], id);
    assert_eq!(same[2]["reasoning_details"][0]["data"], "opaque-replay");
    let switched = model_context(&history, "system", "", "deepseek-v4-pro", 50_000);
    assert_eq!(switched[2]["tool_calls"][0]["id"], "call_fixture");
    assert!(switched[2]["tool_calls"][0]["provider_specific_fields"].is_null());
    assert_eq!(switched[3]["tool_call_id"], "call_fixture");
}

#[sqlx::test(migrations = "../../migrations")]
async fn long_transcripts_page_by_bytes_and_import_checks_auth_first(pool: PgPool) {
    let (_, app) = setup(&pool).await;
    let import = "/api/v1/ask/chats/import";
    request(
        &app,
        "expired",
        "POST",
        import,
        json!({"invalid":true}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let payload = "x".repeat(550_000);
    let messages = (0..5)
        .map(|i| json!({"role":"user","content":format!("{i}{payload}")}))
        .collect::<Vec<_>>();
    let chat = request(
        &app,
        "owner",
        "POST",
        import,
        json!({"version":1,"messages":messages}),
        StatusCode::OK,
    )
    .await;
    let path = format!("/api/v1/ask/chats/{}", chat["id"].as_str().unwrap());
    let first = request(&app, "owner", "GET", &path, Value::Null, StatusCode::OK).await;
    let rows = first["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(first["more"], true);
    let next = request(
        &app,
        "owner",
        "GET",
        &format!("{path}?after={}", rows[2]["seq"]),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(next["rows"].as_array().unwrap().len(), 2);
    assert_eq!(next["more"], false);
    assert!(
        next["rows"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .starts_with('3')
    );
}

#[test]
fn result_above_the_old_limit_reaches_the_model_context() {
    let result = "x".repeat(90_000);
    let history = vec![
        json!({"role":"user","content":"summarize"}),
        json!({"role":"assistant","content":[{"type":"toolCall","id":"fixture","name":"monica_cli","arguments":{"script":"return something"}}]}),
        json!({"role":"toolResult","toolCallId":"fixture","content":[{"type":"text","text":result}]}),
    ];
    let wire = model_context(&history, "system", "", "fixture", 560_000);
    assert_eq!(wire[3]["content"], result);
}

#[sqlx::test(migrations = "../../migrations")]
async fn ask_edits_require_owner_confirmation_and_commit_once(pool: PgPool) {
    let (state, app) = setup(&pool).await;
    let path = new_chat(&app).await;
    let chat = Uuid::parse_str(path.rsplit('/').next().unwrap()).unwrap();
    let run = Uuid::new_v4();
    let lease = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.ask_runs(id,chat_id,model,timezone,people,status,lease_id,lease_until) VALUES($1,$2,'fixture','UTC','[]','running',$3,now()+interval '5 minutes')")
        .bind(run).bind(chat).bind(lease).execute(&pool).await.unwrap();
    let people = [Uuid::new_v4(), Uuid::new_v4()];
    for (id, name) in people.iter().zip(["林同学", "周同学"]) {
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2)",
        )
        .bind(id)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    let [first, second] = people;
    let existing_tag = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.tags(id,name) VALUES($1,'QD')")
        .bind(existing_tag)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.tag_memberships(tag_id,profile_id) VALUES($1,$2)")
        .bind(existing_tag)
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();

    let actions = [
        json!({"action":"add_record","profile_ids":people,"record_type":"note","title":"重聚纪要","body":"在上海见面"}),
        json!({"action":"add_record","profile_ids":[first],"record_type":"activity","title":"线下见面","occurred_at":"2025-03-14T18:00:00+08:00"}),
        json!({"action":"add_tag","profile_ids":people,"tag":"qd"}),
        json!({"action":"create_tag","profile_ids":[first],"tag":"待跟进"}),
        json!({"action":"set_profile_field","profile_id":first,"field":"city","value":"上海"}),
        json!({"action":"add_date","profile_id":first,"date":{"kind":"birthday","precision":"month_day","month":7,"day":12}}),
        json!({"action":"add_relationship","profile_id":first,"related_profile_id":second,"relationship":"曾是同事"}),
    ];
    let mut ids = Vec::new();
    for (n, action) in actions.iter().enumerate() {
        let key = format!("edit-{n}");
        let id = edits::propose(&pool, chat, run, lease, &key, action)
            .await
            .unwrap();
        assert_eq!(
            edits::propose(&pool, chat, run, lease, &key, action)
                .await
                .unwrap(),
            id
        );
        ids.push(id);
    }
    let unknown = json!({"action":"set_profile_field","profile_id":first,"field":"wechat.nickname","value":"禁止"});
    assert!(
        edits::propose(&pool, chat, run, lease, "unknown-write", &unknown)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.activities WHERE title IN ('重聚纪要','线下见面')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    let page = request(&app, "owner", "GET", &path, Value::Null, StatusCode::OK).await;
    assert_eq!(page["edits"].as_array().unwrap().len(), actions.len());
    assert!(
        page["edits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|edit| edit["status"] == "pending")
    );
    let denied = format!("{path}/edits/{}", ids[0]);
    request(
        &app,
        "other",
        "POST",
        &denied,
        json!({"confirm":true}),
        StatusCode::NOT_FOUND,
    )
    .await;
    let owner: (Uuid, String) =
        sqlx::query_as("SELECT id,email FROM crm.app_users WHERE email='owner@example.invalid'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let delegated = auth::delegated_routes(state, owner);
    request(
        &delegated,
        "owner",
        "POST",
        &denied,
        json!({"confirm":true}),
        StatusCode::FORBIDDEN,
    )
    .await;
    for id in ids {
        let endpoint = format!("{path}/edits/{id}");
        let decision = json!({"confirm":true});
        let saved = request(
            &app,
            "owner",
            "POST",
            &endpoint,
            decision.clone(),
            StatusCode::OK,
        )
        .await;
        assert_eq!(saved["status"], "applied");
        assert_eq!(
            request(&app, "owner", "POST", &endpoint, decision, StatusCode::OK).await,
            saved
        );
    }
    let counts: (i64,i64,i64,i64,i64,i64) = sqlx::query_as("SELECT
        (SELECT count(*) FROM crm.activities WHERE title IN ('重聚纪要','线下见面')),
        (SELECT count(*) FROM crm.tag_memberships WHERE tag_id=$3),
        (SELECT count(*) FROM crm.tags t JOIN crm.tag_memberships m ON t.id=m.tag_id WHERE t.name='待跟进' AND m.profile_id=$1),
        (SELECT count(*) FROM crm.field_current c JOIN crm.field_revisions r ON c.revision_id=r.id WHERE c.profile_id=$1 AND c.field_key='monica.residence' AND r.value='\"上海\"'),
        (SELECT count(*) FROM crm.profile_dates WHERE profile_id=$1 AND kind='birthday' AND year IS NULL AND month=7 AND day=12),
        (SELECT count(*) FROM crm.relationship_edges WHERE from_profile_id=$1 AND to_profile_id=$2 AND relationship_type='曾是同事')")
        .bind(first).bind(second).bind(existing_tag).fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (2, 2, 1, 1, 1, 1));

    let replacement =
        json!({"action":"set_profile_field","profile_id":first,"field":"city","value":"南京"});
    let stale = edits::propose(&pool, chat, run, lease, "old-preview", &replacement)
        .await
        .unwrap();
    let newer =
        json!({"action":"set_profile_field","profile_id":first,"field":"city","value":"杭州"});
    let new_id = edits::propose(&pool, chat, run, lease, "new-preview", &newer)
        .await
        .unwrap();
    let new_result = request(
        &app,
        "owner",
        "POST",
        &format!("{path}/edits/{new_id}"),
        json!({"confirm":true}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(new_result["status"], "applied");
    let rejected = request(
        &app,
        "owner",
        "POST",
        &format!("{path}/edits/{stale}"),
        json!({"confirm":true}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rejected["status"], "stale");

    let pending = edits::propose(&pool, chat, run, lease, "cancelled-run", &actions[0])
        .await
        .unwrap();
    request(
        &app,
        "owner",
        "DELETE",
        &format!("{path}/runs/{run}"),
        Value::Null,
        StatusCode::NO_CONTENT,
    )
    .await;
    let stopped = request(
        &app,
        "owner",
        "POST",
        &format!("{path}/edits/{pending}"),
        json!({"confirm":true}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(stopped["status"], "dismissed");
}

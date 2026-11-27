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
async fn exa_settings_only_return_status_and_store_encrypted_key(pool: PgPool) {
    let (state, app) = setup(&pool).await;
    let path = "/api/v1/ai/web-search";
    request(
        &app,
        "wrong",
        "GET",
        path,
        Value::Null,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_eq!(
        request(&app, "owner", "GET", path, Value::Null, StatusCode::OK).await,
        json!({"configured":false})
    );
    assert_eq!(
        request(
            &app,
            "owner",
            "PATCH",
            path,
            json!({"api_key":"synthetic-exa-key"}),
            StatusCode::OK
        )
        .await,
        json!({"configured":true})
    );
    let encrypted: String =
        sqlx::query_scalar("SELECT exa_credential_ciphertext FROM crm.ai_settings WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!encrypted.contains("synthetic-exa-key"));
    assert_eq!(
        state.credentials.decrypt(&encrypted).unwrap(),
        json!({"api_key":"synthetic-exa-key"})
    );
    assert_eq!(
        request(
            &app,
            "owner",
            "PATCH",
            path,
            json!({"clear_key":true}),
            StatusCode::OK
        )
        .await,
        json!({"configured":false})
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn ask_worker_executes_compressed_vision_and_rejects_empty_search(pool: PgPool) {
    use base64::Engine;
    let (mut state, app) = setup(&pool).await;
    let media_root = tempfile::tempdir().unwrap();
    state.media_root = media_root.path().to_owned();
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC")
        .unwrap();
    let person = Uuid::new_v4();
    let post_id = Uuid::new_v4();
    let conversation = Uuid::new_v4();
    let message = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Fixture')",
    )
    .bind(person)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,content,content_hash,version) VALUES($1,'wechat',$1,$2,'post','vision-fixture','{}','vision-fixture',1)")
        .bind(post_id).bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type,profile_id) VALUES($1,'wechat','self','peer','direct',$2)")
        .bind(conversation).bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body) VALUES($1,$2,'wechat','self','vision-fixture','peer',now(),'fixture')")
        .bind(message).bind(conversation).execute(&pool).await.unwrap();
    let mut ids = Vec::new();
    for index in [0, 1, 2, 3] {
        let id = Uuid::new_v4();
        let mut bytes = png.clone();
        bytes.push(index);
        let hash = monica_core::identity::bytes_hash(&bytes);
        let path = monica_core::media::object_path(&hash).unwrap();
        let file = media_root.path().join(&path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, &bytes).unwrap();
        sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,source_type) VALUES($1,$2,'image/png',$3,$4,'manual')")
            .bind(id).bind(hash).bind(bytes.len() as i64).bind(path).execute(&pool).await.unwrap();
        let (entity, kind, role) = if index == 2 {
            (message, "message_event", "attachment".to_owned())
        } else {
            (post_id, "social_post", format!("media-{}", index + 1))
        };
        sqlx::query(
            "INSERT INTO crm.media_links(media_id,entity_type,entity_id,role) VALUES($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(kind)
        .bind(entity)
        .bind(role)
        .execute(&pool)
        .await
        .unwrap();
        ids.push(id);
    }
    sqlx::query("INSERT INTO crm.image_ocr_readings(media_id,status,text) VALUES($1,'complete','Saved photo text')")
        .bind(ids[0]).execute(&pool).await.unwrap();
    let actor: (Uuid, String) =
        sqlx::query_as("SELECT id,email FROM crm.app_users WHERE email='owner@example.invalid'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (notes, error, _) = lookup::run(
        &state,
        actor,
        chrono_tz::UTC,
        format!(r#"return cli("image_notes",{{post_id:"{post_id}"}});"#),
    )
    .await;
    assert!(!error, "{notes}");
    assert_eq!(
        serde_json::from_str::<Value>(&notes).unwrap()["notes"][0]["text"],
        "Saved photo text"
    );
    let attempts = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let fake = Router::new().route(
        "/v1/chat/completions",
        post({
            let attempts = attempts.clone();
            let ids = ids.clone();
            move |Json(body): Json<Value>| {
                let attempts = attempts.clone();
                let ids = ids.clone();
                async move {
                    if body["model"] == "ask-fixture" {
                        let messages = body["messages"].as_array().unwrap();
                        if !messages.iter().any(|m|m["role"]=="tool") {
                            let calls = json!([
                                {"index":0,"id":"vision-fixture","function":{"name":"inspect_images","arguments":json!({"image_ids":ids,"question":"How bright is the pixel?"}).to_string()}},
                                {"index":1,"id":"empty-search","function":{"name":"search_public_web","arguments":json!({"query":" "}).to_string()}}
                            ]);
                            return events(&[(json!({"tool_calls":calls}),json!("tool_calls"))],true);
                        }
                        let tool = |id| {
                            let value = messages.iter().find(|m|m["tool_call_id"]==id).unwrap();
                            serde_json::from_str::<Value>(value["content"].as_str().unwrap()).unwrap()
                        };
                        let observed = tool("vision-fixture");
                        assert_eq!(observed["observation"], "A bright pixel.", "{observed}");
                        assert_eq!(observed["inspected"].as_array().unwrap().len(),2);
                        assert_eq!(observed["skipped"][0]["image_id"],json!(ids[2]));
                        assert_eq!(observed["skipped"][1]["reason"],"视觉模型输入超限");
                        assert!(!observed.to_string().contains("data:image"));
                        assert!(tool("empty-search")["error"].as_str().unwrap().contains("1 至 500"));
                        return events(&[(json!({"content":"Done"}),json!("stop"))],true);
                    }
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(body["model"], "vision-fixture");
                    assert_eq!(body["stream"], false);
                    let images: Vec<_> = body["messages"][1]["content"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|item| item["type"] == "image_url")
                        .collect();
                    assert_eq!(images.len(), 3 - attempt);
                    for image in images {
                        assert!(
                            image["image_url"]["url"]
                                .as_str()
                                .unwrap()
                                .starts_with("data:image/jpeg;base64,/9j/")
                        );
                    }
                    if attempt == 0 {
                        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
                    }
                    Json(json!({"choices":[{"message":{"content":"A bright pixel."}}]}))
                        .into_response()
                }
            }
        }),
    );
    let upstream = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
    request(&app, "owner", "PATCH", "/api/v1/ai/settings",
        json!({"enabled":true,"qa_enabled":true,"model":"ask-fixture","base_url":base_url,"vision_model":"vision-fixture","api_key":"synthetic-model-key"}), StatusCode::OK).await;
    request(
        &app,
        "owner",
        "PATCH",
        "/api/v1/ai/web-search",
        json!({"api_key":"synthetic-exa-key"}),
        StatusCode::OK,
    )
    .await;
    let path = new_chat(&app).await;
    let run = Uuid::new_v4();
    request(
        &app,
        "owner",
        "POST",
        &format!("{path}/runs"),
        json!({"request_id":run,"question":"How bright is the pixel?","model":"ask-fixture"}),
        StatusCode::ACCEPTED,
    )
    .await;
    let worker = crate::ask::launch(state, pool.clone());
    timeout(Duration::from_secs(20), async {
        loop {
            let status: String = sqlx::query_scalar("SELECT status FROM crm.ask_runs WHERE id=$1")
                .bind(run)
                .fetch_one(&pool)
                .await
                .unwrap();
            if status == "done" {
                break;
            }
            assert_ne!(status, "failed", "Ask did not complete");
            sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    upstream.abort();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let mut checkpoint: sqlx::types::Json<Checkpoint> =
        sqlx::query_scalar("SELECT checkpoint FROM crm.ask_runs WHERE id=$1")
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        checkpoint
            .0
            .reserve_external(image_lookup::NAME, "vision-fixture")
            .is_err()
    );
    assert!(
        checkpoint
            .0
            .reserve_external(web_search::NAME, "empty-search")
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn asks_read_contact_insights_and_history_with_filters_and_cursors(pool: PgPool) {
    let (state, _) = setup(&pool).await;
    let actor: Actor =
        sqlx::query_as("SELECT id,email FROM crm.app_users WHERE email='owner@example.invalid'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (person, peer, group, old_id, account, connection, evidence) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name,archived_at) VALUES
        ($1,'person','Alex',NULL),($2,'person','Blake',NULL),($3,'group','Book Club',NULL),($4,'person','Old Alex',now())")
        .bind(person).bind(peer).bind(group).bind(old_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profile_redirects(old_profile_id,new_profile_id) VALUES($1,$2)")
        .bind(old_id)
        .bind(person)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.group_memberships(group_profile_id,person_profile_id,role) VALUES($1,$2,'owner'),($1,$3,'member')")
        .bind(group).bind(person).bind(peer).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.contact_events(id,provider,profile_id,kind,external_id,occurred_at,content) VALUES
        (gen_random_uuid(),'wechat',$1,'call','ask-insights-call','2026-02-25T12:00:00Z','{\"duration_seconds\":45}'),
        (gen_random_uuid(),'wechat',$1,'call','ask-insights-old-call','2026-02-25T13:00:00Z','{\"duration_seconds\":90,\"is_deleted\":true}')")
        .bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES($1,'mail','fake-account','Account','imap')")
        .bind(connection).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.connection_evidence(id,connection_id,external_id,revision,content_hash,kind,occurred_at,included) VALUES
        ($1,$2,'ask-insights-mail',1,'fixture','message','2026-02-25T12:00:00Z',true)")
        .bind(evidence).bind(connection).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.connection_evidence_people(evidence_id,profile_id,role) VALUES($1,$2,'sender')")
        .bind(evidence).bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.import_activity_daily(bucket,provider,account,stream,records,processed,inserted,changed,unchanged,failures)
        VALUES(now(),'mail',$1,'messages',5,5,5,0,0,0),(now()-interval '1 day','mail',$1,'messages',2,1,1,0,0,1)")
        .bind(connection.to_string()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version)
        VALUES(gen_random_uuid(),'wechat',gen_random_uuid(),$1,'post','ask-jan','2026-01-10T12:00:00Z','{\"like_count\":3}','fixture',1),
        (gen_random_uuid(),'wechat',gen_random_uuid(),$1,'post','ask-feb','2026-02-10T12:00:00Z','{\"comment_count\":2}','fixture',1),
        (gen_random_uuid(),'wechat',gen_random_uuid(),$1,'post','ask-deleted','2026-02-11T12:00:00Z','{\"like_count\":99}','fixture',1),
        (gen_random_uuid(),'wechat',gen_random_uuid(),$1,'post','ask-merged','2026-02-12T12:00:00Z','{\"like_count\":99}','fixture',1)")
        .bind(person).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.timeline_events SET status=CASE external_id WHEN 'ask-deleted' THEN 'deleted' ELSE 'merged' END
        WHERE external_id IN ('ask-deleted','ask-merged')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("WITH peer_post AS (
        INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version)
        VALUES(gen_random_uuid(),'wechat',gen_random_uuid(),$2,'post','ask-peer','2026-02-10T12:00:00Z','{}','fixture',1)
        RETURNING id
    )
    INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_profile_id,occurred_at,content,is_deleted)
    SELECT gen_random_uuid(),id,'wechat',v.external_id,'like',$1,'2026-02-25T12:00:00Z','{}',v.deleted
    FROM peer_post CROSS JOIN (VALUES('like-active',false),('like-deleted',true)) v(external_id,deleted)")
        .bind(person).bind(peer).execute(&pool).await.unwrap();
    sqlx::query("SELECT setval(pg_get_serial_sequence('crm.field_revisions','revision_seq'),9007199254740992,true)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation) VALUES
        (gen_random_uuid(),$1,'monica.company',to_jsonb('Before'::text),'fixture','manual','manual_edit'),
        (gen_random_uuid(),$1,'monica.company',to_jsonb('After'::text),'fixture','manual','manual_edit'),
        (gen_random_uuid(),$1,'monica.note',to_jsonb(repeat('旧',3000)),'fixture','manual','manual_edit')")
        .bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'linkedin','ask-insights','person','Source')")
        .bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
        .bind(account)
        .bind(person)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let value = json!({"title":"Historic role"});
    let content = monica_core::content::register(&mut tx, "structured_fact", &value)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("INSERT INTO crm.source_structured_revisions(id,source_account_id,fact_type,fact_key,content_object_id,content_hash,value,source_type,source_record_id,object_id,source_version,observed_at)
        SELECT gen_random_uuid(),$1,'employment','job-'||n,$2,$3,$4,'linkedin','fixture',gen_random_uuid(),1,'2026-02-25T12:00:00.123456Z' FROM generate_series(1,2)n")
        .bind(account).bind(content.id).bind(content.hash).bind(value).execute(&pool).await.unwrap();
    let script = format!(
        r#"
        const profile_id="{person}", group_id="{group}", connection_id="{connection}";
        const members=cli("group_members",{{group_id,limit:1}});
        const imports=cli("import_daily",{{connection_id,limit:1}});
        const history=cli("profile_history",{{profile_id,field_key:"monica.company",limit:1,fields:["value"]}});
        const fact=cli("profile_fact_history",{{profile_id:"{old_id}",fact_type:"employment",limit:1}});
        return {{year:cli("contact_calendar",{{year:2026}}), day:cli("contact_calendar",{{day:"2026-02-25",fields:["name","calls"]}}),
            activity:cli("contact_activity",{{profile_id,from_month:"2026-02",to_month:"2026-02"}}), calls:cli("contact_events_summary",{{profile_id:"{old_id}"}}),
            coverage:cli("source_coverage",{{profile_id:"{old_id}",provider:"mail"}}), imports, olderImports:cli("import_daily",{{connection_id,offset:imports.next_offset,limit:1}}),
            members, nextMember:cli("group_members",{{group_id,offset:members.next_offset,limit:1}}),
            social:cli("social_statistics",{{profile_id:"{old_id}",provider:"wechat",from_month:"2026-02"}}), history,
            olderHistory:cli("profile_history",{{profile_id,field_key:"monica.company",before:history.next,limit:1}}),
            longHistory:cli("profile_history",{{profile_id,field_key:"monica.note"}}), fact,
            olderFact:cli("profile_fact_history",{{profile_id:"{old_id}",fact_type:"employment", ...fact.next,limit:1}})}};
    "#
    );
    let (result, error, _) = lookup::run(&state, actor, chrono_tz::UTC, script).await;
    assert!(!error, "{result}");
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        result["year"]["rows"],
        json!([{"date":"2026-02-25","people":1}])
    );
    assert_eq!(result["day"]["rows"], json!([{"name":"Alex","calls":1}]));
    assert_eq!(result["activity"]["months"][0]["calls"], 1);
    assert_eq!(result["activity"]["months"][0]["interactions"], 1);
    assert_eq!(result["calls"]["profile_id"], json!(person));
    assert_eq!(result["coverage"]["profile_id"], json!(person));
    assert_eq!(result["social"]["profile_id"], json!(person));
    assert_eq!(result["calls"]["calls"]["duration_seconds"], 45);
    assert_eq!(result["coverage"]["sources"][0]["records"], 1);
    assert_eq!(result["imports"]["records"][0]["records"], 5);
    assert_eq!(result["olderImports"]["records"][0]["failures"], 1);
    assert_eq!(result["members"]["members"][0]["role"], "owner");
    assert_eq!(
        result["nextMember"]["members"][0]["profile_id"],
        json!(peer)
    );
    assert_eq!(
        result["social"]["providers"],
        json!([{
            "provider":"wechat","totals":{"posts":2,"likes":3,"comments":2,"authors":1},
            "monthly":[{"provider":"wechat","month":"2026-02","count":1}],
            "first_at":"2026-01-10T12:00:00+00:00","latest_at":"2026-02-10T12:00:00+00:00"
        }])
    );
    assert_eq!(result["history"]["revisions"], json!([{"value":"After"}]));
    assert_eq!(result["olderHistory"]["revisions"][0]["value"], "Before");
    assert_eq!(result["olderHistory"]["next"], Value::Null);
    assert_eq!(
        result["longHistory"]["revisions"][0]["value_truncated"],
        true
    );
    assert_eq!(
        result["fact"]["revisions"][0]["value"]["title"],
        "Historic role"
    );
    assert_ne!(
        result["fact"]["revisions"][0]["id"],
        result["olderFact"]["revisions"][0]["id"]
    );
    assert_eq!(
        result["olderFact"]["revisions"][0]["fact_type"],
        "employment"
    );
    assert_eq!(result["olderFact"]["next"], Value::Null);
}

#[sqlx::test(migrations = "../../migrations")]
async fn ask_reads_saved_aggregations_through_the_javascript_tools(pool: PgPool) {
    let (state, app) = setup(&pool).await;
    let owner: Uuid =
        sqlx::query_scalar("SELECT id FROM crm.app_users WHERE email='owner@example.invalid'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let person = Uuid::from_u128(1);
    let peer = Uuid::from_u128(2);
    let conversation = Uuid::new_v4();
    let post = Uuid::new_v4();
    let week = "2026-02-23";
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Person'),($2,'person','Peer')")
        .bind(person).bind(peer).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type) VALUES(gen_random_uuid(),$1,$2,'friend')")
        .bind(person).bind(peer).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version)
        VALUES($1,'wechat',$1,$2,'post','museum','2026-02-25T12:00:00Z','{\"location\":{\"name\":\"Museum\",\"lat\":41,\"lng\":-87}}','fixture',1),
        (gen_random_uuid(),'wechat',gen_random_uuid(),$2,'post','mall','2026-02-24T12:00:00Z','{\"location\":{\"name\":\"Mall\",\"lat\":42,\"lng\":-87}}','fixture',1)")
        .bind(post).bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) VALUES
        (gen_random_uuid(),'instagram',gen_random_uuid(),$1,'post','nearby','2026-02-25T12:00:00Z','{\"location\":{\"name\":\"Nearby studio\",\"lat\":35.0001,\"lng\":139}}','fixture',1),
        (gen_random_uuid(),'instagram',gen_random_uuid(),$1,'post','outside','2026-02-26T12:00:00Z','{\"location\":{\"name\":\"Outside studio\",\"lat\":35.0004,\"lng\":139}}','fixture',1)")
        .bind(peer).execute(&pool).await.unwrap();
    let filtered = request(
        &app,
        "owner",
        "GET",
        "/api/v1/metrics/locations?query=Nearby",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["points"][0]["label"], "Nearby studio");
    sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type,profile_id) VALUES($1,'wechat','self','peer','direct',$2)")
        .bind(conversation).bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body,message_type,direction) VALUES
        (gen_random_uuid(),$1,'wechat','self','first','peer','2026-03-01T00:30:00Z','fixture','1','received'),
        (gen_random_uuid(),$1,'wechat','self','second','peer','2026-03-01T06:30:00Z','fixture','1','sent')")
        .bind(conversation).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    let weekly = json!({"summary":"Visited a museum","sources":[{"kind":"post","id":post,"quote":"Visited a museum"}]});
    for (kind, subject, output) in [
        (
            "ai.profile",
            person.to_string(),
            json!({"summary":"Likes museums","facts":[],"gaps":[]}),
        ),
        ("ai.person_week", format!("{person}:{week}"), weekly.clone()),
        (
            "ai.person_week",
            format!("{peer}:{week}"),
            json!({"summary":"Peer update","sources":[]}),
        ),
        (
            "ai.digest",
            week.into(),
            json!({"summary":"Week in review","life_events":[{"profile_id":person,"post_id":post,"text":"Museum visit"}]}),
        ),
    ] {
        let job = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.ai_jobs(id,kind,subject,model,input_hash,input,source_versions,privacy,status)
            VALUES($1,$2,$3,'fake','fixture','{}','{}','{\"policy\":{\"include_private_notes\":false,\"include_messages\":false}}','complete')")
            .bind(job).bind(kind).bind(&subject).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.ai_outputs(kind,subject,output,model,input_hash,source_versions,job_id)
            VALUES($1,$2,$3,'fake','fixture','{}',$4)")
            .bind(kind).bind(&subject).bind(output).bind(job).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO crm.person_weekly_updates(profile_id,week_start,output,input_hash) VALUES
        ($1,$3::date,$4,'fixture'),($2,$3::date,'{\"summary\":\"Peer update\",\"sources\":[]}','fixture')")
        .bind(person).bind(peer).bind(week).bind(weekly).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.weekly_digests(week_start,output,input_hash) VALUES($1::date,'{\"facts\":{\"totals\":{\"messages\":2}}}','fixture')")
        .bind(week).execute(&pool).await.unwrap();

    let actor = (owner, "owner@example.invalid".into());
    let script = format!(
        r#"
        const profile_id = "{person}", week = "{week}";
        const people = cli("weekly_people", {{week, limit:1, fields:["name","link"]}});
        const map = cli("places", {{provider:"wechat",year:2026,kind:"post",limit:1,fields:["label","links"]}});
        const near = {{latitude:41,longitude:-87,radius_km:120}};
        const nearest = cli("places", {{near,kind:"post",limit:1,fields:["label","distance_km"]}});
        return {{summary:cli("profile_summary", {{profile_id,week}}),
            people, nextPerson:cli("weekly_people", {{week, after:people.next}}),
            weeks:cli("weekly_review", {{limit:1,fields:["digests.week","digests.link"]}}),
            review:cli("weekly_review", {{week}}), map,
            nextPlace:cli("places", {{provider:"wechat",year:2026,kind:"post",offset:map.next_offset,limit:1}}),
            personMap:cli("places", {{profile_id,query:"museum"}}),
            nearest, nextNear:cli("places", {{near,kind:"post",limit:1,offset:nearest.next_offset}}),
            personNear:cli("places", {{profile_id,near:{{...near,radius_km:75}}}}),
            nearCity:cli("places", {{near:{{city:"Chicago, Illinois, United States",radius_km:70}},kind:"post"}}),
            combined:cli("places", {{query:"mall",near:{{...near,radius_km:75}}}}),
            ambiguous:cli("places", {{near:{{city:"Springfield, US",radius_km:70}}}}),
            globalTimeline:cli("get_posts", {{provider:"wechat", since:"2026-02-25", fields:["name","profile_id","link"]}}),
            withinBucket:cli("places", {{near:{{latitude:35,longitude:139,radius_km:0.015}}}}),
            network:cli("relationship_network", {{profile_id,limit:1}}),
            chat:cli("chat_statistics", {{profile_id,from_month:"2026-02",to_month:"2026-02"}})}};
    "#
    );
    let (response, error, _) =
        lookup::run(&state, actor.clone(), chrono_tz::America::Chicago, script).await;
    assert!(!error, "{response}");
    let result: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(
        result["summary"]["memory"]["summary"], "Likes museums",
        "{result}"
    );
    assert_eq!(
        result["summary"]["recent_weeks"][0]["sources"][0]["link"],
        format!("/timeline?event={post}")
    );
    assert_eq!(
        result["people"]["people"],
        json!([{"name":"Person","link":format!("/profiles/{person}")}])
    );
    assert_eq!(result["people"]["total"], 2);
    assert_eq!(result["nextPerson"]["people"][0]["name"], "Peer");
    assert_eq!(result["nextPerson"]["next"], Value::Null);
    assert_eq!(
        result["weeks"]["digests"],
        json!([{"week":week,"link":format!("/digest?week={week}")}])
    );
    assert_eq!(result["review"]["facts"]["totals"]["messages"], 2);
    assert_eq!(
        result["review"]["narrative"]["life_events"][0]["link"],
        format!("/timeline?event={post}")
    );
    assert_eq!(result["map"]["points"][0]["label"], "Mall");
    assert_eq!(result["map"]["total"], 2);
    assert_eq!(result["nextPlace"]["points"][0]["label"], "Museum");
    assert_eq!(result["personMap"]["total"], 1);
    assert_eq!(result["personMap"]["points"][0]["precision"], "coordinate");
    assert_eq!(
        result["nearest"]["points"],
        json!([{"label":"Museum","distance_km":0}])
    );
    assert_eq!(result["nearest"]["total"], 2);
    assert_eq!(result["nextNear"]["points"][0]["label"], "Mall");
    assert_eq!(result["nextNear"]["next_offset"], Value::Null);
    assert_eq!(result["personNear"]["points"][0]["label"], "Museum");
    assert_eq!(result["personNear"]["total"], 1);
    assert_eq!(result["nearCity"]["near"]["city"], "Chicago · US");
    assert_eq!(result["nearCity"]["points"][0]["label"], "Mall");
    assert_eq!(result["nearCity"]["total"], 1);
    assert_eq!(result["combined"]["total"], 0);
    assert_eq!(
        result["withinBucket"]["points"][0]["label"],
        "Nearby studio"
    );
    assert_eq!(result["withinBucket"]["points"][0]["count"], 1);
    assert_eq!(
        result["globalTimeline"]["posts"],
        json!([{"name":"Person","profile_id":person,
        "link":format!("/timeline?event={post}")}])
    );
    assert!(
        result["ambiguous"]["error"]
            .as_str()
            .unwrap()
            .contains("无法唯一识别")
    );
    assert_eq!(
        result["network"]["edges"][0]["to"]["profile_id"],
        json!(peer)
    );
    assert_eq!(
        result["network"]["edges"][0]["relationship_types"],
        json!(["friend"])
    );
    assert_eq!(
        result["chat"]["months"],
        json!([{"month":"2026-02","sent":0,"received":1,"system":0,"unknown":0,"total":1,"records":1}])
    );
    assert_eq!(result["chat"]["total"], 1);

    sqlx::query("UPDATE crm.ai_settings SET include_messages=true")
        .execute(&pool)
        .await
        .unwrap();
    let script = format!(
        r#"return [cli("profile_summary",{{profile_id:"{person}"}}),cli("weekly_review",{{week:"{week}"}})];"#
    );
    let (response, error, _) =
        lookup::run(&state, actor, chrono_tz::America::Chicago, script).await;
    assert!(!error, "{response}");
    let hidden: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(hidden[0]["memory"], Value::Null);
    assert_eq!(hidden[0]["recent_weeks"], json!([]));
    assert_eq!(hidden[1]["narrative"], Value::Null);
    assert_eq!(hidden[1]["facts"]["totals"]["messages"], 2);
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

async fn fixture_edit_job(pool: &PgPool, path: &str) -> Job {
    let chat = Uuid::parse_str(path.rsplit('/').next().unwrap()).unwrap();
    let run = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.ask_runs(id,chat_id,model,timezone,people) VALUES($1,$2,'fixture','UTC','[]')")
        .bind(run).bind(chat).execute(pool).await.unwrap();
    let job = claim(pool).await.unwrap().unwrap();
    assert_eq!(job.id, run);
    job
}

async fn proposed_id(pool: &PgPool, job: &Job, call: &str, action: &Value) -> Uuid {
    let receipt = save_edit_proposal(pool, job, &job.checkpoint.0, call, action)
        .await
        .unwrap();
    assert_eq!(receipt["isError"], false, "{receipt}");
    Uuid::parse_str(receipt["details"]["editId"].as_str().unwrap()).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn asks_can_read_and_confirm_suggestions_and_list_birthdays(pool: PgPool) {
    let (state, app) = setup(&pool).await;
    let path = new_chat(&app).await;
    let ask_job = fixture_edit_job(&pool, &path).await;
    let person = Uuid::new_v4();
    let peer = Uuid::new_v4();
    let model_job = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Person'),($2,'person','Peer')")
        .bind(person).bind(peer).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profile_dates(id,profile_id,kind,date_precision,month,day) VALUES(gen_random_uuid(),$1,'birthday','month_day',2,3)")
        .bind(peer).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.ai_jobs(id,kind,subject,model,input_hash,input,source_versions,privacy,status)
        VALUES($1,'ai.profile',$2,'fake','fixture','{}','{}','{\"policy\":{\"include_private_notes\":false,\"include_messages\":false}}','complete')")
        .bind(model_job).bind(person.to_string()).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO crm.ai_outputs(kind,subject,output,model,input_hash,source_versions,job_id)
        VALUES('ai.profile',$1,'{}','fake','fixture','{}',$2)",
    )
    .bind(person.to_string())
    .bind(model_job)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.profile_suggestions(profile_id,fingerprint,kind,value,evidence,quote,confidence,input_hash) VALUES
        ($1,'birthday-fixture','birthday','04-05','[]','Born April 5','high','fixture'),
        ($1,'company-fixture','company','Old Company','[]','Company name','high','fixture'),
        ($1,'school-fixture','school','Old School','[]','School name','high','fixture')")
        .bind(person).execute(&pool).await.unwrap();
    request(
        &app,
        "owner",
        "PATCH",
        &format!("/api/v1/profiles/{person}/fields"),
        json!({"field_key":"monica.company","value":"Existing Company"}),
        StatusCode::OK,
    )
    .await;
    let actions = [
        json!({"action":"decide_suggestion","profile_id":person,"fingerprint":"birthday-fixture","decision":"accept"}),
        json!({"action":"decide_suggestion","profile_id":person,"fingerprint":"company-fixture","decision":"accept","value":"Correct Company"}),
        json!({"action":"decide_suggestion","profile_id":person,"fingerprint":"school-fixture","decision":"ignore"}),
    ];
    let mut proposed = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        proposed.push(proposed_id(&pool, &ask_job, &format!("suggestion-{index}"), action).await);
    }
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.profile_suggestion_decisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, 0);
    let receipt = request(
        &app,
        "owner",
        "GET",
        &format!("{path}?after_edit=0"),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        receipt["edits"][1]["preview"]["before"]["原建议"],
        "Old Company"
    );
    assert_eq!(
        receipt["edits"][1]["preview"]["before"]["联系人当前值"],
        "Existing Company"
    );
    assert_eq!(
        receipt["edits"][1]["preview"]["after"]["新值"],
        "Correct Company"
    );
    let actor = (ask_job.owner_id, ask_job.email.clone());
    let script = format!(
        r#"return cli("profile_suggestions",{{profile_id:"{person}",kind:"company",fields:["items.fingerprint","items.value"]}});"#
    );
    let (reply, error, _) = lookup::run(&state, actor.clone(), chrono_tz::UTC, script).await;
    assert!(!error, "{reply}");
    let read: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(
        read["people"][0]["items"],
        json!([{"fingerprint":"company-fixture","value":"Old Company"}])
    );
    for (index, id) in proposed.into_iter().enumerate() {
        let response = request(
            &app,
            "owner",
            "POST",
            &format!("{path}/edits/{id}"),
            json!({"confirm":true}),
            StatusCode::OK,
        )
        .await;
        assert_eq!(response["status"], "applied", "{response}");
        assert_eq!(
            response["result"]["decision"],
            if index == 2 { "ignored" } else { "accepted" }
        );
    }
    let edited: Value = sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key='monica.company'")
        .bind(person).fetch_one(&pool).await.unwrap();
    assert_eq!(edited, "Correct Company");
    let (reply,error,_) = lookup::run(&state,actor,chrono_tz::UTC,r#"
        const first=cli("birthdays",{limit:1});
        return {first, second:cli("birthdays",{offset:first.next_offset,limit:1}), april:cli("birthdays",{month:4})};
    "#.into()).await;
    assert!(!error, "{reply}");
    let read: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(read["first"]["total"], 2);
    assert_eq!(read["first"]["rows"][0]["profile_id"], json!(peer));
    assert_eq!(read["second"]["rows"][0]["profile_id"], json!(person));
    assert_eq!(read["second"]["rows"][0]["year"], Value::Null);
    assert_eq!(read["april"]["rows"][0]["precision"], "month_day");
}

#[sqlx::test(migrations = "../../migrations")]
async fn ask_confirms_linkedin_tracking_and_previews_profile_merges(pool: PgPool) {
    let (state, app) = setup(&pool).await;
    let path = new_chat(&app).await;
    let job = fixture_edit_job(&pool, &path).await;
    let [source, target] = [Uuid::new_v4(), Uuid::new_v4()];
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Source'),($2,'person','Target')")
        .bind(source).bind(target).execute(&pool).await.unwrap();
    let url = "https://www.linkedin.com/in/test-person";
    let action = json!({
        "action":"add_linkedin_target","profile_id":source,
        "account":"https://uk.linkedin.com/in/Test-Person/?trk=fixture"
    });
    let edit = proposed_id(&pool, &job, "linkedin", &action).await;
    let endpoint = format!("{path}/edits/{edit}");
    assert_eq!(
        monica_core::collector::targets(&pool).await.unwrap()["targets"],
        json!([])
    );
    request(
        &app,
        "other",
        "POST",
        &endpoint,
        json!({"confirm":true}),
        StatusCode::NOT_FOUND,
    )
    .await;
    let receipt = request(
        &app,
        "owner",
        "POST",
        &endpoint,
        json!({"confirm":true}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(receipt["status"], "applied");
    assert_eq!(receipt["result"]["profile_url"], url);
    assert_eq!(
        monica_core::collector::targets(&pool).await.unwrap()["targets"][0]["profile_url"],
        url
    );
    let script = format!(
        r#"
        return {{person:cli("get_person",{{profile_id:"{source}",fields:["sources.linkedin.profile_url"]}}),
          merge:cli("profile_merge_preview",{{source_profile_id:"{source}",target_profile_id:"{target}"}})}};
    "#
    );
    let (reply, error, _) =
        lookup::run(&state, (job.owner_id, job.email), chrono_tz::UTC, script).await;
    assert!(!error, "{reply}");
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["person"]["sources"]["linkedin"]["profile_url"], url);
    assert_eq!(reply["merge"]["source"]["name"], "Source");
    assert_eq!(reply["merge"]["target"]["name"], "Target");
    assert_eq!(reply["merge"]["counts"]["accounts"], 1);
    assert_eq!(reply["merge"]["can_merge"], true);
    assert_eq!(
        reply["merge"]["link"],
        format!("/profiles/{source}?merge_with={target}")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.profile_merges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn ask_edits_require_owner_confirmation_and_commit_once(pool: PgPool) {
    let (state, app) = setup(&pool).await;
    let path = new_chat(&app).await;
    let job = fixture_edit_job(&pool, &path).await;
    let run = job.id;
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

    request(
        &app,
        "owner",
        "PATCH",
        &format!("/api/v1/profiles/{first}/fields"),
        json!({"field_key":"monica.custom_fields","value":{"现居":"北京","未修改":"保留"}}),
        StatusCode::OK,
    )
    .await;

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
        let id = proposed_id(&pool, &job, &key, action).await;
        assert_eq!(proposed_id(&pool, &job, &key, action).await, id);
        ids.push(id);
    }
    let unknown = json!({"action":"set_profile_field","profile_id":first,"field":"wechat.nickname","value":"禁止"});
    let invalid = save_edit_proposal(&pool, &job, &job.checkpoint.0, "unknown-write", &unknown)
        .await
        .unwrap();
    assert_eq!(invalid["isError"], true);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.activities WHERE title IN ('重聚纪要','线下见面')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    let page = request(
        &app,
        "owner",
        "GET",
        &format!("{path}?after_edit=0"),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(page["edits"].as_array().unwrap().len(), actions.len());
    assert!(
        page["edits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|edit| edit["status"] == "pending")
    );
    assert!(
        page["edits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edit| edit["preview"]["before"]["常驻地"] == json!(["北京"]))
    );
    let edit_cursor = page["edits"].as_array().unwrap().last().unwrap()["seq"]
        .as_i64()
        .unwrap();
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

    let legacy: Value = sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key='monica.custom_fields'")
        .bind(first).fetch_one(&pool).await.unwrap();
    assert_eq!(legacy, json!({"未修改":"保留"}));
    let updated = request(
        &app,
        "owner",
        "GET",
        &format!("{path}?after_edit={edit_cursor}"),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(updated["edits"].as_array().unwrap().len(), actions.len());
    assert!(
        updated["edits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|edit| edit["status"] == "applied")
    );

    let replacement =
        json!({"action":"set_profile_field","profile_id":first,"field":"city","value":"南京"});
    let stale = proposed_id(&pool, &job, "old-preview", &replacement).await;
    let newer =
        json!({"action":"set_profile_field","profile_id":first,"field":"city","value":"杭州"});
    let new_id = proposed_id(&pool, &job, "new-preview", &newer).await;
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

    // Force the transcript insert to fail after preparing a valid proposal.
    sqlx::query("ALTER TABLE crm.ask_messages ADD CHECK (message->>'toolCallId' IS DISTINCT FROM 'unpublishable')")
        .execute(&pool).await.unwrap();
    assert!(
        save_edit_proposal(&pool, &job, &job.checkpoint.0, "unpublishable", &actions[0])
            .await
            .is_err()
    );
    let orphaned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM crm.ask_edits WHERE run_id=$1 AND tool_call_hash=$2)",
    )
    .bind(run)
    .bind(monica_core::identity::bytes_hash(b"unpublishable"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!orphaned);
    let pending = proposed_id(&pool, &job, "cancelled-run", &actions[0]).await;
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

#[sqlx::test(migrations = "../../migrations")]
async fn model_edit_tool_preserves_arguments_and_long_provider_id(pool: PgPool) {
    let (state, app) = setup(&pool).await;
    let path = new_chat(&app).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let arguments = json!({"action":"create_tag","tag":"同学重聚"});
    let id = format!(
        "call_fixture__thought__{}",
        (0..150)
            .map(|_| Uuid::new_v4().simple().to_string())
            .collect::<String>()
    );
    let fake = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let id = id.clone();
        let arguments = arguments.clone();
        async move {
            let messages = body["messages"].as_array().unwrap();
            if let Some(result) = messages.iter().find(|message| message["role"] == "tool") {
                assert_eq!(result["tool_call_id"], id);
                let output: Value = serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
                assert_eq!(output["status"], "pending_confirmation");
                events(&[(json!({"content":"请确认创建标签。"}),json!("stop"))],true)
            } else {
                let call = json!({"index":0,"id":id,"function":{"name":edits::NAME,"arguments":arguments.to_string()}});
                events(&[(json!({"tool_calls":[call]}),json!("tool_calls"))],true)
            }
        }
    }));
    let upstream = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
    request(&app,"owner","PATCH","/api/v1/ai/settings",
        json!({"enabled":true,"base_url":endpoint,"model":"deepseek-v4-pro","api_key":"synthetic-key"}),StatusCode::OK).await;
    let run_id = Uuid::new_v4();
    request(
        &app,
        "owner",
        "POST",
        &format!("{path}/runs"),
        question(run_id, "创建一个叫同学重聚的标签"),
        StatusCode::ACCEPTED,
    )
    .await;
    let job = claim(&pool).await.unwrap().unwrap();
    assert_eq!(job.id, run_id);
    timeout(Duration::from_secs(15), run(&state, &job))
        .await
        .unwrap()
        .unwrap();
    let saved = request(
        &app,
        "owner",
        "GET",
        &format!("{path}?after_edit=0"),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(saved["run"]["status"], "done");
    let card = &saved["edits"][0];
    assert_eq!(card["status"], "pending");
    assert_eq!(card["preview"]["after"]["标签"], "同学重聚");
    assert!(
        saved["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["message"]["details"]["editId"] == card["id"])
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.tags WHERE name='同学重聚'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    upstream.abort();
}

#[sqlx::test(migrations = "../../migrations")]
async fn edit_receipt_pages_and_case_concurrent_tag_creation(pool: PgPool) {
    let (_, app) = setup(&pool).await;
    let first = new_chat(&app).await;
    let second = new_chat(&app).await;
    let first_job = fixture_edit_job(&pool, &first).await;
    let second_job = fixture_edit_job(&pool, &second).await;
    let a = proposed_id(
        &pool,
        &first_job,
        "case-a",
        &json!({"action":"create_tag","tag":"ClassNotes"}),
    )
    .await;
    let b = proposed_id(
        &pool,
        &second_job,
        "case-b",
        &json!({"action":"create_tag","tag":"classnotes"}),
    )
    .await;
    let first_decision = format!("{first}/edits/{a}");
    let second_decision = format!("{second}/edits/{b}");
    let (a_receipt, b_receipt) = tokio::join!(
        request(
            &app,
            "owner",
            "POST",
            &first_decision,
            json!({"confirm":true}),
            StatusCode::OK
        ),
        request(
            &app,
            "owner",
            "POST",
            &second_decision,
            json!({"confirm":true}),
            StatusCode::OK
        ),
    );
    let statuses = [
        a_receipt["status"].as_str().unwrap(),
        b_receipt["status"].as_str().unwrap(),
    ];
    assert!(statuses.contains(&"applied") && statuses.contains(&"stale"));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.tags WHERE lower(name)='classnotes'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);

    sqlx::query(
        "INSERT INTO crm.ask_edits(id,run_id,tool_call_hash,action,preview,status)
        SELECT gen_random_uuid(),run_id,lpad(n::text,64,'0'),action,preview,'dismissed'
        FROM crm.ask_edits CROSS JOIN generate_series(1,32) n WHERE id=$1",
    )
    .bind(a)
    .execute(&pool)
    .await
    .unwrap();
    let page = request(
        &app,
        "owner",
        "GET",
        &format!("{first}?after_edit=0"),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    let rows = page["edits"].as_array().unwrap();
    assert_eq!(rows.len(), 30);
    assert_eq!(rows[0]["id"], json!(a));
    assert_eq!(page["moreEdits"], true);
    let cursor = rows.last().unwrap()["seq"].as_i64().unwrap();
    let next = request(
        &app,
        "owner",
        "GET",
        &format!("{first}?after_edit={cursor}"),
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert_eq!(next["edits"].as_array().unwrap().len(), 3);
    assert_eq!(next["moreEdits"], false);
}

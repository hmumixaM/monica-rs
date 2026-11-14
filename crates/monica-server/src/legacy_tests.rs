use crate::auth::{self, AppState};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

async fn call(
    app: &Router,
    host: &str,
    method: &str,
    path: &str,
    cookies: &str,
    csrf: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", host)
                .header("cookie", cookies)
                .header("x-csrf-token", csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn same_host_ports_keep_sessions_and_csrf_independent(pool: PgPool) {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic@example.test','synthetic')").bind(id).execute(&pool).await.unwrap();
    for token in ["old-session", "new-session"] {
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(id).bind(format!("{:x}",Sha256::digest(token.as_bytes()))).execute(&pool).await.unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let old = auth::routes(
        AppState::new(
            pool.clone(),
            "synthetic-old-installation-secret-32bytes",
            "https://nas.test:9443".into(),
            root.path().into(),
        )
        .await
        .unwrap(),
    );
    let mut state = AppState::new(
        pool.clone(),
        "synthetic-new-installation-secret-32bytes",
        "https://nas.test:9444".into(),
        root.path().into(),
    )
    .await
    .unwrap();
    assert!(state.configure_cookie_prefix("invalid;cookie").is_err());
    state.configure_cookie_prefix("monica_rs").unwrap();
    let new = auth::routes(state);
    let cookies = "crm_session=old-session; monica_rs_session=new-session; crm_csrf=old-csrf; monica_rs_csrf=new-csrf";
    assert_eq!(
        call(&old, "nas.test:9443", "GET", "/api/v1/me", cookies, "")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(&new, "nas.test:9444", "GET", "/api/v1/me", cookies, "")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(
            &new,
            "nas.test:9444",
            "GET",
            "/api/v1/me",
            "crm_session=old-session",
            ""
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &new,
            "nas.test:9444",
            "POST",
            "/api/v1/auth/logout",
            cookies,
            "old-csrf"
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let csrf = call(
        &new,
        "nas.test:9444",
        "GET",
        "/api/v1/auth/csrf",
        cookies,
        "",
    )
    .await;
    assert!(
        csrf.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .starts_with("monica_rs_csrf=")
    );
    let logout = call(
        &new,
        "nas.test:9444",
        "POST",
        "/api/v1/auth/logout",
        cookies,
        "new-csrf",
    )
    .await;
    assert_eq!(logout.status(), StatusCode::OK);
    assert!(
        logout.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .starts_with("monica_rs_session=")
    );
    assert_eq!(
        call(&old, "nas.test:9443", "GET", "/api/v1/me", cookies, "")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(&new, "nas.test:9444", "GET", "/api/v1/me", cookies, "")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn saved_analysis_is_readable_with_execution_disabled(pool: PgPool) {
    let user = Uuid::new_v4();
    let profile = Uuid::new_v4();
    let job = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic@example.test','synthetic')").bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(user).bind(format!("{:x}",Sha256::digest(b"new-session"))).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic person')").bind(profile).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=false")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.ai_jobs(id,kind,subject,model,input_hash,input,source_versions,privacy,status) VALUES($1,'ai.profile',$2,'historical','original','{}','{}','{\"policy\":{\"include_messages\":false,\"include_private_notes\":false}}','complete')").bind(job).bind(profile.to_string()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.ai_outputs(kind,subject,output,model,input_hash,source_versions,job_id) VALUES('ai.profile',$1,$2,'historical','original','{}',$3)").bind(profile.to_string()).bind(json!({"summary":"Original saved summary","facts":[],"gaps":[]})).bind(job).execute(&pool).await.unwrap();
    monica_core::ai::enqueue(&pool, "ai.profile", &profile.to_string())
        .await
        .unwrap();
    assert!(monica_core::ai::claim(&pool).await.unwrap().is_none());
    let root = tempfile::tempdir().unwrap();
    let app = auth::routes(
        AppState::new(
            pool,
            "synthetic-installation-secret-at-least-32bytes",
            "http://testserver".into(),
            root.path().into(),
        )
        .await
        .unwrap(),
    );
    let response = call(
        &app,
        "testserver",
        "GET",
        &format!("/api/v1/ai/profiles/{profile}"),
        "crm_session=new-session",
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["memory"]["summary"], "Original saved summary");
    let browser = call(
        &app,
        "testserver",
        "GET",
        "/api/v1/ai/browser-model",
        "crm_session=new-session",
        "",
    )
    .await;
    let body: Value =
        serde_json::from_slice(&to_bytes(browser.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["enabled"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn imported_messages_expose_bounded_evidence_and_safe_original_files(pool: PgPool) {
    let account = Uuid::new_v4();
    let conversation = Uuid::new_v4();
    let message = Uuid::new_v4();
    let content = Uuid::new_v4();
    let original = Uuid::new_v4();
    let media = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic@example.test','synthetic')").bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(account).bind(format!("{:x}",Sha256::digest(b"new-session"))).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.conversations(id,conversation_type,provider,account,external_id) VALUES($1,'direct','wechat','synthetic-self','direct:synthetic-other')").bind(conversation).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body,message_type,direction) VALUES($1,$2,'wechat','synthetic-self','synthetic-message','synthetic-self',now(),'Searchable original','3','sent')").bind(message).bind(conversation).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json) VALUES($1,'message',repeat('a',64),30,'{\"text\":\"Complete original message\"}'),($2,'raw_json',repeat('b',64),10,'null')").bind(content).bind(original).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO crm.legacy_message_evidence VALUES($1,'server:synthetic',$2,NULL,'{}')",
    )
    .bind(message)
    .bind(content)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,file_name) VALUES($1,repeat('c',64),'image/jpeg',200,'synthetic','message.jpg')").bind(media).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('message_event',$1,$2,'attachment')").bind(message).bind(media).execute(&pool).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("archive");
    std::fs::create_dir_all(archive.join("raw")).unwrap();
    std::fs::write(archive.join("raw/original.json"), b"0123456789").unwrap();
    std::fs::write(root.path().join("outside"), b"outside archive").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join("outside"), archive.join("raw/escape")).unwrap();
    sqlx::query("INSERT INTO crm.legacy_content_locations(content_object_id,storage_uri) VALUES($1,'/data/raw/original.json')").bind(original).execute(&pool).await.unwrap();
    let mut state = AppState::new(
        pool.clone(),
        "synthetic-installation-secret-32bytes",
        "http://testserver".into(),
        root.path().join("media"),
    )
    .await
    .unwrap();
    state.legacy_archive_root = Some(archive);
    let app = auth::routes(state);
    let response = call(
        &app,
        "testserver",
        "GET",
        &format!("/api/v1/conversations/{conversation}/messages"),
        "crm_session=new-session",
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body[0]["media"][0]["id"], media.to_string());
    assert_eq!(body[0]["direction"], "sent");
    assert_eq!(
        body[0]["citation"],
        format!("/evidence/{message}?revision=1")
    );
    let response = call(
        &app,
        "testserver",
        "GET",
        &format!("/api/v1/evidence/{message}?body_limit=8&original=true"),
        "crm_session=new-session",
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["body"], "Complete");
    assert_eq!(body["next_body_offset"], 8);
    assert_eq!(
        call(
            &app,
            "testserver",
            "GET",
            &format!("/api/v1/evidence/{message}?revision=2"),
            "crm_session=new-session",
            ""
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let snapshot = Uuid::new_v4();
    let revised = Uuid::new_v4();
    let intermediate = Uuid::new_v4();
    let old_sender = Uuid::new_v4();
    let middle_sender = Uuid::new_v4();
    let current_sender = Uuid::new_v4();
    let original_person = Uuid::new_v4();
    let relinked_person = Uuid::new_v4();
    let current_person = Uuid::new_v4();
    for (id, external) in [
        (old_sender, "original-sender"),
        (middle_sender, "intermediate-sender"),
        (current_sender, "current-sender"),
    ] {
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat',$2,'person',$2)").bind(id).bind(external).execute(&pool).await.unwrap();
    }
    for id in [original_person, relinked_person, current_person] {
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic sender')").bind(id).execute(&pool).await.unwrap();
    }
    for (source, person) in [
        (old_sender, relinked_person),
        (current_sender, current_person),
    ] {
        sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
            .bind(source)
            .bind(person)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE crm.message_search SET sender_account_id=$2 WHERE message_id=$1")
        .bind(message)
        .bind(current_sender)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.legacy_chatlog_snapshots(id,manifest,own_id,status) VALUES($1,'{\"files\":[]}','synthetic-self','converted')").bind(snapshot).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json) VALUES($1,'message',repeat('d',64),30,'{\"text\":\"Corrected raw message\"}')").bind(revised).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json) VALUES($1,'message',repeat('e',64),30,'{\"text\":\"Intermediate raw message\",\"sender_external_id\":\"intermediate-sender\"}')").bind(intermediate).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.legacy_chatlog_projection VALUES('message',$1,$2,$3,3)").bind(message).bind(json!({"body":"Searchable original","sender_account_id":old_sender,"sender_profile_id":original_person})).bind(json!({"body":"Corrected visible message","sender_account_id":current_sender})).execute(&pool).await.unwrap();
    for (revision, cid, row) in [
        (
            2,
            intermediate,
            json!({"body":"Intermediate visible message"}),
        ),
        (
            3,
            revised,
            json!({"body":"Corrected visible message","sender_account_id":current_sender}),
        ),
    ] {
        sqlx::query("INSERT INTO crm.legacy_chatlog_versions(entity_type,entity_id,revision,content_object_id,saved_row,snapshot_id) VALUES('message',$1,$2,$3,$4,$5)").bind(message).bind(revision).bind(cid).bind(row).bind(snapshot).execute(&pool).await.unwrap();
    }
    for (query, expected, selected, sender, person) in [
        (
            "",
            "Corrected raw message",
            3,
            "current-sender",
            json!(current_person),
        ),
        (
            "&revision=2",
            "Intermediate raw message",
            2,
            "intermediate-sender",
            Value::Null,
        ),
        (
            "&revision=1",
            "Complete original message",
            1,
            "original-sender",
            json!(original_person),
        ),
    ] {
        let response = call(
            &app,
            "testserver",
            "GET",
            &format!("/api/v1/evidence/{message}?original=true&body_limit=100{query}"),
            "crm_session=new-session",
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(body["body"], expected);
        assert_eq!(body["revision"], selected);
        assert_eq!(body["metadata"]["participants"][0]["id"], sender);
        assert_eq!(body["metadata"]["participants"][0]["profile_id"], person);
    }
    // A manual sender overrides an account link, and older messages can have
    // that confirmed person as their sole attribution.
    for source in [Some(current_sender), None] {
        sqlx::query("UPDATE crm.message_search SET sender_account_id=$2,sender_profile_id=$3 WHERE message_id=$1").bind(message).bind(source).bind(original_person).execute(&pool).await.unwrap();
        let response = call(
            &app,
            "testserver",
            "GET",
            &format!("/api/v1/evidence/{message}"),
            "crm_session=new-session",
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(
            body["metadata"]["participants"][0]["profile_id"],
            json!(original_person)
        );
    }
    let url = format!("/api/v1/imports/legacy/content/{original}");
    assert_eq!(
        call(&app, "testserver", "GET", &url, "", "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&url)
                .header("host", "testserver")
                .header("cookie", "crm_session=new-session")
                .header("range", "bytes=2-4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        &to_bytes(response.into_body(), 65536).await.unwrap()[..],
        b"234"
    );
    for path in ["/data/raw/../outside", "/data/raw/escape", "/etc/passwd"] {
        sqlx::query(
            "UPDATE crm.legacy_content_locations SET storage_uri=$1 WHERE content_object_id=$2",
        )
        .bind(path)
        .bind(original)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            call(
                &app,
                "testserver",
                "GET",
                &url,
                "crm_session=new-session",
                ""
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
    }
    sqlx::query("INSERT INTO crm.legacy_chatlog_tables(snapshot_id,file_name,table_name,mode,expected,processed,status,reason) SELECT $1,'contact/contact.db','archive_'||lpad(n::text,3,'0'),'history',26,26,'complete','Synthetic archive' FROM generate_series(0,100)n")
        .bind(snapshot).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.legacy_chatlog_history SELECT $1,'contact/contact.db','archive_000',n,jsonb_build_object('number',n) FROM generate_series(0,25)n")
        .bind(snapshot).execute(&pool).await.unwrap();
    let tables = format!("/api/v1/imports/chatlog/{snapshot}/tables");
    let history = format!(
        "/api/v1/imports/chatlog/{snapshot}/history?file=contact%2Fcontact.db&table=archive_000"
    );
    for url in [&tables, &history] {
        assert_eq!(
            call(&app, "testserver", "GET", url, "", "").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    for (url, count, first_key) in [
        (tables.clone(), 100, None),
        (
            format!("{tables}?after_file=contact%2Fcontact.db&after_table=archive_099"),
            1,
            None,
        ),
        (history.clone(), 25, Some("0")),
        (format!("{history}&after=24"), 1, Some("25")),
    ] {
        let response = call(
            &app,
            "testserver",
            "GET",
            &url,
            "crm_session=new-session",
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(body["records"].as_array().unwrap().len(), count);
        if let Some(key) = first_key {
            assert_eq!(body["records"][0]["key"], key);
        }
    }
    assert_eq!(
        call(
            &app,
            "testserver",
            "GET",
            &format!("{tables}?after_file=contact.db"),
            "crm_session=new-session",
            ""
        )
        .await
        .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn legacy_relationships_and_tags_follow_links_without_reserving_manual_slots(pool: PgPool) {
    use monica_core::linking::{LinkChange, set_link};
    let actor = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let c = Uuid::new_v4();
    let account = Uuid::new_v4();
    let edge = Uuid::new_v4();
    let tag = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic@example.test','synthetic')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"new-session"))).execute(&pool).await.unwrap();
    for (id, name) in [(a, "Synthetic A"), (b, "Synthetic B"), (c, "Synthetic C")] {
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2)",
        )
        .bind(id)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE crm.profiles SET summary='Manually retained person' WHERE id=$1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat','synthetic-account','person','Synthetic source')").bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
        .bind(account)
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type,note,legacy_source_owned) VALUES($1,$2,$3,'friend','Original source note',true)").bind(edge).bind(a).bind(c).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO crm.legacy_relationship_owners(edge_id,from_account_id) VALUES($1,$2)",
    )
    .bind(edge)
    .bind(account)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.tags(id,name) VALUES($1,'Synthetic source tag')")
        .bind(tag)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.source_account_tags VALUES($1,$2)")
        .bind(account)
        .bind(tag)
        .execute(&pool)
        .await
        .unwrap();
    set_link(
        &pool,
        LinkChange {
            account_id: account,
            profile_id: Some(b),
            expected_profile_id: Some(a),
        },
        Some(actor),
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>(
            "SELECT profile_id FROM crm.presented_tag_memberships WHERE tag_id=$1"
        )
        .bind(tag)
        .fetch_one(&pool)
        .await
        .unwrap(),
        b
    );
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>(
            "SELECT from_profile_id FROM crm.presented_relationship_edges WHERE id=$1"
        )
        .bind(edge)
        .fetch_one(&pool)
        .await
        .unwrap(),
        b
    );
    let root = tempfile::tempdir().unwrap();
    let app = auth::routes(
        AppState::new(
            pool.clone(),
            "synthetic-installation-secret-32bytes",
            "http://testserver".into(),
            root.path().into(),
        )
        .await
        .unwrap(),
    );
    let response=app.oneshot(Request::builder().method("POST").uri(format!("/api/v1/profiles/{a}/relationships")).header("host","testserver").header("content-type","application/json").header("cookie","crm_session=new-session; crm_csrf=synthetic-csrf").header("x-csrf-token","synthetic-csrf").body(Body::from(json!({"to_profile_id":c,"relationship_type":"friend","note":"Manual person relationship"}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    set_link(
        &pool,
        LinkChange {
            account_id: account,
            profile_id: None,
            expected_profile_id: Some(b),
        },
        Some(actor),
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.presented_tag_memberships WHERE tag_id=$1"
        )
        .bind(tag)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT note FROM crm.presented_relationship_edges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "Manual person relationship"
    );
    set_link(
        &pool,
        LinkChange {
            account_id: account,
            profile_id: Some(a),
            expected_profile_id: None,
        },
        Some(actor),
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.presented_relationship_edges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT note FROM crm.presented_relationship_edges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "Manual person relationship"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.relationship_edges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn chatlog_archive_http_is_authenticated_scoped_and_keyset_paged(pool: PgPool) {
    let user = Uuid::new_v4();
    let snapshot = Uuid::new_v4();
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'archive@example.test','synthetic')").bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(user).bind(format!("{:x}",Sha256::digest(b"archive-session"))).execute(&pool).await.unwrap();
    for id in [snapshot, other] {
        sqlx::query("INSERT INTO crm.legacy_chatlog_snapshots(id,manifest,own_id,status) VALUES($1,'{\"files\":[]}','synthetic-self','converted')").bind(id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.legacy_chatlog_tables(snapshot_id,file_name,table_name,mode,expected,processed,status,reason) VALUES($1,'a.db','first','history',2,2,'complete','synthetic'),($1,'b.db','second','history',0,0,'complete','synthetic')").bind(id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.legacy_chatlog_history VALUES($1,'a.db','first',9007199254740993,$2),($1,'a.db','first',9007199254740995,$3)").bind(id).bind(json!({"owner":id,"text":"first"})).bind(json!({"owner":id,"text":"second"})).execute(&pool).await.unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let app = auth::routes(
        AppState::new(
            pool,
            "synthetic-archive-secret-at-least-32bytes",
            "http://testserver".into(),
            root.path().into(),
        )
        .await
        .unwrap(),
    );
    let cookies = "crm_session=archive-session";
    let catalog = format!("/api/v1/imports/chatlog/{snapshot}/tables?limit=1");
    assert_eq!(
        call(&app, "testserver", "GET", &catalog, "", "")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "testserver",
            "GET",
            &format!("/api/v1/imports/chatlog/{snapshot}/tables?after_file=a.db"),
            cookies,
            ""
        )
        .await
        .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let response = call(&app, "testserver", "GET", &catalog, cookies, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["next"],
        json!({"after_file":"a.db","after_table":"first"})
    );
    let response = call(
        &app,
        "testserver",
        "GET",
        &format!("/api/v1/imports/chatlog/{snapshot}/tables?after_file=a.db&after_table=first"),
        cookies,
        "",
    )
    .await;
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["records"][0]["file_name"], "b.db");
    let history =
        format!("/api/v1/imports/chatlog/{snapshot}/history?file=a.db&table=first&limit=1");
    assert_eq!(
        call(&app, "testserver", "GET", &history, "", "")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for (suffix, key, text) in [
        ("", "9007199254740993", "first"),
        ("&after=9007199254740993", "9007199254740995", "second"),
    ] {
        let response = call(
            &app,
            "testserver",
            "GET",
            &(history.clone() + suffix),
            cookies,
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(body["records"][0]["key"], key);
        assert_eq!(body["next"], key);
        assert_eq!(
            body["records"][0]["original"],
            json!({"owner":snapshot,"text":text})
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn historical_relation_evidence_does_not_reserve_a_manual_relationship(pool: PgPool) {
    let user = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let sa = Uuid::new_v4();
    let sb = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'relations@example.test','synthetic')").bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')").bind(Uuid::new_v4()).bind(user).bind(format!("{:x}",Sha256::digest(b"relation-session"))).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','A'),($2,'person','B')").bind(a).bind(b).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat','evidence-a','person','A'),($2,'wechat','evidence-b','person','B')").bind(sa).bind(sb).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2),($3,$4)",
    )
    .bind(sa)
    .bind(a)
    .bind(sb)
    .bind(b)
    .execute(&pool)
    .await
    .unwrap();
    for kind in ["contact_share", "legacy_group", "legacy_interaction"] {
        sqlx::query("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type,evidence_kind) VALUES($1,$2,$3,'friend',$4)").bind(Uuid::new_v4()).bind(a).bind(b).bind(kind).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_relationship_edges(id,from_account_id,to_account_id,relationship_type,source_type,evidence_kind) VALUES($1,$2,$3,'friend','wechat',$4)").bind(Uuid::new_v4()).bind(sa).bind(sb).bind(kind).execute(&pool).await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.presented_relationship_edges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let root = tempfile::tempdir().unwrap();
    let app = auth::routes(
        AppState::new(
            pool.clone(),
            "synthetic-relation-secret-at-least32",
            "http://testserver".into(),
            root.path().into(),
        )
        .await
        .unwrap(),
    );
    for _ in 0..2 {
        let response=app.clone().oneshot(Request::post(format!("/api/v1/profiles/{a}/relationships")).header("host","testserver").header("content-type","application/json").header("cookie","crm_session=relation-session; crm_csrf=synthetic-csrf").header("x-csrf-token","synthetic-csrf").body(Body::from(json!({"to_profile_id":b,"relationship_type":"friend","note":"Confirmed manual relationship"}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.presented_relationship_edges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT note FROM crm.presented_relationship_edges")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "Confirmed manual relationship"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.relationship_edges WHERE evidence_kind<>'relationship'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        3
    );
}

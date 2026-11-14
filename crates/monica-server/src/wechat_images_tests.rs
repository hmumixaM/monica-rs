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

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    authenticated: bool,
    csrf: bool,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "example.test")
        .header("content-type", "application/json");
    if authenticated {
        builder = builder.header(
            "cookie",
            "crm_session=synthetic-images-session; crm_csrf=synthetic-csrf",
        );
    }
    if csrf {
        builder = builder.header("x-csrf-token", "synthetic-csrf");
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_controls_and_coverage_are_authenticated_paged_and_idempotent(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let app = auth::routes(
        AppState::new(
            pool.clone(),
            "synthetic-images-session-secret-32bytes",
            "https://example.test".into(),
            root.path().into(),
        )
        .await
        .unwrap(),
    );
    let path = "/api/v1/imports/wechat-images";
    assert_eq!(
        request(&app, "GET", path, false, false, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let actor = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'synthetic-images@example.test','synthetic')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 day')")
        .bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"synthetic-images-session"))).execute(&pool).await.unwrap();
    let (code, status) = request(&app, "GET", path, true, false, Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["control"]["enabled"], false);
    assert_eq!(status["control"]["paused"], true);
    assert_eq!(status["runs"], json!([]));
    assert_eq!(
        request(&app, "POST", path, true, false, json!({"action":"enable"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "POST", path, true, true, json!({"action":"enable"}))
            .await
            .0,
        StatusCode::OK
    );
    // A control change admits an external worker; it never creates executable jobs.
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.wechat_image_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let run = Uuid::new_v4();
    let group = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.wechat_image_runs(id,account,discovery_complete) VALUES($1,'synthetic',true)").bind(run).execute(&pool).await.unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    for (id, day) in [(a, "2026-01-01"), (b, "2026-01-02")] {
        sqlx::query("INSERT INTO crm.wechat_group_images(run_id,message_id,conversation_id,day) VALUES($1,$2,$3,$4::text::date) ON CONFLICT DO NOTHING").bind(run).bind(id).bind(group).bind(day).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO crm.wechat_group_images(run_id,message_id,conversation_id,day) VALUES($1,$2,$3,'2026-01-01') ON CONFLICT DO NOTHING").bind(run).bind(a).bind(group).execute(&pool).await.unwrap();
    for _ in 0..2 {
        sqlx::query("UPDATE crm.wechat_group_images SET state='available',quality='thumbnail' WHERE run_id=$1 AND message_id=$2").bind(run).bind(a).execute(&pool).await.unwrap();
    }
    let status = request(&app, "GET", path, true, false, Value::Null).await.1;
    assert_eq!(
        status["runs"][0]["counts"],
        json!({"total":2,"pending":1,"available":1,"quality_thumbnail":1})
    );
    let url = format!("{path}/daily?run_id={run}&limit=1");
    let (code, first) = request(&app, "GET", &url, true, false, Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(first["records"][0]["day"], "2026-01-02");
    assert_eq!(first["day_timezone"], "UTC");
    let next = format!(
        "{url}&after_day={}&after_group={}",
        first["next"]["after_day"].as_str().unwrap(),
        first["next"]["after_group"].as_str().unwrap()
    );
    let second = request(&app, "GET", &next, true, false, Value::Null)
        .await
        .1;
    assert_eq!(second["records"][0]["day"], "2026-01-01");
    assert_eq!(second["next"], Value::Null);
    let detail = format!("{path}/items?run_id={run}&conversation_id={group}&day=2026-01-01");
    let rows = request(&app, "GET", &detail, true, false, Value::Null)
        .await
        .1;
    assert_eq!(rows["records"].as_array().unwrap().len(), 1);
    assert_eq!(rows["records"][0]["state"], "available");
    assert_eq!(
        request(&app, "POST", path, true, true, json!({"action":"pause"}))
            .await
            .1["paused"],
        true
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.audit_events WHERE entity_type='wechat_image_control'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
}

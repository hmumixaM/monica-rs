use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn invalid() -> ApiError {
    ApiError(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Invalid message cursor, month or timezone",
    )
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/conversations", get(conversations))
        .route("/api/v1/conversations/{id}/messages", get(conversation))
        .route("/api/v1/profiles/{id}/messages", get(profile))
        .route("/api/v1/profiles/{id}/chat-months", get(months))
        .route("/api/v1/profiles/{id}/contact-events", get(events))
        .route("/api/v1/profiles/{id}/contact-events/summary", get(summary))
}
#[derive(Default, Deserialize)]
struct Params {
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
    before: Option<String>,
    before_id: Option<Uuid>,
    conversation_id: Option<Uuid>,
    conversation_type: Option<String>,
    month: Option<String>,
    #[serde(alias = "tz")]
    timezone: Option<String>,
    message: Option<Uuid>,
    kind: Option<String>,
    offset: Option<i64>,
}
async fn conversations(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Params>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut at = None;
    let mut id = None;
    if let Some(cursor) = q.cursor {
        let value: Value = serde_json::from_slice(&URL_SAFE.decode(cursor).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
        let array = value
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or_else(invalid)?;
        at = array[0].as_str().map(str::to_owned);
        id = Some(Uuid::parse_str(array[1].as_str().ok_or_else(invalid)?).map_err(|_| invalid())?);
        if let Some(at) = &at {
            chrono::DateTime::parse_from_rfc3339(at).map_err(|_| invalid())?;
        }
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(c)||jsonb_build_object('display_name',coalesce(nullif(p.display_name,''),c.source_display_name,a.display_name),'source',c.provider,'avatar_media_id',p.avatar_media_id) FROM crm.conversations c LEFT JOIN crm.profiles p ON p.id=c.profile_id LEFT JOIN crm.source_accounts a ON a.id=c.source_account_id WHERE ($1::text IS NULL OR p.display_name ILIKE '%'||$1||'%' OR c.external_id ILIKE '%'||$1||'%' OR a.display_name ILIKE '%'||$1||'%' OR c.source_display_name ILIKE '%'||$1||'%') AND ($2::uuid IS NULL OR CASE WHEN $3::timestamptz IS NULL THEN c.last_message_at IS NULL AND c.id<$2 ELSE c.last_message_at<$3::timestamptz OR c.last_message_at IS NULL OR (c.last_message_at=$3::timestamptz AND c.id<$2) END) ORDER BY c.last_message_at DESC NULLS LAST,c.id DESC LIMIT $4")
 .bind(q.q).bind(id).bind(at).bind(q.limit.unwrap_or(40).clamp(1,100)).fetch_all(&s.pool).await?;
    let rows =
        rows.into_iter()
            .map(|mut row| {
                row["cursor"] = json!(URL_SAFE.encode(
                    serde_json::to_vec(&json!([row["last_message_at"], row["id"]])).unwrap()
                ));
                row
            })
            .collect::<Vec<_>>();
    Ok(Json(json!(rows)))
}
async fn conversation(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(mut q): Query<Params>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    q.conversation_id = Some(id);
    read(&s, None, q).await
}
async fn profile(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Params>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    read(&s, Some(id), q).await
}
async fn read(s: &AppState, profile: Option<Uuid>, q: Params) -> Result<Json<Value>> {
    if let Some(before) = &q.before {
        chrono::DateTime::parse_from_rfc3339(before).map_err(|_| invalid())?;
    }
    if q.conversation_type
        .as_deref()
        .is_some_and(|x| !matches!(x, "direct" | "group"))
    {
        return Err(invalid());
    }
    let timezone = q.timezone.unwrap_or_else(|| "UTC".into());
    if q.month.is_some() {
        let valid: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)")
                .bind(&timezone)
                .fetch_one(&s.pool)
                .await?;
        if !valid {
            return Err(invalid());
        }
    }
    if let Some(month) = &q.month
        && (month.len() != 7
            || chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").is_err())
    {
        return Err(invalid());
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',m.message_id,'occurred_at',m.occurred_at,'message_type',m.message_type,'sender_profile_id',coalesce(m.sender_profile_id,l.profile_id),'sender_name',coalesce(p.display_name,a.display_name,nullif(m.sender,'')),'conversation_id',c.id,'conversation_type',c.conversation_type,'text',m.body) FROM crm.conversations c JOIN LATERAL(SELECT * FROM crm.message_search m WHERE m.conversation_id=c.id AND NOT m.tombstone AND ($3::timestamptz IS NULL OR (m.occurred_at,m.message_id)<($3::timestamptz,coalesce($4::uuid,'ffffffff-ffff-ffff-ffff-ffffffffffff'::uuid))) AND ($5::text IS NULL OR position(lower($5) in lower(m.body))>0) AND ($6::text IS NULL OR (m.occurred_at>=($6||'-01')::timestamp AT TIME ZONE $7 AND m.occurred_at<(($6||'-01')::timestamp+interval '1 month') AT TIME ZONE $7)) AND ($8::uuid IS NULL OR $3::text IS NOT NULL OR (m.occurred_at,m.message_id)<=(SELECT anchor.occurred_at,anchor.message_id FROM crm.message_search anchor WHERE anchor.message_id=$8 AND anchor.conversation_id=c.id)) ORDER BY m.occurred_at DESC,m.message_id DESC LIMIT $10) m ON true LEFT JOIN crm.source_account_links l ON l.account_id=m.sender_account_id LEFT JOIN crm.profiles p ON p.id=coalesce(m.sender_profile_id,l.profile_id) LEFT JOIN crm.source_accounts a ON a.id=m.sender_account_id WHERE ($1::uuid IS NULL OR c.profile_id=$1) AND ($2::uuid IS NULL OR c.id=$2) AND ($9::text IS NULL OR c.conversation_type=$9) ORDER BY m.occurred_at DESC,m.message_id DESC LIMIT $10")
 .bind(profile).bind(q.conversation_id).bind(q.before).bind(q.before_id).bind(q.q).bind(q.month).bind(timezone).bind(q.message).bind(q.conversation_type).bind(q.limit.unwrap_or(50).clamp(1,100)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn months(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Params>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('month',m.month,'sent',sum(sent),'received',sum(received),'total',sum(total),'first',min(first_message_at),'last',max(last_message_at),'snapshot_at',min(snapshot_at),'timezone',min(timezone)) FROM crm.source_chat_months m JOIN crm.source_account_links l ON l.account_id=m.account_id WHERE l.profile_id=$1 GROUP BY m.month ORDER BY m.month").bind(id).fetch_all(&s.pool).await?;
    if rows.is_empty() {
        let timezone = q.timezone.as_deref().unwrap_or("UTC");
        if ["UTC", "America/Chicago"].contains(&timezone) {
            let table = if timezone == "UTC" {
                "message_months_utc"
            } else {
                "message_months"
            };
            rows=sqlx::query_scalar(&format!("SELECT jsonb_build_object('month',m.month,'sent',sum(m.sent),'received',sum(m.received),'total',sum(m.sent+m.received),'first',min(m.first_message_at),'last',max(m.last_message_at),'snapshot_at',NULL,'timezone',$2::text) FROM crm.{table} m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.profile_id=$1 GROUP BY m.month ORDER BY m.month")).bind(id).bind(timezone).fetch_all(&s.pool).await?;
        } else {
            let valid: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)")
                    .bind(timezone)
                    .fetch_one(&s.pool)
                    .await?;
            if !valid {
                return Err(invalid());
            }
            rows=sqlx::query_scalar("SELECT jsonb_build_object('month',to_char(m.occurred_at AT TIME ZONE $2,'YYYY-MM'),'sent',count(*) FILTER(WHERE m.sender=m.account),'received',count(*) FILTER(WHERE m.sender<>m.account),'total',count(*),'first',min(m.occurred_at),'last',max(m.occurred_at),'snapshot_at',NULL,'timezone',$2::text) FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.profile_id=$1 AND NOT m.tombstone GROUP BY to_char(m.occurred_at AT TIME ZONE $2,'YYYY-MM') ORDER BY to_char(m.occurred_at AT TIME ZONE $2,'YYYY-MM')").bind(id).bind(timezone).fetch_all(&s.pool).await?;
        }
    }
    let sum = |key: &str| {
        rows.iter()
            .map(|r| r[key].as_i64().unwrap_or(0))
            .sum::<i64>()
    };
    let first = rows.iter().filter_map(|r| r["first"].as_str()).min();
    let last = rows.iter().filter_map(|r| r["last"].as_str()).max();
    let snapshot = rows.iter().filter_map(|r| r["snapshot_at"].as_str()).min();
    let peak = rows.iter().reduce(|a, b| {
        if b["total"].as_i64() > a["total"].as_i64() {
            b
        } else {
            a
        }
    });
    Ok(Json(
        json!({"months":rows,"total":sum("total"),"sent":sum("sent"),"received":sum("received"),"first":first,"last":last,"snapshot_at":snapshot,"peak":peak,"timezone":rows.first().and_then(|r|r["timezone"].as_str()).unwrap_or("UTC"),"active_months":rows.iter().filter(|r|r["total"].as_i64().unwrap_or(0)>0).count()}),
    ))
}

async fn events(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Params>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let kind = q.kind.unwrap_or_else(|| "call".into());
    if !matches!(kind.as_str(), "call" | "money") {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Unknown record kind",
        ));
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(e)||jsonb_build_object('account_id',source_account_id,'event_kind',kind) FROM crm.contact_events e WHERE profile_id=$1 AND kind=$2 ORDER BY occurred_at DESC NULLS LAST,id LIMIT $3 OFFSET $4").bind(id).bind(kind).bind(q.limit.unwrap_or(30).clamp(1,100)).bind(q.offset.unwrap_or(0).max(0)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn summary(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    let mut result = crate::profiles::summary::contact_events(&mut tx, id).await?;
    tx.commit().await?;
    if result.get("calls").is_none() {
        result["calls"] = json!({"total":0,"voice":0,"video":0,"completed":0,"incoming":0,"outgoing":0,"duration_seconds":0,"known_duration":0,"inferred_type":0});
    }
    if result.get("money").is_none() {
        result["money"] = json!({"total":0,"transfer":0,"red_packet":0,"unknown_amount":0,"unknown_direction":0,"currencies":{}});
    }
    result["snapshot_at"] = Value::Null;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use sha2::{Digest, Sha256};
    use sqlx::PgPool;
    use tower::ServiceExt;
    async fn request(router: Router, path: &str) -> Value {
        let response = router
            .oneshot(
                Request::get(path)
                    .header("host", "localhost:17494")
                    .header("cookie", "crm_session=fixture-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1000000).await.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{path}: {}",
            String::from_utf8_lossy(&body)
        );
        serde_json::from_slice(&body).unwrap()
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn real_http_message_pagination_linkage_months_and_timeline(pool: PgPool) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            pool.clone(),
            "synthetic-session-secret-32-characters",
            "http://localhost:17494".into(),
            dir.path().into(),
        )
        .await
        .unwrap();
        let router = crate::auth::routes(state);
        let actor = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'fixture@example.test','not-used')").bind(actor).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')").bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"fixture-session"))).execute(&pool).await.unwrap();
        let p = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Fixture')",
        )
        .bind(p)
        .execute(&pool)
        .await
        .unwrap();
        let records = (0..4)
            .map(|i| monica_core::messages::Message {
                account: "self".into(),
                conversation: "friend".into(),
                external_id: format!("message-{i}"),
                sender: "friend".into(),
                sent_at_ms: 1767268800000,
                body: format!("Synthetic {i}"),
            })
            .collect::<Vec<_>>();
        monica_core::messages::append(&pool, monica_core::Provider::Wechat, &records)
            .await
            .unwrap();
        let a = monica_core::identity::account_id(monica_core::Provider::Wechat, "friend").unwrap();
        monica_core::linking::set_link(
            &pool,
            monica_core::linking::LinkChange {
                account_id: a,
                profile_id: Some(p),
                expected_profile_id: None,
            },
            Some(actor),
        )
        .await
        .unwrap();
        let sidebar = request(router.clone(), "/api/v1/conversations").await;
        assert_eq!(sidebar[0]["profile_id"], json!(p));
        assert_eq!(sidebar[0]["message_count"], 4);
        let first = request(
            router.clone(),
            &format!("/api/v1/profiles/{p}/messages?limit=2"),
        )
        .await;
        assert_eq!(first.as_array().unwrap().len(), 2);
        let stamp = first[1]["occurred_at"]
            .as_str()
            .unwrap()
            .replace('+', "%2B");
        let id = first[1]["id"].as_str().unwrap();
        let second = request(
            router.clone(),
            &format!("/api/v1/profiles/{p}/messages?limit=2&before={stamp}&before_id={id}"),
        )
        .await;
        assert_eq!(second.as_array().unwrap().len(), 2);
        assert!(
            second
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["id"] != first[0]["id"] && r["id"] != first[1]["id"])
        );
        let months = request(router.clone(), &format!("/api/v1/profiles/{p}/chat-months")).await;
        assert_eq!(months["total"], 4);
        assert_eq!(months["received"], 4);
        for i in 0..2 {
            monica_core::ingest(&pool,&monica_core::Record{provider:monica_core::Provider::Wechat,account:"friend".into(),object:format!("post-{i}"),kind:"post".into(),event:format!("event-{i}"),payload:json!({"text":format!("Post {i}"),"occurred_at":"2026-01-01T12:00:00Z"})}).await.unwrap();
        }
        while let Some(lease) = monica_core::claim(&pool).await.unwrap() {
            monica_core::project(&pool, &lease).await.unwrap();
        }
        let timeline = request(
            router.clone(),
            &format!("/api/v1/timeline?profile_id={p}&limit=1"),
        )
        .await;
        assert_eq!(timeline.as_array().unwrap().len(), 1);
        let cursor = timeline[0]["cursor"].as_str().unwrap();
        let next = request(
            router.clone(),
            &format!("/api/v1/timeline?profile_id={p}&limit=1&cursor={cursor}"),
        )
        .await;
        assert_ne!(next[0]["id"], timeline[0]["id"]);
        let summary = request(
            router.clone(),
            &format!("/api/v1/timeline/summary?profile_id={p}"),
        )
        .await;
        assert_eq!(summary["total_events"], 2);
        let detail = request(
            router.clone(),
            &format!("/api/v1/timeline/{}", timeline[0]["id"].as_str().unwrap()),
        )
        .await;
        assert_eq!(detail["event_type"], "post");
        assert_eq!(detail["revisions"].as_array().unwrap().len(), 1);
        let metrics = request(router, &format!("/api/v1/metrics/social?profile_id={p}")).await;
        assert_eq!(metrics["providers"]["wechat"]["posts"], 2);
    }
}

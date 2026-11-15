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
    since: Option<String>,
    until: Option<String>,
    before_id: Option<Uuid>,
    conversation_id: Option<Uuid>,
    conversation_type: Option<String>,
    include_group_speech: Option<bool>,
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
    for stamp in [&q.before, &q.since, &q.until].into_iter().flatten() {
        chrono::DateTime::parse_from_rfc3339(stamp).map_err(|_| invalid())?;
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
    // Restrict each candidate stream before its limit. A person's group search
    // includes only that person's speech, even when a room has newer messages
    // from other members. Candidate IDs stay small; bodies/media join afterward.
    // Pick copies within the matching person/time/search scope, independently
    // of the paging cursor, so losing copies cannot return on a later page.
    let matching = |alias: &str| {
        format!(
            r#"NOT {alias}.tombstone
      AND ($12::timestamptz IS NULL OR {alias}.occurred_at>=$12::timestamptz)
      AND ($13::timestamptz IS NULL OR {alias}.occurred_at<$13::timestamptz)
      AND ($5::text IS NULL OR position(lower($5) in lower({alias}.body))>0)
      AND ($6::text IS NULL OR ({alias}.occurred_at>=($6||'-01')::timestamp AT TIME ZONE $7 AND {alias}.occurred_at<(($6||'-01')::timestamp+interval '1 month') AT TIME ZONE $7))"#
        )
    };
    let filters = format!(
        r#"{current}
      AND ($3::timestamptz IS NULL OR (m.occurred_at,m.message_id)<($3::timestamptz,coalesce($4::uuid,'ffffffff-ffff-ffff-ffff-ffffffffffff'::uuid)))
      AND ($8::uuid IS NULL OR $3::text IS NOT NULL OR (m.occurred_at,m.message_id)<=(SELECT anchor.occurred_at,anchor.message_id FROM crm.message_search anchor WHERE anchor.message_id=$8 AND anchor.conversation_id=c.id))
      AND (NOT $14 OR m.provider<>'mail' OR c.conversation_type<>'direct' OR NOT EXISTS(
        SELECT 1 FROM crm.connection_evidence original
        JOIN crm.connection_evidence duplicate ON duplicate.dedup_key=original.dedup_key
        JOIN crm.message_search other ON other.message_id=duplicate.message_id
        JOIN crm.conversations other_room ON other_room.id=other.conversation_id
        WHERE original.message_id=m.message_id AND original.connection_id=c.connection_id
          AND original.dedup_key IS NOT NULL AND duplicate.included AND NOT duplicate.deleted
          AND duplicate.kind='message' AND duplicate.conversation_id=other_room.id AND duplicate.connection_id=other_room.connection_id
          AND other_room.profile_id=$1 AND other_room.conversation_type='direct' AND other.provider='mail'
          AND (other.occurred_at,other.message_id)>(m.occurred_at,m.message_id) AND {other}
      ))"#,
        current = matching("m"),
        other = matching("other")
    );
    let room = r#"($2::uuid IS NULL OR c.id=$2) AND ($9::text IS NULL OR c.conversation_type=$9)"#;
    let query = format!(
        r#"WITH owned_accounts AS MATERIALIZED (
      SELECT account_id FROM crm.source_account_links WHERE profile_id=$1
    ), candidates AS (
      (SELECT m.message_id,m.occurred_at FROM crm.conversations c
       JOIN LATERAL(SELECT m.message_id,m.occurred_at FROM crm.message_search m
        WHERE m.conversation_id=c.id AND {filters}
        ORDER BY m.occurred_at DESC,m.message_id DESC LIMIT $10) m ON true
       WHERE ($1::uuid IS NULL OR c.profile_id=$1) AND {room}
       ORDER BY m.occurred_at DESC,m.message_id DESC LIMIT $10)
      UNION ALL
      (SELECT m.message_id,m.occurred_at FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id
       WHERE $11 AND $1::uuid IS NOT NULL AND m.sender_profile_id=$1
        AND c.conversation_type='group' AND c.profile_id IS DISTINCT FROM $1 AND {room} AND {filters}
       ORDER BY m.occurred_at DESC,m.message_id DESC LIMIT $10)
      UNION ALL
      (SELECT m.message_id,m.occurred_at FROM crm.message_search m JOIN owned_accounts own ON own.account_id=m.sender_account_id
       JOIN crm.conversations c ON c.id=m.conversation_id
       WHERE $11 AND $1::uuid IS NOT NULL AND m.sender_profile_id IS NULL
        AND c.conversation_type='group' AND c.profile_id IS DISTINCT FROM $1 AND {room} AND {filters}
       ORDER BY m.occurred_at DESC,m.message_id DESC LIMIT $10)
    ), page AS MATERIALIZED (
      SELECT * FROM candidates ORDER BY occurred_at DESC,message_id DESC LIMIT $10
    ) SELECT jsonb_build_object('id',m.message_id,'occurred_at',m.occurred_at,'message_type',m.message_type,
      'sender_profile_id',coalesce(m.sender_profile_id,l.profile_id),'sender_name',coalesce(p.display_name,a.display_name,nullif(m.sender,'')),
      'conversation_id',c.id,'conversation_type',c.conversation_type,'text',m.body,'direction',m.direction,
      'evidence_id',e.id,'evidence_revision',e.revision,
      'citation',CASE WHEN e.id IS NOT NULL THEN '/evidence/'||e.id::text||'?revision='||e.revision::text END)
      FROM page JOIN crm.message_search m ON m.message_id=page.message_id JOIN crm.conversations c ON c.id=m.conversation_id
      LEFT JOIN crm.source_account_links l ON l.account_id=m.sender_account_id
      LEFT JOIN crm.profiles p ON p.id=coalesce(m.sender_profile_id,l.profile_id)
      LEFT JOIN crm.source_accounts a ON a.id=m.sender_account_id
      LEFT JOIN crm.connection_evidence e ON e.id=m.message_id AND e.message_id=m.message_id
      ORDER BY page.occurred_at DESC,page.message_id DESC"#
    );
    let mut rows: Vec<Value> = sqlx::query_scalar(&query)
        .bind(profile)
        .bind(q.conversation_id)
        .bind(q.before)
        .bind(q.before_id)
        .bind(q.q)
        .bind(q.month)
        .bind(timezone)
        .bind(q.message)
        .bind(q.conversation_type)
        .bind(q.limit.unwrap_or(50).clamp(1, 100))
        .bind(q.include_group_speech.unwrap_or(false))
        .bind(q.since)
        .bind(q.until)
        // An explicit conversation or anchor keeps that account's originals.
        .bind(profile.is_some() && q.conversation_id.is_none() && q.message.is_none())
        .fetch_all(&s.pool)
        .await?;
    // Resolve attachments only after the global message page is bounded.
    let ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| r["id"].as_str()?.parse().ok())
        .collect();
    let media: Vec<(Uuid,Value)> = sqlx::query_as("SELECT l.entity_id,jsonb_agg(jsonb_build_object('id',a.id,'media_type',a.media_type,'file_name',a.file_name,'byte_length',a.byte_length) ORDER BY l.role,a.id) FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id WHERE l.entity_type='message_event' AND l.entity_id=ANY($1) GROUP BY l.entity_id")
        .bind(&ids).fetch_all(&s.pool).await?;
    let media: std::collections::HashMap<_, _> = media.into_iter().collect();
    let originals: Vec<(Uuid,i64)> = sqlx::query_as(
        "SELECT e.message_id,coalesce(p.revision,1) FROM crm.legacy_message_evidence e LEFT JOIN crm.legacy_chatlog_projection p ON p.entity_type='message' AND p.entity_id=e.message_id WHERE e.message_id=ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&s.pool)
    .await?;
    let originals: std::collections::HashMap<_, _> = originals.into_iter().collect();
    for row in &mut rows {
        let id: Uuid = row["id"].as_str().unwrap().parse().unwrap();
        row["media"] = media.get(&id).cloned().unwrap_or(json!([]));
        if row["evidence_id"].is_null()
            && let Some(revision) = originals.get(&id)
        {
            row["citation"] = json!(format!("/evidence/{id}?revision={revision}"));
        }
    }
    Ok(Json(json!(rows)))
}
pub(super) struct LocalSpeechPage {
    pub profile_id: Uuid,
    pub conversation_id: Uuid,
    pub query: Option<String>,
    pub before: Option<String>,
    pub before_id: Option<Uuid>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: i64,
}
/// The group dialog shares the same indexed author scope, pagination, media and
/// immutable citation service as person messages and Agent/MCP reads.
pub(super) async fn local_group_speech(s: &AppState, page: LocalSpeechPage) -> Result<Json<Value>> {
    read(
        s,
        Some(page.profile_id),
        Params {
            conversation_id: Some(page.conversation_id),
            conversation_type: Some("group".into()),
            include_group_speech: Some(true),
            q: page.query,
            before: page.before,
            before_id: page.before_id,
            since: page.since,
            until: page.until,
            limit: Some(page.limit),
            ..Default::default()
        },
    )
    .await
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
    async fn mail_copies_fold_per_person_before_pages_and_week_inputs(pool: PgPool) {
        use monica_core::{Provider, connections, mail};
        use std::collections::BTreeSet;
        async fn deliver(pool: &PgPool, cid: Uuid, rows: &[Value], cursor: i64) {
            let mut tx = pool.begin().await.unwrap();
            connections::commit_page(
                &mut tx,
                Provider::Mail,
                cid,
                "messages",
                rows,
                &json!({"position":cursor}),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            connections::deliver(pool, Provider::Mail, cid, 100)
                .await
                .unwrap();
        }
        fn mail_record(cid: Uuid, n: usize, first_account: bool) -> Value {
            let owner = format!("owner-{cid}@example.invalid");
            let mut headers = json!({"from":"friend@example.invalid","to":owner,"subject":"Synthetic personal message"});
            if n < 7 || n == 9 {
                headers["message-id"] = json!(format!("<logical-{n}>"));
            }
            let body = if n == 9 {
                format!("Different saved body {cid}")
            } else {
                format!("Shared logical message {n}")
            };
            let at = if n == 0 && first_account {
                "2026-11-09T00:30:00Z"
            } else {
                "2026-11-10T12:00:00Z"
            };
            mail::normalize(json!({"external_id":format!("copy-{n}"),"thread_id":"shared-thread","headers":headers,"body":body,"occurred_at":at}),&BTreeSet::from([owner]),&mail::Decision{category:"personal".into(),reason:"synthetic".into()}).unwrap()
        }
        async fn week_count(pool: &PgPool, person: Option<Uuid>, chats: bool) -> i64 {
            sqlx::query_scalar("SELECT count(*) FROM crm.ai_week_sources('2026-11-09',$1,$2,false) WHERE payload->>'kind'='direct'")
                .bind(person).bind(chats).fetch_one(pool).await.unwrap()
        }
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
        let person = Uuid::new_v4();
        let other = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'mail-copies@example.test','unused')").bind(actor).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')").bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"fixture-session"))).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Mail person'),($2,'person','Other mail person')").bind(person).bind(other).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.person_identifiers(profile_id,kind,value,confirmed) VALUES($1,'email','friend@example.invalid',true)").bind(person).execute(&pool).await.unwrap();
        // The foreign person's copy sorts above both target copies, so a global
        // dedup lookup that forgets current person ownership will lose mail.
        let mut accounts = [
            Uuid::from_u128(101),
            Uuid::from_u128(102),
            Uuid::from_u128(103),
        ];
        accounts.sort_by_key(|cid| connections::evidence_id(*cid, "copy-1"));
        let [first, second, foreign] = accounts;
        for cid in accounts {
            sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,'mail',$2,'Synthetic mailbox','imap',true)").bind(cid).bind(format!("owner-{cid}@example.invalid")).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO crm.connection_identity_links(connection_id,external_id,profile_id,confirmed) VALUES($1,'friend@example.invalid',$2,true)").bind(foreign).bind(other).execute(&pool).await.unwrap();
        for cid in [first, second] {
            let records: Vec<_> = (0..10).map(|n| mail_record(cid, n, cid == first)).collect();
            deliver(&pool, cid, &records, 1).await;
        }
        deliver(&pool, foreign, &[mail_record(foreign, 1, false)], 1).await;
        let mut ids = Vec::new();
        let mut cursor = String::new();
        loop {
            let page = request(
                router.clone(),
                &format!("/api/v1/profiles/{person}/messages?limit=2{cursor}"),
            )
            .await;
            let rows = page.as_array().unwrap();
            if rows.is_empty() {
                break;
            }
            assert_eq!(rows.len(), if ids.len() < 12 { 2 } else { 1 });
            let last = rows.last().unwrap();
            cursor = format!(
                "&before={}&before_id={}",
                last["occurred_at"].as_str().unwrap().replace('+', "%2B"),
                last["id"].as_str().unwrap()
            );
            ids.extend(
                rows.iter()
                    .map(|r| Uuid::parse_str(r["id"].as_str().unwrap()).unwrap()),
            );
            assert!(ids.len() <= 13, "copies resurfaced after the paging cursor");
        }
        assert_eq!(ids.len(), 13);
        assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), 13);
        let distinct:(i64,i64)=sqlx::query_as("SELECT count(DISTINCT dedup_key),count(*) FILTER(WHERE dedup_key IS NULL) FROM crm.connection_evidence WHERE id=ANY($1)").bind(&ids).fetch_one(&pool).await.unwrap();
        assert_eq!(
            distinct,
            (9, 4),
            "missing stable Message-ID or distinct bodies must stay separate"
        );
        let conversation: Uuid =
            sqlx::query_scalar("SELECT id FROM crm.conversations WHERE connection_id=$1")
                .bind(first)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            request(
                router.clone(),
                &format!("/api/v1/conversations/{conversation}/messages?limit=100")
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            10
        );
        assert_eq!(
            request(
                router.clone(),
                &format!(
                    "/api/v1/profiles/{person}/messages?conversation_id={conversation}&limit=100"
                )
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            10
        );
        let raw_id = connections::evidence_id(first, "copy-1");
        assert!(
            request(
                router.clone(),
                &format!("/api/v1/profiles/{person}/messages?message={raw_id}&limit=100")
            )
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == raw_id.to_string())
        );
        let bounded=request(router.clone(),&format!("/api/v1/profiles/{person}/messages?since=2026-11-09T00:00:00Z&until=2026-11-09T01:00:00Z")).await;
        assert_eq!(bounded.as_array().unwrap().len(), 1);
        assert_eq!(
            bounded[0]["id"],
            connections::evidence_id(first, "copy-0").to_string()
        );
        assert_eq!(
            request(
                router.clone(),
                &format!("/api/v1/profiles/{person}/messages?q=Shared%20logical%20message%201")
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            1
        );
        assert_eq!(week_count(&pool, Some(person), true).await, 13);
        assert_eq!(week_count(&pool, Some(other), true).await, 1);
        assert_eq!(week_count(&pool, None, true).await, 14);
        assert_eq!(week_count(&pool, None, false).await, 0);
        let policy: String = sqlx::query_scalar("SELECT crm.ai_week_source_policy()")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE crm.source_connections SET enabled=false,generation=generation+1 WHERE id=$1",
        )
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(week_count(&pool, Some(person), true).await, 9);
        assert_ne!(
            policy,
            sqlx::query_scalar::<_, String>("SELECT crm.ai_week_source_policy()")
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        assert_eq!(
            request(
                router.clone(),
                &format!("/api/v1/profiles/{person}/messages?limit=100")
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            13,
            "paused sources remain readable in the UI"
        );
        sqlx::query(
            "UPDATE crm.source_connections SET enabled=true,generation=generation+1 WHERE id=$1",
        )
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
        let mut excluded = mail_record(second, 1, false);
        excluded["included"] = json!(false);
        deliver(&pool, second, &[excluded], 2).await;
        let mut deleted = mail_record(first, 2, true);
        deleted["deleted"] = json!(true);
        deliver(&pool, first, &[deleted], 2).await;
        assert_eq!(
            request(
                router.clone(),
                &format!("/api/v1/profiles/{person}/messages?limit=100")
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            13
        );
        let old_id = connections::evidence_id(second, "copy-3");
        let mut edited = mail_record(second, 3, false);
        edited["body"] = json!("Corrected content with its own logical identity");
        let edited = mail::normalize(
            edited,
            &BTreeSet::from([format!("owner-{second}@example.invalid")]),
            &mail::Decision {
                category: "personal".into(),
                reason: "synthetic edit".into(),
            },
        )
        .unwrap();
        deliver(&pool, second, &[edited], 3).await;
        let latest = request(
            router.clone(),
            &format!("/api/v1/profiles/{person}/messages?limit=100"),
        )
        .await;
        assert_eq!(latest.as_array().unwrap().len(), 14);
        let changed = latest
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == old_id.to_string())
            .unwrap();
        assert_eq!(changed["evidence_revision"], 2);
        assert_eq!(
            request(router, &format!("/api/v1/evidence/{old_id}?revision=1")).await["body"],
            "Shared logical message 3"
        );
        let originals: (i64, i64) =
            sqlx::query_as("SELECT count(*),sum(revision)::bigint FROM crm.connection_evidence")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(originals, (21, 24));
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
    #[sqlx::test(migrations = "../../migrations")]
    async fn person_group_speech_and_native_versioned_citations(pool: PgPool) {
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
        let person = Uuid::new_v4();
        let other = Uuid::new_v4();
        let group = Uuid::new_v4();
        let account = Uuid::new_v4();
        let conversation = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'speech@example.test','not-used')").bind(actor).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')").bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"fixture-session"))).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Speech owner'),($2,'person','Other author'),($3,'group','Synthetic room')")
            .bind(person).bind(other).bind(group).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat','speech-owner','person','Speech owner')").bind(account).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)")
            .bind(account)
            .bind(person)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type,profile_id) VALUES($1,'wechat','self','room@chatroom','group',$2)").bind(conversation).bind(group).execute(&pool).await.unwrap();
        // More than a page of newer messages must not hide the wanted author's
        // older speech. Explicit native author wins over a linked legacy account.
        sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_profile_id,occurred_at,body) SELECT gen_random_uuid(),$1,'wechat','self',n::text,'other',$2,'2026-01-02T12:00:00Z','Other group member' FROM generate_series(1,205) n")
            .bind(conversation).bind(other).execute(&pool).await.unwrap();
        let explicit = Uuid::new_v4();
        let legacy = Uuid::new_v4();
        let conflict = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_profile_id,sender_account_id,occurred_at,body) VALUES($1,$4,'wechat','self','explicit','native',$5,NULL,'2026-01-01T12:00:00Z','My native speech'),($2,$4,'wechat','self','legacy','legacy',NULL,$6,'2026-01-01T12:00:00Z','My legacy needle'),($3,$4,'wechat','self','conflict','other',$7,$6,'2026-01-01T12:00:00Z','Other explicit owner')")
            .bind(explicit).bind(legacy).bind(conflict).bind(conversation).bind(person).bind(account).bind(other).execute(&pool).await.unwrap();
        assert!(
            request(
                router.clone(),
                &format!("/api/v1/profiles/{person}/messages")
            )
            .await
            .as_array()
            .unwrap()
            .is_empty()
        );
        let mut seen = Vec::new();
        let mut cursor = String::new();
        for _ in 0..2 {
            let rows = request(
                router.clone(),
                &format!(
                    "/api/v1/profiles/{person}/messages?include_group_speech=true&limit=1{cursor}"
                ),
            )
            .await;
            let row = &rows[0];
            seen.push(row["id"].as_str().unwrap().parse::<Uuid>().unwrap());
            assert_eq!(row["sender_profile_id"], person.to_string());
            cursor = format!(
                "&before={}&before_id={}",
                row["occurred_at"].as_str().unwrap().replace('+', "%2B"),
                row["id"].as_str().unwrap()
            );
        }
        seen.sort();
        let mut expected = vec![explicit, legacy];
        expected.sort();
        assert_eq!(seen, expected);
        let filtered=request(router.clone(),&format!("/api/v1/profiles/{person}/messages?include_group_speech=true&q=needle&month=2026-01&timezone=UTC&conversation_type=group")).await;
        assert_eq!(filtered.as_array().unwrap().len(), 1);
        assert_eq!(filtered[0]["id"], legacy.to_string());
        let empty=request(router.clone(),&format!("/api/v1/profiles/{person}/messages?include_group_speech=true&month=2025-12&timezone=UTC")).await;
        assert!(empty.as_array().unwrap().is_empty());
        let scoped=request(router.clone(),&format!("/api/v1/profiles/{person}/messages?include_group_speech=true&conversation_id={conversation}")).await;
        assert_eq!(scoped.as_array().unwrap().len(), 2);
        // The person dialog must work from imported records without a Chatlog
        // URL or a linked WeChat account. Cursor pages preserve equal-time IDs.
        let local = request(
            router.clone(),
            &format!("/api/v1/profiles/{person}/group-speech?limit=1"),
        )
        .await;
        assert_eq!(local["storage"], "local");
        assert_eq!(local["scope"], "imported_records");
        assert_eq!(
            local["groups"][0]["conversation_id"],
            conversation.to_string()
        );
        let next = &local["groups"][0]["next"];
        let later=request(router.clone(),&format!("/api/v1/profiles/{person}/group-speech?conversation_id={conversation}&limit=1&before={}&before_id={}",next["before"].as_str().unwrap().replace('+',"%2B"),next["before_id"].as_str().unwrap())).await;
        assert_ne!(
            local["groups"][0]["messages"][0]["id"],
            later["groups"][0]["messages"][0]["id"]
        );
        assert!(later["groups"][0]["next"].is_null());
        let day = request(
            router.clone(),
            &format!(
                "/api/v1/profiles/{person}/group-speech?q=needle&since=2026-01-01&until=2026-01-01"
            ),
        )
        .await;
        assert_eq!(day["groups"][0]["messages"].as_array().unwrap().len(), 1);
        assert_eq!(day["groups"][0]["messages"][0]["id"], legacy.to_string());
        assert!(
            request(
                router.clone(),
                &format!(
                    "/api/v1/profiles/{person}/group-speech?since=2026-01-02&until=2026-01-02"
                )
            )
            .await["groups"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let unlinked_room = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type) VALUES($1,'whatsapp','offline','unlinked-native-room','group')").bind(unlinked_room).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_profile_id,occurred_at,body) SELECT gen_random_uuid(),$1,'whatsapp','offline',n::text,'other',$2,'2026-01-01T12:00:00Z','Identical text' FROM generate_series(1,2) n").bind(unlinked_room).bind(other).execute(&pool).await.unwrap();
        let native_room = request(
            router.clone(),
            &format!(
                "/api/v1/profiles/{other}/group-speech?conversation_id={unlinked_room}&limit=1"
            ),
        )
        .await;
        assert!(native_room["groups"][0]["group_id"].is_null());
        assert_eq!(native_room["groups"][0]["provider"], "whatsapp");
        assert!(native_room["groups"][0]["next"].is_object());
        let whole_room = request(
            router.clone(),
            &format!("/api/v1/conversations/{conversation}/messages?limit=1"),
        )
        .await;
        assert_eq!(whole_room[0]["text"], "Other group member");
        sqlx::query("UPDATE crm.message_search SET tombstone=true WHERE message_id=$1")
            .bind(legacy)
            .execute(&pool)
            .await
            .unwrap();
        let remaining = request(
            router.clone(),
            &format!("/api/v1/profiles/{person}/messages?include_group_speech=true"),
        )
        .await;
        assert_eq!(remaining.as_array().unwrap().len(), 1);

        use monica_core::{Provider, connections};
        let cid = connections::connection_id(Provider::AppleMessages, "versioned-fixture");
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,'apple_messages','versioned-fixture','Synthetic','local',true)").bind(cid).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.person_identifiers(profile_id,kind,value,confirmed) VALUES($1,'email','speech@example.test',true)").bind(person).execute(&pool).await.unwrap();
        let mut original = String::new();
        for (revision, body) in [(1, "First native body"), (2, "Revised native body")] {
            let value = json!({"external_id":"versioned-message","kind":"message","thread_id":"native-friend","occurred_at":"2026-01-03T12:00:00Z","body":body,"participants":[{"id":"sender","role":"sender","emails":["speech@example.test"]},{"id":"self","role":"recipient","is_self":true}]});
            let mut tx = pool.begin().await.unwrap();
            connections::commit_page(
                &mut tx,
                Provider::AppleMessages,
                cid,
                "messages",
                &[value],
                &json!({"revision":revision}),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            connections::deliver(&pool, Provider::AppleMessages, cid, 100)
                .await
                .unwrap();
            let rows = request(
                router.clone(),
                &format!("/api/v1/profiles/{person}/messages?include_group_speech=true&limit=1"),
            )
            .await;
            let eid = connections::evidence_id(cid, "versioned-message");
            assert_eq!(rows[0]["text"], body);
            assert_eq!(rows[0]["evidence_id"], eid.to_string());
            assert_eq!(rows[0]["evidence_revision"], revision);
            assert_eq!(
                rows[0]["citation"],
                format!("/evidence/{eid}?revision={revision}")
            );
            if revision == 1 {
                original = format!("/api/v1/evidence/{eid}?revision=1");
            }
        }
        assert_eq!(
            request(router, &original).await["body"],
            "First native body"
        );
    }
}

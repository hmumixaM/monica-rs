//! Authentication and cheap committed dependency tokens precede the singleflight.
//! The original handler (including its SQL and JSON construction) runs only on miss.
use super::*;
use crate::auth::user;
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::IntoResponse,
};
use std::sync::{Arc, Mutex};

/// Pages that show values relative to "now" are recomputed at this cadence
/// when nothing they depend on has changed.
const CLOCK_BUCKET_SECONDS: i32 = 300;

fn dependencies(path: &str) -> (&'static [&'static str], bool) {
    match path {
        // Graph weights use recorded facts, not a moving time window.
        "/api/v1/relationships/graph" => (
            &[
                "profile",
                "identity",
                "posts",
                "interactions",
                "community",
                "graph",
            ],
            false,
        ),
        "/api/v1/directory" => (
            &["profile", "organization", "tag", "messages", "media"],
            false,
        ),
        "/api/v1/directory/posts" => (&["posts"], false),
        "/api/v1/metrics/locations" => (&["profile", "posts"], false),
        "/api/v1/metrics/people-overview" => (&["profile", "organization"], false),
        "/api/v1/metrics/snapshots" => (&["metric-snapshot"], false),
        "/api/v1/metrics/summary" => (
            &[
                "profile",
                "identity",
                "organization",
                "posts",
                "metrics",
                "activity",
                "media-assets",
            ],
            false,
        ),
        "/api/v1/search" => (
            &["profile", "posts", "activity", "tag", "organization"],
            false,
        ),
        "/api/v1/home" => (&["profile", "calendar", "weekly-statistics"], true),
        "/api/v1/home/dates" => (&["profile", "calendar", "media"], true),
        "/api/v1/dates/birthdays" => (&["profile", "calendar"], true),
        "/api/v1/insights/orbit" => (
            &[
                "profile",
                "organization",
                "tag",
                "weekly-statistics",
                "contacts",
                "interactions",
                "posts",
                "community",
                "media",
            ],
            true,
        ),
        "/api/v1/insights" => (
            &[
                "profile",
                "calendar",
                "organization",
                "messages",
                "weekly-statistics",
                "contacts",
                "posts",
                "media",
            ],
            true,
        ),
        "/api/v1/insights/calendar" | "/api/v1/insights/calendar/day" => (
            &[
                "profile",
                "calendar",
                "contacts",
                "weekly-statistics",
                "media",
            ],
            true,
        ),
        _ if path.ends_with("/locations") => (&["profile", "posts"], false),
        _ if path.ends_with("/metrics") => (&["metric-snapshot"], false),
        _ if path.ends_with("/ribbon") => (
            &["profile", "messages", "contacts", "interactions", "posts"],
            false,
        ),
        _ => (
            &[
                "profile",
                "organization",
                "tag",
                "messages",
                "contacts",
                "posts",
                "media",
            ],
            true,
        ),
    }
}

pub(crate) async fn version(s: &AppState, path: &str) -> Result<String> {
    let (domains, timed) = dependencies(path);
    let profile = path
        .strip_prefix("/api/v1/profiles/")
        .and_then(|tail| tail.split('/').next())
        .and_then(|id| Uuid::parse_str(id).ok())
        .or_else(|| (path == "/api/v1/metrics/snapshots").then(Uuid::nil));
    let mut scopes: Vec<_> = domains.iter().map(|value| (*value).to_owned()).collect();
    scopes.push("directory".to_owned());
    let value:String=sqlx::query_scalar(r#"WITH RECURSIVE owners(id,depth) AS (
 SELECT $2::uuid,0 WHERE $2::uuid IS NOT NULL UNION ALL SELECT r.new_profile_id,o.depth+1 FROM crm.profile_redirects r JOIN owners o ON o.id=r.old_profile_id WHERE o.depth<32),
 subjects AS(SELECT id FROM owners UNION SELECT CASE WHEN e.from_profile_id=o.id THEN e.to_profile_id ELSE e.from_profile_id END FROM owners o JOIN crm.relationship_edges e ON o.id IN(e.from_profile_id,e.to_profile_id)
 UNION SELECT m.group_profile_id FROM owners o JOIN crm.current_group_memberships m ON m.person_profile_id=o.id WHERE m.left_at IS NULL),
 tokens AS(
 SELECT v.domain,'subject:'||v.subject::text subject,v.version FROM crm.cache_versions v
 WHERE $2::uuid IS NOT NULL AND v.domain=ANY($1) AND v.domain NOT IN('organization','tag','community','directory') AND v.subject IN(SELECT id FROM subjects)
 UNION ALL SELECT v.domain,'shard:'||v.shard::text,v.version FROM crm.cache_domain_versions v
 WHERE v.domain=ANY($1) AND($2::uuid IS NULL OR v.domain IN('organization','tag','community','directory')))
 SELECT md5(coalesce(string_agg(domain||':'||subject||':'||version::text,',' ORDER BY domain,subject),''))||CASE WHEN $3 THEN ':'||floor(extract(epoch FROM now())/$4::integer)::bigint::text ELSE '' END FROM tokens"#)
        .bind(scopes).bind(profile).bind(timed).bind(CLOCK_BUCKET_SECONDS).fetch_one(&s.pool).await?;
    Ok(value)
}

pub(super) async fn middleware(
    State(s): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Err(error) = user(&s, request.headers()).await {
        return error.into_response();
    }
    if request.method() != Method::GET {
        return next.run(request).await;
    }
    let version = match version(&s, request.uri().path()).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let key = format!("page:{}", request.uri());
    let headers = request.headers().clone();
    request
        .headers_mut()
        .remove(axum::http::header::IF_NONE_MATCH);
    let fallback = Arc::new(Mutex::new(None));
    let original = fallback.clone();
    let result = s
        .read_cache
        .get_or_try_init(key, version, || build(request, next, original))
        .await;
    match result {
        Ok(value) => value.response(&headers),
        Err(error) => fallback
            .lock()
            .expect("page error response")
            .take()
            .unwrap_or_else(|| error.into_response()),
    }
}

async fn build(
    request: Request,
    next: Next,
    fallback: Arc<Mutex<Option<Response>>>,
) -> Result<axum::body::Bytes> {
    #[cfg(test)]
    if let Some(counter) = request
        .extensions()
        .get::<std::sync::Arc<std::sync::atomic::AtomicUsize>>()
    {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    let response = next.run(request).await;
    if response.status() != StatusCode::OK {
        let status = response.status();
        *fallback.lock().expect("page error response") = Some(response);
        return Err(ApiError(status, "page cache rebuild failed"));
    }
    to_bytes(response.into_body(), crate::cache::MAX_PAGE_BYTES)
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "response exceeds bounded page size",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request as HttpRequest};
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;

    async fn state(pool: &sqlx::PgPool, root: &std::path::Path) -> AppState {
        AppState::new(
            pool.clone(),
            "synthetic-cache-session-secret-32bytes",
            "http://localhost:17494".into(),
            root.into(),
        )
        .await
        .unwrap()
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn partitioned_versions_are_committed_and_person_invalidation_stays_local(
        pool: sqlx::PgPool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let s = state(&pool, dir.path()).await;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','A'),($2,'person','B')").bind(a).bind(b).execute(&pool).await.unwrap();
        let path = format!("/api/v1/profiles/{a}/overview");
        let global = version(&s, "/api/v1/directory").await.unwrap();
        let local = version(&s, &path).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("UPDATE crm.profiles SET display_name='B changed' WHERE id=$1")
            .bind(b)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(version(&s, "/api/v1/directory").await.unwrap(), global);
        tx.commit().await.unwrap();
        assert_ne!(version(&s, "/api/v1/directory").await.unwrap(), global);
        assert_eq!(version(&s, &path).await.unwrap(), local);
        let global = version(&s, "/api/v1/directory").await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("UPDATE crm.profiles SET display_name='A rolled back' WHERE id=$1")
            .bind(a)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(version(&s, "/api/v1/directory").await.unwrap(), global);
        assert_eq!(version(&s, &path).await.unwrap(), local);
        sqlx::query("UPDATE crm.profiles SET display_name='A committed' WHERE id=$1")
            .bind(a)
            .execute(&pool)
            .await
            .unwrap();
        assert_ne!(version(&s, &path).await.unwrap(), local);
        assert!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.cache_invalidations")
                .fetch_one(&pool)
                .await
                .unwrap()
                == 0
        );
    }

    async fn home(router: &Router, authenticated: bool) -> Response {
        let mut request = HttpRequest::builder()
            .uri("/api/v1/home")
            .header("host", "localhost:17494");
        if authenticated {
            request = request.header("cookie", "crm_session=aggregate-fixture");
        }
        router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
    async fn value(response: Response) -> Value {
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap()
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn weekly_statistics_preserve_auth_and_immediate_identity_changes(pool: sqlx::PgPool) {
        let dir = tempfile::tempdir().unwrap();
        let s = state(&pool, dir.path()).await;
        let actor = Uuid::new_v4();
        let person = Uuid::new_v4();
        let conversation = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,'aggregate@example.invalid','unused')").bind(actor).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')").bind(Uuid::new_v4()).bind(actor).bind(format!("{:x}",Sha256::digest(b"aggregate-fixture"))).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','A')",
        )
        .bind(person)
        .execute(&pool)
        .await
        .unwrap();
        let router = crate::auth::routes(s);
        assert_eq!(
            value(home(&router, true).await).await["key_numbers"]["messages_30d"],
            0
        );
        sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,profile_id,conversation_type) VALUES($1,'wechat','self','peer',$2,'direct')").bind(conversation).bind(person).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body,direction) SELECT gen_random_uuid(),$1,'wechat','self',n::text,'peer',now(),'Synthetic','received' FROM generate_series(1,12) n").bind(conversation).execute(&pool).await.unwrap();
        let old = home(&router, true).await;
        assert_eq!(value(old).await["key_numbers"]["messages_30d"], 0);
        assert_eq!(
            home(&router, false).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(
            !monica_core::pages::refresh_message_statistics(&pool)
                .await
                .unwrap()
        );
        sqlx::query("UPDATE crm.weekly_message_statistics SET week_start=week_start-7")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            monica_core::pages::refresh_message_statistics(&pool)
                .await
                .unwrap()
        );
        assert_eq!(
            value(home(&router, true).await).await["key_numbers"]["messages_30d"],
            12
        );
        sqlx::query("UPDATE crm.ai_settings SET enabled=true")
            .execute(&pool)
            .await
            .unwrap();
        // A cold HTTP response must still read persisted statistics, even when
        // message-history and live counter scans cannot proceed.
        let mut locked = pool.begin().await.unwrap();
        sqlx::query("LOCK crm.message_search,crm.message_stat_counts IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *locked)
            .await
            .unwrap();
        for path in [
            "/api/v1/home?cold=1",
            "/api/v1/insights?dormant_days=90",
            "/api/v1/home/dates",
            "/api/v1/reconnect",
            "/api/v1/insights/calendar",
            "/api/v1/insights/calendar/day?day=2026-11-16",
            "/api/v1/insights/orbit",
        ] {
            let request = HttpRequest::builder()
                .uri(path)
                .header("host", "localhost:17494")
                .header("cookie", "crm_session=aggregate-fixture")
                .body(Body::empty())
                .unwrap();
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                router.clone().oneshot(request),
            )
            .await
            .expect("page read tried to scan locked message history")
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
        locked.rollback().await.unwrap();
        let target = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Target')",
        )
        .bind(target)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE crm.conversations SET profile_id=$2 WHERE id=$1")
            .bind(conversation)
            .bind(target)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT direct_message_count FROM crm.weekly_contact_stats WHERE profile_id=$1"
            )
            .bind(target)
            .fetch_one(&pool)
            .await
            .unwrap(),
            12
        );
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT sum(sent+received)::bigint FROM crm.weekly_profile_months WHERE profile_id=$1")
            .bind(target).fetch_one(&pool).await.unwrap(), 12);
        sqlx::query("UPDATE crm.conversations SET profile_id=$2 WHERE id=$1")
            .bind(conversation)
            .bind(person)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE crm.profiles SET archived_at=now() WHERE id=$1")
            .bind(target)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE crm.profiles SET archived_at=now() WHERE id=$1")
            .bind(person)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            value(home(&router, true).await).await["key_numbers"]["contacts"],
            0
        );
    }
}

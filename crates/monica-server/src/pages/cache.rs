//! Authentication and cheap committed dependency tokens precede the singleflight.
//! The original handler (including its SQL and JSON construction) runs only on miss.
use super::*;
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::IntoResponse,
};
use std::sync::Mutex;

fn dependencies(path: &str) -> (&'static [&'static str], bool) {
    match path {
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
        "/api/v1/home" => (&["profile", "calendar", "messages"], true),
        "/api/v1/insights/orbit" => (
            &[
                "profile",
                "organization",
                "tag",
                "messages",
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
                "organization",
                "messages",
                "contacts",
                "posts",
                "media",
            ],
            true,
        ),
        "/api/v1/insights/calendar" | "/api/v1/insights/calendar/day" => (
            &["profile", "calendar", "contacts", "messages", "media"],
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

pub(super) async fn version(s: &AppState, path: &str) -> Result<String> {
    let (domains, timed) = dependencies(path);
    let profile = path
        .strip_prefix("/api/v1/profiles/")
        .and_then(|tail| tail.split('/').next())
        .and_then(|id| Uuid::parse_str(id).ok())
        .or_else(|| (path == "/api/v1/metrics/snapshots").then(Uuid::nil));
    let scopes: Vec<_> = domains.iter().map(|value| (*value).to_owned()).collect();
    let value:String=sqlx::query_scalar(r#"WITH RECURSIVE owners(id,depth) AS (
 SELECT $2::uuid,0 WHERE $2::uuid IS NOT NULL UNION ALL SELECT r.new_profile_id,o.depth+1 FROM crm.profile_redirects r JOIN owners o ON o.id=r.old_profile_id WHERE o.depth<32),
 subjects AS(SELECT id FROM owners UNION SELECT CASE WHEN e.from_profile_id=o.id THEN e.to_profile_id ELSE e.from_profile_id END FROM owners o JOIN crm.relationship_edges e ON o.id IN(e.from_profile_id,e.to_profile_id)
 UNION SELECT m.group_profile_id FROM owners o JOIN crm.current_group_memberships m ON m.person_profile_id=o.id WHERE m.left_at IS NULL)
 SELECT md5(coalesce(string_agg(v.domain||':'||v.subject::text||':'||v.version::text,',' ORDER BY v.domain,v.subject),''))||CASE WHEN $3 THEN ':'||floor(extract(epoch FROM now())/60)::bigint::text ELSE '' END
 FROM crm.cache_versions v WHERE v.domain=ANY($1) AND($2::uuid IS NULL OR v.subject IN(SELECT id FROM subjects) OR v.domain IN('organization','tag','community'))"#)
        .bind(scopes).bind(profile).bind(timed).fetch_one(&s.pool).await?;
    Ok(value)
}

pub(super) async fn middleware(
    State(s): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.method() != Method::GET {
        return next.run(request).await;
    }
    if let Err(error) = user(&s, request.headers()).await {
        return error.into_response();
    }
    let path = request.uri().path().to_owned();
    let version = match version(&s, &path).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let key = format!("page:{}", request.uri());
    let headers = request.headers().clone();
    request
        .headers_mut()
        .remove(axum::http::header::IF_NONE_MATCH);
    let fallback = Mutex::new(None);
    #[cfg(test)]
    let counter = request
        .extensions()
        .get::<std::sync::Arc<std::sync::atomic::AtomicUsize>>()
        .cloned();
    let result = s
        .read_cache
        .get_or_try_init(key, version, || async {
            #[cfg(test)]
            if let Some(counter) = counter {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let response = next.run(request).await;
            if response.status() != StatusCode::OK {
                let status = response.status();
                *fallback.lock().expect("page error response") = Some(response);
                return Err(ApiError(status, "page cache rebuild failed"));
            }
            let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
                .await
                .map_err(|_| {
                    ApiError(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "response exceeds bounded page size",
                    )
                })?;
            serde_json::from_slice(&bytes).map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "page JSON decoding failed",
                )
            })
        })
        .await;
    match result {
        Ok(value) => value.response(&headers),
        Err(error) => fallback
            .into_inner()
            .expect("page error response")
            .unwrap_or_else(|| error.into_response()),
    }
}

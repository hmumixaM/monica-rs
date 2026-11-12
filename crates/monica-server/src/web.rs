//! Same-origin frontend hosting. Only known browser routes receive the SPA shell.
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::path::PathBuf;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct WebRoot(PathBuf);

pub async fn routes(path: PathBuf) -> anyhow::Result<Router> {
    let path = tokio::fs::canonicalize(path).await?;
    anyhow::ensure!(
        tokio::fs::metadata(path.join("index.html"))
            .await?
            .is_file(),
        "frontend build must contain index.html"
    );
    Ok(Router::new().fallback(serve).with_state(WebRoot(path)))
}

fn browser_route(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    matches!(
        path,
        "" | "/login"
            | "/dashboard"
            | "/profiles"
            | "/linking"
            | "/faces"
            | "/relationships"
            | "/timeline"
            | "/suggestions"
            | "/search"
            | "/imports"
            | "/digest"
            | "/messages"
            | "/settings"
            | "/insights"
            | "/ask"
            | "/sources"
    ) || path
        .strip_prefix("/profiles/")
        .or_else(|| path.strip_prefix("/evidence/"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

async fn serve(State(root): State<WebRoot>, request: Request) -> Response {
    let path = request.uri().path();
    if path == "/api"
        || path.starts_with("/api/")
        || path == "/internal"
        || path.starts_with("/internal/")
    {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    // Bundle outputs should not expose hidden files or encoded path traversal.
    let decoded = match percent_encoding::percent_decode_str(path).decode_utf8() {
        Ok(path) => path,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if decoded.split('/').any(|part| part.starts_with('.')) || decoded.contains('\\') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let html = browser_route(&decoded);
    let immutable = decoded.starts_with("/assets/");
    let result = if html {
        ServeFile::new(root.0.join("index.html"))
            .oneshot(request)
            .await
    } else {
        ServeDir::new(&root.0)
            .append_index_html_on_directories(false)
            .oneshot(request)
            .await
    };
    let mut response = match result {
        Ok(response) => response.into_response(),
        Err(error) => match error {},
    };
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    let cache = if !response.status().is_success() {
        "no-store"
    } else if immutable && !html {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};

    #[tokio::test]
    async fn deep_links_and_assets_do_not_hide_missing_api_or_files() {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::write(root.path().join("index.html"), "<html>Monica</html>")
            .await
            .unwrap();
        tokio::fs::create_dir(root.path().join("assets"))
            .await
            .unwrap();
        tokio::fs::write(root.path().join("assets/app-123.js"), "console.log(1)")
            .await
            .unwrap();
        tokio::fs::write(root.path().join(".env"), "private")
            .await
            .unwrap();
        let app = routes(root.path().to_path_buf()).await.unwrap();
        for path in [
            "/",
            "/profiles",
            "/sources",
            "/ask",
            "/evidence/00000000-0000-0000-0000-000000000001?revision=2",
            "/profiles/00000000-0000-0000-0000-000000000001?tab=chat",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap(),
                "<html>Monica</html>"
            );
        }
        for path in [
            "/api/missing",
            "/internal/missing",
            "/assets/missing.js",
            "/unknown",
            "/.env",
            "/%2eenv",
            "/profiles/not-a-uuid",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/app-123.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

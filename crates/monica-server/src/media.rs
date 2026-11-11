use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    body::Body,
    extract::{Multipart, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
const MAX_FILE: u64 = 500 * 1024 * 1024;
fn storage() -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "media storage unavailable")
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/media",
            post(upload).layer(axum::extract::DefaultBodyLimit::max(
                MAX_FILE as usize + 1024 * 1024,
            )),
        )
        .route("/api/v1/media/{id}", get(download))
        .route("/api/v1/profiles/{id}/photos", get(photos).post(attach))
        .route(
            "/api/v1/profiles/{id}/photos/{media}",
            axum::routing::delete(detach),
        )
}
async fn upload(
    State(s): State<AppState>,
    h: HeaderMap,
    mut parts: Multipart,
) -> Result<(StatusCode, Json<Value>)> {
    user(&s, &h).await?;
    let _permit = s
        .media_io
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "media capacity busy"))?;
    let staging = s.media_root.join(".staging");
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|_| storage())?;
    let temp = tokio::task::spawn_blocking(move || tempfile::NamedTempFile::new_in(staging))
        .await
        .map_err(|_| storage())?
        .map_err(|_| storage())?;
    let mut writer = tokio::fs::OpenOptions::new()
        .write(true)
        .open(temp.path())
        .await
        .map_err(|_| storage())?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut head = Vec::new();
    let mut filename = None;
    let mut found = false;
    while let Some(mut field) = parts
        .next_field()
        .await
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid multipart upload"))?
    {
        if field.name() != Some("file") {
            continue;
        }
        if found {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "one file required",
            ));
        }
        found = true;
        filename = field.file_name().map(|name| {
            name.rsplit(['/', '\\'])
                .next()
                .unwrap_or("file")
                .chars()
                .filter(|c| !c.is_control())
                .take(255)
                .collect::<String>()
        });
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid upload chunk"))?
        {
            bytes += chunk.len() as u64;
            if bytes > MAX_FILE {
                return Err(ApiError(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "File exceeds 500 MB",
                ));
            }
            if head.len() < 16384 {
                head.extend_from_slice(&chunk[..chunk.len().min(16384 - head.len())]);
            }
            hash.update(&chunk);
            writer.write_all(&chunk).await.map_err(|_| storage())?;
        }
    }
    if !found || bytes == 0 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "nonempty file required",
        ));
    }
    writer.sync_all().await.map_err(|_| storage())?;
    drop(writer);
    let digest = format!("{:x}", hash.finalize());
    let kind = infer::get(&head)
        .map(|t| t.mime_type())
        .unwrap_or("application/octet-stream")
        .to_string();
    let relative = format!("sha256/{}/{}/{}", &digest[..2], &digest[2..4], digest);
    let target = s.media_root.join(&relative);
    let parent = target.parent().unwrap();
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|_| storage())?;
    // Hard-link publication is atomic and cannot overwrite a previously published blob.
    match tokio::fs::hard_link(temp.path(), &target).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(storage()),
    }
    let root = s.media_root.clone();
    let parent = parent.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut p = Some(parent.as_path());
        while let Some(dir) = p {
            std::fs::File::open(dir)?.sync_all()?;
            if dir == root {
                break;
            }
            p = dir.parent();
        }
        Ok(())
    })
    .await
    .map_err(|_| storage())?
    .map_err(|_| storage())?;
    // All file I/O has finished before taking a database connection for registration.
    sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,file_name) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(sha256) DO NOTHING")
 .bind(Uuid::new_v4()).bind(&digest).bind(kind).bind(bytes as i64).bind(relative).bind(filename).execute(&s.pool).await?;
    let row: Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM crm.media_assets m WHERE sha256=$1")
            .bind(digest)
            .fetch_one(&s.pool)
            .await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn download(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Result<Response> {
    user(&s, request.headers()).await?;
    let row = sqlx::query("SELECT sha256,media_type,file_name FROM crm.media_assets WHERE id=$1")
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "media not found"))?;
    let hash: String = row.get("sha256");
    let kind: String = row.get("media_type");
    let name: Option<String> = row.get("file_name");
    let path = s
        .media_root
        .join(format!("sha256/{}/{}/{}", &hash[..2], &hash[2..4], hash));
    let etag = format!("\"{hash}\"");
    if request
        .headers()
        .get("if-none-match")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|t| t.trim() == etag || t.trim() == "*" || t.trim() == format!("W/{etag}"))
        })
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response
            .headers_mut()
            .insert("etag", HeaderValue::from_str(&etag).unwrap());
        return Ok(response);
    }
    let mut response = ServeFile::new(path)
        .oneshot(request)
        .await
        .map_err(|_| storage())?
        .map(Body::new);
    response
        .headers_mut()
        .insert("etag", HeaderValue::from_str(&etag).unwrap());

    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    if response.status().is_success() {
        response.headers_mut().insert(
            "cache-control",
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        );
        response.headers_mut().insert(
            "content-type",
            HeaderValue::from_str(&kind).map_err(|_| storage())?,
        );
        let inline = matches!(
            kind.as_str(),
            "image/jpeg"
                | "image/png"
                | "image/gif"
                | "image/webp"
                | "video/mp4"
                | "video/webm"
                | "application/pdf"
        );
        let filename = percent_encoding::utf8_percent_encode(
            name.as_deref().unwrap_or("download"),
            percent_encoding::NON_ALPHANUMERIC,
        );
        response.headers_mut().insert(
            "content-disposition",
            HeaderValue::from_str(&format!(
                "{}; filename*=UTF-8''{}",
                if inline { "inline" } else { "attachment" },
                filename
            ))
            .map_err(|_| storage())?,
        );
    }
    Ok(response)
}
async fn photos(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    exists(&s, id).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT (to_jsonb(m)-'object_path')||jsonb_build_object('media_id',m.id,'url','/api/v1/media/'||m.id::text,'role',l.role) FROM crm.media_links l JOIN crm.media_assets m ON m.id=l.media_id WHERE l.entity_type='profile' AND l.entity_id=$1 AND l.role IN ('manual_photo','manual_file') ORDER BY l.created_at,m.id").bind(id).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn exists(s: &AppState, id: Uuid) -> Result<()> {
    sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 AND archived_at IS NULL")
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "profile not found"))?;
    Ok(())
}
#[derive(Deserialize)]
struct Selection {
    media_ids: Vec<Uuid>,
}
async fn attach(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<Selection>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    if p.media_ids.len() > 1000 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "too many media references",
        ));
    }
    let mut ids = p.media_ids;
    ids.sort();
    ids.dedup();
    let mut tx = s.pool.begin().await?;
    sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 AND archived_at IS NULL FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "profile not found"))?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.media_assets WHERE id=ANY($1)")
        .bind(&ids)
        .fetch_one(&mut *tx)
        .await?;
    if count != ids.len() as i64 {
        return Err(ApiError(StatusCode::NOT_FOUND, "media not found"));
    }
    sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) SELECT 'profile',$1,id,CASE WHEN media_type IN ('image/jpeg','image/png','image/gif','image/webp') THEN 'manual_photo' ELSE 'manual_file' END FROM crm.media_assets WHERE id=ANY($2) ORDER BY id ON CONFLICT DO NOTHING").bind(id).bind(&ids).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) VALUES($1,'profile.photos.added','profile',$2)").bind(actor).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"status":"attached"}))))
}
async fn detach(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, media)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let (actor, _) = user(&s, &h).await?;
    let n=sqlx::query("WITH removed AS (DELETE FROM crm.media_links WHERE entity_type='profile' AND entity_id=$1 AND media_id=$2 AND role IN ('manual_photo','manual_file') RETURNING entity_id) INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) SELECT $3,'profile.photos.removed','profile',entity_id FROM removed").bind(id).bind(media).bind(actor).execute(&s.pool).await?.rows_affected();
    if n == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "manual photo not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    body::Body,
    extract::{Multipart, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
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
pub(crate) fn media_error(error: anyhow::Error) -> ApiError {
    if let Some(error) = error.downcast_ref::<monica_core::media::MediaError>() {
        return ApiError(
            StatusCode::from_u16(error.0).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            error.1,
        );
    }
    tracing::warn!(error=%error,"media operation failed");
    storage()
}
fn store(s: &AppState) -> Result<monica_core::media::MediaStore> {
    monica_core::media::MediaStore::new(s.pool.clone(), s.media_root.clone(), s.resources.clone())
        .map_err(media_error)
}
#[derive(Deserialize)]
struct Initiate {
    expected_sha256: Option<String>,
    expected_byte_length: Option<u64>,
    #[serde(default = "default_type")]
    media_type: String,
    file_name: Option<String>,
}
fn default_type() -> String {
    "application/octet-stream".into()
}
async fn initiate(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(input): Json<Initiate>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    Ok((
        StatusCode::CREATED,
        Json(
            store(&s)?
                .initiate(
                    actor,
                    input.expected_sha256,
                    input.expected_byte_length,
                    input.media_type,
                    input.file_name,
                )
                .await
                .map_err(media_error)?,
        ),
    ))
}
async fn upload_status(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    Ok(Json(
        store(&s)?
            .upload_status(id, actor)
            .await
            .map_err(media_error)?,
    ))
}
async fn upload_chunk(
    State(s): State<AppState>,
    Path((id, chunk)): Path<(Uuid, i32)>,
    request: Request,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, request.headers()).await?;
    // Reserve before buffering, so concurrent clients cannot bypass the host's
    // memory budget while waiting for the disk writer.
    let _permit = s
        .resources
        .acquire(monica_core::resources::TaskResources {
            memory_bytes: monica_core::media::MAX_CHUNK,
            ..Default::default()
        })
        .await
        .map_err(media_error)?;
    let body = axum::body::to_bytes(request.into_body(), monica_core::media::MAX_CHUNK)
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid or oversized upload chunk",
            )
        })?;
    Ok(Json(
        store(&s)?
            .chunk(id, actor, chunk, &body)
            .await
            .map_err(media_error)?,
    ))
}
async fn complete_upload(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    let asset = store(&s)?.complete(id, actor).await.map_err(media_error)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(asset).map_err(|_| storage())?),
    ))
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/imports/media/initiate", post(initiate))
        .route("/api/v1/imports/media/{upload}", get(upload_status))
        .route(
            "/api/v1/imports/media/{upload}/chunks/{chunk}",
            axum::routing::put(upload_chunk).layer(axum::extract::DefaultBodyLimit::max(
                monica_core::media::MAX_CHUNK,
            )),
        )
        .route(
            "/api/v1/imports/media/{upload}/complete",
            post(complete_upload),
        )
        .route(
            "/api/v1/media",
            post(upload).layer(axum::extract::DefaultBodyLimit::max(
                MAX_FILE as usize + 1024 * 1024,
            )),
        )
        .route("/api/v1/media/{id}", get(download))
        .route("/api/v1/media/{id}/preview", get(preview))
        .route("/api/v1/media/{id}/ocr", get(ocr))
        .route("/api/v1/profiles/{id}/photos", get(photos).post(attach))
        .route(
            "/api/v1/profiles/{id}/photos/{media}",
            axum::routing::delete(detach),
        )
}
async fn ocr(State(s): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(sqlx::query_scalar("SELECT jsonb_build_object('status',status,'summary',summary,'description',description,'category',category,'text',text,'model',model,'generated_at',generated_at,'error',error) FROM crm.image_ocr_readings WHERE media_id=$1").bind(id).fetch_optional(&s.pool).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"Image is not in the OCR backfill"))?))
}
#[derive(Deserialize)]
struct PreviewQuery {
    face_id: Option<Uuid>,
}
async fn preview(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<PreviewQuery>,
    request: Request,
) -> Result<Response> {
    user(&s, request.headers()).await?;
    let store = store(&s)?;
    let asset = store.asset(id).await.map_err(media_error)?;
    let crop: Option<Vec<f32>> = if let Some(face) = q.face_id {
        Some(
            sqlx::query_scalar("SELECT box FROM crm.faces WHERE id=$1 AND media_id=$2")
                .bind(face)
                .bind(id)
                .fetch_optional(&s.pool)
                .await?
                .ok_or(ApiError(StatusCode::NOT_FOUND, "face not found"))?,
        )
    } else {
        None
    };
    let (path, key) = store
        .preview(&asset, crop.as_deref())
        .await
        .map_err(media_error)?;
    let etag = format!("\"{key}\"");
    if request
        .headers()
        .get("if-none-match")
        .and_then(|x| x.to_str().ok())
        .is_some_and(|x| {
            x.split(',')
                .any(|t| t.trim() == etag || t.trim() == "*" || t.trim() == format!("W/{etag}"))
        })
    {
        return Ok((StatusCode::NOT_MODIFIED, [("etag", etag)]).into_response());
    }
    let mut response = ServeFile::new(path)
        .oneshot(request)
        .await
        .map_err(|_| storage())?
        .map(Body::new);
    response
        .headers_mut()
        .insert("etag", HeaderValue::from_str(&etag).unwrap());
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("image/webp"));
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok(response)
}
async fn upload(
    State(s): State<AppState>,
    h: HeaderMap,
    mut parts: Multipart,
) -> Result<(StatusCode, Json<Value>)> {
    user(&s, &h).await?;
    let _permit = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        s.resources.acquire(monica_core::resources::TaskResources {
            disk: 1,
            memory_bytes: 256 * 1024,
            ..Default::default()
        }),
    )
    .await
    .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "media capacity busy"))?
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
    let mut bytes = 0_u64;
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
    drop(_permit);
    let store = store(&s)?;
    let asset = store
        .ingest_path(temp.path(), filename.as_deref(), "manual", None, None, None)
        .await
        .map_err(media_error)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(asset).map_err(|_| storage())?),
    ))
}

async fn download(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    mut request: Request,
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
    if !tokio::fs::try_exists(&path).await.map_err(|_| storage())? {
        return Err(ApiError(StatusCode::NOT_FOUND, "media not found"));
    }
    let etag = format!("\"{}\"", &hash[..32]);
    if let Some(condition) = request
        .headers()
        .get("if-range")
        .and_then(|x| x.to_str().ok())
        .map(str::to_owned)
    {
        let modified = tokio::fs::metadata(&path)
            .await
            .map_err(|_| storage())?
            .modified()
            .map_err(|_| storage())?;
        let unchanged = condition == etag
            || chrono::DateTime::parse_from_rfc2822(&condition)
                .ok()
                .is_some_and(|date| {
                    modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .is_ok_and(|mtime| date.timestamp() >= mtime.as_secs() as i64)
                });
        request.headers_mut().remove("if-range");
        if !unchanged {
            request.headers_mut().remove("range");
        }
    }
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
                | "audio/mpeg"
                | "audio/mp4"
                | "audio/wav"
                | "audio/ogg"
                | "audio/webm"
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

use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{FromRequest, OriginalUri, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use tokio::io::AsyncReadExt;
use uuid::Uuid;
#[cfg(test)]
#[path = "faces_tests.rs"]
mod tests;
type Result<T> = std::result::Result<T, ApiError>;
// Admission occurs before buffering and parsing the signed JSON, including
// requests from many devices arriving while another batch holds DB locks.
struct DeviceBody {
    bytes: Bytes,
    _permit: monica_core::resources::ResourcePermit,
}
impl std::ops::Deref for DeviceBody {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}
impl FromRequest<AppState> for DeviceBody {
    type Rejection = ApiError;
    async fn from_request(request: Request, state: &AppState) -> Result<Self> {
        let permit = state
            .resources
            .acquire(monica_core::resources::TaskResources {
                cpu: 1,
                memory_bytes: 32 * 1024 * 1024,
                ..Default::default()
            })
            .await
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "face capacity busy"))?;
        let bytes = axum::body::to_bytes(request.into_body(), 8 * 1024 * 1024)
            .await
            .map_err(|_| ApiError(StatusCode::PAYLOAD_TOO_LARGE, "face request too large"))?;
        Ok(Self {
            bytes,
            _permit: permit,
        })
    }
}
fn invalid(error: anyhow::Error) -> ApiError {
    tracing::warn!(%error,"face request rejected");
    ApiError(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid face annotation or selection",
    )
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/faces/device/work", get(device_work))
        .route("/api/v1/faces/device/anchors", get(device_anchors))
        .route("/api/v1/faces/device/media", post(device_media))
        .route("/api/v1/faces/device/batch", post(device_batch))
        .route(
            "/api/v1/faces/device/status",
            get(device_status).post(device_update),
        )
        .route("/api/v1/faces/summary", get(summary))
        .route("/api/v1/faces/groups", get(groups))
        .route("/api/v1/faces/decide", post(decide))
        .route("/api/v1/faces/restore", post(restore))
        .route("/api/v1/faces/in-media", get(in_media))
        .route("/api/v1/faces/media/{media}/reading", get(reading))
        .route("/api/v1/faces/people/{profile}", get(appearances))
        .route("/api/v1/faces/people/{profile}/photos", get(photos))
        .route(
            "/api/v1/faces/people/{profile}/photos/status",
            get(photo_status),
        )
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
}
async fn reading(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(media): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('kind',r.entity_type,'entity_id',r.entity_id,'summary',r.summary,'items',r.items) FROM crm.media_readings r JOIN crm.media_links l ON l.entity_type=r.entity_type AND l.entity_id=r.entity_id WHERE l.media_id=$1 ORDER BY r.entity_type,r.entity_id").bind(media).fetch_all(&s.pool).await?;
    Ok(Json(json!({"readings":rows})))
}
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    headers
        .get(name)
        .and_then(|x| x.to_str().ok())
        .ok_or(ApiError(
            StatusCode::UNAUTHORIZED,
            "signed faces request required",
        ))
}
async fn signed(
    s: &AppState,
    h: &HeaderMap,
    method: &str,
    target: &str,
    body: &[u8],
) -> Result<()> {
    let secret = s.import_hmac_secret.as_deref().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Face device authentication is not configured",
    ))?;
    let timestamp = header(h, "x-import-timestamp")?;
    let nonce = header(h, "x-import-nonce")?;
    let signature = header(h, "x-import-signature")?;
    let token = header(h, "x-import-token")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let valid = timestamp
        .parse::<i64>()
        .is_ok_and(|time| now.abs_diff(time) <= 300)
        && !nonce.is_empty()
        && nonce.len() <= 255
        && signature.len() == 64
        && signature.bytes().all(|x| x.is_ascii_hexdigit());
    if !valid {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "invalid faces signature",
        ));
    }
    let digest = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&signature[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "invalid faces signature"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid signing configuration",
        )
    })?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(nonce.as_bytes());
    mac.update(b".");
    mac.update(method.as_bytes());
    mac.update(b" ");
    mac.update(target.as_bytes());
    mac.update(b"\n");
    mac.update(body);
    mac.verify_slice(&digest)
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "invalid faces signature"))?;
    let mut tx = s.pool.begin().await?;
    let token:Option<Uuid>=sqlx::query_scalar("SELECT id FROM crm.import_service_tokens WHERE source='faces' AND token_hash=$1 AND revoked_at IS NULL").bind(format!("{:x}",Sha256::digest(token.as_bytes()))).fetch_optional(&mut *tx).await?;
    let token = token.ok_or(ApiError(StatusCode::UNAUTHORIZED, "invalid faces token"))?;
    sqlx::query("UPDATE crm.import_service_tokens SET last_used_at=now() WHERE id=$1 AND (last_used_at IS NULL OR last_used_at<now()-interval '1 minute')").bind(token).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM crm.import_request_nonces WHERE expires_at<now()")
        .execute(&mut *tx)
        .await?;
    let accepted=sqlx::query("INSERT INTO crm.import_request_nonces(nonce,expires_at) VALUES($1,now()+interval '5 minutes') ON CONFLICT DO NOTHING").bind(nonce).execute(&mut *tx).await?.rows_affected();
    if accepted != 1 {
        return Err(ApiError(StatusCode::CONFLICT, "replayed request"));
    }
    tx.commit().await?;
    Ok(())
}
#[derive(Deserialize)]
struct WorkQuery {
    #[serde(default = "work_limit")]
    limit: i64,
}
fn work_limit() -> i64 {
    200
}
async fn device_work(
    State(s): State<AppState>,
    h: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<WorkQuery>,
) -> Result<Json<Value>> {
    signed(&s, &h, "GET", &uri.to_string(), &[]).await?;
    Ok(Json(
        monica_core::faces::work(&s.pool, q.limit)
            .await
            .map_err(invalid)?,
    ))
}
async fn device_anchors(
    State(s): State<AppState>,
    h: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Result<Json<Value>> {
    signed(&s, &h, "GET", &uri.to_string(), &[]).await?;
    Ok(Json(
        monica_core::faces::anchors(&s.pool)
            .await
            .map_err(invalid)?,
    ))
}
async fn device_batch(
    State(s): State<AppState>,
    h: HeaderMap,
    OriginalUri(uri): OriginalUri,
    body: DeviceBody,
) -> Result<Json<Value>> {
    signed(&s, &h, "POST", &uri.to_string(), &body).await?;
    let batch: monica_core::faces::Batch = serde_json::from_slice(&body)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid face batch"))?;
    Ok(Json(
        monica_core::faces::batch(&s.pool, &batch)
            .await
            .map_err(invalid)?,
    ))
}
#[derive(Deserialize)]
struct MediaBatch {
    ids: Vec<Uuid>,
}
async fn device_media(
    State(s): State<AppState>,
    h: HeaderMap,
    OriginalUri(uri): OriginalUri,
    body: DeviceBody,
) -> Result<Response> {
    signed(&s, &h, "POST", &uri.to_string(), &body).await?;
    let input: MediaBatch = serde_json::from_slice(&body)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid media selection"))?;
    if input.ids.is_empty() || input.ids.len() > 100 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "select 1 to 100 eligible files",
        ));
    }
    let rows=sqlx::query(&format!("SELECT a.id,a.sha256 FROM crm.media_assets a JOIN ({}) e ON e.media_id=a.id WHERE a.id=ANY($1) ORDER BY a.id",monica_core::faces::ELIGIBLE)).bind(input.ids).fetch_all(&s.pool).await?;
    let mut files = Vec::new();
    for row in rows {
        let hash: String = row.get("sha256");
        let path = s
            .media_root
            .join(monica_core::media::object_path(&hash).map_err(invalid)?);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            files.push((row.get::<Uuid, _>("id"), path));
        }
    }
    let stream = async_stream::stream! {
        for(id,path) in files{
            let _permit=match s.resources.acquire(monica_core::resources::TaskResources{disk:1,memory_bytes:512*1024,..Default::default()}).await {Ok(p)=>p,Err(e)=>{yield Err(std::io::Error::other(e));return;}};
            let mut file=match tokio::fs::File::open(path).await {Ok(file)=>file,Err(e)=>{yield Err(e);return;}};
            let size=match file.metadata().await{Ok(meta)=>meta.len(),Err(e)=>{yield Err(e);return;}};
            let header=tar_header(&id.to_string(),size);yield Ok::<Bytes,std::io::Error>(Bytes::copy_from_slice(&header));
            let mut remaining=size;let mut chunk=vec![0u8;256*1024];
            while remaining>0{
                let amount=(remaining as usize).min(chunk.len());let n=match file.read(&mut chunk[..amount]).await{Ok(n)=>n,Err(e)=>{yield Err(e);return;}};
                if n==0{yield Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof,"media truncated during transfer"));return;}
                remaining-=n as u64;yield Ok(Bytes::copy_from_slice(&chunk[..n]));
            }
            if size%512!=0{yield Ok(Bytes::from(vec![0;512-(size%512) as usize]));}
        }
        yield Ok(Bytes::from_static(&[0;1024]));
    };
    let body = Body::from_stream(stream);
    Ok((
        [
            ("content-type", "application/x-tar"),
            ("cache-control", "no-store"),
        ],
        body,
    )
        .into_response())
}
fn tar_header(name: &str, size: u64) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[148..156].fill(b' ');
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u64 = h.iter().map(|x| u64::from(*x)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}
async fn device_status(
    State(s): State<AppState>,
    h: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Result<Json<Value>> {
    signed(&s, &h, "GET", &uri.to_string(), &[]).await?;
    Ok(Json(
        sqlx::query_scalar(
            "SELECT to_jsonb(r)-'singleton' FROM crm.face_device_runtime r WHERE singleton",
        )
        .fetch_one(&s.pool)
        .await?,
    ))
}
#[derive(Deserialize)]
struct DeviceStatus {
    state: String,
    model: Option<String>,
    error: Option<String>,
}
async fn device_update(
    State(s): State<AppState>,
    h: HeaderMap,
    OriginalUri(uri): OriginalUri,
    body: DeviceBody,
) -> Result<Json<Value>> {
    signed(&s, &h, "POST", &uri.to_string(), &body).await?;
    let state: DeviceStatus = serde_json::from_slice(&body)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid device state"))?;
    if !["needs_models", "ready", "running", "error", "stopped"].contains(&state.state.as_str())
        || state.model.as_ref().is_some_and(|x| x.len() > 255)
        || state.error.as_ref().is_some_and(|x| x.len() > 2000)
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid device state",
        ));
    }
    let value:Value=sqlx::query_scalar("UPDATE crm.face_device_runtime SET state=$1,model=$2,error=$3,updated_at=now() WHERE singleton RETURNING to_jsonb(face_device_runtime)-'singleton'").bind(state.state).bind(state.model).bind(state.error).fetch_one(&s.pool).await?;
    Ok(Json(value))
}
async fn summary(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(sqlx::query_scalar("SELECT jsonb_build_object('scanned',(SELECT count(*) FROM crm.face_scans),'last_scan_at',(SELECT max(scanned_at) FROM crm.face_scans),'faces',count(*),'to_review',count(*) FILTER(WHERE status='pending' AND in_post AND quality>=0.6 AND (suggested_profile_id IS NOT NULL OR cluster_key IS NOT NULL)),'confirmed',count(*) FILTER(WHERE status='confirmed'),'people',count(DISTINCT profile_id) FILTER(WHERE status='confirmed')) FROM crm.faces").fetch_one(&s.pool).await?))
}
#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: i64,
    #[serde(default = "page_limit")]
    limit: i64,
}
fn page_limit() -> i64 {
    24
}
async fn groups(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if q.offset < 0 || !(1..=40).contains(&q.limit) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid group page",
        ));
    }
    let mut rows:Vec<Value>=sqlx::query_scalar("WITH grouped AS (SELECT suggested_profile_id AS person,CASE WHEN suggested_profile_id IS NULL THEN cluster_key END AS cluster,count(*) AS size,max(suggestion_score) AS best FROM crm.faces WHERE status='pending' AND in_post AND quality>=0.6 AND (suggested_profile_id IS NOT NULL OR cluster_key IS NOT NULL) GROUP BY 1,2),page AS MATERIALIZED(SELECT coalesce('p:'||person,'c:'||cluster) AS key,* FROM grouped ORDER BY (person IS NOT NULL) DESC,size DESC,coalesce('p:'||person,'c:'||cluster) OFFSET $1 LIMIT $2) SELECT jsonb_build_object('key',g.key,'size',g.size,'best',g.best,'person',CASE WHEN p.id IS NULL THEN NULL ELSE jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id) END,'faces',(SELECT jsonb_agg(x) FROM (SELECT f.id,f.media_id,f.box,f.quality,f.suggestion_score AS score,(SELECT l.entity_id FROM crm.media_links l WHERE l.media_id=f.media_id AND l.entity_type='social_post' LIMIT 1) AS post_id FROM crm.faces f WHERE f.status='pending' AND f.in_post AND f.quality>=0.6 AND (f.suggested_profile_id=g.person OR (g.person IS NULL AND f.suggested_profile_id IS NULL AND f.cluster_key=g.cluster)) ORDER BY f.suggestion_score DESC NULLS LAST,f.quality DESC,f.id LIMIT 18) x)) FROM page g LEFT JOIN crm.profiles p ON p.id=g.person ORDER BY (g.person IS NOT NULL) DESC,g.size DESC,g.key")
        .bind(q.offset).bind(q.limit+1).fetch_all(&s.pool).await?;
    let next = (rows.len() > q.limit as usize).then_some(q.offset + q.limit);
    rows.truncate(q.limit as usize);
    Ok(Json(json!({"items":rows,"next_offset":next})))
}
async fn decide(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(input): Json<monica_core::faces::Decision>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(
        monica_core::faces::decide(&s.pool, &input)
            .await
            .map_err(invalid)?,
    ))
}
async fn restore(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(input): Json<Vec<monica_core::faces::FaceState>>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(
        monica_core::faces::restore(&s.pool, &input)
            .await
            .map_err(invalid)?,
    ))
}
#[derive(Deserialize)]
struct MediaIds {
    ids: String,
}
async fn in_media(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<MediaIds>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if q.ids.len() > 4000 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "too many media ids",
        ));
    }
    let ids = q
        .ids
        .split(',')
        .filter(|x| !x.is_empty())
        .map(Uuid::parse_str)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid media id"))?;
    let faces:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',f.id,'media_id',f.media_id,'box',f.box,'status',f.status,'score',f.suggestion_score,'profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id) FROM crm.faces f JOIN crm.profiles p ON p.id=CASE WHEN f.status='confirmed' THEN f.profile_id ELSE f.suggested_profile_id END WHERE f.media_id=ANY($1) AND f.status<>'ignored' AND f.quality>=0.6 ORDER BY f.media_id,f.box[1]").bind(ids).fetch_all(&s.pool).await?;
    Ok(Json(json!({"faces":faces})))
}
async fn appearances(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(profile): Path<Uuid>,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if q.offset < 0 || !(1..=60).contains(&q.limit) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid appearances page",
        ));
    }
    let mut rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(x) FROM (SELECT DISTINCT ON(sp.occurred_at,f.media_id) f.id,f.media_id,f.box,sp.id AS post_id,sp.occurred_at,sp.provider,author.display_name AS author,(SELECT jsonb_agg(jsonb_build_object('profile_id',o.profile_id,'name',op.display_name)) FROM crm.faces o JOIN crm.profiles op ON op.id=o.profile_id WHERE o.media_id=f.media_id AND o.status='confirmed' AND o.profile_id<>$1) AS others FROM crm.faces f JOIN crm.media_links l ON l.media_id=f.media_id AND l.entity_type='social_post' JOIN crm.social_posts sp ON sp.id=l.entity_id LEFT JOIN crm.profiles author ON author.id=sp.profile_id WHERE f.profile_id=$1 AND f.status='confirmed' ORDER BY sp.occurred_at DESC,f.media_id OFFSET $2 LIMIT $3) x").bind(profile).bind(q.offset).bind(q.limit+1).fetch_all(&s.pool).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT media_id) FROM crm.faces WHERE profile_id=$1 AND status='confirmed'",
    )
    .bind(profile)
    .fetch_one(&s.pool)
    .await?;
    let next = (rows.len() > q.limit as usize).then_some(q.offset + q.limit);
    rows.truncate(q.limit as usize);
    Ok(Json(json!({"total":total,"items":rows,"next_offset":next})))
}

const PHOTO_BASE: &str = "WITH matched AS MATERIALIZED(SELECT f.* FROM crm.faces f WHERE (f.status='confirmed' AND f.profile_id=$1) OR (f.status='pending' AND f.suggested_profile_id=$1 AND f.rejected_profile_id IS DISTINCT FROM $1 AND f.quality>=0.6)),photos AS(SELECT f.media_id,bool_or(f.status='pending') AS pending,bool_or(f.status='confirmed') AS confirmed,max(f.suggestion_score) AS score,coalesce(max(t.occurred_at),a.capture_at,a.created_at) AS occurred_at,a.width,a.height FROM matched f JOIN crm.media_assets a ON a.id=f.media_id LEFT JOIN crm.media_links l ON l.media_id=f.media_id LEFT JOIN crm.timeline_events t ON t.id=l.entity_id AND l.entity_type IN ('social_post','social_story') LEFT JOIN crm.source_accounts sa ON sa.id=coalesce(t.source_account_id,CASE WHEN l.entity_type='source_account' THEN l.entity_id END,l.source_account_id) WHERE a.media_type LIKE 'image/%' AND NOT coalesce(sa.provider='wechat' AND left(sa.external_id,3)='gh_',false) GROUP BY f.media_id,a.id) ";
async fn photo_counts(pool: &sqlx::PgPool, profile: Uuid) -> Result<Value> {
    Ok(sqlx::query_scalar(&format!("{PHOTO_BASE} SELECT jsonb_build_object('all',count(*),'pending',count(*) FILTER(WHERE pending),'confirmed',count(*) FILTER(WHERE confirmed),'revision',md5(coalesce(string_agg(media_id::text||occurred_at::text||pending::text||confirmed::text||coalesce(score::text,''),',' ORDER BY media_id),''))) FROM photos")).bind(profile).fetch_one(pool).await?)
}
async fn photo_status(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(profile): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let mut counts = photo_counts(&s.pool, profile).await?;
    let revision = counts.as_object_mut().unwrap().remove("revision");
    Ok(Json(json!({"revision":revision,"counts":counts})))
}
#[derive(Deserialize)]
struct PhotoQuery {
    #[serde(default = "all")]
    state: String,
    #[serde(default = "time")]
    sort: String,
    cursor: Option<String>,
    #[serde(default = "page_limit")]
    limit: i64,
    media_id: Option<Uuid>,
}
fn all() -> String {
    "all".into()
}
fn time() -> String {
    "time".into()
}
async fn photos(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(profile): Path<Uuid>,
    Query(q): Query<PhotoQuery>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !["all", "pending", "confirmed"].contains(&q.state.as_str())
        || !["time", "score"].contains(&q.sort.as_str())
        || !(1..=60).contains(&q.limit)
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid photo page",
        ));
    }
    let cursor: Option<Value> = q
        .cursor
        .as_ref()
        .map(|cursor| {
            if cursor.len() > 1024 {
                return Err(ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid photo cursor",
                ));
            }
            let raw = URL_SAFE_NO_PAD
                .decode(cursor.trim_end_matches('='))
                .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid photo cursor"))?;
            let value: Value = serde_json::from_slice(&raw)
                .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid photo cursor"))?;
            if !value
                .get("score")
                .is_some_and(|score| score.is_null() || score.as_f64().is_some_and(f64::is_finite))
                || value["profile"] != profile.to_string()
                || value["state"] != q.state
                || value["sort"] != q.sort
                || value["id"]
                    .as_str()
                    .and_then(|x| Uuid::parse_str(x).ok())
                    .is_none()
                || value["time"]
                    .as_str()
                    .and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok())
                    .is_none()
            {
                return Err(ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid photo cursor",
                ));
            }
            Ok(value)
        })
        .transpose()?;
    let after = cursor
        .as_ref()
        .and_then(|v| v["id"].as_str())
        .and_then(|x| Uuid::parse_str(x).ok());
    let stamp = cursor.as_ref().and_then(|v| v["time"].as_str());
    let score = cursor
        .as_ref()
        .and_then(|v| v["score"].as_f64())
        .map(|score| score as f32);
    let condition = if q.state == "all" { "true" } else { &q.state };
    let order = if q.sort == "time" {
        "occurred_at DESC,media_id DESC"
    } else {
        "score DESC NULLS LAST,media_id DESC"
    };
    let mut rows:Vec<Value>=sqlx::query_scalar(&format!("{PHOTO_BASE} SELECT to_jsonb(p) FROM photos p WHERE {condition} AND ($2::uuid IS NULL OR media_id=$2) AND ($3::uuid IS NULL OR CASE WHEN $6='time' THEN (occurred_at,media_id)<($4::timestamptz,$3) WHEN $5::real IS NULL THEN score IS NULL AND media_id<$3 ELSE score IS NULL OR score<$5 OR (score=$5 AND media_id<$3) END) ORDER BY {order} LIMIT $7")).bind(profile).bind(q.media_id).bind(after).bind(stamp).bind(score).bind(&q.sort).bind(q.limit+1).fetch_all(&s.pool).await?;
    let more = rows.len() > q.limit as usize;
    rows.truncate(q.limit as usize);
    let ids = rows
        .iter()
        .filter_map(|r| r["media_id"].as_str().and_then(|x| Uuid::parse_str(x).ok()))
        .collect::<Vec<_>>();
    let annotations:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',f.id,'media_id',f.media_id,'box',f.box,'status',f.status,'score',f.suggestion_score,'profile_id',p.id,'name',p.display_name) FROM crm.faces f LEFT JOIN crm.profiles p ON p.id=CASE WHEN f.status='confirmed' THEN f.profile_id ELSE f.suggested_profile_id END WHERE f.media_id=ANY($1) AND f.status<>'ignored' AND (f.status='confirmed' OR (f.quality>=0.6 AND (f.suggested_profile_id IS NULL OR f.suggested_profile_id IS DISTINCT FROM f.rejected_profile_id))) ORDER BY f.id").bind(&ids).fetch_all(&s.pool).await?;
    let sources:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('media_id',l.media_id,'entity_type',l.entity_type,'entity_id',l.entity_id,'role',l.role,'provider',coalesce(t.provider,sa.provider,c.provider),'occurred_at',coalesce(t.occurred_at,me.occurred_at),'author',coalesce(p.display_name,sa.display_name),'conversation_id',c.id,'conversation_type',c.conversation_type,'conversation_name',cp.display_name,'resolved',coalesce(t.id,sa.id,p.id,me.message_id) IS NOT NULL,'allowed',NOT coalesce(sa.provider='wechat' AND left(sa.external_id,3)='gh_',false),'href',CASE WHEN t.id IS NOT NULL THEN '/timeline?event='||t.id WHEN l.entity_type='profile' THEN '/profiles/'||l.entity_id WHEN c.id IS NOT NULL THEN '/messages?conversation='||c.id||'&message='||me.message_id ELSE NULL END) FROM crm.media_links l LEFT JOIN crm.timeline_events t ON t.id=l.entity_id AND l.entity_type IN ('social_post','social_story') LEFT JOIN crm.message_search me ON me.message_id=l.entity_id AND l.entity_type='message_event' LEFT JOIN crm.conversations c ON c.id=me.conversation_id LEFT JOIN crm.profiles cp ON cp.id=c.profile_id LEFT JOIN crm.source_accounts sa ON sa.id=coalesce(t.source_account_id,me.sender_account_id,CASE WHEN l.entity_type='source_account' THEN l.entity_id END,l.source_account_id) LEFT JOIN crm.profiles p ON p.id=coalesce(t.profile_id,CASE WHEN l.entity_type='profile' THEN l.entity_id END) WHERE l.media_id=ANY($1) AND NOT coalesce(sa.provider='wechat' AND left(sa.external_id,3)='gh_',false) ORDER BY coalesce(t.occurred_at,me.occurred_at) DESC NULLS LAST,l.entity_id").bind(&ids).fetch_all(&s.pool).await?;
    for row in &mut rows {
        row["faces"] = json!(
            annotations
                .iter()
                .filter(|f| f["media_id"] == row["media_id"])
                .collect::<Vec<_>>()
        );
        row["sources"] = json!(
            sources
                .iter()
                .filter(|f| f["media_id"] == row["media_id"])
                .collect::<Vec<_>>()
        );
    }
    let next = if more {
        rows.last().map(|row|URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"profile":profile,"state":q.state,"sort":q.sort,"id":row["media_id"],"time":row["occurred_at"],"score":row["score"]})).unwrap()))
    } else {
        None
    };
    let mut counts = photo_counts(&s.pool, profile).await?;
    let revision = counts.as_object_mut().unwrap().remove("revision");
    Ok(Json(
        json!({"items":rows,"counts":counts,"revision":revision,"next_cursor":next}),
    ))
}

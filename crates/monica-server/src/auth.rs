use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE};
use fernet::Fernet;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use totp_rs::{Algorithm, Secret, TOTP};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub media_root: std::path::PathBuf,
    pub media_io: Arc<Semaphore>,
    cipher: Fernet,
    passwords: Arc<Semaphore>,
    dummy: String,
    pub origin: String,
    secure: bool,
}
impl AppState {
    pub async fn new(
        pool: PgPool,
        secret: &str,
        origin: String,
        media_root: std::path::PathBuf,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            secret.len() >= 32,
            "MONICA_SESSION_SECRET must contain at least 32 bytes"
        );
        let cipher = Fernet::new(&URL_SAFE.encode(Sha256::digest(secret.as_bytes()))).unwrap();
        let dummy = tokio::task::spawn_blocking(|| hash_password(&token())).await??;
        let secure = origin.starts_with("https://");
        Ok(Self {
            pool,
            media_root,
            media_io: Arc::new(Semaphore::new(if cfg!(target_os = "macos") {
                2
            } else {
                1
            })),
            cipher,
            passwords: Arc::new(Semaphore::new(2)),
            dummy,
            origin,
            secure,
        })
    }
}
#[derive(Debug)]
pub struct ApiError(pub(crate) StatusCode, pub(crate) &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"detail":self.1}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(error=%error,"database operation failed");
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "Database is busy; retry this request",
        )
    }
}
type ApiResult<T> = Result<T, ApiError>;
fn bad(code: StatusCode, msg: &'static str) -> ApiError {
    ApiError(code, msg)
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn hash_password(value: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(value.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|x| x.to_string())
        .map_err(|_| anyhow::anyhow!("password hashing failed"))
}
fn cookie<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (k, v) = part.trim().split_once('=')?;
            (k == key).then_some(v)
        })
}
fn cookie_response(
    s: &AppState,
    key: &str,
    value: &str,
    http_only: bool,
    max_age: u32,
    body: Value,
) -> Response {
    let mut response = Json(body).into_response();
    let text = format!(
        "{key}={value}; Path=/; SameSite=Lax; Max-Age={max_age}{}{}",
        if http_only { "; HttpOnly" } else { "" },
        if s.secure { "; Secure" } else { "" }
    );
    response
        .headers_mut()
        .insert("set-cookie", HeaderValue::from_str(&text).unwrap());
    response
}
pub async fn user(s: &AppState, h: &HeaderMap) -> ApiResult<(Uuid, String)> {
    let t = cookie(h, "crm_session").ok_or(bad(StatusCode::UNAUTHORIZED, "login required"))?;
    sqlx::query_as("SELECT u.id,u.email FROM crm.sessions s JOIN crm.app_users u ON u.id=s.user_id WHERE s.token_hash=$1 AND s.revoked_at IS NULL AND s.expires_at>now()")
  .bind(digest(t)).fetch_optional(&s.pool).await?.ok_or(bad(StatusCode::UNAUTHORIZED,"invalid session"))
}
async fn boundary(State(s): State<AppState>, request: Request, next: Next) -> Response {
    let expected = s.origin.split_once("://").map(|x| x.1).unwrap_or("");
    if request.headers().get("host").and_then(|x| x.to_str().ok()) != Some(expected) {
        return bad(StatusCode::FORBIDDEN, "invalid Host").into_response();
    }
    if let Some(origin) = request.headers().get("origin")
        && origin.to_str().ok() != Some(s.origin.as_str())
    {
        return bad(StatusCode::FORBIDDEN, "invalid Origin").into_response();
    }
    if !["GET", "HEAD", "OPTIONS"].contains(&request.method().as_str())
        && !["/api/v1/auth/bootstrap", "/api/v1/auth/login"].contains(&request.uri().path())
    {
        let c = cookie(request.headers(), "crm_csrf").unwrap_or("");
        let h = request
            .headers()
            .get("x-csrf-token")
            .and_then(|x| x.to_str().ok())
            .unwrap_or("");
        if c.is_empty() || !bool::from(c.as_bytes().ct_eq(h.as_bytes())) {
            return bad(StatusCode::FORBIDDEN, "CSRF token required").into_response();
        }
    }
    next.run(request).await
}
pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/auth/bootstrap", post(bootstrap))
        .route("/api/v1/auth/csrf", get(csrf))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/me", get(me))
        .route("/api/v1/auth/totp/setup", post(totp_setup))
        .route("/api/v1/auth/totp/verify", post(totp_verify))
        .route("/api/v1/auth/totp/status", get(totp_status))
        .route("/api/v1/auth/sessions", get(sessions))
        .route("/api/v1/auth/sessions/{id}/revoke", post(revoke))
        .route("/api/v1/audit/events", get(audit))
        .merge(crate::profiles::routes())
        .merge(crate::media::routes())
        .layer(axum::extract::DefaultBodyLimit::max(65536))
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        .with_state(state)
}
#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
    totp_code: Option<String>,
    recovery_code: Option<String>,
}
fn validate(c: &Credentials) -> ApiResult<()> {
    if !(3..=320).contains(&c.email.len()) || !(12..=512).contains(&c.password.len()) {
        return Err(bad(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid email or password length",
        ));
    }
    Ok(())
}
async fn bootstrap(
    State(s): State<AppState>,
    Json(c): Json<Credentials>,
) -> ApiResult<Json<Value>> {
    validate(&c)?;
    let permit = s
        .passwords
        .clone()
        .try_acquire_owned()
        .map_err(|_| bad(StatusCode::SERVICE_UNAVAILABLE, "password capacity busy"))?;
    let hash = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(&c.password)
    })
    .await
    .map_err(|_| bad(StatusCode::INTERNAL_SERVER_ERROR, "password worker failed"))?
    .map_err(|_| bad(StatusCode::INTERNAL_SERVER_ERROR, "password hashing failed"))?;
    let id = Uuid::new_v4();
    let mut tx = s.pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO crm.app_users(id,email,password_hash) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(&c.email)
    .bind(hash)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted != 1 {
        return Err(bad(StatusCode::CONFLICT, "bootstrap already completed"));
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action) VALUES($1,'auth.bootstrap')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"email":c.email})))
}
async fn csrf(State(s): State<AppState>) -> Response {
    let t = token();
    cookie_response(&s, "crm_csrf", &t, false, 2592000, json!({"csrf_token":t}))
}
async fn login(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(c): Json<Credentials>,
) -> ApiResult<Response> {
    validate(&c)?;
    let key = digest(&c.email.trim().to_lowercase());
    let attempts:i32=sqlx::query_scalar("INSERT INTO crm.auth_login_attempts VALUES($1,1,now()) ON CONFLICT(attempt_key) DO UPDATE SET attempts=CASE WHEN crm.auth_login_attempts.window_started_at<now()-interval '15 minutes' THEN 1 ELSE crm.auth_login_attempts.attempts+1 END,window_started_at=CASE WHEN crm.auth_login_attempts.window_started_at<now()-interval '15 minutes' THEN now() ELSE crm.auth_login_attempts.window_started_at END RETURNING attempts").bind(&key).fetch_one(&s.pool).await?;
    if attempts > 5 {
        return Err(bad(
            StatusCode::TOO_MANY_REQUESTS,
            "too many login attempts; try again later",
        ));
    }
    let row = sqlx::query("SELECT id,password_hash FROM crm.app_users WHERE email=$1")
        .bind(&c.email)
        .fetch_optional(&s.pool)
        .await?;
    let encoded: String = row
        .as_ref()
        .map(|r| r.get("password_hash"))
        .unwrap_or(s.dummy.clone());
    let comparison = encoded.clone();
    let permit = s
        .passwords
        .clone()
        .try_acquire_owned()
        .map_err(|_| bad(StatusCode::SERVICE_UNAVAILABLE, "password capacity busy"))?;
    let valid = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        PasswordHash::new(&comparison).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(c.password.as_bytes(), &hash)
                .is_ok()
        })
    })
    .await
    .map_err(|_| bad(StatusCode::INTERNAL_SERVER_ERROR, "password worker failed"))?;
    if !valid || row.is_none() {
        return Err(bad(StatusCode::UNAUTHORIZED, "invalid credentials"));
    }
    let id: Uuid = row.unwrap().get("id");
    let mut tx = s.pool.begin().await?;
    let current =
        sqlx::query("SELECT password_hash,totp_secret FROM crm.app_users WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if current.get::<String, _>("password_hash") != encoded {
        return Err(bad(StatusCode::UNAUTHORIZED, "credentials changed"));
    }
    if let Some(encrypted) = current.get::<Option<String>, _>("totp_secret") {
        let totp = decrypt_totp(&s, &encrypted, &c.email)?;
        let valid = c
            .totp_code
            .as_ref()
            .is_some_and(|code| totp.check_current(code).unwrap_or(false));
        if !valid {
            let code = c.recovery_code.as_deref().unwrap_or("").trim();
            let n=sqlx::query("UPDATE crm.auth_recovery_codes SET consumed_at=now() WHERE user_id=$1 AND code_hash=$2 AND consumed_at IS NULL").bind(id).bind(digest(code)).execute(&mut *tx).await?.rows_affected();
            if code.is_empty() || n != 1 {
                return Err(bad(StatusCode::UNAUTHORIZED, "TOTP code required"));
            }
        }
    }
    let t = token();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at,user_agent) VALUES($1,$2,$3,now()+interval '30 days',$4)")
 .bind(Uuid::new_v4()).bind(id).bind(digest(&t)).bind(h.get("user-agent").and_then(|x|x.to_str().ok())).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.app_users SET last_login_at=now() WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM crm.auth_login_attempts WHERE attempt_key=$1")
        .bind(key)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action) VALUES($1,'auth.login')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(cookie_response(
        &s,
        "crm_session",
        &t,
        true,
        2592000,
        json!({"status":"authenticated"}),
    ))
}
async fn me(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let (id, email) = user(&s, &h).await?;
    Ok(Json(json!({"id":id,"email":email})))
}
async fn logout(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Response> {
    if let Some(t) = cookie(&h, "crm_session") {
        sqlx::query("WITH revoked AS (UPDATE crm.sessions SET revoked_at=now() WHERE token_hash=$1 AND revoked_at IS NULL RETURNING user_id) INSERT INTO crm.audit_events(user_id,action) SELECT user_id,'auth.logout' FROM revoked").bind(digest(t)).execute(&s.pool).await?;
    }
    Ok(cookie_response(
        &s,
        "crm_session",
        "",
        true,
        0,
        json!({"status":"logged_out"}),
    ))
}
fn totp(secret: Vec<u8>, email: &str) -> ApiResult<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret,
        Some("Monica".into()),
        email.into(),
    )
    .map_err(|_| {
        bad(
            StatusCode::INTERNAL_SERVER_ERROR,
            "TOTP configuration invalid",
        )
    })
}
fn decrypt_totp(s: &AppState, encrypted: &str, email: &str) -> ApiResult<TOTP> {
    totp(
        s.cipher.decrypt(encrypted).map_err(|_| {
            bad(
                StatusCode::INTERNAL_SERVER_ERROR,
                "TOTP configuration invalid",
            )
        })?,
        email,
    )
}
async fn totp_setup(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let (id, email) = user(&s, &h).await?;
    let secret = Secret::generate_secret().to_bytes().map_err(|_| {
        bad(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secret generation failed",
        )
    })?;
    let t = totp(secret.clone(), &email)?;
    let n = sqlx::query(
        "UPDATE crm.app_users SET pending_totp_secret=$2 WHERE id=$1 AND totp_secret IS NULL",
    )
    .bind(id)
    .bind(s.cipher.encrypt(&secret))
    .execute(&s.pool)
    .await?
    .rows_affected();
    if n != 1 {
        return Err(bad(StatusCode::CONFLICT, "TOTP is already enabled"));
    }
    Ok(Json(
        json!({"secret":t.get_secret_base32(),"otpauth_url":t.get_url()}),
    ))
}
#[derive(Deserialize)]
struct Code {
    code: String,
}
async fn totp_verify(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(c): Json<Code>,
) -> ApiResult<Json<Value>> {
    let (id, email) = user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    let encrypted: Option<String> =
        sqlx::query_scalar("SELECT pending_totp_secret FROM crm.app_users WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let encrypted = encrypted.ok_or(bad(StatusCode::CONFLICT, "TOTP is not configured"))?;
    if !decrypt_totp(&s, &encrypted, &email)?
        .check_current(&c.code)
        .unwrap_or(false)
    {
        return Err(bad(StatusCode::UNPROCESSABLE_ENTITY, "invalid TOTP code"));
    }
    sqlx::query("UPDATE crm.app_users SET totp_secret=pending_totp_secret,pending_totp_secret=NULL WHERE id=$1").bind(id).execute(&mut *tx).await?;
    let codes: Vec<String> = (0..8).map(|_| token()[..16].to_owned()).collect();
    for code in &codes {
        sqlx::query("INSERT INTO crm.auth_recovery_codes(user_id,code_hash) VALUES($1,$2)")
            .bind(id)
            .bind(digest(code))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"status":"verified","recovery_codes":codes})))
}
async fn totp_status(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let (id, _) = user(&s, &h).await?;
    let enabled: bool =
        sqlx::query_scalar("SELECT totp_secret IS NOT NULL FROM crm.app_users WHERE id=$1")
            .bind(id)
            .fetch_one(&s.pool)
            .await?;
    Ok(Json(json!({"enabled":enabled})))
}
async fn sessions(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let (id, _) = user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'created_at',created_at,'expires_at',expires_at,'revoked_at',revoked_at,'user_agent',user_agent,'ip_address',ip_address,'current',token_hash=$2) FROM crm.sessions WHERE user_id=$1 ORDER BY created_at DESC")
 .bind(id).bind(digest(cookie(&h,"crm_session").unwrap_or(""))).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}
async fn revoke(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(session): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (id, _) = user(&s, &h).await?;
    let n=sqlx::query("WITH revoked AS (UPDATE crm.sessions SET revoked_at=now() WHERE id=$1 AND user_id=$2 AND revoked_at IS NULL RETURNING user_id,id) INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) SELECT user_id,'auth.session.revoked','session',id FROM revoked")
 .bind(session).bind(id).execute(&s.pool).await?.rows_affected();
    if n == 0 {
        return Err(bad(StatusCode::NOT_FOUND, "session not found"));
    }
    Ok(Json(json!({"status":"revoked"})))
}
#[derive(Deserialize)]
struct Limit {
    limit: Option<i64>,
}
async fn audit(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Limit>,
) -> ApiResult<Json<Value>> {
    let (id, _) = user(&s, &h).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(e)-'user_id' FROM crm.audit_events e WHERE user_id=$1 ORDER BY created_at DESC,id DESC LIMIT $2").bind(id).bind(q.limit.unwrap_or(100).clamp(1,200)).fetch_all(&s.pool).await?;
    Ok(Json(json!(rows)))
}

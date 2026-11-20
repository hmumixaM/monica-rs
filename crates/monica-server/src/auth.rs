use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{
        SaltString,
        rand_core::{OsRng, RngCore},
    },
};
use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, FromRequest, Path, Query, Request, State},
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
use std::net::SocketAddr;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use totp_rs::{Algorithm, Secret, TOTP};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub native_sources_available: bool,
    pub cdc_enabled: bool,
    pub media_root: std::path::PathBuf,
    pub legacy_archive_root: Option<std::path::PathBuf>,
    pub legacy_migration_root: Option<std::path::PathBuf>,
    pub resources: monica_core::resources::ResourceBudget,
    pub read_cache: crate::cache::ReadCache,
    pub import_hmac_secret: Option<String>,
    pub credentials: monica_core::credentials::CredentialCipher,
    cipher: Fernet,
    passwords: Arc<Semaphore>,
    dummy: String,
    pub origin: String,
    session_cookie: String,
    csrf_cookie: String,
    secure: bool,
    delegated_actor: Option<(Uuid, String)>,
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
            native_sources_available: cfg!(target_os = "macos")
                && std::env::var_os("MONICA_NATIVE_ROOT").is_some(),
            cdc_enabled: monica_core::cdc::enabled()?,
            pool,
            media_root,
            legacy_archive_root: None,
            legacy_migration_root: None,
            import_hmac_secret: None,
            credentials: monica_core::credentials::CredentialCipher::new(secret)?,
            resources: monica_core::resources::ResourceBudget::new(Default::default())?,
            read_cache: crate::cache::ReadCache::default(),
            cipher,
            passwords: Arc::new(Semaphore::new(2)),
            dummy,
            origin,
            session_cookie: "crm_session".into(),
            csrf_cookie: "crm_csrf".into(),
            secure,
            delegated_actor: None,
        })
    }

    pub fn configure_cookie_prefix(&mut self, prefix: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=32).contains(&prefix.len())
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "cookie prefix must contain 1–32 lowercase ASCII letters, digits or underscores"
        );
        self.session_cookie = format!("{prefix}_session");
        self.csrf_cookie = format!("{prefix}_csrf");
        Ok(())
    }

    pub(crate) fn session_token<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        cookie(headers, &self.session_cookie)
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
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
    if let Some(actor) = &s.delegated_actor {
        return Ok(actor.clone());
    }
    let t = s
        .session_token(h)
        .ok_or(bad(StatusCode::UNAUTHORIZED, "login required"))?;
    sqlx::query_as("SELECT u.id,u.email FROM crm.sessions s JOIN crm.app_users u ON u.id=s.user_id WHERE s.token_hash=$1 AND s.revoked_at IS NULL AND s.expires_at>now()")
  .bind(digest(t)).fetch_optional(&s.pool).await?.ok_or(bad(StatusCode::UNAUTHORIZED,"invalid session"))
}
async fn boundary(State(s): State<AppState>, request: Request, next: Next) -> Response {
    if s.delegated_actor.is_some() {
        return next.run(request).await;
    }
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
        && ![
            "/api/v1/auth/bootstrap",
            "/api/v1/auth/login",
            "/api/v1/imports/batches",
        ]
        .contains(&request.uri().path())
        && !request.uri().path().starts_with("/api/v1/faces/device/")
    {
        let c = cookie(request.headers(), &s.csrf_cookie).unwrap_or("");
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
        .route("/api/v1/auth/password", post(change_password))
        .merge(crate::mcp::settings_routes())
        .merge(crate::import_status::routes())
        .merge(crate::legacy::routes())
        .merge(crate::profiles::routes())
        .merge(crate::media::routes())
        .merge(crate::imports::routes())
        .merge(crate::wechat_images::routes())
        .merge(crate::linking::routes())
        .merge(crate::messages::routes())
        .merge(crate::timeline::routes())
        .merge(crate::faces::routes())
        .merge(crate::pages::routes(state.clone()))
        .merge(crate::ai::routes())
        .merge(crate::collector::routes())
        .merge(crate::connections::routes())
        .merge(crate::cdc::routes())
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
fn string_errors(
    body: &Value,
    key: &str,
    required: bool,
    min: usize,
    max: usize,
    errors: &mut Vec<Value>,
) {
    let location = json!(["body", key]);
    let Some(value) = body.get(key) else {
        if required {
            errors
                .push(json!({"type":"missing","loc":location,"msg":"Field required","input":body}));
        }
        return;
    };
    if !required && value.is_null() {
        return;
    }
    let Some(value_string) = value.as_str() else {
        errors.push(json!({"type":"string_type","loc":location,"msg":"Input should be a valid string","input":value}));
        return;
    };
    let len = value_string.chars().count();
    if len < min {
        errors.push(json!({"type":"string_too_short","loc":location,"msg":format!("String should have at least {min} characters"),"input":value,"ctx":{"min_length":min}}));
    } else if len > max {
        errors.push(json!({"type":"string_too_long","loc":location,"msg":format!("String should have at most {max} characters"),"input":value,"ctx":{"max_length":max}}));
    }
}
fn invalid_input(errors: Vec<Value>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"detail":errors})),
    )
        .into_response()
}
fn object_input(body: &Value) -> Result<(), Vec<Value>> {
    if body.is_object() {
        return Ok(());
    }
    Err(vec![
        json!({"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":body}),
    ])
}
impl FromRequest<AppState> for Credentials {
    type Rejection = Response;
    async fn from_request(request: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        let login = request.uri().path().ends_with("/login");
        let Json(mut body) = Json::<Value>::from_request(request, state)
            .await
            .map_err(IntoResponse::into_response)?;
        object_input(&body).map_err(invalid_input)?;
        let mut errors = Vec::new();
        string_errors(&body, "email", true, 3, 320, &mut errors);
        string_errors(&body, "password", true, 12, 512, &mut errors);
        if login {
            string_errors(&body, "totp_code", false, 6, 8, &mut errors);
            string_errors(&body, "recovery_code", false, 0, usize::MAX, &mut errors);
        } else {
            body.as_object_mut().unwrap().remove("totp_code");
            body.as_object_mut().unwrap().remove("recovery_code");
        }
        if !errors.is_empty() {
            return Err(invalid_input(errors));
        }
        serde_json::from_value(body).map_err(|_| StatusCode::BAD_REQUEST.into_response())
    }
}
async fn bootstrap(State(s): State<AppState>, c: Credentials) -> ApiResult<Json<Value>> {
    let email = c.email.trim().to_lowercase();
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
    .bind(&email)
    .bind(hash)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted != 1 {
        return Err(bad(StatusCode::CONFLICT, "bootstrap already completed"));
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,metadata) VALUES($1,'auth.bootstrap',jsonb_build_object('email',$2::text))")
        .bind(id)
        .bind(&email)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"email":c.email})))
}
async fn csrf(State(s): State<AppState>, h: HeaderMap) -> Response {
    let t = cookie(&h, &s.csrf_cookie)
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .unwrap_or_else(token);
    cookie_response(
        &s,
        &s.csrf_cookie,
        &t,
        false,
        1209600,
        json!({"csrf_token":t}),
    )
}
async fn login(
    State(s): State<AppState>,
    h: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    c: Credentials,
) -> ApiResult<Response> {
    let ip = peer.map(|Extension(ConnectInfo(address))| address.ip().to_string());
    let key = digest(&format!(
        "{}|{}",
        c.email.trim().to_lowercase(),
        ip.as_deref().unwrap_or("unknown")
    ));
    let blocked:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.auth_login_attempts WHERE attempt_key=$1 AND blocked_until>now())").bind(&key).fetch_one(&s.pool).await?;
    if blocked {
        return Err(bad(
            StatusCode::TOO_MANY_REQUESTS,
            "too many login attempts; try again later",
        ));
    }
    let row =
        sqlx::query("SELECT id,password_hash FROM crm.app_users WHERE lower(btrim(email))=$1")
            .bind(c.email.trim().to_lowercase())
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
        login_failure(&s, &key, ip.as_deref()).await?;
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
        let totp = decrypt_totp(&s, &encrypted, &c.email)
            .map_err(|_| bad(StatusCode::UNAUTHORIZED, "TOTP configuration invalid"))?;
        let valid = c
            .totp_code
            .as_ref()
            .is_some_and(|code| totp.check_current(code).unwrap_or(false));
        if !valid {
            let code = c.recovery_code.as_deref().unwrap_or("").trim();
            let n=sqlx::query("UPDATE crm.auth_recovery_codes SET consumed_at=now() WHERE user_id=$1 AND code_hash=$2 AND consumed_at IS NULL").bind(id).bind(digest(code)).execute(&mut *tx).await?.rows_affected();
            if code.is_empty() || n != 1 {
                tx.rollback().await?;
                login_failure(&s, &key, ip.as_deref()).await?;
                return Err(bad(StatusCode::UNAUTHORIZED, "TOTP code required"));
            }
        }
    }
    let t = token();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at,user_agent,ip_address) VALUES($1,$2,$3,now()+interval '14 days',$4,$5)")
 .bind(Uuid::new_v4()).bind(id).bind(digest(&t)).bind(h.get("user-agent").and_then(|x|x.to_str().ok())).bind(&ip).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.app_users SET last_login_at=now() WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM crm.auth_login_attempts WHERE attempt_key=$1")
        .bind(key)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,metadata) VALUES($1,'auth.login',jsonb_build_object('ip',$2::text))")
        .bind(id)
        .bind(ip)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(cookie_response(
        &s,
        &s.session_cookie,
        &t,
        true,
        1209600,
        json!({"status":"authenticated"}),
    ))
}
async fn login_failure(s: &AppState, key: &str, ip: Option<&str>) -> ApiResult<()> {
    // Count actual failures atomically. Password hashing has already finished,
    // so this transaction never holds a database lock during expensive work.
    sqlx::query("WITH failure AS (INSERT INTO crm.auth_login_attempts(attempt_key,attempts,window_started_at,blocked_until) VALUES($1,1,now(),NULL) ON CONFLICT(attempt_key) DO UPDATE SET attempts=CASE WHEN crm.auth_login_attempts.window_started_at<now()-interval '15 minutes' THEN 1 ELSE crm.auth_login_attempts.attempts+1 END, window_started_at=CASE WHEN crm.auth_login_attempts.window_started_at<now()-interval '15 minutes' THEN now() ELSE crm.auth_login_attempts.window_started_at END, blocked_until=CASE WHEN crm.auth_login_attempts.window_started_at>=now()-interval '15 minutes' AND crm.auth_login_attempts.attempts+1>=5 THEN now()+interval '15 minutes' ELSE NULL END RETURNING attempts) INSERT INTO crm.audit_events(action,metadata) SELECT 'auth.login.failed',jsonb_build_object('ip',$2::text,'attempts',attempts) FROM failure")
        .bind(key).bind(ip).execute(&s.pool).await?;
    Ok(())
}
async fn me(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let (id, email) = user(&s, &h).await?;
    Ok(Json(json!({"id":id,"email":email})))
}
async fn logout(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Response> {
    if let Some(t) = s.session_token(&h) {
        sqlx::query("WITH revoked AS (UPDATE crm.sessions SET revoked_at=now() WHERE token_hash=$1 AND revoked_at IS NULL RETURNING user_id) INSERT INTO crm.audit_events(user_id,action) SELECT user_id,'auth.logout' FROM revoked").bind(digest(t)).execute(&s.pool).await?;
    }
    Ok(cookie_response(
        &s,
        &s.session_cookie,
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
        Some("Monica Next CRM".into()),
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
impl FromRequest<AppState> for Code {
    type Rejection = Response;
    async fn from_request(request: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        let Json(body) = Json::<Value>::from_request(request, state)
            .await
            .map_err(IntoResponse::into_response)?;
        object_input(&body).map_err(invalid_input)?;
        let mut errors = Vec::new();
        string_errors(&body, "code", true, 6, 8, &mut errors);
        if !errors.is_empty() {
            return Err(invalid_input(errors));
        }
        serde_json::from_value(body).map_err(|_| StatusCode::BAD_REQUEST.into_response())
    }
}
async fn totp_verify(State(s): State<AppState>, h: HeaderMap, c: Code) -> ApiResult<Json<Value>> {
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
 .bind(id).bind(digest(s.session_token(&h).unwrap_or(""))).fetch_all(&s.pool).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt;

    #[sqlx::test(migrations = "../../migrations")]
    async fn email_case_and_whitespace_remain_compatible_with_password_recovery(
        pool: sqlx::PgPool,
    ) {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(
            pool.clone(),
            "synthetic-email-recovery-secret-at-least32",
            "http://testserver".into(),
            root.path().into(),
        )
        .await
        .unwrap();
        let credentials = |email: &str, password: &str| Credentials {
            email: email.into(),
            password: password.into(),
            totp_code: None,
            recovery_code: None,
        };
        let _ = bootstrap(
            State(state.clone()),
            credentials(" Alice@Example.com ", "synthetic-original-password"),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT email FROM crm.app_users")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "alice@example.com"
        );
        // Compatibility also covers records created before normalization.
        sqlx::query("UPDATE crm.app_users SET email='Alice@Example.com'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            login(
                State(state.clone()),
                HeaderMap::new(),
                None,
                credentials(" ALICE@EXAMPLE.COM ", "synthetic-original-password")
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        let _ = reset_password(
            &state,
            ResetPassword {
                email: " alice@example.com ".into(),
                password: "synthetic-replacement-password".into(),
                reset_totp: false,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            login(
                State(state),
                HeaderMap::new(),
                None,
                credentials("Alice@Example.com", "synthetic-replacement-password")
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn real_python_auth_validation_contract() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../docs/contracts/auth-validation.json"))
                .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://synthetic:synthetic@127.0.0.1:1/no_database")
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(
            pool,
            "synthetic-test-secret-at-least-32bytes",
            "http://testserver".into(),
            root.path().to_path_buf(),
        )
        .await
        .unwrap();
        let app = routes(state);
        for case in fixture["fixtures"].as_array().unwrap() {
            let request = &case["request"];
            let mut builder = Request::builder()
                .method(request["method"].as_str().unwrap())
                .uri(request["path"].as_str().unwrap())
                .header("host", "testserver")
                .header("content-type", "application/json");
            for (key, value) in request["headers"].as_object().unwrap() {
                builder = builder.header(key, value.as_str().unwrap());
            }
            let response = app
                .clone()
                .oneshot(
                    builder
                        .body(Body::from(serde_json::to_vec(&request["json"]).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status().as_u16(),
                case["response"]["status"].as_u64().unwrap() as u16,
                "request: {request}"
            );
            let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
            let actual: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(actual, case["response"]["json"], "request: {request}");
        }
    }
}

// Only the MCP dispatcher constructs this scoped in-process router. No request
// header can select an actor. The dispatcher exposes an explicit tool allowlist.
pub(crate) fn delegated_routes(mut state: AppState, actor: (Uuid, String)) -> Router {
    state.delegated_actor = Some(actor);
    routes(state)
}
#[derive(Deserialize)]
pub(crate) struct ResetPassword {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub reset_totp: bool,
}
pub(crate) async fn reset_password(
    s: &AppState,
    body: ResetPassword,
    expected_hash: Option<String>,
) -> ApiResult<Json<Value>> {
    if !(12..=512).contains(&body.password.chars().count()) {
        return Err(bad(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Password must contain 12–512 characters",
        ));
    }
    let _permit = s
        .passwords
        .clone()
        .try_acquire_owned()
        .map_err(|_| bad(StatusCode::SERVICE_UNAVAILABLE, "password capacity busy"))?;
    let encoded = tokio::task::spawn_blocking(move || hash_password(&body.password))
        .await
        .map_err(|_| bad(StatusCode::INTERNAL_SERVER_ERROR, "password worker failed"))?
        .map_err(|_| bad(StatusCode::INTERNAL_SERVER_ERROR, "password hashing failed"))?;
    let mut tx = s.pool.begin().await?;
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id,password_hash FROM crm.app_users WHERE lower(btrim(email))=$1 FOR UPDATE",
    )
    .bind(body.email.trim().to_lowercase())
    .fetch_optional(&mut *tx)
    .await?;
    let (id, current) = row.ok_or(bad(
        StatusCode::NOT_FOUND,
        "Account not found; create a first-use account",
    ))?;
    if expected_hash.is_some_and(|old| old != current) {
        return Err(bad(StatusCode::CONFLICT, "Password changed; retry"));
    }
    sqlx::query("UPDATE crm.app_users SET password_hash=$2,totp_secret=CASE WHEN $3 THEN NULL ELSE totp_secret END,pending_totp_secret=CASE WHEN $3 THEN NULL ELSE pending_totp_secret END WHERE id=$1")
        .bind(id).bind(encoded).bind(body.reset_totp).execute(&mut *tx).await?;
    if body.reset_totp {
        sqlx::query("DELETE FROM crm.auth_recovery_codes WHERE user_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE crm.sessions SET revoked_at=now() WHERE user_id=$1 AND revoked_at IS NULL")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM crm.auth_login_attempts")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,metadata) VALUES($1,'auth.password_reset',jsonb_build_object('reset_totp',$2::boolean))")
        .bind(id).bind(body.reset_totp).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"status":"password_reset"})))
}
#[derive(Deserialize)]
struct PasswordChange {
    current_password: String,
    password: String,
}
async fn change_password(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(body): Json<PasswordChange>,
) -> ApiResult<Json<Value>> {
    let (id, email) = user(&s, &h).await?;
    let encoded: String = sqlx::query_scalar("SELECT password_hash FROM crm.app_users WHERE id=$1")
        .bind(id)
        .fetch_one(&s.pool)
        .await?;
    let check = encoded.clone();
    let _permit = s
        .passwords
        .clone()
        .try_acquire_owned()
        .map_err(|_| bad(StatusCode::SERVICE_UNAVAILABLE, "password capacity busy"))?;
    let valid = tokio::task::spawn_blocking(move || {
        PasswordHash::new(&check).ok().is_some_and(|hash| {
            Argon2::default()
                .verify_password(body.current_password.as_bytes(), &hash)
                .is_ok()
        })
    })
    .await
    .map_err(|_| bad(StatusCode::INTERNAL_SERVER_ERROR, "password worker failed"))?;
    drop(_permit);
    if !valid {
        return Err(bad(
            StatusCode::UNAUTHORIZED,
            "Current password is incorrect",
        ));
    }
    reset_password(
        &s,
        ResetPassword {
            email,
            password: body.password,
            reset_totp: false,
        },
        Some(encoded),
    )
    .await
}

//! Authenticated MCP transport and an allowlisted in-process adapter to the
//! existing business handlers. No cookie/session is created for an AI call.
use crate::auth::{self, ApiError, AppState, user};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use rmcp::{
    ServerHandler,
    model::*,
    service::{RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
type Result<T> = std::result::Result<T, ApiError>;
fn invalid() -> ApiError {
    ApiError(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Invalid MCP settings or key",
    )
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub fn settings_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/mcp/settings", get(settings).patch(save))
        .route("/api/v1/mcp/keys", get(keys).post(create_key))
        .route("/api/v1/mcp/keys/{id}/revoke", post(revoke))
        .route("/api/v1/mcp/keys/{id}/rotate", post(rotate))
}
async fn settings(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(
        sqlx::query_scalar("SELECT to_jsonb(s) FROM crm.mcp_settings s")
            .fetch_one(&s.pool)
            .await?,
    ))
}
#[derive(Deserialize)]
struct Settings {
    enabled: bool,
    public_url: Option<String>,
}
async fn save(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(v): Json<Settings>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let url = v.public_url.filter(|v| !v.trim().is_empty());
    if let Some(url) = &url {
        let uri = url.parse::<axum::http::Uri>().map_err(|_| invalid())?;
        if uri.scheme_str() != Some("https")
            || uri.authority().is_none_or(|a| a.as_str().contains('@'))
            || uri.path() != "/mcp"
            || uri.query().is_some()
        {
            return Err(invalid());
        }
    }
    let mut tx = s.pool.begin().await?;
    let value = sqlx::query_scalar(
        "UPDATE crm.mcp_settings SET enabled=$1,public_url=$2 RETURNING to_jsonb(mcp_settings)",
    )
    .bind(v.enabled)
    .bind(url)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action) VALUES($1,'mcp.settings')")
        .bind(actor)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(value))
}
async fn keys(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    Ok(Json(json!(sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('id',id,'name',name,'created_at',created_at,'last_used_at',last_used_at,'revoked_at',revoked_at) FROM crm.mcp_keys WHERE user_id=$1 ORDER BY created_at DESC").bind(actor).fetch_all(&s.pool).await?)))
}
#[derive(Deserialize)]
struct Key {
    name: String,
    key: Option<String>,
}
async fn create_key(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(v): Json<Key>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let key = v.key.filter(|v| !v.is_empty()).unwrap_or_else(|| {
        format!(
            "monica_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        )
    });
    if v.name.trim().is_empty()
        || v.name.chars().count() > 100
        || !(32..=512).contains(&key.len())
        || !key.is_ascii()
        || key
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(invalid());
    }
    let id = Uuid::new_v4();
    let mut tx = s.pool.begin().await?;
    sqlx::query("INSERT INTO crm.mcp_keys(id,user_id,name,token_hash) VALUES($1,$2,$3,$4) ON CONFLICT(token_hash) DO NOTHING").bind(id).bind(actor).bind(v.name.trim()).bind(hash(&key)).execute(&mut *tx).await?.rows_affected().eq(&1).then_some(()).ok_or(ApiError(StatusCode::CONFLICT,"Key already exists; choose another key"))?;
    sqlx::query(
        "INSERT INTO crm.audit_events(user_id,action,entity_id) VALUES($1,'mcp.key_created',$2)",
    )
    .bind(actor)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"key":key})))
}
async fn revoke(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    sqlx::query(
        "UPDATE crm.mcp_keys SET revoked_at=coalesce(revoked_at,now()) WHERE id=$1 AND user_id=$2",
    )
    .bind(id)
    .bind(actor)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO crm.audit_events(user_id,action,entity_id) VALUES($1,'mcp.key_revoked',$2)",
    )
    .bind(actor)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"revoked":true})))
}
async fn rotate(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let key = format!(
        "monica_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    let mut tx = s.pool.begin().await?;
    let n=sqlx::query("UPDATE crm.mcp_keys SET token_hash=$3,last_used_at=NULL WHERE id=$1 AND user_id=$2 AND revoked_at IS NULL").bind(id).bind(actor).bind(hash(&key)).execute(&mut *tx).await?.rows_affected();
    if n == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "Active key not found"));
    }
    sqlx::query(
        "INSERT INTO crm.audit_events(user_id,action,entity_id) VALUES($1,'mcp.key_rotated',$2)",
    )
    .bind(actor)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"key":key})))
}
pub fn routes(s: AppState) -> Router {
    Router::new()
        .route("/mcp", any(transport))
        .layer(axum::extract::DefaultBodyLimit::max(65536))
        .with_state(s)
}
async fn transport(State(s): State<AppState>, req: Request) -> Response {
    match authorized_transport(s, req).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}
async fn authorized_transport(s: AppState, req: Request) -> Result<Response> {
    let credential = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    let actor:Option<(Uuid,String)>=sqlx::query_as("SELECT u.id,u.email FROM crm.mcp_keys k JOIN crm.app_users u ON u.id=k.user_id CROSS JOIN crm.mcp_settings s WHERE s.enabled AND k.revoked_at IS NULL AND k.token_hash=$1").bind(hash(credential)).fetch_optional(&s.pool).await?;
    let actor = actor.ok_or(ApiError(
        StatusCode::UNAUTHORIZED,
        "MCP access key required or MCP disabled",
    ))?;
    let public: Option<String> = sqlx::query_scalar("SELECT public_url FROM crm.mcp_settings")
        .fetch_one(&s.pool)
        .await?;
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
    config.json_response = true;
    config.max_request_body_bytes = 65536;
    for url in std::iter::once(&s.origin).chain(public.iter()) {
        if let Ok(uri) = url.parse::<axum::http::Uri>() {
            if let Some(authority) = uri.authority() {
                config.allowed_hosts.push(authority.to_string());
            }
            if let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) {
                config
                    .allowed_origins
                    .push(format!("{scheme}://{authority}"));
            }
        }
    }
    // The transport validates Host/Origin before a tool is dispatched. Every
    // HTTP request checks the current key, including after key revocation.
    sqlx::query("UPDATE crm.mcp_keys SET last_used_at=now() WHERE token_hash=$1 AND (last_used_at IS NULL OR last_used_at<now()-interval '1 minute')").bind(hash(credential)).execute(&s.pool).await?;
    let service = StreamableHttpService::new(
        move || {
            Ok(Monica {
                router: auth::delegated_routes(s.clone(), actor.clone()),
            })
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Ok(service
        .oneshot(req)
        .await
        .unwrap_or_else(|e| match e {})
        .into_response())
}
#[derive(Clone)]
struct Monica {
    router: Router,
}
struct Spec {
    name: &'static str,
    description: &'static str,
    method: &'static str,
    path: &'static str,
    query: &'static [&'static str],
    body: Value,
}
fn specs() -> Vec<Spec> {
    vec![
        Spec {
            name: "get_profiles",
            description: "搜索人物。使用 cursor 继续翻页。",
            method: "GET",
            path: "/api/v1/profiles",
            query: &["q", "cursor", "limit"],
            body: Value::Null,
        },
        Spec {
            name: "read_profile",
            description: "读取人物完整资料、关联账号和字段。",
            method: "GET",
            path: "/api/v1/profiles/{id}",
            query: &[],
            body: Value::Null,
        },
        Spec {
            name: "get_memory",
            description: "读取人物 Memory。",
            method: "GET",
            path: "/api/v1/ai/profiles/{id}",
            query: &[],
            body: Value::Null,
        },
        Spec {
            name: "get_relationships",
            description: "读取人物关系。",
            method: "GET",
            path: "/api/v1/profiles/{id}/relationships",
            query: &[],
            body: Value::Null,
        },
        Spec {
            name: "search_messages",
            description: "查询人物消息及其本人群聊发言；before 和 before_id 翻页。",
            method: "GET",
            path: "/api/v1/profiles/{id}/messages",
            query: &["q", "before", "before_id", "limit", "include_group_speech"],
            body: Value::Null,
        },
        Spec {
            name: "search_evidence",
            description: "搜索证据，返回稳定 ID、版本和来源；before 和 before_id 翻页。",
            method: "GET",
            path: "/api/v1/evidence",
            query: &[
                "q",
                "profile_id",
                "provider",
                "before",
                "before_id",
                "limit",
            ],
            body: Value::Null,
        },
        Spec {
            name: "read_evidence",
            description: "读取证据原文及版本。长正文用 body_offset 继续。",
            method: "GET",
            path: "/api/v1/evidence/{id}",
            query: &["revision", "body_offset", "body_limit", "original"],
            body: Value::Null,
        },
        Spec {
            name: "get_timeline",
            description: "查询人物或全局时间线，before 翻页。",
            method: "GET",
            path: "/api/v1/timeline",
            query: &["profile_id", "provider", "before", "before_id", "limit"],
            body: Value::Null,
        },
        Spec {
            name: "get_coverage",
            description: "读取人物的来源覆盖。",
            method: "GET",
            path: "/api/v1/profiles/{id}/source-coverage",
            query: &[],
            body: Value::Null,
        },
        Spec {
            name: "get_import_status",
            description: "读取来源和导入状态。",
            method: "GET",
            path: "/api/v1/imports/status",
            query: &[],
            body: Value::Null,
        },
        Spec {
            name: "get_import_runs",
            description: "读取分页导入运行记录。",
            method: "GET",
            path: "/api/v1/imports/runs",
            query: &["provider", "connection_id", "before", "limit"],
            body: Value::Null,
        },
        Spec {
            name: "get_import_daily",
            description: "读取每日进度，timezone 为 IANA 时区。",
            method: "GET",
            path: "/api/v1/imports/daily",
            query: &["provider", "connection_id", "days", "timezone"],
            body: Value::Null,
        },
        Spec {
            name: "create_profile",
            description: "创建人物或群组。",
            method: "POST",
            path: "/api/v1/profiles",
            query: &[],
            body: json!({"type":"object","properties":{"display_name":{"type":"string"},"profile_type":{"type":"string","enum":["person","group"]},"summary":{"type":"string"}},"required":["display_name"],"additionalProperties":false}),
        },
        Spec {
            name: "edit_field",
            description: "编辑人物手工字段。笔记使用 field_key=notes，内容放入 value。",
            method: "PATCH",
            path: "/api/v1/profiles/{id}/fields",
            query: &[],
            body: json!({"type":"object","properties":{"field_key":{"type":"string"},"value":{}},"required":["field_key","value"],"additionalProperties":false}),
        },
        Spec {
            name: "list_tags",
            description: "读取标签。",
            method: "GET",
            path: "/api/v1/tags",
            query: &[],
            body: Value::Null,
        },
        Spec {
            name: "create_tag",
            description: "创建标签。",
            method: "POST",
            path: "/api/v1/tags",
            query: &[],
            body: json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false}),
        },
        Spec {
            name: "attach_tag",
            description: "给人物添加标签。",
            method: "POST",
            path: "/api/v1/profiles/{id}/tags/{tag}",
            query: &[],
            body: Value::Null,
        },
        Spec {
            name: "detach_tag",
            description: "移除人物标签。",
            method: "DELETE",
            path: "/api/v1/profiles/{id}/tags/{tag}",
            query: &[],
            body: Value::Null,
        },
        Spec {
            name: "link_account",
            description: "确认或解除来源账号的人物关联。profile_id=null 解除关联。",
            method: "PUT",
            path: "/api/v1/source-accounts/{id}/link",
            query: &[],
            body: json!({"type":"object","properties":{"profile_id":{"type":["string","null"],"format":"uuid"},"expected_profile_id":{"type":["string","null"],"format":"uuid"}},"required":["profile_id"],"additionalProperties":false}),
        },
    ]
}
impl Spec {
    fn tool(&self) -> Tool {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for key in ["id", "tag"] {
            if self.path.contains(&format!("{{{key}}}")) {
                props.insert(key.into(), json!({"type":"string","format":"uuid"}));
                required.push(key);
            }
        }
        for key in self.query {
            props.insert((*key).into(), json!({"type":"string"}));
        }
        if !self.body.is_null() {
            props.insert("body".into(), self.body.clone());
            required.push("body");
        }
        Tool::new(self.name,self.description,json!({"type":"object","properties":props,"required":required,"additionalProperties":false}).as_object().unwrap().clone())
    }
}
impl ServerHandler for Monica {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::default();
        config.capabilities = ServerCapabilities::builder().enable_tools().build();
        config.instructions =
            Some("用稳定人物 ID 查询。结论引用实际读取的证据，注明来源、版本和日期。".into());
        config
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult {
            tools: specs().iter().map(Spec::tool).collect(),
            ..Default::default()
        })
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        specs().iter().find(|s| s.name == name).map(Spec::tool)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, rmcp::ErrorData> {
        let Some(spec) = specs().into_iter().find(|s| s.name == request.name) else {
            return Err(rmcp::ErrorData::invalid_params("Unknown tool", None));
        };
        let args = request.arguments.unwrap_or_default();
        let result = self.dispatch(&spec, &args).await;
        Ok(match result {
            Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        }
        .into())
    }
}
impl Monica {
    async fn dispatch(
        &self,
        spec: &Spec,
        args: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, String> {
        let mut path = spec.path.to_owned();
        for key in ["id", "tag"] {
            if path.contains(&format!("{{{key}}}")) {
                let id = args
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or("Missing UUID")?
                    .parse::<Uuid>()
                    .map_err(|_| "Invalid UUID")?;
                path = path.replace(&format!("{{{key}}}"), &id.to_string());
            }
        }
        let mut query = Vec::new();
        for key in spec.query {
            if let Some(v) = args.get(*key) {
                let text = v.as_str().ok_or("Query values must be strings")?;
                query.push(format!(
                    "{key}={}",
                    percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC)
                ));
            }
        }
        if !query.is_empty() {
            path.push('?');
            path.push_str(&query.join("&"));
        }
        let req = Request::builder()
            .method(spec.method)
            .uri(&path)
            .header("content-type", "application/json")
            .body(Body::from(
                args.get("body").unwrap_or(&json!({})).to_string(),
            ))
            .map_err(|e| e.to_string())?;
        let response = self
            .router
            .clone()
            .oneshot(req)
            .await
            .unwrap_or_else(|e| match e {});
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .map_err(|_| "Response too large; use pagination")?;
        let data: Value = serde_json::from_slice(&bytes).map_err(|_| "Response unavailable")?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {data}"));
        }
        Ok(json!({"data":data,"reference":path}))
    }
}
#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

//! Ask model selection and shared LiteLLM transport.
use super::{ApiError, AppState, Result, user};
use axum::{
    Json, Router,
    body::Body,
    extract::{FromRequest, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::time::Duration;
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ai/browser-model", get(model))
        .route(
            "/api/v1/ai/chat/completions",
            axum::routing::post(completion)
                .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
}
async fn model(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(model_settings(&s).await?))
}
pub(crate) async fn model_settings(s: &AppState) -> Result<Value> {
    let row = sqlx::query(
        "SELECT enabled,execution_enabled,qa_enabled,model,model_routes,qa_models,context_window,max_tokens FROM crm.ai_settings WHERE id",
    )
    .fetch_one(&s.pool)
    .await?;
    let mut models = choices(&row);
    let presets: Vec<_> = models
        .iter()
        .filter_map(|model| deepseek_medium(model))
        .filter(|preset| !models.contains(preset))
        .collect();
    models.extend(presets);
    Ok(
        json!({"enabled":row.get::<bool,_>("enabled")&&row.get::<bool,_>("execution_enabled")&&row.get::<bool,_>("qa_enabled"),"model":models[0],"models":models,"context_window":row.get::<i32,_>("context_window"),"max_tokens":row.get::<i32,_>("max_tokens")}),
    )
}
/// The models a question may be answered with: the configured one first, then
/// the others the user may pick.
fn choices(row: &sqlx::postgres::PgRow) -> Vec<String> {
    let mut models = vec![monica_core::ai::models::resolve(
        &row.get::<String, _>("model"),
        &row.get::<Value, _>("model_routes"),
        "ai.qa",
    )];
    for model in row.get::<Vec<String>, _>("qa_models") {
        if !models.contains(&model) {
            models.push(model);
        }
    }
    models
}
pub(crate) fn deepseek_medium(model: &str) -> Option<String> {
    let name = model.rsplit('/').next()?;
    (name == "deepseek-v4-pro").then(|| format!("{model}-medium"))
}

fn resolve_choice<'a>(models: &'a [String], requested: Option<&str>) -> (&'a str, bool) {
    if let Some(requested) = requested {
        if let Some(model) = models.iter().find(|model| model.as_str() == requested) {
            return (model, false);
        }
        if let Some(model) = models
            .iter()
            .find(|model| deepseek_medium(model).as_deref() == Some(requested))
        {
            return (model, true);
        }
    }
    (&models[0], false)
}

#[derive(Deserialize)]
struct Completion {
    /// One of the configured choices; anything else means the default.
    model: Option<String>,
    messages: Vec<Value>,
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
    #[serde(default = "yes")]
    stream: bool,
    max_tokens: Option<i32>,
    temperature: Option<f64>,
}
fn yes() -> bool {
    true
}
impl Completion {
    /// The request as the provider receives it: the configured model, and an
    /// output limit no larger than the configured one. Everything else is the
    /// agent's conversation; the provider rejects what it cannot read.
    fn body(mut self, model: &str, max: i32) -> Value {
        let model_name = model.rsplit('/').next().unwrap_or(model);
        if model_name == "deepseek-flash" || model_name.starts_with("deepseek-v4-") {
            // DeepSeek's default thinking mode requires this field with tool history,
            // including on older assistant messages created by another model.
            for message in &mut self.messages {
                if message["role"] == "assistant" && message["reasoning_content"].is_null() {
                    message["reasoning_content"] = json!("");
                }
            }
        }
        let mut body = json!({"model":model,"messages":self.messages,"stream":self.stream,"max_tokens":self.max_tokens.unwrap_or(max).min(max)});
        if let Some(tools) = self.tools {
            body["tools"] = json!(tools)
        }
        if let Some(choice) = self.tool_choice {
            body["tool_choice"] = choice
        }
        if let Some(temperature) = self.temperature {
            body["temperature"] = json!(temperature)
        }
        body
    }
}
async fn completion(State(s): State<AppState>, request: Request) -> Result<Response> {
    user(&s, request.headers()).await?;
    // Admit before buffering the JSON body. Long-lived responses share memory
    // with the rest of Monica and release both reservations when disconnected.
    let resources = s
        .resources
        .try_acquire_model_stream(4 * 1024 * 1024)
        .ok_or(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "模型服务正忙，请稍后重试",
        ))?;
    let input = match Json::<Completion>::from_request(request, &s).await {
        Ok(Json(input)) => input,
        Err(rejection) => return Ok(rejection.into_response()),
    };
    forward(&s, input, resources).await
}

pub(crate) async fn ask_completion(s: &AppState, body: Value) -> Result<Response> {
    let resources = s
        .resources
        .try_acquire_model_stream(4 * 1024 * 1024)
        .ok_or(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "模型服务正忙，请稍后重试",
        ))?;
    let input = serde_json::from_value(body)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "问答请求格式无效"))?;
    forward(s, input, resources).await
}

async fn forward(
    s: &AppState,
    input: Completion,
    resources: monica_core::resources::ResourcePermit,
) -> Result<Response> {
    let row = sqlx::query(
        "SELECT enabled,execution_enabled,qa_enabled,model,model_routes,qa_models,base_url,max_tokens FROM crm.ai_settings WHERE id",
    )
    .fetch_one(&s.pool)
    .await?;
    if !row.get::<bool, _>("enabled")
        || !row.get::<bool, _>("execution_enabled")
        || !row.get::<bool, _>("qa_enabled")
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "请先在 Monica 设置中启用模型服务",
        ));
    }
    let models = choices(&row);
    let (model, medium) = resolve_choice(&models, input.model.as_deref());
    let mut body = input.body(model, row.get("max_tokens"));
    if medium {
        // DeepSeek maps its compatibility value "medium" to the native high
        // rung. Use the native spelling for older LiteLLM transforms as well.
        body["reasoning_effort"] = json!("high");
        // Older LiteLLM versions need an explicit pass-through for this
        // parameter on DeepSeek routes. Scope it to this server-owned preset.
        body["allowed_openai_params"] = json!(["reasoning_effort"]);
    }
    let provider = monica_core::ai::configured_provider(&s.pool, Some(&s.credentials))
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "模型服务配置无效"))?
        .ok_or(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "模型服务尚未配置",
        ))?;
    let mut upstream = provider
        .forward(&body)
        .await
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "模型服务请求失败"))?;
    if !upstream.status().is_success() {
        let status =
            StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        tracing::warn!(%status, "model service rejected a question");
        let error = if matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ) || (status.is_server_error() && model.starts_with("gemini-"))
        {
            tokio::time::timeout(Duration::from_secs(2), read_provider_error(&mut upstream))
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        if status == StatusCode::PAYLOAD_TOO_LARGE
            || error.as_ref().is_some_and(context_limit_error)
        {
            return Err(ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "模型上下文超出上限",
            ));
        }
        if status == StatusCode::PAYMENT_REQUIRED
            || error.as_ref().is_some_and(prepaid_credits_depleted)
        {
            return Err(ApiError(
                StatusCode::PAYMENT_REQUIRED,
                "模型服务的预付余额已耗尽，请切换模型。",
            ));
        }
        let detail = if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            "模型服务拒绝授权，请检查模型设置"
        } else {
            "模型服务请求失败"
        };
        let mut response = ApiError(status, detail).into_response();
        if matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) && let Some(delay) = upstream.headers().get(header::RETRY_AFTER)
        {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, delay.clone());
        }
        return Ok(response);
    }
    Ok(stream_response(upstream, resources))
}
async fn read_provider_error(
    upstream: &mut monica_core::ai::provider::ForwardResponse,
) -> Option<Value> {
    const LIMIT: usize = 32 * 1024;
    let mut bytes = Vec::new();
    loop {
        match upstream.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= LIMIT => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(None) => break,
            _ => return None,
        }
    }
    let body: Value = serde_json::from_slice(&bytes).ok()?;
    Some(body.get("error").cloned().unwrap_or(body))
}

fn provider_error_message(error: &Value) -> String {
    error["message"]
        .as_str()
        .or(error.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn context_limit_error(error: &Value) -> bool {
    let message = provider_error_message(error);
    error["code"].as_str() == Some("context_length_exceeded")
        || error["type"].as_str() == Some("context_length_exceeded")
        || message.contains("context_length_exceeded")
        || message.contains("maximum context length")
        || message.contains("context window exceeded")
}

fn prepaid_credits_depleted(error: &Value) -> bool {
    // LiteLLM can wrap the Gemini Developer API's 402 as a gateway 500.
    let message = provider_error_message(error);
    message.contains("prepayment credits") && message.contains("depleted")
}
fn stream_response(
    mut upstream: monica_core::ai::provider::ForwardResponse,
    resources: monica_core::resources::ResourcePermit,
) -> Response {
    let content_type = upstream
        .headers()
        .get("content-type")
        .cloned()
        .unwrap_or_else(|| axum::http::HeaderValue::from_static("text/event-stream"));
    let stream = async_stream::stream! {
        let _resources=resources;
        loop{
            match upstream.chunk().await{
                Ok(Some(bytes))=>yield Ok::<axum::body::Bytes,std::io::Error>(bytes),
                Ok(None)=>break,
                Err(_)=>{yield Err(std::io::Error::other("model stream interrupted"));break}
            }
        }
    };
    let mut response = Body::from_stream(stream).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[test]
    fn only_context_errors_trigger_compaction() {
        assert!(context_limit_error(
            &json!({"code":"context_length_exceeded"})
        ));
        assert!(context_limit_error(&json!({
            "message":"This model's maximum context length is 8192 tokens"
        })));
        assert!(!context_limit_error(
            &json!({"message":"Invalid model name"})
        ));
        assert!(!context_limit_error(&json!("unstructured upstream error")));
    }
    #[test]
    fn a_gateway_wrapped_gemini_balance_error_is_permanent() {
        assert!(prepaid_credits_depleted(&json!({
            "message":"Gemini API returned 402: Your prepayment credits are depleted."
        })));
        assert!(!prepaid_credits_depleted(&json!({
            "message":"Gemini API connection interrupted."
        })));
    }
    #[test]
    fn browser_payload_is_stateless_and_capped() {
        let input:Completion=serde_json::from_value(json!({"messages":[{"role":"user","content":"Synthetic only"}],"tools":[{"type":"function","function":{"name":"lookup"}}],"tool_choice":"auto","max_tokens":99999,"temperature":0.1})).unwrap();
        let body = input.body("synthetic-model", 8192);
        assert_eq!(body["model"], "synthetic-model");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["function"]["name"], "lookup");
        assert!(body.get("session_id").is_none());
    }
    #[test]
    fn deepseek_replays_reasoning_and_accepts_old_assistant_history() {
        let messages = json!([
            {"role":"user","content":"Synthetic only"},
            {"role":"assistant","content":"Earlier answer"},
            {"role":"assistant","reasoning_content":"Kept","tool_calls":[]},
            {"role":"tool","content":"A result"}
        ]);
        for name in [
            "deepseek-flash",
            "deepseek/deepseek-v4-pro",
            "gemini-4-neon",
        ] {
            let input: Completion = serde_json::from_value(json!({"messages":messages})).unwrap();
            let body = input.body(name, 8192);
            assert_eq!(body["messages"][2]["reasoning_content"], "Kept");
            assert!(body["messages"][0].get("reasoning_content").is_none());
            assert!(body["messages"][3].get("reasoning_content").is_none());
            if name.starts_with("deepseek") {
                assert_eq!(body["messages"][1]["reasoning_content"], "");
            } else {
                assert!(body["messages"][1].get("reasoning_content").is_none());
            }
        }
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_question_may_pick_among_the_configured_models_only(pool: sqlx::PgPool) {
        sqlx::query("UPDATE crm.ai_settings SET model='default-model',model_routes='{\"qa\":\"qa-model\"}',qa_models='{other-model,qa-model}' WHERE id")
            .execute(&pool)
            .await
            .unwrap();
        let row = sqlx::query("SELECT model,model_routes,qa_models FROM crm.ai_settings WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let models = choices(&row);
        assert_eq!(models, ["qa-model", "other-model"]);
        let pick = |asked: Option<&str>| {
            asked
                .map(str::to_owned)
                .filter(|model| models.contains(model))
                .unwrap_or_else(|| models[0].clone())
        };
        assert_eq!(pick(Some("other-model")), "other-model");
        assert_eq!(pick(Some("not-offered")), "qa-model");
        assert_eq!(pick(None), "qa-model");
    }
    async fn fake_upstream(
        keep_open: bool,
    ) -> (
        monica_core::ai::provider::ForwardResponse,
        tokio::task::JoinHandle<bool>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let n = socket.read(&mut request).await.unwrap();
            assert!(
                String::from_utf8_lossy(&request[..n]).starts_with("POST /v1/chat/completions")
            );
            if keep_open {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
                loop {
                    if socket
                        .write_all(b"e\r\ndata: test\n\nxx\r\n")
                        .await
                        .is_err()
                    {
                        return true;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            } else {
                let data = b"data: {\"synthetic\":true}\n\ndata: [DONE]\n\n";
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
                    data.len()
                );
                socket.write_all(headers.as_bytes()).await.unwrap();
                socket.write_all(data).await.unwrap();
                true
            }
        });
        let provider = monica_core::ai::provider::OpenAiProvider::new(
            &format!("http://{address}/v1"),
            "synthetic-browser-key".into(),
        )
        .unwrap();
        let response = provider
            .forward(&json!({"messages":[],"model":"synthetic-model","stream":true}))
            .await
            .unwrap();
        (response, server)
    }
    #[tokio::test]
    async fn sse_bytes_pass_through_and_disconnect_releases_memory() {
        let resources =
            monica_core::resources::ResourceBudget::new(monica_core::resources::ResourceLimits {
                memory_bytes: 4 * 1024 * 1024,
                ..Default::default()
            })
            .unwrap();
        let request = monica_core::resources::TaskResources {
            memory_bytes: 4 * 1024 * 1024,
            ..Default::default()
        };
        let (upstream, server) = fake_upstream(false).await;
        let response = stream_response(upstream, resources.acquire(request).await.unwrap());
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(
            body.as_ref(),
            b"data: {\"synthetic\":true}\n\ndata: [DONE]\n\n"
        );
        assert!(server.await.unwrap());
        let (upstream, server) = fake_upstream(true).await;
        let response = stream_response(upstream, resources.acquire(request).await.unwrap());
        assert_eq!(resources.status().memory_available_bytes, 0);
        drop(response);
        let available = tokio::time::timeout(Duration::from_secs(1), resources.acquire(request))
            .await
            .unwrap()
            .unwrap();
        drop(available);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
        );
    }
}

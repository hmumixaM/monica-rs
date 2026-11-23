//! Stateless browser-owned agent transport. No transcript or tool loop on server.
use super::{ApiError, AppState, Result, user};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
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
    let row = sqlx::query(
        "SELECT enabled,execution_enabled,qa_enabled,model,model_routes,qa_models,context_window,max_tokens FROM crm.ai_settings WHERE id",
    )
    .fetch_one(&s.pool)
    .await?;
    let models = choices(&row);
    Ok(Json(
        json!({"enabled":row.get::<bool,_>("enabled")&&row.get::<bool,_>("execution_enabled")&&row.get::<bool,_>("qa_enabled"),"model":models[0],"models":models,"context_window":row.get::<i32,_>("context_window"),"max_tokens":row.get::<i32,_>("max_tokens")}),
    ))
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
    /// browser agent's own conversation; the provider rejects what it cannot read.
    fn body(self, model: &str, max: i32) -> Value {
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
async fn completion(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(input): Json<Completion>,
) -> Result<Response> {
    user(&s, &h).await?;
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
    let model = input
        .model
        .clone()
        .filter(|model| models.contains(model))
        .unwrap_or_else(|| models[0].clone());
    let body = input.body(&model, row.get("max_tokens"));
    let provider = monica_core::ai::configured_provider(&s.pool, Some(&s.credentials))
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "模型服务配置无效"))?
        .ok_or(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "模型服务尚未配置",
        ))?;
    // Auth/settings queries have completed and released their connections before
    // any semaphore wait, upstream request or long-lived response stream.
    let slot = monica_core::ai::slots::interactive()
        .await
        .map_err(|_| ApiError(StatusCode::TOO_MANY_REQUESTS, "模型服务正忙，请稍后重试"))?;
    let resources = s
        .resources
        .acquire(monica_core::resources::TaskResources {
            network: 1,
            memory_bytes: 4 * 1024 * 1024,
            ..Default::default()
        })
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "模型资源暂时不可用"))?;
    let mut upstream = provider
        .forward(&body)
        .await
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "模型服务请求失败"))?;
    if !upstream.status().is_success() {
        let status =
            StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        tracing::warn!(%status, "model service rejected a question");
        if status == StatusCode::PAYLOAD_TOO_LARGE
            || (matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ) && tokio::time::timeout(Duration::from_secs(2), context_rejection(&mut upstream))
                .await
                .unwrap_or(false))
        {
            return Err(ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "模型上下文超出上限",
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
    Ok(stream_response(upstream, slot, resources))
}
async fn context_rejection(upstream: &mut monica_core::ai::provider::ForwardResponse) -> bool {
    const LIMIT: usize = 32 * 1024;
    let mut bytes = Vec::new();
    loop {
        match upstream.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= LIMIT => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(None) => break,
            _ => return false,
        }
    }
    context_limit_error(&bytes)
}

fn context_limit_error(bytes: &[u8]) -> bool {
    let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let error = body.get("error").unwrap_or(&body);
    let message = error["message"]
        .as_str()
        .or(error.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    error["code"].as_str() == Some("context_length_exceeded")
        || error["type"].as_str() == Some("context_length_exceeded")
        || message.contains("context_length_exceeded")
        || message.contains("maximum context length")
        || message.contains("context window exceeded")
}
fn stream_response(
    mut upstream: monica_core::ai::provider::ForwardResponse,
    slot: tokio::sync::OwnedSemaphorePermit,
    resources: monica_core::resources::ResourcePermit,
) -> Response {
    let content_type = upstream
        .headers()
        .get("content-type")
        .cloned()
        .unwrap_or_else(|| axum::http::HeaderValue::from_static("text/event-stream"));
    let stream = async_stream::stream! {
        let _slot=slot;let _resources=resources;
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
            br#"{"error":{"code":"context_length_exceeded"}}"#
        ));
        assert!(context_limit_error(
            br#"{"error":{"message":"This model's maximum context length is 8192 tokens"}}"#
        ));
        assert!(!context_limit_error(
            br#"{"error":{"message":"Invalid model name"}}"#
        ));
        assert!(!context_limit_error(b"unstructured upstream error"));
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
    async fn sse_bytes_pass_through_and_disconnect_releases_reserved_slot() {
        let resources = monica_core::resources::ResourceBudget::new(Default::default()).unwrap();
        let (upstream, server) = fake_upstream(false).await;
        let slot = monica_core::ai::slots::interactive().await.unwrap();
        let budget = resources.acquire(Default::default()).await.unwrap();
        let response = stream_response(upstream, slot, budget);
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
        let slot = monica_core::ai::slots::interactive().await.unwrap();
        let response = stream_response(
            upstream,
            slot,
            resources.acquire(Default::default()).await.unwrap(),
        );
        drop(response);
        let available = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            monica_core::ai::slots::interactive(),
        )
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

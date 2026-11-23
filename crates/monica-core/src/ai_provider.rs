//! OpenAI-compatible structured transport, with an entirely offline fake.
use super::Lease;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, time::Duration};
pub type ForwardResponse = reqwest::Response;

pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn complete<'a>(
        &'a self,
        lease: &'a Lease,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;
}
/// An injected result makes tests exercise the same validation and commit path.
pub struct FakeProvider {
    pub output: Value,
}
impl Provider for FakeProvider {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn complete<'a>(
        &'a self,
        _lease: &'a Lease,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async { Ok(self.output.clone()) })
    }
}

/// A rate-limited or failed request is tried three times. Each try carries the
/// wait before the next one; the last carries none.
fn retries() -> [Option<Duration>; 3] {
    [
        Some(Duration::from_secs(1)),
        Some(Duration::from_secs(4)),
        None,
    ]
}

/// Some models wrap their JSON answer in a Markdown code fence.
fn unfenced(content: &str) -> &str {
    let content = content.trim();
    content
        .strip_prefix("```")
        .and_then(|rest| rest.strip_suffix("```"))
        .map(|rest| rest.strip_prefix("json").unwrap_or(rest).trim())
        .unwrap_or(content)
}

pub struct OpenAiProvider {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    api_key: String,
}
impl OpenAiProvider {
    /// Configuration is supplied by the runtime; the key never enters DB/job JSON.
    pub fn new(base_url: &str, api_key: String) -> Result<Self> {
        let mut base = reqwest::Url::parse(base_url)?;
        ensure!(
            base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none(),
            "invalid provider URL"
        );
        ensure!(
            matches!(base.scheme(), "http" | "https"),
            "model URL must use HTTP or HTTPS"
        );
        ensure!(
            !api_key.is_empty() && !api_key.contains(['\r', '\n']),
            "missing or invalid provider key"
        );
        base.set_path(&format!(
            "{}/chat/completions",
            base.path().trim_end_matches('/')
        ));
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(180))
            .build()?;
        Ok(Self {
            client,
            endpoint: base,
            api_key,
        })
    }
    async fn request(&self, lease: &Lease) -> Result<Value> {
        let mut input = lease.input.clone();
        let images = input.as_object_mut().and_then(|m| m.remove("image_data"));
        let vision = matches!(lease.kind.as_str(), "ai.image_ocr" | "ai.post_media");
        ensure!(
            !vision || images.is_some(),
            "vision request has no image input"
        );
        let content = if let Some(images) = images {
            let caption = input["post"]["text"].as_str().unwrap_or("");
            let mut content = vec![
                json!({"type":"text","text":format!("配文：{}",if caption.is_empty(){"（无）"}else{caption})}),
            ];
            let images = match images {
                Value::String(url) => vec![json!({"index":1,"kind":"photo","url":url})],
                Value::Array(images) => images,
                _ => bail!("invalid vision images"),
            };
            for image in images {
                content.push(json!({"type":"text","text":format!("图 {}（{}）",image["index"],if image["kind"]=="video"{"视频封面"}else{"照片"})}));
                content.push(json!({"type":"image_url","image_url":{"url":image["url"]}}));
            }
            json!(content)
        } else {
            json!(serde_json::to_string(
                &json!({"task":lease.kind,"input":input})
            )?)
        };
        let format = if vision {
            super::post_media::response_format()
        } else {
            json!({"type":"json_object"})
        };
        let request = json!({"model":lease.model,"messages":[{"role":"system","content":super::prompts::system(&lease.kind)},{"role":"user","content":content}],"response_format":format,"max_tokens":8192});
        for retry in retries() {
            let response = self
                .client
                .post(self.endpoint.clone())
                .bearer_auth(&self.api_key)
                .json(&request)
                .send()
                .await;
            match response {
                Ok(mut response) => {
                    let status = response.status();
                    if (status == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || status == reqwest::StatusCode::REQUEST_TIMEOUT
                        || status.is_server_error())
                        && let Some(delay) = retry
                    {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    if !status.is_success() {
                        let detail: String = response
                            .text()
                            .await
                            .unwrap_or_default()
                            .chars()
                            .take(300)
                            .collect();
                        bail!("model request rejected: {status} {detail}");
                    }
                    ensure!(
                        response.content_length().is_none_or(|n| n <= 1024 * 1024),
                        "model response too large"
                    );
                    let mut bytes = Vec::new();
                    while let Some(chunk) = response
                        .chunk()
                        .await
                        .context("model response interrupted")?
                    {
                        ensure!(
                            bytes.len() + chunk.len() <= 1024 * 1024,
                            "model response too large"
                        );
                        bytes.extend_from_slice(&chunk)
                    }
                    let envelope: Value =
                        serde_json::from_slice(&bytes).context("invalid model response")?;
                    let message = &envelope["choices"][0]["message"];
                    ensure!(message["refusal"].is_null(), "model refused request");
                    let value =
                        if let Some(parsed) = message.get("parsed").filter(|v| v.is_object()) {
                            parsed.clone()
                        } else {
                            serde_json::from_str(unfenced(
                                message["content"]
                                    .as_str()
                                    .context("missing model content")?,
                            ))
                            .with_context(|| {
                                format!(
                                    "invalid structured model result (finish_reason {})",
                                    envelope["choices"][0]["finish_reason"]
                                )
                            })?
                        };
                    ensure!(value.is_object(), "model output must be an object");
                    return Ok(value);
                }
                Err(error) => {
                    if let Some(delay) = retry {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    bail!("model request failed: {}", error.without_url())
                }
            }
        }
        bail!("model retry limit")
    }
    /// Stateless transport for the browser agent, which owns the retry budget.
    pub async fn forward(&self, body: &Value) -> Result<reqwest::Response> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(180))
            .build()?;
        client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|error| error.without_url().into())
    }
}
impl Provider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }
    fn complete<'a>(
        &'a self,
        lease: &'a Lease,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.request(lease))
    }
}

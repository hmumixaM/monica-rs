//! Owner-configured public search. The Exa credential stays on the server.
use super::{ApiError, AppState, Result, invalid, user};
use anyhow::{Context, ensure};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::OnceLock, time::Duration};

pub(crate) const NAME: &str = "search_public_web";
const EXA_SEARCH: &str = "https://api.exa.ai/search";
const RESPONSE_LIMIT: usize = 512 * 1024;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/ai/web-search", get(read).patch(save))
}

pub(crate) async fn configured(s: &AppState) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT exa_credential_ciphertext IS NOT NULL FROM crm.ai_settings WHERE id")
        .fetch_one(&s.pool)
        .await
}

async fn read(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    Ok(Json(json!({"configured":configured(&s).await?})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    api_key: Option<String>,
    #[serde(default)]
    clear_key: bool,
}

async fn save(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(v): Json<Settings>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let key = v.api_key.as_deref().map(str::trim);
    let encrypted = match (key, v.clear_key) {
        (Some(key), false)
            if !key.is_empty()
                && key.len() <= 1024
                && key.bytes().all(|byte| byte.is_ascii_graphic()) =>
        {
            Some(
                s.credentials
                    .encrypt(&json!({"api_key":key}))
                    .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "无法保存 Exa 密钥"))?,
            )
        }
        (None, true) => None,
        _ => return Err(invalid("请提供有效的 Exa API Key 或选择删除现有密钥")),
    };
    let mut tx = s.pool.begin().await?;
    sqlx::query("UPDATE crm.ai_settings SET exa_credential_ciphertext=$1 WHERE id")
        .bind(&encrypted)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,metadata) VALUES($1,'ai.web_search.settings','settings',$2)")
        .bind(actor).bind(json!({"configured":encrypted.is_some()})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"configured":encrypted.is_some()})))
}

pub(crate) fn tool() -> Value {
    json!({"type":"function","function":{
        "name":NAME,
        "description":"通过 Exa 检索公开网页，主要用于核实好友已公开的职业或主页、最新消息以及地点名称、地址或营业信息。私人往来和本地资料优先用 Monica 查询。只发送所需的公开搜索词，禁止发送私聊、私人笔记、未公开联系方式、住址或 Monica 内部 ID。返回网页链接、日期和摘录；同名人物需核对身份。",
        "parameters":{"type":"object","properties":{"query":{"type":"string","maxLength":500,"description":"只包含核实公开信息所需的关键词"}},"required":["query"],"additionalProperties":false}
    }})
}

pub(crate) async fn search(s: &AppState, args: &Value) -> anyhow::Result<Value> {
    let query = args["query"].as_str().map(str::trim).unwrap_or_default();
    ensure!(
        !query.is_empty() && query.chars().count() <= 500,
        "公开搜索词须为 1 至 500 个字符。"
    );
    let encrypted: Option<String> = sqlx::query_scalar("SELECT exa_credential_ciphertext FROM crm.ai_settings WHERE id AND enabled AND execution_enabled AND qa_enabled")
        .fetch_optional(&s.pool).await?.flatten();
    let credentials = s
        .credentials
        .decrypt(&encrypted.context("请先在设置中启用 Ask Monica 并配置 Exa。")?)
        .map_err(|_| anyhow::anyhow!("Exa 密钥无法读取，请重新配置。"))?;
    let key = credentials["api_key"]
        .as_str()
        .context("Exa 密钥不可用。")?;
    search_at(client(), EXA_SEARCH, key, query).await
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("Exa client configuration")
    })
}

async fn search_at(
    client: &reqwest::Client,
    endpoint: &str,
    key: &str,
    query: &str,
) -> anyhow::Result<Value> {
    let mut response = client
        .post(endpoint)
        .header("x-api-key", key)
        .json(&json!({"query":query,"contents":{"highlights":true}}))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("连接 Exa 失败。"))?;
    let status = response.status();
    ensure!(
        status.is_success(),
        "Exa 查询失败（HTTP {}）。",
        status.as_u16()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("Exa 结果传输中断。"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= RESPONSE_LIMIT,
            "Exa 返回内容过长。请缩小范围。"
        );
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes).context("Exa 响应格式无效。")?;
    let rows = body["results"].as_array().context("Exa 响应缺少结果。")?;
    let excerpt = |value: &Value, limit: usize| {
        value
            .as_str()
            .map(|text| text.chars().take(limit).collect::<String>())
    };
    let results: Vec<_> = rows.iter().take(10).filter_map(|row| {
        let url = url::Url::parse(row["url"].as_str()?).ok()?;
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        let highlights: Vec<_> = row["highlights"].as_array().into_iter().flatten()
            .filter_map(|item| excerpt(item, 700)).take(3).collect();
        Some(json!({"title":excerpt(&row["title"],200),"link":url.as_str(),
            "published_at":row["publishedDate"],"author":excerpt(&row["author"],120),"highlights":highlights}))
    }).collect();
    Ok(json!({"results":results}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;

    #[tokio::test]
    async fn exa_sends_only_search_terms_and_projects_public_sources() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
        let app = Router::new().route("/search", post(|headers: HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(headers["x-api-key"], "synthetic-exa-key");
            assert_eq!(body, json!({"query":"public museum hours","contents":{"highlights":true}}));
            Json(json!({"results":[
                {"url":"https://example.invalid/museum","title":"Museum hours","publishedDate":"2026-02-03T00:00:00Z","highlights":["Open at 10"],"text":"Unrequested text"},
                {"url":"javascript:invalid","title":"Invalid source"}
            ]}))
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let value = search_at(
            client(),
            &endpoint,
            "synthetic-exa-key",
            "public museum hours",
        )
        .await
        .unwrap();
        server.abort();
        assert_eq!(
            value,
            json!({"results":[{"link":"https://example.invalid/museum","title":"Museum hours",
            "published_at":"2026-02-03T00:00:00Z","author":null,"highlights":["Open at 10"]}]})
        );
    }
}

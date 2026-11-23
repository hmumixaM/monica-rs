//! Authenticated, local model configuration shared by Mac and NAS.
use super::{ApiError, AppState, Result, invalid, user};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/ai/settings", get(read).patch(save))
}
async fn read(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let row = sqlx::query("SELECT a.enabled,a.execution_enabled,a.qa_enabled,a.model,a.model_routes,a.qa_models,o.model AS vision_model,a.base_url,a.include_messages,a.include_private_notes,a.context_window,a.max_tokens,a.credential_ciphertext IS NOT NULL AS has_key FROM crm.ai_settings a CROSS JOIN crm.image_ocr_settings o WHERE a.id AND o.id")
        .fetch_one(&s.pool).await?;
    Ok(Json(json!({
        "enabled":row.get::<bool,_>("enabled"),"execution_enabled":row.get::<bool,_>("execution_enabled"),"qa_enabled":row.get::<bool,_>("qa_enabled"),
        "model":row.get::<String,_>("model"),"base_url":row.get::<Option<String>,_>("base_url"),
        "model_routes":row.get::<Value,_>("model_routes"),"qa_models":row.get::<Vec<String>,_>("qa_models"),"vision_model":row.get::<String,_>("vision_model"),
        "include_messages":row.get::<bool,_>("include_messages"),"include_private_notes":row.get::<bool,_>("include_private_notes"),
        "context_window":row.get::<i32,_>("context_window"),"max_tokens":row.get::<i32,_>("max_tokens"),
        "has_key":row.get::<bool,_>("has_key") || std::env::var("MONICA_AI_API_KEY").is_ok_and(|key| !key.is_empty()),
    })))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    enabled: Option<bool>,
    qa_enabled: Option<bool>,
    model: Option<String>,
    model_routes: Option<Value>,
    qa_models: Option<Vec<String>>,
    vision_model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    #[serde(default)]
    clear_key: bool,
    include_messages: Option<bool>,
    include_private_notes: Option<bool>,
    context_window: Option<i32>,
    max_tokens: Option<i32>,
}
async fn save(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(v): Json<Settings>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let routes = v
        .model_routes
        .as_ref()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| invalid("模型分工需要是对象"))?;
            if object.iter().any(|(k, v)| {
                !monica_core::ai::models::ROLES.contains(&k.as_str())
                    || v.as_str().is_none_or(|s| s.len() > 255)
            }) {
                return Err(invalid("模型分工无效"));
            }
            Ok(Value::Object(
                object
                    .iter()
                    .filter(|(_, v)| v.as_str().is_some_and(|s| !s.trim().is_empty()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ))
        })
        .transpose()?;
    if v.model
        .as_ref()
        .is_some_and(|s| s.trim().is_empty() || s.len() > 255)
        || v.vision_model
            .as_ref()
            .is_some_and(|s| s.trim().is_empty() || s.len() > 255)
        || v.base_url.as_ref().is_some_and(|s| s.len() > 2048)
        || v.api_key
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 8192 || s.contains(['\r', '\n']))
        || v.context_window
            .is_some_and(|n| !(8192..=2097152).contains(&n))
        || v.max_tokens.is_some_and(|n| !(1..=131072).contains(&n))
        || (v.clear_key && v.api_key.is_some())
    {
        return Err(invalid("模型设置无效"));
    }
    if let Some(base) = &v.base_url {
        monica_core::ai::provider::OpenAiProvider::new(
            base,
            "configuration-validation-only".into(),
        )
        .map_err(|_| invalid("模型地址需要 HTTPS，本机服务可使用 HTTP"))?;
    }
    let encrypted = v
        .api_key
        .as_ref()
        .map(|key| s.credentials.encrypt(&json!({"api_key":key})))
        .transpose()
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "无法保存模型凭据"))?;
    let mut tx = s.pool.begin().await?;
    let old = sqlx::query("SELECT * FROM crm.ai_settings WHERE id FOR UPDATE")
        .fetch_one(&mut *tx)
        .await?;
    let context = v.context_window.unwrap_or(old.get("context_window"));
    let max = v.max_tokens.unwrap_or(old.get("max_tokens"));
    if max + 4000 >= context {
        return Err(invalid("上下文窗口需要大于输出上限与预留空间之和"));
    }
    let enabled = v.enabled.unwrap_or(old.get("enabled"));
    let base = v
        .base_url
        .as_ref()
        .cloned()
        .or(old.get::<Option<String>, _>("base_url"));
    let has_env_key = std::env::var("MONICA_AI_API_KEY").is_ok_and(|key| !key.is_empty());
    if enabled
        && old.get::<bool, _>("execution_enabled")
        && (base.is_none() && std::env::var("MONICA_AI_BASE_URL").is_err()
            || (!has_env_key
                && (v.clear_key
                    || encrypted.is_none()
                        && old
                            .get::<Option<String>, _>("credential_ciphertext")
                            .is_none())))
    {
        return Err(invalid("请先填写模型地址与 API Key"));
    }
    sqlx::query("UPDATE crm.ai_settings SET enabled=coalesce($1,enabled),qa_enabled=coalesce($2,qa_enabled),provider='openai',model=coalesce($3,model),base_url=coalesce($4,base_url),credential_ciphertext=CASE WHEN $5 THEN NULL ELSE coalesce($6,credential_ciphertext) END,include_messages=coalesce($7,include_messages),include_private_notes=coalesce($8,include_private_notes),context_window=$9,max_tokens=$10,updated_at=now() WHERE id")
        .bind(v.enabled).bind(v.qa_enabled).bind(v.model).bind(v.base_url).bind(v.clear_key).bind(encrypted)
        .bind(v.include_messages).bind(v.include_private_notes).bind(context).bind(max).execute(&mut *tx).await?;
    if let Some(routes) = routes {
        sqlx::query("UPDATE crm.ai_settings SET model_routes=$1 WHERE id")
            .bind(routes)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(models) = v.qa_models {
        let models: Vec<String> = models
            .iter()
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty())
            .collect();
        if models.len() > 64 || models.iter().any(|model| model.len() > 255) {
            return Err(invalid("问 Monica 的可选模型无效"));
        }
        sqlx::query("UPDATE crm.ai_settings SET qa_models=$1 WHERE id")
            .bind(models)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(model) = v.vision_model {
        sqlx::query("UPDATE crm.image_ocr_settings SET model=$1,updated_at=now() WHERE id")
            .bind(model)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,metadata) VALUES($1,'ai.settings','settings',$2)")
        .bind(actor).bind(json!({"enabled":enabled,"key_changed":v.api_key.is_some() || v.clear_key})).execute(&mut *tx).await?;
    tx.commit().await?;
    read(State(s), h).await
}

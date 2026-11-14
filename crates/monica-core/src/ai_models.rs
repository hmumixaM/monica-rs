//! Task-specific names share the installation's existing provider and credential.
use serde_json::Value;
use sqlx::{PgPool, Row};

pub const ROLES: &[&str] = &["batch", "person", "writer", "qa"];
pub fn resolve(default: &str, routes: &Value, kind: &str) -> String {
    let role = match kind {
        "ai.profile" | "ai.person_week" | "ai.memory_final" => "person",
        "ai.digest" => "writer",
        "ai.qa" => "qa",
        _ => "batch",
    };
    routes[role]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            if role == "qa" {
                routes["writer"].as_str().filter(|s| !s.trim().is_empty())
            } else {
                None
            }
        })
        .unwrap_or(default)
        .to_owned()
}

pub const OVERVIEW_FIELDS: &[&str] = &[
    "wechat.alias",
    "wechat.remark",
    "wechat.nickname",
    "wechat.description",
    "wechat.signature",
    "wechat.labels",
    "wechat.city",
    "wechat.province",
    "wechat.country",
    "linkedin.headline",
    "linkedin.about",
    "linkedin.location",
    "籍贯",
];

pub async fn permits_call(
    pool: &PgPool,
    lease: &super::Lease,
    kind: &str,
    model: &str,
) -> anyhow::Result<bool> {
    let row=sqlx::query("SELECT a.*,o.model AS vision_model FROM crm.ai_settings a CROSS JOIN crm.image_ocr_settings o WHERE a.id AND o.id").fetch_one(pool).await?;
    if !row.get::<bool, _>("enabled")
        || !row.get::<bool, _>("execution_enabled")
        || lease.privacy["provider"] != row.get::<String, _>("provider")
        || lease.privacy["policy"]
            != serde_json::json!({"include_messages":row.get::<bool,_>("include_messages"),"include_private_notes":row.get::<bool,_>("include_private_notes")})
    {
        return Ok(false);
    }
    let routes: Value = row.get("model_routes");
    let default: String = row.get("model");
    let selected = if matches!(kind, "ai.image_ocr" | "ai.post_media") {
        row.get::<String, _>("vision_model")
    } else {
        resolve(&default, &routes, kind)
    };
    if selected != model {
        return Ok(false);
    }
    if lease.kind == "ai.profile" {
        let chunk = lease.input["chunk_model"].as_str().unwrap_or(&lease.model);
        if chunk != resolve(&default, &routes, "ai.memory_chunk") {
            return Ok(false);
        }
    }
    Ok(true)
}

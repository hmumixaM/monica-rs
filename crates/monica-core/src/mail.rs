//! Header-first mailbox ingestion; transport implementations never mutate remote mail.
use anyhow::{Context, Result, ensure};
use mail_parser::{MessageParser, MimeHeaders};
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, future::Future, pin::Pin, sync::LazyLock};
use uuid::Uuid;
mod html;
pub mod http;
pub mod imap;
pub mod sync;
pub const MAX_BODY: usize = 16 * 1024 * 1024;
pub type MailFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
#[derive(Debug, Clone)]
pub struct MailPage {
    pub records: Vec<Value>,
    pub cursor: Value,
    pub complete: bool,
}
pub trait MailTransport: Send {
    fn scan<'a>(&'a mut self, folder: &'a str, cursor: &'a Value) -> MailFuture<'a, MailPage>;
    fn metadata<'a>(&'a mut self, id: &'a str) -> MailFuture<'a, Value>;
    fn body<'a>(&'a mut self, id: &'a str) -> MailFuture<'a, Vec<u8>>;
    fn reconcile<'a>(
        &'a mut self,
        _folder: &'a str,
        known: &'a [Value],
    ) -> MailFuture<'a, Vec<Value>> {
        Box::pin(async move {
            ensure!(known.len() <= 25, "mail verification page exceeds limit");
            let mut records = Vec::new();
            for old in known {
                records.push(
                    self.metadata(
                        old["external_id"]
                            .as_str()
                            .context("mail external_id required")?,
                    )
                    .await?,
                );
            }
            Ok(records)
        })
    }
    fn checkpoint(&mut self) -> MailFuture<'_, Option<Value>> {
        Box::pin(async { Ok(None) })
    }
    fn folders<'a>(&'a mut self, cursor: &'a Value) -> MailFuture<'a, MailPage> {
        Box::pin(async move {
            Ok(MailPage {
                records: vec![],
                cursor: cursor.clone(),
                complete: true,
            })
        })
    }
}
#[derive(Debug)]
pub struct ExpiredCursor;
impl std::fmt::Display for ExpiredCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mail cursor expired; reconciliation required")
    }
}
impl std::error::Error for ExpiredCursor {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub category: String,
    pub reason: String,
}
static TRANSACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(receipt|invoice|reservation|itinerary|boarding|payment received|order confirmation|invitation)\b|订单|账单|行程|预订确认|邀请").unwrap()
});
static PROMOTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(sale|discount|unsubscribe|newsletter|limited time|promo code)\b|促销|优惠码|限时折扣").unwrap()
});
pub fn classify(record: &Value, rule: Option<&str>, has_reply: bool) -> Result<Decision> {
    let mut record = record.clone();
    if let Some(headers) = record["headers"].as_object() {
        record["headers"] = json!(
            headers
                .iter()
                .map(|(key, value)| (key.to_lowercase(), value.clone()))
                .collect::<serde_json::Map<_, _>>()
        );
    }
    let decision = if let Some(rule) = rule {
        ensure!(
            ["personal", "transactional", "marketing", "pending"].contains(&rule),
            "invalid mail rule category"
        );
        (rule, "user_rule")
    } else if has_reply {
        ("personal", "conversation_reply")
    } else if TRANSACTION.is_match(record["headers"]["subject"].as_str().unwrap_or("")) {
        ("transactional", "transaction_subject")
    } else {
        let promoted = record["labels"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == "CATEGORY_PROMOTIONS"));
        let bulk = record["headers"]["list-id"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
            || ["bulk", "list"].contains(
                &record["headers"]["precedence"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .as_str(),
            );
        if promoted
            || bulk
                && PROMOTION.is_match(&format!(
                    "{} {}",
                    record["headers"]["subject"].as_str().unwrap_or(""),
                    record["preview"].as_str().unwrap_or("")
                ))
        {
            ("marketing", "bulk_promotion")
        } else {
            ("pending", "needs_classification")
        }
    };
    Ok(Decision {
        category: decision.0.into(),
        reason: decision.1.into(),
    })
}
/// Match Python's default JSON separators and optional ensure_ascii for pinned hashes.
pub fn python_json(value: &Value, ascii: bool) -> String {
    match value {
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(|v| python_json(v, ascii))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(m) => {
            let mut keys: Vec<_> = m.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|k| format!(
                        "{}: {}",
                        python_json(&json!(k), ascii),
                        python_json(&m[k], ascii)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::String(s) if ascii => {
            let quoted = serde_json::to_string(s).unwrap();
            let mut out = String::new();
            for c in quoted.chars() {
                if c.is_ascii() {
                    out.push(c)
                } else {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        out.push_str(&format!("\\u{unit:04x}"))
                    }
                }
            }
            out
        }
        _ => serde_json::to_string(value).unwrap(),
    }
}
pub fn fingerprint(record: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(python_json(
            &json!({"headers":record.get("headers").cloned().unwrap_or(json!({})),"preview":record.get("preview").cloned().unwrap_or(json!(""))}),
            true
        ))
    )
}
pub fn addresses(record: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    for (field, role) in [("from", "sender"), ("to", "recipient"), ("cc", "recipient")] {
        let header = format!(
            "{field}: {}\r\n\r\n",
            record["headers"][field].as_str().unwrap_or("")
        );
        let parsed = MessageParser::default().parse(header.as_bytes());
        let address = parsed.as_ref().and_then(|m| match field {
            "from" => m.from(),
            "to" => m.to(),
            _ => m.cc(),
        });
        let before = result.len();
        if let Some(address) = address {
            for a in address.iter() {
                if let Some(email) = a.address().filter(|v| v.contains('@')) {
                    let email = crate::identity::casefold(email);
                    result.push(json!({"id":email,"name":a.name().unwrap_or(""),"emails":[email],"role":role}));
                }
            }
        }
        if result.len() == before {
            let fallback = if field == "from" {
                vec![record["sender"].clone()]
            } else {
                record[field].as_array().cloned().unwrap_or_default()
            };
            for a in fallback {
                if let Some(email) = a["emailAddress"]["address"]
                    .as_str()
                    .filter(|v| v.contains('@'))
                {
                    let email = crate::identity::casefold(email);
                    result.push(json!({"id":email,"name":a["emailAddress"]["name"].as_str().unwrap_or(""),"emails":[email],"role":role}));
                }
            }
        }
    }
    result
}
pub fn search_body(body: &str, content_type: &str) -> String {
    let html = if content_type == "text/html" {
        html::text(body)
    } else {
        body.into()
    };
    let mut lines = Vec::new();
    static QUOTE_START: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(On .+wrote:|--\s*$|_{5,}|-{5,}Original Message)").unwrap());
    for line in html.lines() {
        if QUOTE_START.is_match(line) {
            break;
        }
        if !line.trim_start().starts_with('>') {
            lines.push(line.trim_end());
        }
    }
    lines.join("\n").trim().into()
}
pub fn normalize(
    mut record: Value,
    owners: &BTreeSet<String>,
    decision: &Decision,
) -> Result<Value> {
    let mut participants = addresses(&record);
    for p in &mut participants {
        p["is_self"] = json!(owners.contains(p["id"].as_str().unwrap_or("")));
    }
    let headers = record.get("headers").cloned().unwrap_or(json!({}));
    let body = record["body"].as_str().unwrap_or("").to_owned();
    let mid = headers["message-id"].as_str().unwrap_or("").trim();
    let attachments: Vec<_> = record["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| json!([a.get("filename"), a.get("blob")]))
        .collect();
    let dedup = if !mid.is_empty() && !body.is_empty() {
        Some(format!(
            "{:x}",
            Sha256::digest(python_json(
                &json!([
                    mid,
                    crate::identity::casefold(headers["from"].as_str().unwrap_or("")),
                    body.replace("\r\n", "\n"),
                    attachments
                ]),
                false
            ))
        ))
    } else {
        None
    };
    let occurred = record
        .get("occurred_at")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| {
            record["timestamp_ms"]
                .as_i64()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|v| json!(v.to_rfc3339()))
        })
        .or_else(|| {
            headers["date"]
                .as_str()
                .and_then(|v| chrono::DateTime::parse_from_rfc2822(v).ok())
                .map(|v| json!(v.with_timezone(&chrono::Utc).to_rfc3339()))
        })
        .unwrap_or(Value::Null);
    let thread = record["thread_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            headers["references"]
                .as_str()
                .and_then(|v| v.split(' ').next())
                .filter(|v| !v.is_empty())
        })
        .or_else(|| headers["message-id"].as_str().filter(|v| !v.is_empty()))
        .or_else(|| record["external_id"].as_str())
        .context("mail external_id required")?
        .to_owned();
    let direction = if participants
        .iter()
        .any(|p| p["role"] == "sender" && p["is_self"] == true)
    {
        "sent"
    } else {
        "received"
    };
    let searchable = search_body(&body, record["body_type"].as_str().unwrap_or("text/plain"));
    let additions = json!({"dedup_key":dedup,"kind":"message","participants":participants,"occurred_at":occurred,"direction":direction,"title":headers["subject"].as_str().unwrap_or(""),"category":decision.category,"classification_reason":decision.reason,"included":(["personal","transactional"].contains(&decision.category.as_str())),"thread_id":thread,"raw_body":body,"body":searchable});
    record
        .as_object_mut()
        .context("mail record must be object")?
        .extend(additions.as_object().unwrap().clone());
    Ok(record)
}
pub async fn archive_mime(
    store: &crate::media::MediaStore,
    cid: Uuid,
    raw: &[u8],
) -> Result<Value> {
    ensure!(raw.len() <= MAX_BODY, "mail MIME exceeds 16 MiB");
    let message = MessageParser::default()
        .parse(raw)
        .context("invalid MIME message")?;
    let mut headers = serde_json::Map::new();
    for (name, value) in message.headers_raw() {
        headers.insert(name.to_lowercase(), json!(value.trim()));
    }
    if let Some(subject) = message.subject() {
        headers.insert("subject".into(), json!(subject));
    }
    let (body, kind) = if message.text_body_count() > 0 {
        (
            message.body_text(0).unwrap_or_default().into_owned(),
            "text/plain",
        )
    } else {
        (
            message.body_html(0).unwrap_or_default().into_owned(),
            "text/html",
        )
    };
    let original = crate::source_blobs::archive_bytes(store, "mail", cid, raw).await?;
    let mut attachments = Vec::new();
    for part in message.attachments() {
        let blob = crate::source_blobs::archive_bytes(store, "mail", cid, part.contents()).await?;
        let content_type = part
            .content_type()
            .map(|t| {
                format!(
                    "{}/{}",
                    t.c_type,
                    t.c_subtype.as_deref().unwrap_or("octet-stream")
                )
            })
            .unwrap_or("application/octet-stream".into());
        attachments.push(json!({"filename":part.attachment_name(),"content_type":content_type,"blob":blob.blob,"size":blob.size}));
    }
    Ok(
        json!({"headers":headers,"body":body,"body_type":kind,"attachments":attachments,"original":{"blob":original.blob,"size":original.size,"filename":"message.eml","content_type":"message/rfc822"}}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headers_do_not_promote_unsubscribe_alone() {
        let r =
            json!({"headers":{"subject":"Hello","list-unsubscribe":"<https://example.invalid>"}});
        assert_eq!(classify(&r, None, false).unwrap().category, "pending");
        assert_eq!(classify(&r, None, true).unwrap().category, "personal");
        assert_eq!(
            classify(&r, Some("marketing"), true).unwrap().reason,
            "user_rule"
        );
    }
    #[test]
    fn originals_dedupe_but_different_authors_do_not() {
        let record = json!({"external_id":"m1","headers":{"message-id":"<id>","from":"Alice <a@example.invalid>","to":"b@example.invalid","subject":"invoice"},"body":"Hello\r\n> quoted\r\n--\r\nsignature","attachments":[]});
        let d = classify(&record, None, false).unwrap();
        let n = normalize(record.clone(), &BTreeSet::new(), &d).unwrap();
        assert_eq!(n["body"], "Hello");
        assert_eq!(n["participants"].as_array().unwrap().len(), 2);
        let mut other = record;
        other["headers"]["from"] = json!("other@example.invalid");
        assert_ne!(
            n["dedup_key"],
            normalize(other, &BTreeSet::new(), &d).unwrap()["dedup_key"]
        );
    }
}

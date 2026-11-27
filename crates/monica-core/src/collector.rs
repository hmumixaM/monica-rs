//! Collector target identities and bounded, read-only Chatlog transport.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use std::{collections::BTreeSet, time::Duration};
use uuid::Uuid;

pub fn casefold(value: &str) -> String {
    crate::identity::casefold(value)
}
pub fn canonical_linkedin(value: &str) -> Result<(String, String)> {
    let mut value = value.trim().to_string();
    ensure!(
        !value.is_empty() && value.len() <= 2048,
        "invalid LinkedIn account"
    );
    let lower = value.to_lowercase();
    if value.contains("://")
        || lower.starts_with("linkedin.com/")
        || lower
            .split('/')
            .next()
            .is_some_and(|s| s.ends_with(".linkedin.com"))
    {
        let url = url::Url::parse(&if value.contains("://") {
            value.clone()
        } else {
            format!("https://{value}")
        })?;
        let host = url.host_str().unwrap_or_default();
        let sub = host.strip_suffix(".linkedin.com");
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none_or(|p| p == 80 || p == 443)
                && (host == "linkedin.com"
                    || sub.is_some_and(|s| s == "www"
                        || ((2..=3).contains(&s.len())
                            && s.chars().all(|c| c.is_ascii_alphabetic())))),
            "invalid LinkedIn URL"
        );
        let parts = url.path().trim_matches('/').split('/').collect::<Vec<_>>();
        ensure!(
            parts.len() == 2 && parts[0] == "in",
            "not a LinkedIn person URL"
        );
        value = parts[1].into();
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position] == b'%'
            && position + 2 < bytes.len()
            && let Some(n) = std::str::from_utf8(&bytes[position + 1..position + 3])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
        {
            decoded.push(n);
            position += 3;
            continue;
        }
        decoded.push(bytes[position]);
        position += 1;
    }
    let slug = casefold(&String::from_utf8(decoded)?);
    ensure!(
        (1..=255).contains(&slug.chars().count())
            && slug != "."
            && slug != ".."
            && slug
                .chars()
                .all(|c| c.is_alphanumeric() || "_.~-".contains(c))
            && slug.chars().any(char::is_alphanumeric),
        "invalid LinkedIn slug"
    );
    let mut encoded = String::new();
    for b in slug.as_bytes() {
        if b.is_ascii_alphanumeric() || b"_.~-".contains(b) {
            encoded.push(*b as char)
        } else {
            encoded.push_str(&format!("%{b:02X}"))
        }
    }
    Ok((slug, format!("https://www.linkedin.com/in/{encoded}")))
}
pub struct LockedLinkedinAccount {
    id: Uuid,
    slug: String,
    url: String,
}

pub async fn lock_linkedin_account(
    tx: &mut Transaction<'_, Postgres>,
    account: &str,
) -> Result<LockedLinkedinAccount> {
    let (slug, url) = canonical_linkedin(account)?;
    let aid = crate::identity::account_id(crate::Provider::Linkedin, &slug)?;
    // Source identity is independent of its optional CRM link. Account lock first
    // matches import/relink ordering; set_links acquires sorted profile locks.
    sqlx::query("INSERT INTO linkedin.accounts(id,external_id,object_kind,display_name,username,profile_url) VALUES($1,$2,'person',$2,$2,$3) ON CONFLICT DO NOTHING").bind(aid).bind(&slug).bind(&url).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name,username,profile_url) VALUES($1,'linkedin',$2,'person',$2,$2,$3) ON CONFLICT DO NOTHING").bind(aid).bind(&slug).bind(&url).execute(&mut **tx).await?;
    sqlx::query("SELECT id FROM crm.source_accounts WHERE id=$1 FOR UPDATE")
        .bind(aid)
        .fetch_one(&mut **tx)
        .await?;
    Ok(LockedLinkedinAccount { id: aid, slug, url })
}

pub async fn save_linkedin(
    tx: &mut Transaction<'_, Postgres>,
    profile: Uuid,
    account: &str,
    actor: Uuid,
) -> Result<Value> {
    let account = lock_linkedin_account(tx, account).await?;
    save_linkedin_locked(tx, profile, account, actor).await
}

pub async fn save_linkedin_locked(
    tx: &mut Transaction<'_, Postgres>,
    profile: Uuid,
    account: LockedLinkedinAccount,
    actor: Uuid,
) -> Result<Value> {
    let LockedLinkedinAccount { id: aid, slug, url } = account;
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT profile_id FROM crm.source_account_links WHERE account_id=$1")
            .bind(aid)
            .fetch_optional(&mut **tx)
            .await?;
    ensure!(
        existing.is_none_or(|id| id == profile),
        "LinkedIn account belongs to another person"
    );
    let kind: Option<String> = sqlx::query_scalar(
        "SELECT profile_type FROM crm.profiles WHERE id=$1 AND archived_at IS NULL FOR UPDATE",
    )
    .bind(profile)
    .fetch_optional(&mut **tx)
    .await?;
    ensure!(
        kind.as_deref() == Some("person"),
        "LinkedIn targets require an active person"
    );
    crate::linking::set_links(
        tx,
        &[crate::linking::LinkChange {
            account_id: aid,
            profile_id: Some(profile),
            expected_profile_id: existing,
        }],
        Some(actor),
        &json!({"kind":"manual-linkedin-target","profile_url":url}),
    )
    .await?;
    sqlx::query("INSERT INTO crm.linkedin_collection_targets(account_id,created_by) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(aid).bind(actor).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'linkedin.collection.enabled','profile',$2,$3)").bind(actor).bind(profile).bind(json!({"account_id":aid})).execute(&mut **tx).await?;
    Ok(json!({"account_id":aid,"external_id":slug,"profile_url":url}))
}
pub async fn targets(pool: &PgPool) -> Result<Value> {
    let rows:Vec<String>=sqlx::query_scalar("SELECT a.external_id FROM crm.linkedin_collection_targets t JOIN crm.source_accounts a ON a.id=t.account_id JOIN crm.source_account_links l ON l.account_id=a.id JOIN crm.profiles p ON p.id=l.profile_id WHERE p.profile_type='person' AND p.archived_at IS NULL AND NOT EXISTS(SELECT 1 FROM crm.identities i WHERE i.source_account_id=a.id AND i.status='invalid') ORDER BY a.external_id").fetch_all(pool).await?;
    let mut targets = Vec::new();
    for row in rows {
        let (slug, url) = canonical_linkedin(&row)?;
        targets.push(json!({"profile_url":url,"external_id":slug,"metadata":{"provider_user_id":slug,"crm_manual":true}}));
    }
    Ok(json!({"version":1,"targets":targets}))
}
pub fn display_text(value: &str) -> String {
    let body = value
        .split_once(":\n")
        .map_or(value, |(_, body)| body)
        .trim();
    if let Some((_, rest)) = body.split_once("<title>")
        && let Some((title, _)) = rest.split_once("</title>")
    {
        return title.trim().into();
    }
    body.into()
}
pub fn filter_messages(
    messages: &[Value],
    keys: &BTreeSet<String>,
    query: &str,
    limit: usize,
) -> Vec<Value> {
    let needle = casefold(query.trim());
    messages.iter().filter_map(|m|{
        let sender=m["sender"].as_str().unwrap_or_default();let content=m["content"].as_str().unwrap_or_default();let prefix=content.split_once(":\n").map_or("",|(a,_)|a);
        if !keys.contains(&casefold(sender.trim()))&&!keys.contains(&casefold(prefix.trim())){return None}
        let text=display_text(content);if !needle.is_empty()&&!casefold(content).contains(&needle)&&!casefold(&text).contains(&needle){return None}
        Some(json!({"time":m["time"],"timestamp":m["timestamp"],"sender":sender,"text":text,"type":m["type"].as_str().unwrap_or_default()}))
    }).take(limit).collect()
}
/// Imported rows already passed the authoritative sender predicate in SQL.
pub fn imported_rows(messages: &[Value], query: &str, limit: usize) -> Vec<Value> {
    let needle = casefold(query.trim());
    messages
        .iter()
        .filter_map(|m| {
            let content = m["content"].as_str().unwrap_or_default();
            let text = display_text(content);
            if !needle.is_empty()
                && !casefold(content).contains(&needle)
                && !casefold(&text).contains(&needle)
            {
                return None;
            }
            Some(json!({"time":m["time"],"timestamp":null,"sender":"","text":text,"type":"text"}))
        })
        .take(limit)
        .collect()
}
pub struct Chatlog {
    client: reqwest::Client,
    base: url::Url,
}
impl Chatlog {
    pub fn new(base: &str) -> Result<Self> {
        let base = url::Url::parse(base)?;
        ensure!(
            matches!(base.scheme(), "https" | "http")
                && base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none(),
            "invalid Chatlog base URL"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(12))
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()?,
            base,
        })
    }
    pub async fn history(
        &self,
        chat: &str,
        since: &str,
        until: &str,
        when: &str,
    ) -> Result<Vec<Value>> {
        ensure!(chat.ends_with("@chatroom"), "missing Chatlog room identity");
        let mut endpoint = self.base.clone();
        endpoint.set_path(&format!(
            "{}/api/v1/history",
            self.base.path().trim_end_matches('/')
        ));
        {
            let mut params = endpoint.query_pairs_mut();
            params
                .append_pair("format", "json")
                .append_pair("chat", chat)
                .append_pair("limit", "2000");
            if !when.is_empty() {
                params.append_pair("time", when);
            } else {
                if !since.is_empty() {
                    params.append_pair("since", since);
                }
                if !until.is_empty() {
                    params.append_pair("until", until);
                }
            }
        }
        let mut response = self
            .client
            .get(endpoint)
            .send()
            .await
            .context("Chatlog unavailable")?;
        ensure!(response.status().is_success(), "Chatlog rejected history");
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                "Chatlog response exceeds budget"
            );
            bytes.extend_from_slice(&chunk)
        }
        let payload: Value = serde_json::from_slice(&bytes)?;
        Ok(payload
            .as_array()
            .or_else(|| payload["messages"].as_array())
            .context("invalid Chatlog response")?
            .iter()
            .filter(|m| m.is_object())
            .take(2000)
            .cloned()
            .collect())
    }
}

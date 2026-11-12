//! Source identities and canonical bytes from the pinned Python implementations.
//!
//! Source observations hash canonical JSON. Legacy CRM content_hash treats strings
//! as raw UTF-8, so callers must choose the correct boundary explicitly.
use crate::Provider;
use anyhow::{Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const SOURCE_NAMESPACE: Uuid = Uuid::from_u128(0x534b626897a14d99a086f8c9e3955815);

pub fn account_id(provider: Provider, external_id: &str) -> Result<Uuid> {
    if external_id.trim().is_empty() {
        bail!("An actual source account ID is required");
    }
    Ok(Uuid::new_v5(
        &SOURCE_NAMESPACE,
        format!("{}:account:{external_id}", provider.schema()).as_bytes(),
    ))
}

pub fn object_id(account: Uuid, kind: &str, external_id: &str) -> Uuid {
    Uuid::new_v5(&account, format!("{kind}:{external_id}").as_bytes())
}

/// Remove NULs recursively, including object keys, before sorting. Preserve input
/// key order until this step so colliding sanitized keys follow Python's last win.
pub fn json_safe(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(s.replace('\0', "")),
        Value::Array(items) => Value::Array(items.iter().map(json_safe).collect()),
        Value::Object(items) => Value::Object(
            items
                .iter()
                .map(|(key, value)| (key.replace('\0', ""), json_safe(value)))
                .collect(),
        ),
        // Python json.loads rounds fractional literals to binary64 while keeping
        // arbitrary-size integer literals exact. Normalize before persistence too.
        Value::Number(n) if n.is_f64() => Value::Number(
            serde_json::Number::from_f64(n.as_f64().expect("finite JSON float"))
                .expect("finite JSON float"),
        ),
        Value::Number(n) if n.to_string() == "-0" => Value::Number(0.into()),
        _ => value.clone(),
    }
}

/// Nonfinite Python JSON extensions are outside PostgreSQL JSON storage. Reject
/// overflow before source persistence rather than silently changing its hash.
pub fn validate_json(value: &Value) -> Result<()> {
    match value {
        Value::Number(n) => {
            let raw = n.to_string();
            if raw.contains(['.', 'e', 'E']) && !n.as_f64().is_some_and(f64::is_finite) {
                bail!("nonfinite JSON number");
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_json(value)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_json(value)?;
            }
        }
        _ => (),
    }
    Ok(())
}

pub fn canonical_json(value: &Value) -> Vec<u8> {
    let mut out = String::new();
    encode(&json_safe(value), &mut out);
    out.into_bytes()
}

fn encode(value: &Value, out: &mut String) {
    match value {
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                encode(item, out);
            }
            out.push(']');
        }
        Value::Object(items) => {
            let mut pairs: Vec<_> = items.iter().collect();
            pairs.sort_unstable_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (index, (key, value)) in pairs.iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("JSON key"));
                out.push(':');
                encode(value, out);
            }
            out.push('}');
        }
        Value::Number(number) if number.is_f64() => {
            out.push_str(&python_float(&number.to_string()));
        }
        _ => out.push_str(&serde_json::to_string(value).expect("JSON value")),
    }
}

/// Python and serde use shortest round-trip float digits but different notation
/// thresholds and exponent padding. Retain float type (1.0 and -0.0 included).
fn python_float(repr: &str) -> String {
    let (sign, unsigned) = repr.strip_prefix('-').map_or(("", repr), |s| ("-", s));
    let (mantissa, exp) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, 0), |(m, e)| (m, e.parse::<i32>().unwrap()));
    let decimal = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let raw = mantissa.replace('.', "");
    let trimmed = raw.trim_start_matches('0');
    if trimmed.is_empty() {
        return format!("{sign}0.0");
    }
    let leading = raw.len() - trimmed.len();
    let digits = trimmed.trim_end_matches('0');
    let exponent = exp + decimal - leading as i32 - 1;
    if !(-4..16).contains(&exponent) {
        let fraction = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!("{sign}{}{fraction}e{exponent:+03}", &digits[..1])
    } else {
        let point = exponent + 1;
        if point <= 0 {
            format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
        } else if point as usize >= digits.len() {
            format!(
                "{sign}{digits}{}.0",
                "0".repeat(point as usize - digits.len())
            )
        } else {
            format!(
                "{sign}{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        }
    }
}

pub fn observation_hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(canonical_json(value)))
}

pub fn content_hash(value: &Value) -> String {
    match value {
        Value::String(s) => bytes_hash(s.as_bytes()),
        _ => observation_hash(value),
    }
}

pub fn bytes_hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

/// Python 3.12 / Unicode 15 case-folding, frozen with the reference contract.
/// This is account presentation normalization, never a replacement source UUID.
pub fn casefold(text: &str) -> String {
    static FOLD: std::sync::OnceLock<std::collections::HashMap<char, String>> =
        std::sync::OnceLock::new();
    let mapping = FOLD.get_or_init(|| {
        let data: Value = serde_json::from_str(include_str!("../data/unicode-casefold.json"))
            .expect("bundled Unicode mapping");
        data["mapping"]
            .as_object()
            .expect("Unicode mapping")
            .iter()
            .map(|(key, value)| {
                (
                    key.chars().next().unwrap(),
                    value.as_str().unwrap().to_owned(),
                )
            })
            .collect()
    });
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if let Some(folded) = mapping.get(&ch) {
            result.push_str(folded);
        } else {
            result.push(ch);
        }
    }
    result
}

pub fn presented_identity_key(
    provider: &str,
    external: &str,
    profile_url: Option<&str>,
) -> Option<String> {
    if !["linkedin", "instagram"].contains(&provider) {
        return Some(external.to_owned());
    }
    for value in [profile_url, Some(external)]
        .into_iter()
        .flatten()
        .filter(|s| s.contains("://"))
    {
        let Ok(parsed) = url::Url::parse(value) else {
            continue;
        };
        let host = parsed.host_str().unwrap_or("").trim_start_matches("www.");
        let parts = parsed
            .path()
            .trim_matches('/')
            .split('/')
            .map(|part| {
                percent_encoding::percent_decode_str(part)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        if provider == "linkedin"
            && (host == "linkedin.com" || host.ends_with(".linkedin.com"))
            && parts.len() == 2
            && parts[0] == "in"
        {
            return Some(casefold(&parts[1]));
        }
        if provider == "instagram"
            && host == "instagram.com"
            && parts.len() == 1
            && !["p", "reel", "stories"].contains(&parts[0].as_str())
        {
            return Some(casefold(&parts[0]));
        }
    }
    let value = external.strip_prefix("username:").unwrap_or(external);
    let value = value.strip_prefix('@').unwrap_or(value);
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        return Some(casefold(value));
    }
    if provider == "linkedin" {
        if let Some(id) = value
            .strip_prefix("urn:li:fsd_profile:")
            .or_else(|| value.strip_prefix("urn:li:member:"))
            && !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
        {
            return Some(value.to_owned());
        }
        let decoded = percent_encoding::percent_decode_str(value).decode_utf8_lossy();
        if !decoded.is_empty()
            && decoded
                .chars()
                .all(|c| c.is_alphanumeric() || "_.~-".contains(c))
        {
            return Some(casefold(&decoded));
        }
    }
    None
}

pub fn present_identities(rows: Vec<Value>) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for mut row in rows {
        let provider = row["provider"].as_str().unwrap_or("").to_owned();
        if provider == "monica" {
            continue;
        }
        let Some(mut key) = presented_identity_key(
            &provider,
            row["external_id"].as_str().unwrap_or(""),
            row["profile_url"].as_str(),
        ) else {
            continue;
        };
        if provider == "instagram"
            && let Some(username) = row["username"].as_str().filter(|s| !s.is_empty())
        {
            key = casefold(username.strip_prefix('@').unwrap_or(username));
        }
        if provider == "linkedin" {
            row["profile_url"] = if key.starts_with("urn:") {
                Value::Null
            } else {
                Value::String(format!("https://www.linkedin.com/in/{key}"))
            };
        } else if provider == "instagram" {
            row["profile_url"] = if key.chars().all(|c| c.is_numeric()) {
                Value::Null
            } else {
                Value::String(format!("https://www.instagram.com/{key}/"))
            };
        } else if provider.starts_with("wechat") {
            row["profile_url"] = Value::Null;
        }
        if seen.insert((provider, key)) {
            result.push(row);
        }
    }
    result
}

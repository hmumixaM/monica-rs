//! Legacy CRM content addressing with atomic typed-collision resolution.
use crate::identity;
use anyhow::{Result, bail};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub struct ContentObject {
    pub id: Uuid,
    pub hash: String,
}

pub async fn register(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    value: &Value,
) -> Result<ContentObject> {
    Ok(register_many(tx, kind, std::slice::from_ref(value))
        .await?
        .remove(0))
}

/// Reserve all addresses in a deterministic order before a multi-field write.
/// Call this once for the entire transaction's values, not once per object.
pub async fn register_many(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    values: &[Value],
) -> Result<Vec<ContentObject>> {
    let mut keys = std::collections::BTreeSet::new();
    for value in values {
        identity::validate_json(value)?;
        let value = identity::json_safe(value);
        let canonical = identity::canonical_json(&value);
        let mut typed = b"json\0".to_vec();
        typed.extend_from_slice(&canonical);
        keys.insert(format!("{kind}:{}", identity::content_hash(&value)));
        keys.insert(format!("{kind}:{}", identity::bytes_hash(&typed)));
    }
    sqlx::query("SELECT crm.lock_content_addresses($1)")
        .bind(keys.into_iter().collect::<Vec<_>>())
        .execute(&mut **tx)
        .await?;
    let mut objects = Vec::with_capacity(values.len());
    for value in values {
        objects.push(register_locked(tx, kind, value).await?);
    }
    Ok(objects)
}

async fn register_locked(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    value: &Value,
) -> Result<ContentObject> {
    identity::validate_json(value)?;
    let value = identity::json_safe(value);
    let canonical = identity::canonical_json(&value);
    let bytes = value
        .as_str()
        .map_or_else(|| canonical.clone(), |s| s.as_bytes().to_vec());
    let base = identity::bytes_hash(&bytes);
    let mut typed = b"json\0".to_vec();
    typed.extend_from_slice(&canonical);
    let fallback = identity::bytes_hash(&typed);
    for hash in [base, fallback] {
        let id = Uuid::new_v4();
        let inserted:Option<Uuid>=sqlx::query_scalar("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json) VALUES($1,$2,$3,$4,$5) ON CONFLICT(content_kind,sha256) DO NOTHING RETURNING id")
            .bind(id).bind(kind).bind(&hash).bind(bytes.len() as i64).bind(&value).fetch_optional(&mut **tx).await?;
        if let Some(id) = inserted {
            return Ok(ContentObject { id, hash });
        }
        // A separate READ COMMITTED statement sees a concurrent winner after
        // INSERT's unique-index wait. Existing content is never overwritten.
        let (id, stored): (Uuid, Value) = sqlx::query_as(
            "SELECT id,payload_json FROM crm.content_objects WHERE content_kind=$1 AND sha256=$2",
        )
        .bind(kind)
        .bind(&hash)
        .fetch_one(&mut **tx)
        .await?;
        if identity::canonical_json(&stored) == canonical {
            return Ok(ContentObject { id, hash });
        }
    }
    bail!("content address has conflicting JSON content")
}

//! Legacy CRM content addressing with atomic typed-collision resolution.
use crate::identity;
use anyhow::{Result, bail};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet};
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

/// Reuse published addresses with SELECT; reserve missing addresses in a
/// deterministic order. Supply the transaction's values together, not per object.
pub async fn register_many(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    values: &[Value],
) -> Result<Vec<ContentObject>> {
    let mut addresses = Vec::with_capacity(values.len());
    let mut hashes = BTreeSet::new();
    for value in values {
        identity::validate_json(value)?;
        let value = identity::json_safe(value);
        let canonical = identity::canonical_json(&value);
        let mut typed = b"json\0".to_vec();
        typed.extend_from_slice(&canonical);
        let bytes = value
            .as_str()
            .map_or_else(|| canonical.as_slice(), str::as_bytes);
        let pair = [identity::bytes_hash(bytes), identity::bytes_hash(&typed)];
        hashes.extend(pair.iter().cloned());
        addresses.push((canonical, pair));
    }
    if values.is_empty() {
        return Ok(Vec::new());
    }
    // A published object cannot be rewritten or deleted. Its address and body
    // can therefore be reused using SELECT alone, even in a read-only tx.
    let rows: Vec<(String, Uuid, Value)> = sqlx::query_as(
        "SELECT sha256,id,payload_json FROM crm.content_objects WHERE content_kind=$1 AND sha256=ANY($2)",
    )
    .bind(kind)
    .bind(hashes.into_iter().collect::<Vec<_>>())
    .fetch_all(&mut **tx)
    .await?;
    let existing: BTreeMap<_, _> = rows
        .into_iter()
        .map(|(hash, id, value)| (hash, (id, identity::canonical_json(&value))))
        .collect();
    let found: Vec<_> = addresses
        .iter()
        .map(|(canonical, pair)| {
            pair.iter().find_map(|hash| {
                existing.get(hash).and_then(|(id, stored)| {
                    (stored == canonical).then(|| ContentObject {
                        id: *id,
                        hash: hash.clone(),
                    })
                })
            })
        })
        .collect();
    let keys: BTreeSet<_> = addresses
        .iter()
        .zip(&found)
        .filter(|(_, object)| object.is_none())
        .flat_map(|((_, pair), _)| pair.iter().map(|hash| format!("{kind}:{hash}")))
        .collect();
    if !keys.is_empty() {
        // Reserve all missing addresses together; reversed batches and typed
        // hash collisions retain the same deterministic writer lock order.
        sqlx::query("SELECT crm.lock_content_addresses($1)")
            .bind(keys.into_iter().collect::<Vec<_>>())
            .execute(&mut **tx)
            .await?;
    }
    let mut objects = Vec::with_capacity(values.len());
    for (value, object) in values.iter().zip(found) {
        objects.push(match object {
            Some(object) => object,
            None => register_locked(tx, kind, value).await?,
        });
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

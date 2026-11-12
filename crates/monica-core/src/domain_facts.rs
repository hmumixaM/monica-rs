//! Typed source facts retain source identity across reordering and account relinks.
use crate::identity;
use anyhow::{Result, ensure};
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
fn meaningful(v: &Value) -> bool {
    !v.is_null() && v != &Value::String(String::new()) && v.as_array().is_none_or(|v| !v.is_empty())
}
fn text(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
pub fn fact_key(source: &str, record: &str, kind: &str, value: &Value) -> String {
    let stable = if let Some(o) = value.as_object() {
        ["source_key", "id", "urn"]
            .iter()
            .find_map(|k| o.get(*k).filter(|v| meaningful(v)))
            .map(text)
            .unwrap_or_else(|| {
                let keys: &[&str] = match kind {
                    "employment" => &[
                        "company_name",
                        "company",
                        "linkedin_url",
                        "start_date",
                        "from_date",
                    ],
                    "education" => &[
                        "school_name",
                        "school",
                        "linkedin_url",
                        "degree",
                        "start_date",
                        "from_date",
                    ],
                    _ => &["label", "type", "address", "city"],
                };
                let subset: serde_json::Map<String, Value> = keys
                    .iter()
                    .filter_map(|k| {
                        o.get(*k)
                            .filter(|v| meaningful(v))
                            .map(|v| ((*k).into(), v.clone()))
                    })
                    .collect();
                if subset.is_empty() {
                    identity::observation_hash(value)
                } else {
                    identity::observation_hash(&Value::Object(subset))
                }
            })
    } else {
        text(value)
    };
    format!("{source}:{record}:{stable}")
}
fn label(value: &Value, kind: &str) -> Option<Vec<String>> {
    value.as_object()?;
    let keys: &[&str] = match kind {
        "employment" => &["title"],
        "education" => &["degree", "field_of_study"],
        _ => &["label", "type"],
    };
    Some(
        keys.iter()
            .map(|k| value[*k].as_str().unwrap_or("").trim().into())
            .collect(),
    )
}
fn collection_keys(
    source: &str,
    record: &str,
    kind: &str,
    values: &[Value],
    current: &BTreeMap<String, Value>,
) -> Result<Vec<(String, Value)>> {
    let mut groups: Vec<(String, Vec<(String, Value)>)> = vec![];
    for value in values {
        let base = fact_key(source, record, kind, value);
        let digest = identity::observation_hash(value);
        if let Some((_, entries)) = groups.iter_mut().find(|(b, _)| *b == base) {
            if !entries.iter().any(|(d, _)| *d == digest) {
                entries.push((digest, value.clone()));
            }
        } else {
            groups.push((base, vec![(digest, value.clone())]));
        }
    }
    let mut result = vec![];
    for (base, entries) in groups {
        let mut existing = current
            .iter()
            .filter(|(k, v)| {
                **k == base
                    || k.starts_with(&format!("{base}:entry:"))
                    || (k.starts_with(&format!("{source}:{record}:"))
                        && fact_key(source, record, kind, v) == base)
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>();
        existing.sort_by_key(|(k, _)| (k != &base, !k.contains(":entry:"), k.clone()));
        let mut seen = BTreeSet::new();
        existing.retain(|(_, v)| seen.insert(identity::observation_hash(v)));
        if entries.len() == 1
            && (existing.is_empty() || (existing.len() == 1 && existing[0].0 == base))
        {
            result.push((base, entries[0].1.clone()));
            continue;
        }
        ensure!(
            !entries.iter().any(|(_, v)| ["source_key", "id", "urn"]
                .iter()
                .any(|k| v.get(*k).is_some_and(meaningful))),
            "Distinct simultaneous facts share one explicit source ID"
        );
        let mut remaining = entries.clone();
        let mut assigned = BTreeMap::new();
        for (digest, _) in &entries {
            let matches = existing
                .iter()
                .enumerate()
                .filter(|(_, (_, v))| identity::observation_hash(v) == *digest)
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            if matches.len() == 1 {
                assigned.insert(digest.clone(), existing.remove(matches[0]).0);
                remaining.retain(|(d, _)| d != digest);
            }
        }
        for (digest, value) in remaining.clone() {
            let wanted = label(&value, kind);
            let candidates = existing
                .iter()
                .enumerate()
                .filter(|(_, (_, v))| wanted.is_some() && label(v, kind) == wanted)
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            if candidates.len() == 1
                && remaining
                    .iter()
                    .filter(|(_, v)| label(v, kind) == wanted)
                    .count()
                    == 1
            {
                assigned.insert(digest.clone(), existing.remove(candidates[0]).0);
                remaining.retain(|(d, _)| *d != digest);
            }
        }
        if remaining.len() == 1 && existing.len() == 1 {
            assigned.insert(remaining[0].0.clone(), existing[0].0.clone());
            remaining.clear();
            existing.clear();
        }
        ensure!(
            remaining.is_empty() || existing.is_empty(),
            "Ambiguous simultaneous fact edits need explicit source IDs"
        );
        for (digest, _) in remaining {
            let key = if !current.contains_key(&base) && !assigned.values().any(|k| k == &base) {
                base.clone()
            } else {
                format!("{base}:entry:{digest}")
            };
            assigned.insert(digest, key);
        }
        result.extend(
            entries
                .into_iter()
                .map(|(digest, value)| (assigned.remove(&digest).unwrap(), value)),
        );
    }
    Ok(result)
}
fn entity_name(kind: &str, value: &Value) -> Option<String> {
    if !["employment", "education"].contains(&kind) {
        return None;
    }
    if let Some(s) = value.as_str() {
        return Some(s.trim().into());
    }
    let keys: &[&str] = if kind == "employment" {
        &["company", "company_name", "employer", "name"]
    } else {
        &["school", "school_name", "institution", "name"]
    };
    keys.iter()
        .find_map(|k| {
            value.get(*k).filter(|v| meaningful(v)).map(|v| {
                v.as_str()
                    .or_else(|| v["name"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| text(v))
            })
        })
        .map(|s| s.trim().into())
}
pub struct Source<'a> {
    pub account: Uuid,
    pub profile: Option<Uuid>,
    pub source: &'a str,
    pub object: Uuid,
    pub version: i64,
    pub record: &'a str,
    pub observed: &'a str,
}
pub async fn project(
    tx: &mut Transaction<'_, Postgres>,
    context: Source<'_>,
    kind: &str,
    payload: &Value,
) -> Result<()> {
    let Source {
        account,
        profile,
        source,
        object,
        version,
        record,
        observed,
    } = context;
    let (table, column) = if profile.is_some() {
        ("notebook_structured_current", "profile_id")
    } else {
        ("source_structured_current", "source_account_id")
    };
    let source_account = if profile.is_none() {
        Some(account)
    } else {
        None
    };
    let mut sets = Vec::new();
    if ["address", "employment", "education"].contains(&kind) {
        sets.push((kind, vec![payload.clone()], true));
    } else if ["profile", "contact", "record"].contains(&kind) {
        let normalized = payload;
        for (kind, keys) in [
            (
                "address",
                vec!["address", "location", "locations", "addresses"],
            ),
            (
                "employment",
                vec![
                    "employment",
                    "experience",
                    "experiences",
                    "work_history",
                    "work",
                ],
            ),
            (
                "education",
                vec!["education", "educations", "education_history", "schools"],
            ),
        ] {
            if let Some(value) = keys
                .iter()
                .find_map(|k| normalized.get(*k).filter(|v| meaningful(v)))
            {
                sets.push((
                    kind,
                    value
                        .as_array()
                        .cloned()
                        .unwrap_or_else(|| vec![value.clone()]),
                    false,
                ));
            }
        }
    }
    for (kind, values, direct) in sets {
        let rows=sqlx::query(&format!("SELECT c.fact_key,r.value FROM crm.{table} c JOIN crm.source_structured_revisions r ON r.id=c.revision_id WHERE c.{column}=$1 AND c.fact_type=$2")).bind(account).bind(kind).fetch_all(&mut **tx).await?;
        let current = rows
            .iter()
            .map(|r| (r.get("fact_key"), r.get("value")))
            .collect();
        let entries = if direct {
            vec![(record.into(), payload.clone())]
        } else {
            collection_keys(source, record, kind, &values, &current)?
        };
        let content_values = entries.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>();
        let contents =
            crate::content::register_many(tx, &format!("structured_{kind}"), &content_values)
                .await?;
        let organization_kind = if kind == "employment" {
            "company"
        } else {
            "school"
        };
        let mut organizations = BTreeMap::new();
        for (_, value) in &entries {
            if let Some(name) = entity_name(kind, value) {
                let normalized = name
                    .to_lowercase()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !normalized.is_empty() {
                    organizations.insert(normalized, name);
                }
            }
        }
        let mut entity_ids = BTreeMap::new();
        for (normalized, name) in organizations {
            let id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("monica:{organization_kind}:{normalized}").as_bytes(),
            );
            let id:Uuid=sqlx::query_scalar("INSERT INTO crm.organizations(id,kind,normalized_name,name) VALUES($1,$2,$3,$4) ON CONFLICT(kind,normalized_name) DO UPDATE SET name=excluded.name RETURNING id").bind(id).bind(organization_kind).bind(&normalized).bind(name).fetch_one(&mut **tx).await?;
            entity_ids.insert(normalized, id);
        }
        for (position, ((key, value), content)) in entries.into_iter().zip(contents).enumerate() {
            let previous:Option<(Uuid,Value)>=sqlx::query_as(&format!("SELECT r.id,r.value FROM crm.{table} c JOIN crm.source_structured_revisions r ON r.id=c.revision_id WHERE c.{column}=$1 AND c.fact_type=$2 AND c.fact_key=$3")).bind(account).bind(kind).bind(&key).fetch_optional(&mut **tx).await?;
            if previous.as_ref().is_some_and(|(_, v)| {
                identity::canonical_json(v) == identity::canonical_json(&value)
            }) {
                continue;
            }
            let entity = entity_name(kind, &value).and_then(|name| {
                entity_ids
                    .get(
                        &name
                            .to_lowercase()
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                    .copied()
            });
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO crm.source_structured_revisions(id,source_account_id,fact_type,fact_key,content_object_id,content_hash,value,source_type,source_record_id,object_id,source_version,observed_at,previous_revision_id,position,entity_id,notebook_profile_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12::timestamptz,$13,$14,$15,$16)").bind(id).bind(source_account).bind(kind).bind(&key).bind(content.id).bind(content.hash).bind(value).bind(source).bind(record).bind(object).bind(version).bind(observed).bind(previous.map(|p|p.0)).bind(position as i32).bind(entity).bind(profile).execute(&mut **tx).await?;
            sqlx::query(&format!("INSERT INTO crm.{table} VALUES($1,$2,$3,$4) ON CONFLICT({column},fact_type,fact_key) DO UPDATE SET revision_id=excluded.revision_id")).bind(account).bind(kind).bind(key).bind(id).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

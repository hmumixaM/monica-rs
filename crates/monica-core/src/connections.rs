//! Connection-scoped immutable journals and atomic CRM delivery.
use crate::{Provider, identity};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeSet;
use uuid::Uuid;
#[path = "connections_social.rs"]
mod social;

pub fn catalogue() -> Value {
    json!([
 {"id":"wechat","name":"微信","capabilities":["contacts","messages","posts","media"],"connection_methods":["local"]},
 {"id":"instagram","name":"Instagram","capabilities":["contacts","posts","media"],"connection_methods":["browser"]},
 {"id":"linkedin","name":"LinkedIn","capabilities":["contacts","messages","posts","media"],"connection_methods":["browser"]},
 {"id":"whatsapp","name":"WhatsApp","capabilities":["contacts","messages","media"],"connection_methods":["qr","archive"]},
 {"id":"apple_messages","name":"Messages","capabilities":["messages","media"],"connection_methods":["local"]},
 {"id":"apple_contacts","name":"Contacts","capabilities":["contacts"],"connection_methods":["local"]},
 {"id":"mail","name":"邮件","capabilities":["messages","media"],"connection_methods":["gmail","outlook","imap"]}])
}
pub fn connection_id(provider: Provider, external: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::from_u128(0xa874aa7b06504950ac70f979c6b020eb),
        format!("{}:{external}", provider.schema()).as_bytes(),
    )
}
pub fn evidence_id(connection: Uuid, external: &str) -> Uuid {
    Uuid::new_v5(&connection, format!("evidence:{external}").as_bytes())
}
pub fn normalize_identifier(kind: &str, value: &str) -> Option<String> {
    let value = value.trim();
    match kind {
        "email" => {
            let (local, domain) = value
                .rsplit_once('@')
                .map_or((value, None), |(a, b)| (a, Some(b)));
            let local = casefold(local);
            Some(match domain {
                Some(d) => format!(
                    "{local}@{}",
                    idna::domain_to_ascii(&casefold(d)).ok()?.to_lowercase()
                ),
                None => local,
            })
        }
        "phone" => {
            static DIGITS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            let digits_table = DIGITS.get_or_init(|| {
                serde_json::from_str(include_str!("../data/connection-digits.json"))
                    .expect("digit table")
            });
            let digits: String = value
                .chars()
                .filter(|c| digits_table.contains(*c))
                .collect();
            (value.starts_with('+') && (7..=15).contains(&digits.chars().count()))
                .then(|| format!("+{digits}"))
        }
        _ => Some(value.to_owned()),
    }
}
fn casefold(value: &str) -> String {
    static SPECIAL: std::sync::OnceLock<std::collections::BTreeMap<String, String>> =
        std::sync::OnceLock::new();
    let special = SPECIAL.get_or_init(|| {
        serde_json::from_str(include_str!("../data/connection-casefold.json"))
            .expect("casefold table")
    });
    value
        .chars()
        .map(|c| {
            special
                .get(&c.to_string())
                .cloned()
                .unwrap_or_else(|| c.to_lowercase().collect())
        })
        .collect()
}

fn truth(value: Option<&Value>, default: bool) -> bool {
    match value {
        None => default,
        Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}
fn message_direction(value: &Value, profile: Option<Uuid>, sender: Option<Uuid>) -> &'static str {
    match value.get("direction").and_then(Value::as_str) {
        Some("received") => "received",
        Some("sent") => "sent",
        Some("unknown") => "unknown",
        _ => {
            if profile.is_some() && sender == profile {
                "received"
            } else {
                "sent"
            }
        }
    }
}
fn message_text(value: &Value) -> &str {
    value
        .get("text")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| string(value, "body", ""))
}
fn string<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(default)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBinding {
    pub profile_id: Option<Uuid>,
    pub sender_profile_id: Option<Uuid>,
    pub conversation_type: String,
    #[serde(default)]
    pub infer_profile: bool,
    #[serde(default)]
    pub infer_sender: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub sequence: i64,
    pub connection_id: Uuid,
    pub external_id: String,
    pub revision: i64,
    pub content_hash: String,
    pub payload: Value,
    pub observed_at: String,
    #[serde(default)]
    pub binding: Option<MessageBinding>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fence {
    pub connection_id: Uuid,
    pub owner: Uuid,
    pub generation: i64,
}
pub async fn verify_fence(tx: &mut Transaction<'_, Postgres>, fence: &Fence) -> Result<()> {
    lock_connection(tx, fence.connection_id).await?;
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.source_connections WHERE id=$1 AND lease_owner=$2 AND generation=$3 AND enabled AND lease_until>clock_timestamp() FOR UPDATE)").bind(fence.connection_id).bind(fence.owner).bind(fence.generation).fetch_one(&mut **tx).await?;
    if !valid {
        bail!("connection lease lost")
    }
    Ok(())
}
pub async fn lock_connection(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("connection:{id}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub async fn commit_page(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    stream: &str,
    records: &[Value],
    next_cursor: &Value,
) -> Result<u64> {
    commit_page_impl(tx, provider, cid, stream, records, next_cursor, true).await
}
pub(crate) async fn commit_import_page(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    stream: &str,
    records: &[Value],
    next_cursor: &Value,
) -> Result<u64> {
    commit_page_impl(tx, provider, cid, stream, records, next_cursor, false).await
}
async fn commit_page_impl(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    stream: &str,
    records: &[Value],
    next_cursor: &Value,
    track: bool,
) -> Result<u64> {
    lock_connection(tx, cid).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM crm.source_connections WHERE id=$1 AND provider=$2)",
    )
    .bind(cid)
    .bind(provider.schema())
    .fetch_one(&mut **tx)
    .await?;
    if !exists {
        bail!("connection provider mismatch")
    }
    use std::collections::BTreeMap;
    let s = provider.schema();
    let externals: Vec<&str> = records
        .iter()
        .map(|r| {
            r.get("external_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .context("record external_id required")
        })
        .collect::<Result<_>>()?;
    let prior:Vec<(String,i64,String)>=sqlx::query_as(&format!("SELECT external_id,revision,content_hash FROM {s}.connection_record_heads WHERE connection_id=$1 AND external_id=ANY($2)")).bind(cid).bind(&externals).fetch_all(&mut **tx).await?;
    let mut heads: BTreeMap<String, (i64, String)> =
        prior.into_iter().map(|(id, r, h)| (id, (r, h))).collect();
    let mut payloads: BTreeMap<String, Value> = BTreeMap::new();
    let mut refs = Vec::new();
    let mut changed_heads = BTreeMap::new();
    for (record, external) in records.iter().zip(externals) {
        identity::validate_json(record)?;
        let hash = identity::bytes_hash(&identity::canonical_json(record));
        let previous = heads.get(external);
        if previous.is_some_and(|(_, h)| h == &hash) {
            continue;
        }
        let revision = previous.map_or(1, |(r, _)| r + 1);
        payloads
            .entry(hash.clone())
            .or_insert_with(|| record.clone());
        refs.push((external.to_owned(), revision, hash.clone()));
        changed_heads.insert(external.to_owned(), (revision, hash.clone()));
        heads.insert(external.to_owned(), (revision, hash));
    }
    let changed = refs.len() as u64;
    if changed > 0 {
        let hashes: Vec<_> = payloads.keys().cloned().collect();
        let values: Vec<_> = payloads.into_values().collect();
        sqlx::query(&format!("INSERT INTO {s}.connection_payloads SELECT * FROM unnest($1::text[],$2::jsonb[]) ORDER BY 1 ON CONFLICT DO NOTHING")).bind(hashes).bind(values).execute(&mut **tx).await?;
        let ids: Vec<_> = refs.iter().map(|r| r.0.clone()).collect();
        let versions: Vec<_> = refs.iter().map(|r| r.1).collect();
        let hashes: Vec<_> = refs.iter().map(|r| r.2.clone()).collect();
        sqlx::query(&format!("INSERT INTO {s}.connection_revision_refs(connection_id,external_id,revision,content_hash) SELECT $1,* FROM unnest($2::text[],$3::bigint[],$4::text[])")).bind(cid).bind(&ids).bind(&versions).bind(hashes).execute(&mut **tx).await?;
        let last_ids: Vec<_> = changed_heads.keys().cloned().collect();
        let last_versions: Vec<_> = changed_heads.values().map(|r| r.0).collect();
        let last_hashes: Vec<_> = changed_heads.values().map(|r| r.1.clone()).collect();
        sqlx::query(&format!("INSERT INTO {s}.connection_record_heads(connection_id,external_id,revision,content_hash) SELECT $1,* FROM unnest($2::text[],$3::bigint[],$4::text[]) ON CONFLICT(connection_id,external_id) DO UPDATE SET revision=excluded.revision,content_hash=excluded.content_hash,updated_at=now()")).bind(cid).bind(last_ids).bind(last_versions).bind(last_hashes).execute(&mut **tx).await?;
        sqlx::query(&format!("INSERT INTO {s}.connection_outbox(connection_id,external_id,revision) SELECT $1,id,version FROM unnest($2::text[],$3::bigint[]) WITH ORDINALITY r(id,version,position) ORDER BY position")).bind(cid).bind(ids).bind(versions).execute(&mut **tx).await?;
    }
    if track {
        // Stable page identity includes committed revision numbers, so retries do
        // not inflate counts while A-B-A source changes remain distinct.
        let page_hash = identity::bytes_hash(&identity::canonical_json(&json!([
            records,
            next_cursor,
            heads
        ])));
        let inserted = refs.iter().filter(|r| r.1 == 1).count() as i64;
        sqlx::query("INSERT INTO crm.import_activity(id,provider,connection_id,stream,kind,state,received_at,records,inserted,changed,unchanged) VALUES($1,$2,$3,$4,'page','saved',now(),$5,$6,$7,$8) ON CONFLICT DO NOTHING")
        .bind(format!("page:{cid}:{stream}:{page_hash}" )).bind(s).bind(cid).bind(stream)
        .bind(records.len() as i64).bind(inserted).bind(changed as i64-inserted).bind(records.len() as i64-changed as i64).execute(&mut **tx).await?;
    }
    set_cursor(tx, provider, cid, stream, next_cursor).await?;
    Ok(changed)
}
pub async fn commit_page_fenced(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    fence: &Fence,
    stream: &str,
    records: &[Value],
    next_cursor: &Value,
) -> Result<u64> {
    verify_fence(tx, fence).await?;
    commit_page(
        tx,
        provider,
        fence.connection_id,
        stream,
        records,
        next_cursor,
    )
    .await
}
pub async fn cursor(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    stream: &str,
) -> Result<Value> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT cursor FROM {}.connection_cursors WHERE connection_id=$1 AND stream=$2",
        provider.schema()
    ))
    .bind(cid)
    .bind(stream)
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or(json!({})))
}
pub async fn set_cursor(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    stream: &str,
    value: &Value,
) -> Result<()> {
    sqlx::query(&format!("INSERT INTO {}.connection_cursors VALUES($1,$2,$3) ON CONFLICT(connection_id,stream) DO UPDATE SET cursor=excluded.cursor",provider.schema())).bind(cid).bind(stream).bind(value).execute(&mut **tx).await?;
    Ok(())
}
pub async fn pending(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    limit: i64,
) -> Result<Vec<Revision>> {
    pending_bounded(tx, provider, cid, limit, 64 * 1024 * 1024).await
}
pub async fn pending_bounded(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    limit: i64,
    max_bytes: i64,
) -> Result<Vec<Revision>> {
    let s = provider.schema();
    let rows=sqlx::query(&format!("WITH candidates AS MATERIALIZED(SELECT o.sequence,r.external_id,r.revision,r.content_hash,r.observed_at,p.byte_length FROM {s}.connection_outbox o JOIN {s}.connection_revision_refs r USING(connection_id,external_id,revision) JOIN {s}.connection_payloads p USING(content_hash) WHERE o.connection_id=$1 AND o.delivered_at IS NULL ORDER BY o.sequence LIMIT $2),sized AS MATERIALIZED(SELECT *,sum(byte_length) OVER(ORDER BY sequence) total,row_number() OVER(ORDER BY sequence) position FROM candidates),chosen AS MATERIALIZED(SELECT * FROM sized WHERE total<=$3 OR position=1) SELECT c.*,c.observed_at::text observed,p.payload,CASE WHEN b.connection_id IS NULL THEN NULL ELSE to_jsonb(b)||jsonb_build_object('profile_id',crm.canonical_profile(b.profile_id),'sender_profile_id',crm.canonical_profile(b.sender_profile_id)) END binding FROM chosen c JOIN {s}.connection_payloads p USING(content_hash) LEFT JOIN crm.connection_import_bindings b ON b.connection_id=$1 AND b.external_id=c.external_id AND b.revision=c.revision ORDER BY c.sequence")).bind(cid).bind(limit.clamp(1,1000)).bind(max_bytes.max(1)).fetch_all(&mut **tx).await?;
    Ok(rows
        .into_iter()
        .map(|r| Revision {
            sequence: r.get("sequence"),
            connection_id: cid,
            external_id: r.get("external_id"),
            revision: r.get("revision"),
            content_hash: r.get("content_hash"),
            payload: r.get("payload"),
            observed_at: r.get("observed"),
            binding: r
                .get::<Option<Value>, _>("binding")
                .map(|v| serde_json::from_value(v).expect("typed binding")),
        })
        .collect())
}
pub async fn acknowledge(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    sequences: &[i64],
) -> Result<()> {
    sqlx::query(&format!("UPDATE {}.connection_outbox SET delivered_at=now() WHERE connection_id=$1 AND sequence=ANY($2)",provider.schema())).bind(cid).bind(sequences).execute(&mut **tx).await?;
    Ok(())
}
pub async fn deliver(pool: &PgPool, provider: Provider, cid: Uuid, limit: i64) -> Result<usize> {
    deliver_bounded(pool, provider, cid, limit, 64 * 1024 * 1024).await
}
pub async fn deliver_bounded(
    pool: &PgPool,
    provider: Provider,
    cid: Uuid,
    limit: i64,
    max_bytes: i64,
) -> Result<usize> {
    let mut tx = pool.begin().await?;
    lock_connection(&mut tx, cid).await?;
    let rows = pending_bounded(&mut tx, provider, cid, limit, max_bytes).await?;
    if !fast::project_new_messages(&mut tx, provider, cid, &rows).await? {
        reserve_projection(&mut tx, cid, &rows).await?;
        for row in &rows {
            project_revision_reserved(&mut tx, provider, row, false).await?;
        }
    }
    crate::ai::memory::connection_evidence_many(
        &mut tx,
        &rows
            .iter()
            .map(|r| evidence_id(cid, &r.external_id))
            .collect::<Vec<_>>(),
    )
    .await?;
    acknowledge(
        &mut tx,
        provider,
        cid,
        &rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
    )
    .await?;
    sqlx::query(&format!("UPDATE crm.import_activity SET state='complete',processed=records,completed_at=now(),updated_at=now() WHERE connection_id=$1 AND kind IN ('page','batch') AND state='saved' AND NOT EXISTS(SELECT 1 FROM {s}.connection_outbox WHERE connection_id=$1 AND delivered_at IS NULL)",s=provider.schema())).bind(cid).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(rows.len())
}

pub async fn resolve_person(
    tx: &mut Transaction<'_, Postgres>,
    cid: Uuid,
    p: &Value,
    contact: bool,
) -> Result<Option<Uuid>> {
    let external = string(p, "id", "");
    if let Some((profile,blocked))=sqlx::query_as::<_,(Option<Uuid>,bool)>("SELECT profile_id,blocked FROM crm.connection_identity_links WHERE connection_id=$1 AND external_id=$2 AND confirmed").bind(cid).bind(external).fetch_optional(&mut **tx).await?{return Ok(if blocked{None}else{profile})}
    sqlx::query(
        "DELETE FROM crm.connection_identity_candidates WHERE connection_id=$1 AND external_id=$2",
    )
    .bind(cid)
    .bind(external)
    .execute(&mut **tx)
    .await?;
    let mut found = BTreeSet::new();
    let mut ambiguous = false;
    for (kind, value) in identifiers(p) {
        if contact {
            ambiguous|=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM crm.connection_identifiers WHERE connection_id=$1 AND external_id<>$2 AND kind=$3 AND value=$4 AND active)").bind(cid).bind(external).bind(&kind).bind(&value).fetch_one(&mut **tx).await?;
        }
        let matches:Vec<Uuid>=sqlx::query_scalar("SELECT profile_id FROM crm.person_identifiers WHERE kind=$1 AND value=$2 UNION SELECT profile_id FROM crm.connection_identifiers WHERE kind=$1 AND value=$2 AND active").bind(kind).bind(value).fetch_all(&mut **tx).await?;
        ambiguous |= matches.len() > 1;
        found.extend(matches);
    }
    if found.len() == 1 && !ambiguous {
        return Ok(found.first().copied());
    }
    for profile in found {
        sqlx::query("INSERT INTO crm.connection_identity_candidates VALUES($1,$2,$3,'conflicting_identifier') ON CONFLICT DO NOTHING").bind(cid).bind(external).bind(profile).execute(&mut **tx).await?;
    }
    Ok(None)
}
fn identifiers(p: &Value) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for kind in ["email", "phone"] {
        for value in p
            .get(format!("{kind}s"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(v) = value.as_str().and_then(|v| normalize_identifier(kind, v)) {
                out.insert((kind.into(), v));
            }
        }
    }
    out
}
pub async fn queue_identifier(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    value: &str,
) -> Result<u64> {
    let Some(value) = normalize_identifier(kind, value) else {
        return Ok(0);
    };
    let pattern = if kind == "source" {
        json!({"participants":[{"id":value}]})
    } else {
        json!({"participants":[{format!("{kind}s"):[value]}]})
    };
    let rows=sqlx::query("INSERT INTO crm.connection_relink(evidence_id) SELECT id FROM crm.connection_evidence WHERE metadata @> $1 ON CONFLICT(evidence_id) DO UPDATE SET requested_at=now() RETURNING evidence_id").bind(pattern).fetch_all(&mut **tx).await?;

    Ok(rows.len() as u64)
}
async fn owned_identifiers(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<BTreeSet<(String, String)>> {
    let mut owned: BTreeSet<(String, String)> =
        sqlx::query_as("SELECT kind,value FROM crm.person_identifiers WHERE is_self")
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .collect();
    let mail: Vec<(String, Value)> = sqlx::query_as(
        "SELECT external_account,settings FROM crm.source_connections WHERE provider='mail'",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (account, settings) in mail {
        for a in std::iter::once(&Value::String(account)).chain(
            settings
                .get("aliases")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        ) {
            if let Some(v) = a.as_str().and_then(|a| normalize_identifier("email", a)) {
                owned.insert(("email".into(), v));
            }
        }
    }
    Ok(owned)
}
async fn ownership(tx: &mut Transaction<'_, Postgres>, value: Value) -> Result<Value> {
    Ok(apply_ownership(value, &owned_identifiers(tx).await?))
}
fn apply_ownership(mut value: Value, owned: &BTreeSet<(String, String)>) -> Value {
    if let Some(ps) = value.get_mut("participants").and_then(Value::as_array_mut) {
        for p in ps {
            let own = truth(p.get("is_self"), false)
                || owned.contains(&("source".into(), string(p, "id", "").into()))
                || identifiers(p).iter().any(|i| owned.contains(i));
            for kind in ["email", "phone"] {
                let values: Vec<Value> = p
                    .get(format!("{kind}s"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|v| {
                        v.as_str()
                            .and_then(|v| normalize_identifier(kind, v))
                            .map(Value::String)
                    })
                    .collect();
                p[format!("{kind}s")] = json!(values);
            }
            p["is_self"] = json!(own);
        }
    }
    let unresolved_direction = value.get("direction").and_then(Value::as_str) == Some("unknown")
        && !value
            .get("participants")
            .and_then(Value::as_array)
            .is_some_and(|ps| ps.iter().any(|p| truth(p.get("is_self"), false)));
    if !unresolved_direction
        && let Some(sender) = value
            .get("participants")
            .and_then(Value::as_array)
            .and_then(|p| p.iter().find(|p| string(p, "role", "") == "sender"))
    {
        value["direction"] = json!(if truth(sender.get("is_self"), false) {
            "sent"
        } else {
            "received"
        });
    }
    value
}

async fn reserve_projection(
    tx: &mut Transaction<'_, Postgres>,
    cid: Uuid,
    rows: &[Revision],
) -> Result<()> {
    lock_connection(tx, cid).await?;
    let participants: Vec<&Value> = rows
        .iter()
        .flat_map(|r| {
            r.payload
                .get("participants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .collect();
    let keys: BTreeSet<_> = participants
        .iter()
        .flat_map(|p| {
            identifiers(p).into_iter().chain(std::iter::once((
                "source".into(),
                string(p, "id", "").into(),
            )))
        })
        .collect();
    let mut profiles = BTreeSet::new();
    for (kind, value) in keys {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("identifier:{kind}:{value}"))
            .execute(&mut **tx)
            .await?;
        let ids:Vec<Uuid>=sqlx::query_scalar("SELECT profile_id FROM crm.person_identifiers WHERE kind=$1 AND value=$2 UNION SELECT profile_id FROM crm.connection_identifiers WHERE kind=$1 AND value=$2 AND active").bind(kind).bind(value).fetch_all(&mut **tx).await?;
        profiles.extend(ids);
    }
    let external: Vec<&str> = participants.iter().map(|p| string(p, "id", "")).collect();
    let explicit:Vec<Uuid>=sqlx::query_scalar("SELECT profile_id FROM crm.connection_identity_links WHERE connection_id=$1 AND external_id=ANY($2) AND profile_id IS NOT NULL").bind(cid).bind(external).fetch_all(&mut **tx).await?;
    profiles.extend(explicit);
    let evidence: Vec<Uuid> = rows
        .iter()
        .map(|r| evidence_id(cid, &r.external_id))
        .collect();
    let previous: Vec<Uuid> = sqlx::query_scalar(
        "SELECT profile_id FROM crm.connection_evidence_people WHERE evidence_id=ANY($1)",
    )
    .bind(evidence)
    .fetch_all(&mut **tx)
    .await?;
    profiles.extend(previous);
    profiles.extend(
        rows.iter()
            .filter_map(|r| r.binding.as_ref())
            .flat_map(|b| [b.profile_id, b.sender_profile_id].into_iter().flatten()),
    );
    sqlx::query("SELECT id FROM crm.profiles WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(profiles.into_iter().collect::<Vec<_>>())
        .fetch_all(&mut **tx)
        .await?;
    let fields: Vec<Value> = rows
        .iter()
        .filter(|r| string(&r.payload, "kind", "") == "contact")
        .flat_map(|r| {
            ["emails", "phones", "organization", "job_title", "birthday"]
                .into_iter()
                .filter_map(|k| r.payload.get(k).cloned())
                .chain(
                    r.payload
                        .get("fields")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flat_map(|fields| fields.values().cloned()),
                )
        })
        .collect();
    crate::content::register_many(tx, "field_value", &fields).await?;
    Ok(())
}
pub async fn project_revision(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    row: &Revision,
    force: bool,
) -> Result<bool> {
    reserve_projection(tx, row.connection_id, std::slice::from_ref(row)).await?;
    let changed = project_revision_reserved(tx, provider, row, force).await?;
    if changed {
        crate::ai::memory::connection_evidence(
            tx,
            evidence_id(row.connection_id, &row.external_id),
        )
        .await?;
    }
    Ok(changed)
}
async fn project_revision_reserved(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    row: &Revision,
    force: bool,
) -> Result<bool> {
    let cid = row.connection_id;
    let eid = evidence_id(cid, &row.external_id);
    lock_connection(tx, cid).await?;
    let previous: Option<(i64, Option<Uuid>)> =
        sqlx::query_as("SELECT revision,conversation_id FROM crm.connection_evidence WHERE id=$1")
            .bind(eid)
            .fetch_optional(&mut **tx)
            .await?;
    if previous
        .as_ref()
        .is_some_and(|(r, _)| *r > row.revision || (*r == row.revision && !force))
    {
        return Ok(false);
    }
    let value = ownership(tx, row.payload.clone()).await?;
    let deleted = truth(value.get("deleted"), false);
    let included = truth(value.get("included"), true) && !deleted;
    let kind = string(&value, "kind", "message");
    let mut metadata = value.clone();
    for key in ["body", "raw_body", "raw_message", "raw_revoke"] {
        metadata
            .as_object_mut()
            .context("record must be object")?
            .remove(key);
    }
    let at = value.get("occurred_at").and_then(Value::as_str);
    sqlx::query("INSERT INTO crm.connection_evidence(id,connection_id,external_id,revision,content_hash,kind,occurred_at,title,body,category,included,deleted,metadata,dedup_key) VALUES($1,$2,$3,$4,$5,$6,$7::text::timestamptz,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,content_hash=excluded.content_hash,occurred_at=coalesce(excluded.occurred_at,crm.connection_evidence.occurred_at),title=excluded.title,body=excluded.body,category=excluded.category,included=excluded.included,deleted=excluded.deleted,metadata=excluded.metadata,dedup_key=excluded.dedup_key")
 .bind(eid).bind(cid).bind(&row.external_id).bind(row.revision).bind(&row.content_hash).bind(kind).bind(at).bind(string(&value,"title","")).bind(if included{string(&value,"body","")}else{""}).bind(value.get("category").and_then(Value::as_str)).bind(included).bind(deleted).bind(&metadata).bind(if provider.schema()=="mail"{value.get("dedup_key").and_then(Value::as_str)}else{None}).execute(&mut **tx).await?;
    let mut affected: BTreeSet<Uuid> = sqlx::query_scalar(
        "SELECT profile_id FROM crm.connection_evidence_people WHERE evidence_id=$1",
    )
    .bind(eid)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    sqlx::query("DELETE FROM crm.connection_evidence_people WHERE evidence_id=$1")
        .bind(eid)
        .execute(&mut **tx)
        .await?;
    let mut changed_ids: BTreeSet<(String, String)> = if kind == "contact" {
        sqlx::query_as("UPDATE crm.connection_identifiers SET active=false WHERE connection_id=$1 AND external_id=$2 AND active RETURNING kind,value").bind(cid).bind(&row.external_id).fetch_all(&mut **tx).await?.into_iter().collect()
    } else {
        BTreeSet::new()
    };
    let participants = value
        .get("participants")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut people = vec![];
    for p in &participants {
        let external = string(p, "id", "");
        if external.is_empty() {
            bail!("participant id required")
        }
        let name = string(p, "name", external);
        let own = truth(p.get("is_self"), false);
        sqlx::query("INSERT INTO crm.connection_people(connection_id,external_id,display_name,is_self) VALUES($1,$2,$3,$4) ON CONFLICT(connection_id,external_id) DO UPDATE SET display_name=excluded.display_name,is_self=excluded.is_self").bind(cid).bind(external).bind(name).bind(own).execute(&mut **tx).await?;
        let (blocked,official):(bool,bool)=sqlx::query_as("SELECT coalesce((SELECT blocked FROM crm.connection_identity_links WHERE connection_id=$1 AND external_id=$2),false),$3 AND crm.wechat_is_official($2)").bind(cid).bind(external).bind(kind == "contact" && matches!(provider, Provider::Wechat)).fetch_one(&mut **tx).await?;
        let directory_contact = kind == "contact" && !official;
        let mut person = resolve_person(tx, cid, p, directory_contact).await?;
        if person.is_none() && !blocked && directory_contact && !deleted {
            let id = Uuid::new_v5(&cid, format!("contact:{external}").as_bytes());
            sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person',$2) ON CONFLICT DO NOTHING").bind(id).bind(name.chars().take(255).collect::<String>()).execute(&mut **tx).await?;
            person = Some(id);
        }
        if let Some(profile) = person {
            if directory_contact && !deleted {
                sqlx::query("INSERT INTO crm.connection_identity_links(connection_id,external_id,profile_id,confirmed) VALUES($1,$2,$3,true) ON CONFLICT DO NOTHING").bind(cid).bind(external).bind(profile).execute(&mut **tx).await?;
                for (kind, value) in identifiers(p) {
                    sqlx::query("INSERT INTO crm.connection_identifiers VALUES($1,$2,$3,$4,$5,true) ON CONFLICT(connection_id,external_id,kind,value) DO UPDATE SET active=true,profile_id=excluded.profile_id").bind(cid).bind(&row.external_id).bind(profile).bind(&kind).bind(&value).execute(&mut **tx).await?;
                    changed_ids.insert((kind, value));
                }
                import_contact_fields(tx, provider, profile, row, &value).await?;
            }
            let role = string(p, "role", "participant");
            people.push((profile, role, own));
            affected.insert(profile);
            sqlx::query("INSERT INTO crm.connection_evidence_people VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(eid).bind(profile).bind(role).execute(&mut **tx).await?;
        }
        sqlx::query("UPDATE crm.connection_people SET resolved_profile_id=$3 WHERE connection_id=$1 AND external_id=$2").bind(cid).bind(external).bind(person).execute(&mut **tx).await?;
    }
    if let Some(binding) = &row.binding {
        for (profile, role) in [
            (binding.profile_id, "participant"),
            (binding.sender_profile_id, "sender"),
        ] {
            if let Some(profile) = profile {
                affected.insert(profile);
                sqlx::query("INSERT INTO crm.connection_evidence_people VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(eid).bind(profile).bind(role).execute(&mut **tx).await?;
            }
        }
    }
    for (kind, id) in changed_ids {
        queue_identifier(tx, &kind, &id).await?;
    }
    if !affected.is_empty() {
        sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=ANY($1)")
            .bind(affected.into_iter().collect::<Vec<_>>())
            .execute(&mut **tx)
            .await?;
    }
    if ["post", "story"].contains(&kind) {
        let owner = row.binding.as_ref().and_then(|b| b.profile_id).or_else(|| {
            people
                .iter()
                .find(|(_, role, own)| !*own && ["author", "sender"].contains(role))
                .or_else(|| people.iter().find(|(_, _, own)| !*own))
                .map(|(id, _, _)| *id)
        });
        let mut content = value.clone();
        if content.get("caption").is_none() {
            content["caption"] = json!(string(&value, "body", ""));
        }
        let stored = crate::content::register(
            tx,
            if kind == "post" {
                "post_body"
            } else {
                "story_body"
            },
            &content,
        )
        .await?;
        sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,status,version,connection_id) VALUES($1,$2,$1,$3,$4,$5,$6::text::timestamptz,$7,$8,$9,$10,$11) ON CONFLICT(id) DO UPDATE SET profile_id=excluded.profile_id,occurred_at=coalesce(excluded.occurred_at,crm.timeline_events.occurred_at),content=excluded.content,content_hash=excluded.content_hash,status=excluded.status,version=excluded.version,last_seen_at=now()").bind(eid).bind(provider.schema()).bind(owner).bind(kind).bind(&row.external_id).bind(at).bind(content).bind(&stored.hash).bind(if included{"active"}else{"deleted"}).bind(row.revision).bind(cid).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO crm.connection_social_revisions VALUES($1,$2,$3,$4,$5::text::timestamptz,$6) ON CONFLICT DO NOTHING").bind(eid).bind(row.revision).bind(stored.hash).bind(stored.id).bind(&row.observed_at).bind(&row.external_id).execute(&mut **tx).await?;
        social::project_interactions(tx, cid, eid, provider, &value).await?;
    }
    if kind == "contact" {
        let current = people.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
        sqlx::query("DELETE FROM crm.field_current c USING crm.field_revisions r WHERE r.id=c.revision_id AND r.source_type=$1 AND r.source_record_id=$2 AND NOT(c.profile_id=ANY($3))").bind(provider.schema()).bind(format!("{}:{}",row.connection_id,row.external_id)).bind(if deleted{Vec::new()}else{current}).execute(&mut **tx).await?;
        let owners:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT profile_id FROM crm.field_revisions WHERE source_type=$1 AND source_record_id=$2 ORDER BY profile_id").bind(provider.schema()).bind(format!("{}:{}",row.connection_id,row.external_id)).fetch_all(&mut **tx).await?;
        for owner in owners {
            crate::ai::memory::profile_facts(tx, owner).await?;
        }
    }
    let old_message: Option<MessagePosition> = sqlx::query_as("SELECT conversation_id,occurred_at,direction FROM crm.message_search WHERE message_id=$1 AND NOT tombstone").bind(eid).fetch_optional(&mut **tx).await?;
    if !included || kind != "message" || at.is_none() {
        sqlx::query("DELETE FROM crm.message_search WHERE message_id=$1")
            .bind(eid)
            .execute(&mut **tx)
            .await?;
    } else {
        let external_people: BTreeSet<_> = participants
            .iter()
            .filter(|p| !truth(p.get("is_self"), false))
            .map(|p| string(p, "id", ""))
            .collect();
        let direct = row.binding.as_ref().map_or(
            external_people.len() == 1 && !truth(value.get("is_group"), false),
            |b| b.conversation_type == "direct",
        );
        let typ = if direct { "direct" } else { "group" };
        let thread = string(&value, "thread_id", &row.external_id);
        let conversation = Uuid::new_v5(&cid, format!("conversation:{typ}:{thread}").as_bytes());
        let key = format!("{typ}:{thread}");
        let counterparts: BTreeSet<_> = people
            .iter()
            .filter(|(_, _, own)| !*own)
            .map(|(p, _, _)| *p)
            .collect();
        let profile = row
            .binding
            .as_ref()
            .filter(|b| !b.infer_profile)
            .map_or_else(
                || {
                    if direct && counterparts.len() == 1 {
                        counterparts.first().copied()
                    } else {
                        None
                    }
                },
                |b| b.profile_id,
            );
        sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,profile_id,conversation_type,connection_id,source_external_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(id) DO UPDATE SET profile_id=excluded.profile_id").bind(conversation).bind(provider.schema()).bind(cid.to_string()).bind(key).bind(profile).bind(typ).bind(cid).bind(thread).execute(&mut **tx).await?;
        let sender = participants
            .iter()
            .find(|p| string(p, "role", "") == "sender")
            .map_or("", |p| string(p, "id", ""));
        let sender_profile = row
            .binding
            .as_ref()
            .filter(|b| !b.infer_sender)
            .map_or_else(
                || {
                    people
                        .iter()
                        .find(|(_, role, _)| *role == "sender")
                        .map(|(p, _, _)| *p)
                },
                |b| b.sender_profile_id,
            );
        sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_profile_id,occurred_at,body,message_type,direction) VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::timestamptz,$9,$10,$11) ON CONFLICT(message_id) DO UPDATE SET conversation_id=excluded.conversation_id,sender=excluded.sender,sender_profile_id=excluded.sender_profile_id,occurred_at=excluded.occurred_at,body=excluded.body,message_type=excluded.message_type,direction=excluded.direction,tombstone=false")
 .bind(eid).bind(conversation).bind(provider.schema()).bind(cid.to_string()).bind(&row.external_id).bind(sender).bind(sender_profile).bind(at).bind(message_text(&value)).bind(string(&value,"message_type","text")).bind(message_direction(&value,profile,sender_profile)).execute(&mut **tx).await?;
        sqlx::query("UPDATE crm.conversations SET source_display_name=crm.conversation_source_name(conversation_type,$2),source_display_at=$3::text::timestamptz WHERE id=$1 AND (source_display_at IS NULL OR source_display_at<=$3::text::timestamptz) AND crm.conversation_source_name(conversation_type,$2) IS NOT NULL").bind(conversation).bind(&metadata).bind(at).execute(&mut **tx).await?;
        sqlx::query(
            "UPDATE crm.connection_evidence SET message_id=$2,conversation_id=$3 WHERE id=$1",
        )
        .bind(eid)
        .bind(eid)
        .bind(conversation)
        .execute(&mut **tx)
        .await?;
    }
    let new_message: Option<MessagePosition> = sqlx::query_as("SELECT conversation_id,occurred_at,direction FROM crm.message_search WHERE message_id=$1 AND NOT tombstone").bind(eid).fetch_optional(&mut **tx).await?;
    refresh_conversations(tx, old_message, new_message).await?;
    Ok(true)
}
async fn import_contact_fields(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    profile: Uuid,
    row: &Revision,
    value: &Value,
) -> Result<()> {
    let fields: Vec<(String, Value)> = match provider {
        Provider::AppleContacts => ["emails", "phones", "organization", "job_title", "birthday"]
            .into_iter()
            .filter_map(|key| {
                value
                    .get(key)
                    .map(|v| (format!("apple_contacts.{key}"), v.clone()))
            })
            .collect(),
        Provider::Wechat => [
            ("alias", "alias"),
            ("remark", "remark"),
            ("nick_name", "nickname"),
            ("nickname", "nickname"),
            ("description", "description"),
            ("big_head_url", "avatar_url"),
            ("small_head_url", "avatar_url"),
        ]
        .into_iter()
        .filter_map(|(raw, key)| {
            value
                .get("fields")
                .and_then(|f| f.get(raw))
                .or_else(|| value.get(raw))
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
                .map(|v| (format!("wechat.{key}"), v.clone()))
        })
        .fold(std::collections::BTreeMap::new(), |mut fields, (k, v)| {
            fields.entry(k).or_insert(v);
            fields
        })
        .into_iter()
        .collect(),
        _ => Vec::new(),
    };
    let objects = crate::content::register_many(
        tx,
        "field_value",
        &fields.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>(),
    )
    .await?;
    for ((key, value), object) in fields.into_iter().zip(objects) {
        let previous:Option<(Uuid,String,String)>=sqlx::query_as("SELECT r.id,r.content_hash,r.source_type FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key=$2").bind(profile).bind(&key).fetch_optional(&mut **tx).await?;
        if previous
            .as_ref()
            .is_some_and(|(_, hash, _)| hash == &object.hash)
        {
            continue;
        }
        let latest:Option<String>=sqlx::query_scalar("SELECT content_hash FROM crm.field_revisions WHERE profile_id=$1 AND field_key=$2 AND source_type=$3 AND source_record_id=$4 ORDER BY revision_seq DESC LIMIT 1").bind(profile).bind(&key).bind(provider.schema()).bind(format!("{}:{}",row.connection_id,row.external_id)).fetch_optional(&mut **tx).await?;
        if previous.is_some() && latest.as_deref() == Some(&object.hash) {
            continue;
        }
        let id = Uuid::new_v4();
        let manual = previous
            .as_ref()
            .is_some_and(|(_, _, source)| source == "manual");
        sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,observed_at,is_conflict,previous_revision_id,content_object_id,source_record_id) VALUES($1,$2,$3,$4,$5,$11,'import',$6::text::timestamptz,$7,$8,$9,$10)").bind(id).bind(profile).bind(&key).bind(value).bind(object.hash).bind(&row.observed_at).bind(manual).bind(previous.map(|(id,_,_)|id)).bind(object.id).bind(format!("{}:{}",row.connection_id,row.external_id)).bind(provider.schema()).execute(&mut **tx).await?;
        if !manual {
            sqlx::query("INSERT INTO crm.field_current VALUES($1,$2,$3) ON CONFLICT(profile_id,field_key) DO UPDATE SET revision_id=excluded.revision_id").bind(profile).bind(key).bind(id).execute(&mut **tx).await?;
        }
    }
    Ok(())
}
type MessagePosition = (Uuid, chrono::DateTime<chrono::Utc>, Option<String>);
pub(crate) async fn refresh_conversations(
    tx: &mut Transaction<'_, Postgres>,
    old: Option<MessagePosition>,
    new: Option<MessagePosition>,
) -> Result<()> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<Uuid, i64> = BTreeMap::new();
    let mut months: BTreeMap<(Uuid, &str, &str, String), (i64, i64)> = BTreeMap::new();
    for (message, delta) in [(old, -1), (new, 1)] {
        if let Some((id, at, direction)) = message {
            *counts.entry(id).or_default() += delta;
            for (table, zone, month) in [
                ("message_months_utc", "UTC", at.format("%Y-%m").to_string()),
                (
                    "message_months",
                    "America/Chicago",
                    at.with_timezone(&chrono_tz::America::Chicago)
                        .format("%Y-%m")
                        .to_string(),
                ),
            ] {
                let entry = months.entry((id, table, zone, month)).or_default();
                if direction.as_deref() == Some("sent") {
                    entry.0 += delta
                } else {
                    entry.1 += delta
                }
            }
        }
    }
    for (id, delta) in counts {
        sqlx::query("UPDATE crm.conversations SET message_count=message_count+$2,first_message_at=(SELECT occurred_at FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone ORDER BY occurred_at,message_id LIMIT 1),last_message_at=(SELECT occurred_at FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1),last_message_id=(SELECT message_id FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1),last_message_preview=(SELECT left(btrim(regexp_replace(body,'[[:space:]]+',' ','g')),80) FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone AND btrim(body)<>'' ORDER BY occurred_at DESC,message_id DESC LIMIT 1) WHERE id=$1").bind(id).bind(delta).execute(&mut **tx).await?;
    }
    for ((id, table, zone, month), (sent, received)) in months {
        sqlx::query(&format!("INSERT INTO crm.{table}(conversation_id,month,sent,received) VALUES($1,$2,$3,$4) ON CONFLICT(conversation_id,month) DO UPDATE SET sent=crm.{table}.sent+excluded.sent,received=crm.{table}.received+excluded.received")).bind(id).bind(&month).bind(sent).bind(received).execute(&mut **tx).await?;
        sqlx::query(&format!(
            "DELETE FROM crm.{table} WHERE conversation_id=$1 AND month=$2 AND sent+received=0"
        ))
        .bind(id)
        .bind(&month)
        .execute(&mut **tx)
        .await?;
        let range = "conversation_id=$1 AND NOT tombstone AND occurred_at >= (($2||'-01')::timestamp AT TIME ZONE $3) AND occurred_at < ((($2||'-01')::timestamp+interval '1 month') AT TIME ZONE $3)";
        sqlx::query(&format!("UPDATE crm.{table} SET first_message_at=(SELECT occurred_at FROM crm.message_search WHERE {range} ORDER BY occurred_at,message_id LIMIT 1),last_message_at=(SELECT occurred_at FROM crm.message_search WHERE {range} ORDER BY occurred_at DESC,message_id DESC LIMIT 1) WHERE conversation_id=$1 AND month=$2")).bind(id).bind(&month).bind(zone).execute(&mut **tx).await?;
    }
    Ok(())
}

/// Re-evaluate saved evidence after a confirmed identity/ownership change.
pub async fn relink_pending(pool: &PgPool, limit: i64) -> Result<usize> {
    let requests:Vec<(Uuid,Uuid,String,String)>=sqlx::query_as("SELECT r.evidence_id,e.connection_id,c.provider,e.external_id FROM crm.connection_relink r JOIN crm.connection_evidence e ON e.id=r.evidence_id JOIN crm.source_connections c ON c.id=e.connection_id ORDER BY r.requested_at,r.evidence_id LIMIT $1").bind(limit.clamp(1,1000)).fetch_all(pool).await?;
    for (eid, cid, provider, external) in &requests {
        let mut tx = pool.begin().await?;
        lock_connection(&mut tx, *cid).await?;
        let requested: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT requested_at FROM crm.connection_relink WHERE evidence_id=$1",
        )
        .bind(eid)
        .fetch_optional(&mut *tx)
        .await?;
        if requested.is_none() {
            continue;
        }
        let provider = Provider::parse(provider)?;
        let r=sqlx::query(&format!("SELECT r.*,r.updated_at::text observed,CASE WHEN b.connection_id IS NULL THEN NULL ELSE to_jsonb(b)||jsonb_build_object('profile_id',crm.canonical_profile(b.profile_id),'sender_profile_id',crm.canonical_profile(b.sender_profile_id)) END binding FROM {}.connection_records r LEFT JOIN crm.connection_import_bindings b USING(connection_id,external_id,revision) WHERE r.connection_id=$1 AND r.external_id=$2",provider.schema())).bind(cid).bind(external).fetch_optional(&mut *tx).await?;
        if let Some(r) = r {
            project_revision(
                &mut tx,
                provider,
                &Revision {
                    sequence: 0,
                    connection_id: *cid,
                    external_id: external.clone(),
                    revision: r.get("revision"),
                    content_hash: r.get("content_hash"),
                    payload: r.get("payload"),
                    observed_at: r.get("observed"),
                    binding: r
                        .get::<Option<Value>, _>("binding")
                        .map(|v| serde_json::from_value(v).expect("typed binding")),
                },
                true,
            )
            .await?;
        }
        sqlx::query("DELETE FROM crm.connection_relink WHERE evidence_id=$1 AND requested_at=$2")
            .bind(eid)
            .bind(requested)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(requests.len())
}

#[path = "connections_native.rs"]
mod native;
pub use native::{
    NativeSnapshot, commit_native_page, finish_native_snapshots, finish_native_snapshots_bounded,
};

#[path = "connections_fast.rs"]
mod fast;

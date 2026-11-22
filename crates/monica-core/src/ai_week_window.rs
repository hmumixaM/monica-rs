//! Whole-window evidence stays in PostgreSQL; providers receive cached, bounded
//! source parts. No transaction or explicit source-row lock spans inference.
use super::{Lease, Privacy, provider::Provider};
use anyhow::{Context, Result, ensure};
use chrono::NaiveDate;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

const PROMPT: &str = "full-week-v1";
pub const CONTEXT_PENDING: &str = "source observation context pending";
fn hash(value: &Value) -> String {
    crate::identity::observation_hash(value)
}

fn scope(kind: &str, subject: &str) -> Result<(NaiveDate, Option<Uuid>)> {
    if kind == "ai.person_week" {
        let (person, week) = subject.split_once(':').context("week subject")?;
        Ok((week.parse()?, Some(person.parse()?)))
    } else {
        Ok((subject.parse()?, None))
    }
}

pub async fn manifest(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    subject: &str,
    privacy: &Privacy,
) -> Result<Value> {
    let (week, person) = scope(kind, subject)?;
    let pending: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.ai_social_observed_pending q JOIN crm.timeline_events e ON e.id=q.event_id WHERE $1::uuid IS NULL OR e.profile_id=$1)")
        .bind(person).fetch_one(&mut **tx).await?;
    ensure!(!pending, CONTEXT_PENDING);
    let row=sqlx::query("SELECT count(*) source_count,encode(sha256(convert_to(coalesce(string_agg(source_key||':'||encode(sha256(convert_to(payload::text,'UTF8')),'hex'),',' ORDER BY source_key),''),'UTF8')),'hex') source_hash FROM crm.ai_week_sources($1,$2,$3,$4)")
        .bind(week).bind(person).bind(privacy.include_messages).bind(privacy.include_private_notes).fetch_one(&mut **tx).await?;
    let source_policy: String = sqlx::query_scalar("SELECT crm.ai_week_source_policy()")
        .fetch_one(&mut **tx)
        .await?;
    Ok(
        json!({"source_policy":source_policy,"version":PROMPT,"source_count":row.get::<i64,_>("source_count"),"source_hash":row.get::<String,_>("source_hash")}),
    )
}

async fn freeze(pool: &PgPool, lease: &Lease) -> Result<bool> {
    let (week, person) = scope(&lease.kind, &lease.subject)?;
    let mut tx = pool.begin().await?;
    // Retention takes the same mutable job lock, so an admitted replay cannot
    // race with pruning its capture. No source rows are explicitly locked.
    let owned: Option<Uuid>=sqlx::query_scalar("SELECT id FROM crm.ai_jobs WHERE id=$1 AND token=$2 AND status='running' AND lease_until>clock_timestamp() FOR UPDATE")
        .bind(lease.id).bind(lease.token).fetch_optional(&mut *tx).await?;
    if owned.is_none() {
        return Ok(false);
    }
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM crm.ai_week_runs WHERE job_id=$1 AND NOT pruned)",
    )
    .bind(lease.id)
    .fetch_one(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(true);
    }
    // A retained metadata row is not an intact capture. Rebuild it from the
    // original evidence when an unchanged terminal job is explicitly retried.
    sqlx::query("DELETE FROM crm.ai_week_runs WHERE job_id=$1 AND pruned")
        .bind(lease.id)
        .execute(&mut *tx)
        .await?;
    // Only this job's mutable run record is reserved. Source facts are read
    // under the INSERT statement's MVCC snapshot, without FOR SHARE/UPDATE.
    sqlx::query("INSERT INTO crm.ai_week_runs(job_id,source_hash,source_count) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(lease.id).bind(lease.input["window"]["source_hash"].as_str().context("window hash")?)
        .bind(lease.input["window"]["source_count"].as_i64().context("window count")?).execute(&mut *tx).await?;
    sqlx::query("SELECT job_id FROM crm.ai_week_runs WHERE job_id=$1 FOR UPDATE")
        .bind(lease.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.ai_week_evidence(job_id,source_key,source_id,profile_id,kind,bucket,payload_hash,payload) SELECT $1,source_key,(payload->>'id')::uuid,(payload->>'profile_id')::uuid,payload->>'kind',left(source_key,2),encode(sha256(convert_to(payload::text,'UTF8')),'hex'),payload FROM crm.ai_week_sources($2,$3,$4,$5) ON CONFLICT DO NOTHING")
        .bind(lease.id).bind(week).bind(person)
        .bind(lease.privacy["policy"]["include_messages"].as_bool().unwrap_or(false))
        .bind(lease.privacy["policy"]["include_private_notes"].as_bool().unwrap_or(false))
        .execute(&mut *tx).await?;
    let captured: String=sqlx::query_scalar("SELECT encode(sha256(convert_to(coalesce(string_agg(source_key||':'||payload_hash,',' ORDER BY source_key),''),'UTF8')),'hex') FROM crm.ai_week_evidence WHERE job_id=$1")
        .bind(lease.id).fetch_one(&mut *tx).await?;
    if Some(captured.as_str()) != lease.input["window"]["source_hash"].as_str() {
        tx.rollback().await?;
        let changed=sqlx::query("UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,completed_at=now() WHERE id=$1 AND token=$2 AND status='running'")
            .bind(lease.id).bind(lease.token).execute(pool).await?.rows_affected();
        if changed > 0 {
            request_refresh(pool, &lease.kind, &lease.subject).await?;
        }
        return Ok(false);
    }
    // Small windows share a single bucket. Larger windows group by cumulative
    // bytes, avoiding a model call for every sparse UUID radix prefix.
    sqlx::query("WITH sized AS (SELECT source_key,lpad(((sum(octet_length(payload::text)) OVER(ORDER BY source_key)-octet_length(payload::text))/131072)::bigint::text,20,'0') bucket FROM crm.ai_week_evidence WHERE job_id=$1) UPDATE crm.ai_week_evidence e SET bucket=s.bucket FROM sized s WHERE e.job_id=$1 AND e.source_key=s.source_key")
        .bind(lease.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}

/// Source timestamps are deterministic input work, independent of inference.
/// Only the mutable pending reference is reserved; source history is read MVCC.
pub async fn refresh_observations(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    let ids: Vec<Uuid>=sqlx::query_scalar("SELECT event_id FROM crm.ai_social_observed_pending ORDER BY event_id LIMIT 25 FOR UPDATE SKIP LOCKED")
        .fetch_all(&mut *tx).await?;
    if ids.is_empty() {
        return Ok(0);
    }
    sqlx::query("INSERT INTO crm.ai_social_observed(event_id,first_observed_at) SELECT id,crm.ai_first_social_observation(id) FROM crm.timeline_events WHERE id=ANY($1) ORDER BY id ON CONFLICT(event_id) DO UPDATE SET first_observed_at=excluded.first_observed_at,checked_at=now()")
        .bind(&ids).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM crm.ai_social_observed_pending WHERE event_id=ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ids.len())
}

pub async fn request_refresh(pool: &PgPool, kind: &str, subject: &str) -> Result<()> {
    sqlx::query("INSERT INTO crm.ai_week_refresh(kind,subject) VALUES($1,$2) ON CONFLICT(kind,subject) DO UPDATE SET version=gen_random_uuid(),requested_at=now()")
        .bind(kind).bind(subject).execute(pool).await?;
    Ok(())
}

pub async fn enqueue_pending(pool: &PgPool) -> Result<()> {
    let rows=sqlx::query("SELECT kind,subject,version FROM crm.ai_week_refresh ORDER BY requested_at,kind,subject LIMIT 25")
        .fetch_all(pool).await?;
    for row in rows {
        let kind: String = row.get("kind");
        let subject: String = row.get("subject");
        let inactive = if kind == "ai.person_week" {
            let (_, person) = scope(&kind, &subject)?;
            !sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM crm.profiles WHERE id=$1 AND archived_at IS NULL AND profile_type='person')")
                .bind(person).fetch_one(pool).await?
        } else {
            false
        };
        let settled = if inactive {
            true
        } else {
            match super::enqueue(pool, &kind, &subject).await {
                Ok(_) => true,
                Err(error) => error.to_string() == "no evidence in this week",
            }
        };
        if settled {
            // enqueue has committed before this delete; no profile/head lock is
            // held while waiting for a newer pending request.
            sqlx::query(
                "DELETE FROM crm.ai_week_refresh WHERE kind=$1 AND subject=$2 AND version=$3",
            )
            .bind(kind)
            .bind(subject)
            .bind(row.get::<Uuid, _>("version"))
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

fn references(input: &Value) -> std::collections::BTreeSet<String> {
    input["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| &s["id"])
        .chain(
            input["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|p| {
                    p["evidence"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|e| &e["id"])
                }),
        )
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

async fn normalize(pool: &PgPool, lease: &Lease, input: &Value, output: Value) -> Result<Value> {
    ensure!(
        output.is_object() && output["summary"].is_string(),
        "week part summary required"
    );
    ensure!(
        serde_json::to_vec(&output)?.len() <= 64 * 1024,
        "week part response too large"
    );
    let allowed = references(input);
    let mut seen = std::collections::BTreeSet::new();
    let ids: Vec<Uuid> = output["life_events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| &e["post_id"])
        .chain(output["sources"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .filter(|id| allowed.contains(*id))
        .filter_map(|id| id.parse().ok())
        .filter(|id| seen.insert(*id))
        .take(16)
        .collect();
    let evidence: Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',source_id,'profile_id',profile_id,'kind',kind,'at',payload->'at','temporal_kind',payload->'temporal_kind','discovered',payload->'discovered') FROM crm.ai_week_evidence WHERE job_id=$1 AND source_id=ANY($2) ORDER BY array_position($2,source_id),source_key LIMIT 16")
        .bind(lease.id).bind(ids).fetch_all(pool).await?;
    let mut checker = lease.clone();
    checker.kind = "ai.digest".into();
    checker.input = json!({"evidence":evidence,"facts":{}});
    let validated = super::validate_output(&checker, output)?;
    let events: Vec<Value> = validated["life_events"]
        .as_array()
        .into_iter()
        .flatten()
        .take(10)
        .map(|e| {
            let mut event = e.clone();
            event["text"] = json!(
                e["text"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
            event
        })
        .collect();
    let suggestions: Vec<Value> = validated["suggestions"]
        .as_array()
        .into_iter()
        .flatten()
        .take(5)
        .map(|s| {
            let mut suggestion = s.clone();
            suggestion["reason"] = json!(
                s["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>()
            );
            suggestion
        })
        .collect();
    let part = json!({"summary":validated["summary"].as_str().unwrap_or_default().chars().take(2000).collect::<String>(),"life_events":events,"suggestions":suggestions,"evidence":evidence});
    ensure!(
        part.to_string().chars().count() <= 12000,
        "week summary exceeds reduction budget"
    );
    Ok(part)
}

async fn part<P: Provider>(
    pool: &PgPool,
    provider: &P,
    lease: &Lease,
    input: Value,
    budget: &mut usize,
) -> Result<Option<Value>> {
    let model = lease.input["chunk_model"]
        .as_str()
        .context("week batch model")?;
    let input = json!({"week_start":lease.input["week_start"],"profile":lease.input["profile"],"content":input});
    let key = hash(&json!([
        PROMPT,
        &lease.kind,
        &lease.subject,
        model,
        &lease.privacy,
        &input
    ]));
    if let Some(saved) =
        sqlx::query_scalar("SELECT output FROM crm.ai_week_parts WHERE cache_key=$1")
            .bind(&key)
            .fetch_optional(pool)
            .await?
    {
        return Ok(Some(saved));
    }
    if *budget == 0 {
        return Ok(None);
    }
    ensure!(
        super::models::permits_call(pool, lease, "ai.week_chunk", model).await?,
        "week model or privacy changed"
    );
    ensure!(super::heartbeat(pool, lease).await?, "week lease lost");
    *budget -= 1;
    let mut submitted = lease.clone();
    submitted.kind = "ai.week_chunk".into();
    submitted.model = model.into();
    submitted.input = input.clone();
    let request = tokio::time::timeout(super::TEXT_CALL_DEADLINE, provider.complete(&submitted));
    tokio::pin!(request);
    let output = loop {
        tokio::select! {
            result=&mut request=>break result.context("week provider deadline")??,
            _=tokio::time::sleep(std::time::Duration::from_secs(20))=>ensure!(super::heartbeat(pool,lease).await?,"week lease lost")
        }
    };
    let output = normalize(pool, lease, &input["content"], output).await?;
    sqlx::query(
        "INSERT INTO crm.ai_week_parts(cache_key,output) VALUES($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(key)
    .bind(&output)
    .execute(pool)
    .await?;
    Ok(Some(output))
}

async fn reduce<P: Provider>(
    pool: &PgPool,
    provider: &P,
    lease: &Lease,
    mut values: Vec<Value>,
    budget: &mut usize,
) -> Result<Option<Value>> {
    while values.len() > 1 {
        let mut groups: Vec<Vec<Value>> = Vec::new();
        let mut group = Vec::new();
        let mut chars = 0;
        for value in values {
            let size = value.to_string().chars().count();
            if group.len() >= 2 && chars + size > 24000 {
                groups.push(std::mem::take(&mut group));
                chars = 0;
            }
            chars += size;
            group.push(value);
        }
        if !group.is_empty() {
            groups.push(group);
        }
        let mut next = Vec::new();
        for group in groups {
            if group.len() == 1 {
                next.extend(group);
                continue;
            }
            let Some(output) = part(pool, provider, lease, json!({"parts":group}), budget).await?
            else {
                return Ok(None);
            };
            next.push(output);
        }
        values = next;
    }
    Ok(Some(
        values
            .pop()
            .unwrap_or_else(|| json!({"summary":"","evidence":[]})),
    ))
}

pub async fn prepare<P: Provider>(
    pool: &PgPool,
    provider: &P,
    lease: &Lease,
) -> Result<Option<Value>> {
    let source_policy: String = sqlx::query_scalar("SELECT crm.ai_week_source_policy()")
        .fetch_one(pool)
        .await?;
    if Some(source_policy.as_str()) != lease.input["window"]["source_policy"].as_str() {
        let changed=sqlx::query("UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,completed_at=now() WHERE id=$1 AND token=$2 AND status='running' AND lease_until>clock_timestamp()")
            .bind(lease.id).bind(lease.token).execute(pool).await?.rows_affected();
        if changed > 0 {
            request_refresh(pool, &lease.kind, &lease.subject).await?;
        }
        return Ok(None);
    }
    if !freeze(pool, lease).await? {
        return Ok(None);
    }
    if let Some(ready) =
        sqlx::query_scalar("SELECT ready_summary FROM crm.ai_week_runs WHERE job_id=$1")
            .bind(lease.id)
            .fetch_one(pool)
            .await?
    {
        return Ok(Some(ready));
    }
    let row=sqlx::query("SELECT e.bucket FROM crm.ai_week_evidence e WHERE e.job_id=$1 AND NOT EXISTS(SELECT 1 FROM crm.ai_week_buckets b WHERE b.job_id=e.job_id AND b.bucket=e.bucket) GROUP BY e.bucket ORDER BY e.bucket LIMIT 1")
        .bind(lease.id).fetch_optional(pool).await?;
    let mut budget = 1;
    if let Some(row) = row {
        let bucket: String = row.get("bucket");
        let sources: Vec<(String,Value)>=sqlx::query_as("SELECT source_key,payload FROM crm.ai_week_evidence WHERE job_id=$1 AND bucket=$2 ORDER BY source_key")
            .bind(lease.id).bind(&bucket).fetch_all(pool).await?;
        let mut outputs = Vec::new();
        for node in super::memory::leaves("", &sources) {
            let mut input = node.input;
            for source in input["sources"].as_array_mut().into_iter().flatten() {
                if let Some((_, original)) = sources
                    .iter()
                    .find(|(key, _)| Some(key.as_str()) == source["source_key"].as_str())
                {
                    for field in ["id", "profile_id", "kind", "at", "temporal_kind"] {
                        source[field] = original[field].clone();
                    }
                }
            }
            let Some(output) = part(pool, provider, lease, input, &mut budget).await? else {
                return Ok(None);
            };
            outputs.push(output);
        }
        let Some(output) = reduce(pool, provider, lease, outputs, &mut budget).await? else {
            return Ok(None);
        };
        sqlx::query("INSERT INTO crm.ai_week_buckets(job_id,bucket,output,source_count) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(lease.id).bind(bucket).bind(output).bind(sources.len() as i64).execute(pool).await?;
        return Ok(None);
    }
    let buckets: Vec<Value> = sqlx::query_scalar(
        "SELECT output FROM crm.ai_week_buckets WHERE job_id=$1 ORDER BY bucket",
    )
    .bind(lease.id)
    .fetch_all(pool)
    .await?;
    let Some(ready) = reduce(pool, provider, lease, buckets, &mut budget).await? else {
        return Ok(None);
    };
    sqlx::query("UPDATE crm.ai_week_runs SET ready_summary=$2 WHERE job_id=$1")
        .bind(lease.id)
        .bind(ready)
        .execute(pool)
        .await?;
    // Final writing occurs on the next turn, so one lane turn never combines
    // a slow reduction request and another slow writing request.
    Ok(None)
}

/// Citations resolve against the immutable captured window. Only short excerpts
/// enter published result JSON; the complete source remains at its evidence URL.
pub async fn output_lease(pool: &PgPool, lease: &Lease) -> Result<Lease> {
    let ready: Value =
        sqlx::query_scalar("SELECT ready_summary FROM crm.ai_week_runs WHERE job_id=$1")
            .bind(lease.id)
            .fetch_one(pool)
            .await?;
    // Digest suggestions cite people rather than individual post IDs. Keep the
    // bounded, already validated context even when the final response has no
    // sources field or life event. Output validators still reject invented IDs.
    let ids: Vec<Uuid> = ready["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["id"].as_str())
        .filter_map(|id| id.parse().ok())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(64)
        .collect();
    let evidence: Vec<Value>=sqlx::query_scalar("SELECT (payload-'visual'-'text')||jsonb_build_object('text',left(payload->>'text',8000)) FROM crm.ai_week_evidence WHERE job_id=$1 AND source_id=ANY($2) ORDER BY source_key LIMIT 64")
        .bind(lease.id).bind(ids).fetch_all(pool).await?;
    let mut resolved = lease.clone();
    resolved.input["evidence"] = json!(evidence);
    Ok(resolved)
}

/// Retain detailed captured inputs and cached reductions for 90 days. Published
/// results and all original source evidence remain intact. Each tick is bounded.
pub async fn prune_completed(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await?;
    let id:Option<Uuid>=sqlx::query_scalar("SELECT j.id FROM crm.ai_jobs j JOIN crm.ai_week_runs r ON r.job_id=j.id WHERE r.captured_at<now()-interval '90 days' AND j.status IN ('complete','cancelled','failed') AND (NOT r.pruned OR EXISTS(SELECT 1 FROM crm.ai_week_evidence e WHERE e.job_id=j.id) OR EXISTS(SELECT 1 FROM crm.ai_week_buckets b WHERE b.job_id=j.id)) ORDER BY r.captured_at,r.job_id LIMIT 1 FOR UPDATE OF j SKIP LOCKED")
        .fetch_optional(&mut *tx).await?;
    if let Some(id) = id {
        sqlx::query("UPDATE crm.ai_week_runs SET pruned=true,ready_summary=NULL WHERE job_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM crm.ai_week_evidence WHERE job_id=$1 AND source_key IN (SELECT source_key FROM crm.ai_week_evidence WHERE job_id=$1 ORDER BY source_key LIMIT 1000)")
            .bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM crm.ai_week_buckets WHERE job_id=$1 AND bucket IN (SELECT bucket FROM crm.ai_week_buckets WHERE job_id=$1 ORDER BY bucket LIMIT 1000)")
            .bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    sqlx::query("DELETE FROM crm.ai_week_parts WHERE cache_key IN (SELECT cache_key FROM crm.ai_week_parts WHERE created_at<now()-interval '90 days' AND NOT EXISTS(SELECT 1 FROM crm.ai_jobs WHERE kind IN ('ai.person_week','ai.digest') AND status IN ('queued','running')) ORDER BY created_at,cache_key LIMIT 1000)")
        .execute(pool).await?;
    Ok(())
}

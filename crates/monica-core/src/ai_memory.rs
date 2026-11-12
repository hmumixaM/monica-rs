//! Persistent source-addressed inputs and cached radix reductions. Source writers
//! replace only changed source IDs; unchanged buckets never load their payloads.
use super::{Lease, Privacy, provider::Provider};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;
#[path = "ai_memory_legacy.rs"]
pub mod legacy;
const CHARS: usize = 24000;
const PROMPT: &str = "memory-radix-v2-attribution";
fn hash(v: &Value) -> String {
    crate::identity::observation_hash(v)
}
fn policy_hash(policy: &Privacy) -> String {
    hash(&json!(policy))
}

/// Atomically replace a bounded set of current source inputs. One person lock
/// and one set operation coalesce generation changes for the complete batch.
pub async fn replace_sources(
    tx: &mut Transaction<'_, Postgres>,
    profile: Uuid,
    kind: &str,
    items: &[(String, Option<Value>)],
    private: bool,
) -> Result<bool> {
    ensure!(
        items.len() <= 1000
            && ["profile", "chat", "social", "note", "connection", "mail"].contains(&kind),
        "invalid Memory source batch"
    );
    if items.is_empty() {
        return Ok(false);
    }
    let private = private || ["chat", "mail", "note"].contains(&kind);
    let mut seen = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    for (id, payload) in items {
        ensure!(
            !id.is_empty() && id.len() <= 2048 && seen.insert(id),
            "invalid or duplicate Memory source identity"
        );
        let key = hash(&json!([kind, id]));
        if let Some(payload) = payload {
            crate::identity::validate_json(payload)?;
            let payload = crate::identity::json_safe(payload);
            let size = serde_json::to_vec(&payload)?.len();
            bytes += size;
            ensure!(
                bytes <= 16 * 1024 * 1024,
                "Memory source batch exceeds byte budget"
            );
            rows.push(json!({"source_id":id,"source_key":key,"payload":payload,"input_hash":hash(&payload),"payload_bytes":size}));
        } else {
            rows.push(json!({"source_id":id,"source_key":key,"payload":null,"input_hash":"","payload_bytes":0}));
        }
    }
    sqlx::query("INSERT INTO crm.memory_heads(profile_id) VALUES($1) ON CONFLICT DO NOTHING")
        .bind(profile)
        .execute(&mut **tx)
        .await?;
    sqlx::query("SELECT generation FROM crm.memory_heads WHERE profile_id=$1 FOR UPDATE")
        .bind(profile)
        .fetch_one(&mut **tx)
        .await?;
    let changed:i64=sqlx::query_scalar(r#"WITH rows AS MATERIALIZED (
       SELECT * FROM jsonb_to_recordset($2) AS x(source_id text,source_key text,payload jsonb,input_hash text,payload_bytes integer)
     ), inserted AS (
       INSERT INTO crm.memory_inputs(profile_id,source_key,source_kind,source_id,bucket,payload,input_hash,private,payload_bytes)
       SELECT $1,source_key,$3,source_id,left(source_key,2),payload,input_hash,$4,payload_bytes FROM rows WHERE payload IS NOT NULL AND payload<>'null'::jsonb ORDER BY source_key
       ON CONFLICT(profile_id,source_key) DO UPDATE SET payload=excluded.payload,input_hash=excluded.input_hash,private=excluded.private,payload_bytes=excluded.payload_bytes
       WHERE memory_inputs.input_hash<>excluded.input_hash OR memory_inputs.private<>excluded.private RETURNING profile_id
     ), removed AS (
       DELETE FROM crm.memory_inputs m USING rows r WHERE m.profile_id=$1 AND m.source_key=r.source_key AND (r.payload IS NULL OR r.payload='null'::jsonb) RETURNING profile_id
     ), advanced AS (
       UPDATE crm.memory_heads SET generation=generation+1,updated_at=now() WHERE profile_id=$1 AND (EXISTS(SELECT 1 FROM inserted) OR EXISTS(SELECT 1 FROM removed)) RETURNING profile_id
     ) SELECT count(*) FROM advanced"#).bind(profile).bind(json!(rows)).bind(kind).bind(private).fetch_one(&mut **tx).await?;
    Ok(changed == 1)
}

/// Current compact notebook facts are cheap to synchronize; no source history
/// or chat bodies are scanned here. Large historical sources use writer hooks.
pub async fn profile_facts(tx: &mut Transaction<'_, Postgres>, profile: Uuid) -> Result<bool> {
    let input:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',p.id,'kind','profile','name',p.display_name,'summary',p.summary,'fields',(SELECT jsonb_object_agg(c.field_key,r.value) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=p.id AND (r.source_type IN ('manual','monica') OR c.field_key IN ('wechat.alias','wechat.remark','wechat.nickname','wechat.description') OR c.field_key LIKE 'monica.%' OR c.field_key LIKE 'apple_contacts.%')),'structured',(SELECT jsonb_agg(jsonb_build_object('kind',fact_type,'key',fact_key,'value',value) ORDER BY fact_type,fact_key) FROM crm.profile_structured_current WHERE profile_id=p.id),'dates',(SELECT jsonb_agg(jsonb_build_object('kind',kind,'label',label,'precision',date_precision,'year',year,'month',month,'day',day) ORDER BY kind,id) FROM crm.profile_dates WHERE profile_id=p.id),'tags',(SELECT jsonb_agg(t.name ORDER BY t.name) FROM crm.tag_memberships m JOIN crm.tags t ON t.id=m.tag_id WHERE m.profile_id=p.id)) FROM crm.profiles p WHERE p.id=$1 AND p.archived_at IS NULL AND p.profile_type='person'").bind(profile).fetch_optional(&mut **tx).await?;
    replace_sources(tx, profile, "profile", &[("current".into(), input)], false).await
}

/// Called after connection_evidence and its people/direction projection commit
/// together. Track previous owners so unlink, filter and tombstone changes also
/// remove the former person's Memory input. Identical account copies use one
/// dedup source key, without reanalyzing their repeated message body.
pub async fn connection_evidence(tx: &mut Transaction<'_, Postgres>, evidence: Uuid) -> Result<()> {
    connection_evidence_many(tx, &[evidence]).await
}
/// Set-based projection hook: a source page reserves sorted person heads once,
/// replaces all changed references in one statement, and bumps each person once.
pub async fn connection_evidence_many(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<()> {
    ensure!(ids.len() <= 1000, "Memory connection batch exceeds budget");
    if ids.is_empty() {
        return Ok(());
    }
    let people:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT profile_id FROM (SELECT profile_id FROM crm.memory_evidence_owners WHERE evidence_id=ANY($1) UNION SELECT own.profile_id FROM crm.connection_evidence_people own JOIN crm.connection_evidence e ON e.id=own.evidence_id LEFT JOIN crm.conversations c ON c.id=e.conversation_id JOIN crm.profiles p ON p.id=own.profile_id WHERE e.id=ANY($1) AND p.archived_at IS NULL AND p.profile_type='person' AND NOT EXISTS(SELECT 1 FROM crm.person_identifiers i WHERE i.profile_id=p.id AND i.is_self) AND (coalesce(c.conversation_type,CASE WHEN coalesce((e.metadata->>'is_group')::boolean,false) THEN 'group' ELSE 'direct' END)<>'group' OR own.role='sender')) q ORDER BY profile_id").bind(ids).fetch_all(&mut **tx).await?;
    if people.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO crm.memory_heads(profile_id) SELECT unnest($1::uuid[]) ON CONFLICT DO NOTHING",
    )
    .bind(&people)
    .execute(&mut **tx)
    .await?;
    sqlx::query("SELECT profile_id FROM crm.memory_heads WHERE profile_id=ANY($1) ORDER BY profile_id FOR UPDATE").bind(&people).fetch_all(&mut **tx).await?;
    sqlx::query(r#"WITH new_owners AS MATERIALIZED (
      SELECT DISTINCT e.id evidence_id,own.profile_id,coalesce(e.dedup_key,e.id::text) source_id
      FROM crm.connection_evidence e JOIN crm.connection_evidence_people own ON own.evidence_id=e.id JOIN crm.profiles p ON p.id=own.profile_id
      LEFT JOIN crm.conversations conv ON conv.id=e.conversation_id
      WHERE e.id=ANY($1) AND p.archived_at IS NULL AND p.profile_type='person'
      AND NOT EXISTS(SELECT 1 FROM crm.person_identifiers i WHERE i.profile_id=p.id AND i.is_self)
      AND (e.kind NOT IN ('post','story') OR own.role IN ('author','sender')) AND (coalesce(conv.conversation_type,CASE WHEN coalesce((e.metadata->>'is_group')::boolean,false) THEN 'group' ELSE 'direct' END)<>'group' OR own.role='sender')
    ), affected AS MATERIALIZED (
      SELECT profile_id,source_id FROM crm.memory_evidence_owners WHERE evidence_id=ANY($1)
      UNION SELECT profile_id,source_id FROM new_owners
    ), live AS MATERIALIZED (
      SELECT DISTINCT ON(a.profile_id,a.source_id) a.profile_id,a.source_id,e.id,e.kind,e.occurred_at,e.title,e.body,e.metadata,e.conversation_id,c.provider
      FROM affected a JOIN crm.connection_evidence e ON e.dedup_key=a.source_id OR e.id::text=a.source_id
      JOIN crm.source_connections c ON c.id=e.connection_id AND c.enabled
      LEFT JOIN crm.conversations conv ON conv.id=e.conversation_id
      WHERE e.included AND NOT e.deleted AND (c.provider<>'mail' OR e.category='personal') AND EXISTS(SELECT 1 FROM crm.profiles p WHERE p.id=a.profile_id AND p.archived_at IS NULL) AND NOT EXISTS(SELECT 1 FROM crm.person_identifiers i WHERE i.profile_id=a.profile_id AND i.is_self)
      AND EXISTS(SELECT 1 FROM crm.connection_evidence_people own WHERE own.evidence_id=e.id AND own.profile_id=a.profile_id
      AND (e.kind NOT IN ('post','story') OR own.role IN ('author','sender')) AND (coalesce(conv.conversation_type,CASE WHEN coalesce((e.metadata->>'is_group')::boolean,false) THEN 'group' ELSE 'direct' END)<>'group' OR own.role='sender'))
      ORDER BY a.profile_id,a.source_id,e.id
    ), wanted AS MATERIALIZED (
      SELECT a.profile_id,a.source_id,l.id evidence_id,encode(sha256(convert_to('["connection",'||to_jsonb(a.source_id)::text||']','UTF8')),'hex') source_key,
        CASE WHEN l.id IS NOT NULL THEN jsonb_build_object('id',a.source_id,'kind',l.kind,'provider',l.provider,'at',l.occurred_at,'title',l.title,'text',l.body,'participants',l.metadata->'participants','direction',l.metadata->'direction','conversation_id',l.conversation_id) END payload,
        coalesce(l.kind NOT IN ('contact','post','story'),true) private
      FROM affected a LEFT JOIN live l USING(profile_id,source_id)
    ), inserted AS (
      INSERT INTO crm.memory_inputs(profile_id,source_key,source_kind,source_id,bucket,payload,input_hash,private,payload_bytes)
      SELECT profile_id,source_key,'connection',source_id,left(source_key,2),jsonb_build_object('_evidence_id',evidence_id),encode(sha256(convert_to(payload::text,'UTF8')),'hex'),private,octet_length(payload::text)
      FROM wanted WHERE payload IS NOT NULL ORDER BY profile_id,source_key
      ON CONFLICT(profile_id,source_key) DO UPDATE SET payload=excluded.payload,input_hash=excluded.input_hash,private=excluded.private,payload_bytes=excluded.payload_bytes
      WHERE memory_inputs.input_hash<>excluded.input_hash OR memory_inputs.private<>excluded.private OR memory_inputs.payload<>excluded.payload
      RETURNING profile_id
    ), removed AS (
      DELETE FROM crm.memory_inputs m USING wanted w WHERE m.profile_id=w.profile_id AND m.source_key=w.source_key AND w.payload IS NULL RETURNING m.profile_id
    ), owner_save AS (
      INSERT INTO crm.memory_evidence_owners SELECT * FROM new_owners ON CONFLICT(evidence_id,profile_id) DO UPDATE SET source_id=excluded.source_id WHERE memory_evidence_owners.source_id<>excluded.source_id RETURNING evidence_id
    ), owner_delete AS (
      DELETE FROM crm.memory_evidence_owners o WHERE o.evidence_id=ANY($1) AND NOT EXISTS(SELECT 1 FROM new_owners n WHERE n.evidence_id=o.evidence_id AND n.profile_id=o.profile_id) RETURNING o.evidence_id
    ) UPDATE crm.memory_heads SET generation=generation+1,updated_at=now() WHERE profile_id IN (SELECT profile_id FROM inserted UNION SELECT profile_id FROM removed)"#).bind(ids).execute(&mut **tx).await?;
    Ok(())
}

pub async fn enqueue_pending(pool: &PgPool) -> Result<()> {
    legacy::drain(pool).await?;
    let settings = sqlx::query(
        "SELECT enabled,model,include_private_notes,include_messages FROM crm.ai_settings WHERE id",
    )
    .fetch_one(pool)
    .await?;
    if !settings.get::<bool, _>("enabled") {
        return Ok(());
    }
    let policy = policy_hash(&Privacy {
        include_messages: settings.get("include_messages"),
        include_private_notes: settings.get("include_private_notes"),
    });
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT h.profile_id FROM crm.memory_heads h JOIN crm.profiles p ON p.id=h.profile_id AND p.archived_at IS NULL AND p.profile_type='person' LEFT JOIN crm.memory_current c ON c.profile_id=h.profile_id WHERE NOT EXISTS(SELECT 1 FROM crm.memory_legacy_pending s WHERE s.profile_id=h.profile_id) AND (c.profile_id IS NULL OR c.generation<>h.generation OR c.model<>$1 OR c.privacy_hash<>$2) ORDER BY h.updated_at,h.profile_id LIMIT 25").bind(settings.get::<String,_>("model")).bind(policy).fetch_all(pool).await?;
    for profile in ids {
        let id = super::enqueue(pool, "ai.profile", &profile.to_string()).await?;
        sqlx::query("UPDATE crm.ai_jobs SET status='queued',attempts=0,completed_at=NULL WHERE id=$1 AND status='complete'").bind(id).execute(pool).await?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Node {
    pub key: String,
    pub input: Value,
    pub hash: String,
}
/// Stable source identities partition before packing. A change under one hex
/// prefix cannot shift chunks under its siblings. Large single sources split at
/// Unicode character boundaries, retaining source/fragment citation identity.
pub fn leaves(prefix: &str, rows: &[(String, Value)]) -> Vec<Node> {
    let size: usize = rows
        .iter()
        .map(|(_, v)| v.to_string().chars().count())
        .sum();
    if size > CHARS && rows.len() > 1 && prefix.len() < 64 {
        let mut groups = std::collections::BTreeMap::<char, Vec<(String, Value)>>::new();
        for (key, value) in rows {
            groups
                .entry(key.as_bytes()[prefix.len()] as char)
                .or_default()
                .push((key.clone(), value.clone()));
        }
        return groups
            .into_iter()
            .flat_map(|(digit, rows)| leaves(&format!("{prefix}{digit}"), &rows))
            .collect();
    }
    let mut sources = Vec::new();
    for (key, value) in rows {
        let encoded = serde_json::to_string(value).expect("JSON Memory input");
        let chars = encoded.chars().collect::<Vec<_>>();
        if chars.len() <= CHARS {
            sources.push(json!({"source_key":key,"source":value}));
        } else {
            for (index, part) in chars.chunks(CHARS).enumerate() {
                sources.push(json!({"source_key":key,"fragment":index,"serialized_source_fragment":part.iter().collect::<String>()}));
            }
        }
    }
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut length = 0;
    for source in sources {
        let n = source.to_string().chars().count();
        if !current.is_empty() && length + n > CHARS + 512 {
            chunks.push(std::mem::take(&mut current));
            length = 0;
        }
        current.push(source);
        length += n;
    }
    if !current.is_empty() {
        chunks.push(current)
    }
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, rows)| {
            let input = json!({"sources":rows});
            Node {
                key: format!("leaf:{prefix}:{i}"),
                hash: hash(&json!([PROMPT, &input])),
                input,
            }
        })
        .collect()
}

async fn call<P: Provider>(
    pool: &PgPool,
    provider: &P,
    lease: &Lease,
    kind: &str,
    input: Value,
) -> Result<Value> {
    let mut part = lease.clone();
    part.kind = kind.into();
    part.input = input;
    let request =
        tokio::time::timeout(std::time::Duration::from_secs(45), provider.complete(&part));
    tokio::pin!(request);
    let output = loop {
        tokio::select! {result=&mut request=>break result.context("Memory provider deadline")??,_=tokio::time::sleep(std::time::Duration::from_secs(20))=>ensure!(super::heartbeat(pool,lease).await?,"Memory lease lost")}
    };
    ensure!(
        output.is_object() && output["summary"].is_string(),
        "Memory summary required"
    );
    ensure!(
        serde_json::to_vec(&output)?.len() <= 64 * 1024,
        "Memory result exceeds budget"
    );
    let facts = output["facts"]
        .as_array()
        .map(|a| a.iter().filter_map(|fact|{
            if let Some(value)=fact.as_str(){Some(json!(value.chars().take(500).collect::<String>()))}
            else{Some(json!({"label":fact["label"].as_str()?.chars().take(80).collect::<String>(),"value":fact["value"].as_str()?.chars().take(500).collect::<String>()}))}
        }).take(15).collect::<Vec<_>>())
        .unwrap_or_default();
    let gaps = output["gaps"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|v| v.is_string())
                .take(6)
                .map(|v| {
                    json!(
                        v.as_str()
                            .unwrap_or_default()
                            .chars()
                            .take(80)
                            .collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(
        json!({"summary":output["summary"].as_str().unwrap_or_default().chars().take(4000).collect::<String>(),"facts":facts,"gaps":gaps}),
    )
}
fn pack_parts(parts: &[Value]) -> Vec<Vec<Value>> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    let mut size = 0;
    for part in parts {
        let n = part.to_string().chars().count();
        if !current.is_empty() && (size + n > CHARS || current.len() >= 20) {
            groups.push(std::mem::take(&mut current));
            size = 0
        }
        current.push(part.clone());
        size += n;
    }
    if !current.is_empty() {
        groups.push(current)
    }
    groups
}
async fn part<P: Provider>(
    pool: &PgPool,
    provider: &P,
    lease: &Lease,
    profile: Uuid,
    node: &Node,
    policy: &str,
    budget: &mut usize,
) -> Result<Option<Value>> {
    let saved:Option<Value>=sqlx::query_scalar("SELECT output FROM crm.memory_parts WHERE profile_id=$1 AND node=$2 AND input_hash=$3 AND model=$4 AND privacy_hash=$5").bind(profile).bind(&node.key).bind(&node.hash).bind(&lease.model).bind(policy).fetch_optional(pool).await?;
    if let Some(value) = saved {
        return Ok(Some(value));
    }
    if *budget == 0 {
        return Ok(None);
    }
    *budget -= 1;
    let output = call(
        pool,
        provider,
        lease,
        if node.key == "final" {
            "ai.memory_final"
        } else {
            "ai.memory_chunk"
        },
        node.input.clone(),
    )
    .await?;
    sqlx::query("INSERT INTO crm.memory_parts(profile_id,node,input_hash,model,privacy_hash,output) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING").bind(profile).bind(&node.key).bind(&node.hash).bind(&lease.model).bind(policy).bind(&output).execute(pool).await?;
    Ok(Some(output))
}
#[derive(Debug)]
pub enum Refresh {
    NoInputs,
    Pending,
    Ready { output: Value, generation: i64 },
}

/// One dirty bucket per turn, then a bounded final reduction. A timeout or pause
/// preserves independently committed parts. No transaction spans model calls.
pub async fn refresh<P: Provider>(pool: &PgPool, provider: &P, lease: &Lease) -> Result<Refresh> {
    let profile: Uuid = lease.subject.parse()?;
    let mut read = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *read)
        .await?;
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_pending WHERE profile_id=$1::uuid)",
    )
    .bind(profile)
    .fetch_one(&mut *read)
    .await?
    {
        return Ok(Refresh::Pending);
    }

    let generation: Option<i64> =
        sqlx::query_scalar("SELECT generation FROM crm.memory_heads WHERE profile_id=$1")
            .bind(profile)
            .fetch_optional(&mut *read)
            .await?;
    let Some(generation) = generation else {
        return Ok(Refresh::NoInputs);
    };
    let policy: Privacy = serde_json::from_value(lease.privacy["policy"].clone())?;
    let policy = policy_hash(&policy);
    let include_private = lease.privacy["policy"]["include_messages"] == true;
    let include_notes = lease.privacy["policy"]["include_private_notes"] == true;
    // Hash-only projection: unchanged bucket bodies are not read or deserialized.
    let buckets=sqlx::query("SELECT bucket,encode(sha256(convert_to(string_agg(source_key||':'||input_hash,',' ORDER BY source_key),'UTF8')),'hex') signature,count(*) count,sum(payload_bytes)::bigint bytes FROM crm.memory_inputs WHERE profile_id=$1 AND (NOT private OR (source_kind='note' AND $3) OR (source_kind<>'note' AND $2)) GROUP BY bucket ORDER BY bucket").bind(profile).bind(include_private).bind(include_notes).fetch_all(&mut *read).await?;
    let mut summaries = std::collections::BTreeMap::new();
    let mut count = 0i64;
    let mut budget = 1usize;
    for bucket in buckets {
        let name: String = bucket.get("bucket");
        let signature = hash(&json!([
            PROMPT,
            bucket.get::<String, _>("signature"),
            lease.model,
            policy
        ]));
        let source_count: i64 = bucket.get("count");
        let saved:Option<Value>=sqlx::query_scalar("SELECT output FROM crm.memory_bucket_outputs WHERE profile_id=$1 AND bucket=$2 AND signature=$3 AND model=$4 AND privacy_hash=$5").bind(profile).bind(&name).bind(&signature).bind(&lease.model).bind(&policy).fetch_optional(&mut *read).await?;
        if let Some(output) = saved {
            summaries.insert(name.clone(), output);
            count += source_count;
            continue;
        }
        if (bucket.get::<i64, _>("bytes") > 8 * 1024 * 1024 || source_count > 10000)
            && source_count > 1
        {
            ensure!(name.len() < 64, "invalid Memory radix key");
            read.rollback().await?;
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT generation FROM crm.memory_heads WHERE profile_id=$1 FOR UPDATE")
                .bind(profile)
                .fetch_one(&mut *tx)
                .await?;
            sqlx::query("UPDATE crm.memory_inputs SET bucket=left(source_key,$3) WHERE profile_id=$1 AND bucket=$2").bind(profile).bind(&name).bind(name.len() as i32+1).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM crm.memory_bucket_outputs WHERE profile_id=$1 AND bucket=$2")
                .bind(profile)
                .bind(&name)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE crm.memory_heads SET generation=generation+1 WHERE profile_id=$1")
                .bind(profile)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(Refresh::Pending);
        }
        let rows:Vec<(String,Value)>=sqlx::query_as("SELECT m.source_key,CASE WHEN m.source_kind='connection' AND m.payload ? '_evidence_id' THEN jsonb_build_object('id',m.source_id,'kind',e.kind,'provider',c.provider,'at',e.occurred_at,'title',e.title,'text',e.body,'participants',e.metadata->'participants','direction',e.metadata->'direction','conversation_id',e.conversation_id) WHEN m.payload ? '_legacy_id' THEN legacy.payload ELSE m.payload END FROM crm.memory_inputs m LEFT JOIN crm.connection_evidence e ON e.id=CASE WHEN m.source_kind='connection' AND m.payload ? '_evidence_id' THEN (m.payload->>'_evidence_id')::uuid END LEFT JOIN crm.source_connections c ON c.id=e.connection_id LEFT JOIN crm.memory_legacy_current legacy ON legacy.kind=m.payload->>'_legacy_kind' AND legacy.entity_id=CASE WHEN m.payload ? '_legacy_id' THEN (m.payload->>'_legacy_id')::uuid END AND legacy.profile_id=m.profile_id WHERE m.profile_id=$1 AND m.bucket=$2 AND (NOT m.private OR (m.source_kind='note' AND $4) OR (m.source_kind<>'note' AND $3)) ORDER BY m.source_key").bind(profile).bind(&name).bind(include_private).bind(include_notes).fetch_all(&mut *read).await?;
        read.commit().await?;
        ensure!(rows.len() <= 10000, "Memory bucket requires further paging");
        let mut parts = Vec::new();
        for node in leaves(&name, &rows) {
            let Some(output) =
                part(pool, provider, lease, profile, &node, &policy, &mut budget).await?
            else {
                return Ok(Refresh::Pending);
            };
            parts.push(output);
        }
        let mut level = 0;
        while parts.len() > 1 {
            let mut reduced = Vec::new();
            for (index, group) in pack_parts(&parts).into_iter().enumerate() {
                let input = json!({"parts":group});
                let node = Node {
                    key: format!("reduce:{name}:{level}:{index}"),
                    hash: hash(&json!([PROMPT, &input])),
                    input,
                };
                let Some(output) =
                    part(pool, provider, lease, profile, &node, &policy, &mut budget).await?
                else {
                    return Ok(Refresh::Pending);
                };
                reduced.push(output);
            }
            parts = reduced;
            level += 1;
        }
        let output = parts
            .pop()
            .unwrap_or(json!({"summary":"","facts":[],"gaps":[]}));
        // The signature includes exact current input hashes. If inputs changed
        // concurrently this cached result simply misses on the next turn.
        sqlx::query("INSERT INTO crm.memory_bucket_outputs VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(profile_id,bucket) DO UPDATE SET signature=excluded.signature,model=excluded.model,privacy_hash=excluded.privacy_hash,output=excluded.output,source_count=excluded.source_count").bind(profile).bind(name).bind(signature).bind(&lease.model).bind(&policy).bind(output).bind(source_count).execute(pool).await?;
        return Ok(Refresh::Pending);
    }
    read.commit().await?;
    let name: String = sqlx::query_scalar(
        "SELECT display_name FROM crm.profiles WHERE id=$1 AND archived_at IS NULL",
    )
    .bind(profile)
    .fetch_one(pool)
    .await?;
    let max_depth = summaries.keys().map(String::len).max().unwrap_or(0);
    for depth in (1..=max_depth).rev() {
        let keys = summaries
            .keys()
            .filter(|key| key.len() == depth)
            .cloned()
            .collect::<Vec<_>>();
        let mut groups = std::collections::BTreeMap::<String, Vec<Value>>::new();
        for key in keys {
            let parent = key[..key.len() - 1].to_owned();
            groups
                .entry(parent)
                .or_default()
                .push(summaries.remove(&key).expect("Memory branch"));
        }
        for (prefix, mut values) in groups {
            if let Some(terminal) = summaries.remove(&prefix) {
                values.insert(0, terminal)
            }
            let mut level = 0;
            while values.len() > 1 {
                let mut reduced = Vec::new();
                for (index, group) in pack_parts(&values).into_iter().enumerate() {
                    let input = json!({"parts":group});
                    let node = Node {
                        key: format!("global:{prefix}:{level}:{index}"),
                        hash: hash(&json!([PROMPT, &input])),
                        input,
                    };
                    let Some(output) =
                        part(pool, provider, lease, profile, &node, &policy, &mut budget).await?
                    else {
                        return Ok(Refresh::Pending);
                    };
                    reduced.push(output);
                }
                values = reduced;
                level += 1;
            }
            if let Some(value) = values.pop() {
                summaries.insert(prefix, value);
            }
        }
    }
    let input = json!({"person":name,"evidence":summaries.into_values().collect::<Vec<_>>(),"source_count":count});
    let signature = hash(&json!([PROMPT, lease.model, policy, &input]));
    let saved:Option<Value>=sqlx::query_scalar("SELECT output FROM crm.memory_current WHERE profile_id=$1 AND input_hash=$2 AND model=$3 AND privacy_hash=$4").bind(profile).bind(&signature).bind(&lease.model).bind(&policy).fetch_optional(pool).await?;
    let before = budget;
    let output = if let Some(output) = saved {
        output
    } else {
        let node = Node {
            key: "final".into(),
            hash: signature.clone(),
            input,
        };
        let Some(output) =
            part(pool, provider, lease, profile, &node, &policy, &mut budget).await?
        else {
            return Ok(Refresh::Pending);
        };
        output
    };
    let newly_generated = before != budget;
    let mut tx = pool.begin().await?;
    let current: i64 =
        sqlx::query_scalar("SELECT generation FROM crm.memory_heads WHERE profile_id=$1 FOR SHARE")
            .bind(profile)
            .fetch_one(&mut *tx)
            .await?;
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.ai_jobs j JOIN crm.ai_subject_heads h ON h.job_id=j.id JOIN crm.ai_settings s ON s.id AND s.enabled WHERE j.id=$1 AND j.token=$2 AND j.status='running' AND j.lease_until>clock_timestamp() AND j.privacy->'policy'=jsonb_build_object('include_messages',s.include_messages,'include_private_notes',s.include_private_notes))").bind(lease.id).bind(lease.token).fetch_one(&mut *tx).await?;
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_pending WHERE profile_id=$1)",
    )
    .bind(profile)
    .fetch_one(&mut *tx)
    .await?;
    if current != generation || !active || pending {
        tx.rollback().await?;
        return Ok(Refresh::Pending);
    }
    sqlx::query("INSERT INTO crm.memory_current(profile_id,generation,input_hash,model,privacy_hash,output) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(profile_id) DO UPDATE SET generation=excluded.generation,input_hash=excluded.input_hash,model=excluded.model,privacy_hash=excluded.privacy_hash,output=excluded.output,generated_at=now()").bind(profile).bind(generation).bind(signature).bind(&lease.model).bind(policy).bind(&output).execute(&mut *tx).await?;
    tx.commit().await?;
    if newly_generated {
        Ok(Refresh::Pending)
    } else {
        Ok(Refresh::Ready { output, generation })
    }
}

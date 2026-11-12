//! Durable, resumable, bounded imports. Request receipts survive worker crashes.
use crate::{Provider, Record, domain, identity};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BatchRecord {
    pub external_id: String,
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub source_account_external_id: Option<String>,
    #[serde(default)]
    pub occurred_at: Option<String>,
    #[serde(default)]
    pub conversation_external_id: Option<String>,
    #[serde(default)]
    pub conversation_type: Option<String>,
    #[serde(default)]
    pub message_type: Option<String>,
    pub content: Value,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}
#[derive(Clone, Debug)]
pub enum ImportSource {
    Platform(Provider),
    Notebook,
}
impl ImportSource {
    pub fn schema(&self) -> &'static str {
        match self {
            Self::Platform(p) => p.schema(),
            Self::Notebook => "monica",
        }
    }
    pub fn provider(&self) -> Result<Provider> {
        match self {
            Self::Platform(p) => Ok(*p),
            Self::Notebook => bail!("Notebook imports use CRM projection"),
        }
    }
}
impl Serialize for ImportSource {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.schema())
    }
}
impl<'de> Deserialize<'de> for ImportSource {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == "monica" {
            Ok(Self::Notebook)
        } else {
            Provider::parse(&value)
                .map(Self::Platform)
                .map_err(serde::de::Error::custom)
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Batch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<Uuid>,
    pub source: ImportSource,
    pub stream: String,
    #[serde(default = "one")]
    pub schema_version: i32,
    pub batch_id: String,
    pub idempotency_key: String,
    pub observed_at: String,
    #[serde(default)]
    pub cursor_before: Option<String>,
    #[serde(default)]
    pub cursor_after: Option<String>,
    #[serde(default)]
    pub records: Vec<BatchRecord>,
    #[serde(default)]
    pub raw_objects: Vec<Value>,
    #[serde(default)]
    pub media_manifest: Vec<Value>,
}
fn one() -> i32 {
    1
}
impl Batch {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.stream.is_empty()
                && self.stream.len() <= 120
                && !self.batch_id.is_empty()
                && self.batch_id.len() <= 255
                && !self.idempotency_key.is_empty()
                && self.idempotency_key.len() <= 255
                && self.schema_version > 0,
            "invalid batch identity"
        );
        ensure!(self.records.len() <= 10000, "batch exceeds 10000 records");
        for record in &self.records {
            crate::media::validate_record_media(record)?;
            ensure!(
                !record.external_id.is_empty() && record.external_id.len() <= 1024,
                "invalid record identity"
            );
        }
        Ok(())
    }
}
pub async fn receive(
    tx: &mut Transaction<'_, Postgres>,
    batch: &Batch,
    hash: &str,
) -> Result<Uuid> {
    batch.validate()?;
    if let Some(cid) = batch.connection_id {
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM crm.source_connections WHERE id=$1 AND provider=$2)",
        )
        .bind(cid)
        .bind(batch.source.schema())
        .fetch_one(&mut **tx)
        .await?;
        ensure!(valid, "Source connection does not match import provider");
    }
    let id = Uuid::new_v4();
    let row=sqlx::query("INSERT INTO crm.import_batches(id,source,stream,schema_version,batch_id,idempotency_key,body_hash,cursor_before,cursor_after,observed_at,record_count,connection_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::timestamptz,$11,$12) ON CONFLICT(idempotency_key) DO UPDATE SET idempotency_key=excluded.idempotency_key RETURNING id,body_hash")
        .bind(id).bind(batch.source.schema()).bind(&batch.stream).bind(batch.schema_version).bind(&batch.batch_id).bind(&batch.idempotency_key).bind(hash).bind(&batch.cursor_before).bind(&batch.cursor_after).bind(&batch.observed_at).bind(batch.records.len() as i64).bind(batch.connection_id).fetch_one(&mut **tx).await?;
    ensure!(
        row.get::<String, _>("body_hash") == hash,
        "idempotency key reused with different batch"
    );
    let id: Uuid = row.get("id");
    sqlx::query("INSERT INTO crm.import_batch_inputs(batch_id,payload) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(id).bind(serde_json::to_value(batch)?).execute(&mut **tx).await?;
    if batch.records.iter().any(|record| {
        record
            .extra
            .get("media")
            .and_then(Value::as_array)
            .is_some_and(|media| !media.is_empty())
    }) {
        sqlx::query("INSERT INTO crm.import_record_media_progress(batch_id) VALUES($1) ON CONFLICT DO NOTHING").bind(id).execute(&mut **tx).await?;
    }
    Ok(id)
}
pub async fn receipt(pool: &PgPool, id: Uuid) -> Result<Value> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(b)-'body_hash'-(CASE WHEN connection_id IS NULL THEN 'connection_id' ELSE '' END) FROM crm.import_batches b WHERE id=$1")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

pub async fn resume(pool: &PgPool, id: Uuid) -> Result<Value> {
    while !resume_step(pool, id).await? {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    receipt(pool, id).await
}

pub async fn resume_step(pool: &PgPool, id: Uuid) -> Result<bool> {
    Ok(resume_chunk(pool, id).await?.unwrap_or(false))
}

pub async fn resume_chunk(pool: &PgPool, id: Uuid) -> Result<Option<bool>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='2s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout='15s'")
        .execute(&mut *tx)
        .await?;
    let row=sqlx::query("SELECT b.status,b.processed_count,i.payload FROM crm.import_batches b JOIN crm.import_batch_inputs i ON i.batch_id=b.id WHERE b.id=$1 FOR UPDATE OF b SKIP LOCKED")
            .bind(id).fetch_optional(&mut *tx).await?;
    let Some(row) = row else { return Ok(None) };
    if row.get::<String, _>("status") == "complete" {
        tx.commit().await?;
        return Ok(Some(true));
    }
    let batch: Batch = serde_json::from_value(row.get("payload"))?;
    let start = row.get::<i64, _>("processed_count") as usize;
    let end = (start
        + if batch.stream == "messages" {
            1000
        } else {
            100
        })
    .min(batch.records.len());
    let mut counts = [0i64; 3];
    if matches!(batch.source, ImportSource::Notebook) {
        counts = notebook::project(&mut tx, &batch, start, end).await?;
    } else if let Some(cid) = batch.connection_id {
        counts = commit_connection_chunk(&mut tx, &batch, cid, start, end).await?;
    } else if batch.stream == "messages" {
        let messages = batch.records[start..end]
            .iter()
            .map(|r| {
                Ok(crate::messages::Message {
                    account: r
                        .content
                        .get("account")
                        .and_then(Value::as_str)
                        .or(r.source_account_external_id.as_deref())
                        .context("message account required")?
                        .into(),
                    conversation: r
                        .content
                        .get("conversation")
                        .or_else(|| r.content.get("conversation_external_id"))
                        .and_then(Value::as_str)
                        .or(r.conversation_external_id.as_deref())
                        .context("message conversation required")?
                        .into(),
                    external_id: r.external_id.clone(),
                    sent_at_ms: r
                        .content
                        .get("sent_at_ms")
                        .and_then(Value::as_i64)
                        .or_else(|| {
                            r.occurred_at
                                .as_deref()
                                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                .map(|d| d.timestamp_millis())
                        })
                        .context("message sent_at_ms required")?,
                    sender: r
                        .content
                        .get("sender")
                        .or_else(|| r.content.get("sender_external_id"))
                        .and_then(Value::as_str)
                        .context("message sender required")?
                        .into(),
                    body: r
                        .content
                        .get("body")
                        .or_else(|| r.content.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        counts[0] = append_messages(&mut tx, batch.source.provider()?, &messages).await? as i64;
        counts[2] = messages.len() as i64 - counts[0];
        let types=batch.records[start..end].iter().zip(&messages).filter_map(|(r,m)| {
            r.message_type.as_deref().or_else(||r.content.get("message_type").and_then(Value::as_str)).map(|kind| {
                let conversation=domain::conversation_id(batch.source.provider().expect("platform import"),&m.account,&m.conversation).expect("validated account");
                json!({"id":identity::object_id(conversation,"message",&m.external_id),"kind":kind})
            })
        }).collect::<Vec<_>>();
        if !types.is_empty() {
            sqlx::query("UPDATE crm.message_search m SET message_type=x.kind FROM jsonb_to_recordset($1) AS x(id uuid,kind text) WHERE m.message_id=x.id AND m.message_type IS DISTINCT FROM x.kind").bind(json!(types)).execute(&mut *tx).await?;
        }
    } else if ["calls", "money", "contact_events"].contains(&batch.stream.as_str()) {
        counts[0] = append_events(
            &mut tx,
            batch.source.provider()?,
            &batch.stream,
            &batch.records[start..end],
        )
        .await? as i64;
        counts[2] = (end - start) as i64 - counts[0];
    } else {
        let provider = batch.source.provider()?;
        let records = batch.records[start..end]
            .iter()
            .map(|r| {
                let kind = r
                    .entity_type
                    .as_deref()
                    .unwrap_or(match batch.stream.as_str() {
                        "posts" | "moments" => "post",
                        "stories" => "story",
                        "chat_months" => "chat_month",
                        "addresses" => "address",
                        "employment" => "employment",
                        "education" => "education",
                        "calls" => "call",
                        "money" => "money",
                        _ => "profile",
                    });
                let account = r
                    .source_account_external_id
                    .as_deref()
                    .or_else(|| {
                        r.content
                            .get("account")
                            .or_else(|| r.content.get("username"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or(&r.external_id);
                Record {
                    provider,
                    account: account.into(),
                    object: r.external_id.clone(),
                    kind: kind.into(),
                    event: format!("batch:{}:{}", batch.idempotency_key, r.external_id),
                    payload: r.content.clone(),
                }
            })
            .collect::<Vec<_>>();
        let mut keys = BTreeSet::new();
        for record in &records {
            keys.insert(crate::object_id(record)?);
        }
        for key in keys {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('source-object:'||$1,0))")
                .bind(key.to_string())
                .execute(&mut *tx)
                .await?;
        }
        for (offset, record) in records.iter().enumerate() {
            let observed = domain::observe(&mut tx, record).await?;
            counts[match observed.state {
                "new" => 0,
                "changed" => 1,
                _ => 2,
            }] += 1;
            let s = batch.source.schema();
            sqlx::query(&format!("INSERT INTO {s}.revision_metadata(object_id,version,occurred_at,observed_at) VALUES($1,$2,$3::timestamptz,$4::timestamptz) ON CONFLICT DO NOTHING"))
                    .bind(observed.object).bind(observed.version).bind(&batch.records[start+offset].occurred_at).bind(&batch.observed_at).execute(&mut *tx).await?;
        }
    }
    let complete = end == batch.records.len();
    sqlx::query("UPDATE crm.import_batches SET processed_count=$2,inserted_count=inserted_count+$3,changed_count=changed_count+$4,unchanged_count=unchanged_count+$5,status=$6,completed_at=CASE WHEN $7 THEN now() END,error_summary=NULL,retry_after=clock_timestamp() WHERE id=$1")
            .bind(id).bind(end as i64).bind(counts[0]).bind(counts[1]).bind(counts[2]).bind(if complete{"complete"}else{"running"}).bind(complete).execute(&mut *tx).await?;
    if complete && batch.connection_id.is_none() {
        let moved=sqlx::query("INSERT INTO crm.source_cursors(source,stream,cursor,batch_id) VALUES($1,$2,$3,$4) ON CONFLICT(source,stream) DO UPDATE SET cursor=excluded.cursor,batch_id=excluded.batch_id,updated_at=now() WHERE crm.source_cursors.cursor IS NOT DISTINCT FROM $5 OR crm.source_cursors.batch_id=$4")
                .bind(batch.source.schema()).bind(&batch.stream).bind(&batch.cursor_after).bind(id).bind(&batch.cursor_before).execute(&mut *tx).await?.rows_affected();
        ensure!(moved == 1, "source cursor prerequisite has not committed");
    }
    tx.commit().await?;
    Ok(Some(complete))
}

pub async fn resume_pending(pool: &PgPool) -> Result<usize> {
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM crm.import_batches WHERE status IN ('received','running') AND retry_after<=now() ORDER BY retry_after,received_at LIMIT 10").fetch_all(pool).await?;
    for id in &ids {
        match resume_chunk(pool, *id).await {
            Ok(None) => continue,
            Ok(Some(_)) => return Ok(1),
            Err(error) => {
                // Receipt survives; isolate poison batches while unrelated sources run.
                let reason = if error.downcast_ref::<sqlx::Error>().is_some() {
                    "database operation failed"
                } else {
                    "batch validation or identity conflict"
                };
                sqlx::query("UPDATE crm.import_batches SET failure_count=failure_count+1,error_summary=$2,retry_after=now()+interval '1 minute',status=CASE WHEN failure_count>=4 THEN 'failed' ELSE status END WHERE id=$1")
                .bind(id).bind(reason).execute(pool).await?;
                return Ok(1);
            }
        }
    }
    Ok(0)
}

/// One bounded set-based source + typed message projection transaction.
pub async fn append_messages(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    records: &[crate::messages::Message],
) -> Result<u64> {
    ensure!(
        records.len() <= crate::messages::MAX_RECORDS,
        "message batch exceeds record limit"
    );
    if records.is_empty() {
        return Ok(0);
    }
    let s = provider.schema();
    let mut input = Vec::with_capacity(records.len());
    let mut seen = std::collections::BTreeMap::new();
    for r in records {
        ensure!(
            [&r.account, &r.conversation, &r.external_id, &r.sender]
                .iter()
                .all(|x| !x.is_empty() && x.len() <= 1024),
            "invalid message identity"
        );
        let key = (&r.account, &r.conversation, &r.external_id);
        if let Some(old) = seen.insert(key, r) {
            ensure!(old == r, "conflicting identities inside batch");
            continue;
        }
        let conversation = domain::conversation_id(provider, &r.account, &r.conversation)?;
        input.push(json!({"account":r.account,"conversation":r.conversation,"external_id":r.external_id,"sent_at_ms":r.sent_at_ms,"sender":r.sender,"body":r.body,"conversation_id":conversation,"message_id":identity::object_id(conversation,"message",&r.external_id),"account_id":identity::account_id(provider,&r.conversation)?,"sender_id":identity::account_id(provider,&r.sender)?}));
    }
    let memory_ids = input
        .iter()
        .map(|r| serde_json::from_value::<Uuid>(r["message_id"].clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let input = json!(input);
    ensure!(
        serde_json::to_vec(&input)?.len() <= crate::messages::MAX_BYTES,
        "message batch exceeds byte limit"
    );
    let shape = "account text,conversation text,external_id text,sent_at_ms bigint,sender text,body text,conversation_id uuid,message_id uuid,account_id uuid,sender_id uuid";
    sqlx::query(&format!("WITH i AS (SELECT * FROM jsonb_to_recordset($1) AS x({shape})),a AS (SELECT account_id AS id,conversation AS external_id FROM i UNION SELECT sender_id,sender FROM i) INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) SELECT id,$2,external_id,CASE WHEN $2='wechat' AND right(external_id,9)='@chatroom' THEN 'group' ELSE 'person' END,external_id FROM a ORDER BY id ON CONFLICT DO NOTHING"))
        .bind(&input).bind(s).execute(&mut **tx).await?;
    sqlx::query(&format!("SELECT id FROM crm.source_accounts WHERE id IN(SELECT account_id FROM jsonb_to_recordset($1) AS x({shape}) UNION SELECT sender_id FROM jsonb_to_recordset($1) AS x({shape})) ORDER BY id FOR SHARE"))
        .bind(&input).fetch_all(&mut **tx).await?;
    sqlx::query(&format!("WITH i AS (SELECT DISTINCT ON(conversation_id) * FROM jsonb_to_recordset($1) AS x({shape}) ORDER BY conversation_id) INSERT INTO crm.conversations(id,provider,account,external_id,source_account_id,profile_id,conversation_type) SELECT i.conversation_id,$2,i.account,i.conversation,i.account_id,l.profile_id,CASE WHEN right(i.conversation,9)='@chatroom' THEN 'group' ELSE 'direct' END FROM i LEFT JOIN crm.source_account_links l ON l.account_id=i.account_id ORDER BY i.conversation_id ON CONFLICT DO NOTHING"))
        .bind(&input).bind(s).execute(&mut **tx).await?;
    sqlx::query(&format!("SELECT id FROM crm.conversations WHERE id IN(SELECT conversation_id FROM jsonb_to_recordset($1) AS x({shape})) ORDER BY id FOR UPDATE"))
        .bind(&input).fetch_all(&mut **tx).await?;
    let count:i64=sqlx::query_scalar(&format!(r#"WITH i AS (SELECT * FROM jsonb_to_recordset($1) AS x({shape})),
      source_added AS (INSERT INTO {s}.messages(account,conversation,external_id,sent_at_ms,sender,body) SELECT account,conversation,external_id,sent_at_ms,sender,body FROM i ORDER BY account,conversation,external_id ON CONFLICT DO NOTHING RETURNING *),
      added AS (INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_account_id,occurred_at,body) SELECT message_id,conversation_id,$2,account,external_id,sender,sender_id,to_timestamp(sent_at_ms/1000.0),body FROM i ORDER BY message_id ON CONFLICT DO NOTHING RETURNING *),
      counts AS (SELECT conversation_id,count(*) AS n,min(occurred_at) AS first_at FROM added GROUP BY conversation_id),
      latest AS (SELECT DISTINCT ON(conversation_id) conversation_id,occurred_at,message_id,body FROM added ORDER BY conversation_id,occurred_at DESC,message_id DESC),
      updated AS (UPDATE crm.conversations c SET message_count=c.message_count+n,first_message_at=least(c.first_message_at,k.first_at),last_message_at=greatest(c.last_message_at,l.occurred_at),last_message_id=CASE WHEN c.last_message_at IS NULL OR (c.last_message_at,c.last_message_id)<(l.occurred_at,l.message_id) THEN l.message_id ELSE c.last_message_id END,last_message_preview=CASE WHEN c.last_message_at IS NULL OR (c.last_message_at,c.last_message_id)<(l.occurred_at,l.message_id) THEN left(l.body,80) ELSE c.last_message_preview END FROM counts k JOIN latest l USING(conversation_id) WHERE c.id=k.conversation_id RETURNING c.id),
      months AS (INSERT INTO crm.message_months(conversation_id,month,sent,received,first_message_at,last_message_at) SELECT conversation_id,to_char(occurred_at AT TIME ZONE 'America/Chicago','YYYY-MM'),count(*) FILTER(WHERE sender=account),count(*) FILTER(WHERE sender<>account),min(occurred_at),max(occurred_at) FROM added GROUP BY conversation_id,2 ORDER BY conversation_id,2 ON CONFLICT(conversation_id,month) DO UPDATE SET sent=crm.message_months.sent+excluded.sent,received=crm.message_months.received+excluded.received,first_message_at=least(crm.message_months.first_message_at,excluded.first_message_at),last_message_at=greatest(crm.message_months.last_message_at,excluded.last_message_at) RETURNING 1),
      months_utc AS (INSERT INTO crm.message_months_utc(conversation_id,month,sent,received,first_message_at,last_message_at) SELECT conversation_id,to_char(occurred_at AT TIME ZONE 'UTC','YYYY-MM'),count(*) FILTER(WHERE sender=account),count(*) FILTER(WHERE sender<>account),min(occurred_at),max(occurred_at) FROM added GROUP BY conversation_id,2 ORDER BY conversation_id,2 ON CONFLICT(conversation_id,month) DO UPDATE SET sent=crm.message_months_utc.sent+excluded.sent,received=crm.message_months_utc.received+excluded.received,first_message_at=least(crm.message_months_utc.first_message_at,excluded.first_message_at),last_message_at=greatest(crm.message_months_utc.last_message_at,excluded.last_message_at) RETURNING 1),
      source_counts AS (SELECT account,conversation,count(*) AS n FROM source_added GROUP BY account,conversation),
      source_latest AS (SELECT DISTINCT ON(account,conversation) account,conversation,sent_at_ms,external_id,left(body,80) AS preview FROM source_added ORDER BY account,conversation,sent_at_ms DESC,external_id DESC),
      state AS (INSERT INTO crm.conversation_state AS current SELECT $2,c.account,c.conversation,c.n,l.sent_at_ms,l.external_id,l.preview,1 FROM source_counts c JOIN source_latest l USING(account,conversation) ORDER BY c.account,c.conversation ON CONFLICT(provider,account,conversation) DO UPDATE SET message_count=current.message_count+excluded.message_count,last_at_ms=greatest(current.last_at_ms,excluded.last_at_ms),last_id=CASE WHEN (excluded.last_at_ms,excluded.last_id)>(current.last_at_ms,current.last_id) THEN excluded.last_id ELSE current.last_id END,preview=CASE WHEN (excluded.last_at_ms,excluded.last_id)>(current.last_at_ms,current.last_id) THEN excluded.preview ELSE current.preview END,generation=current.generation+1 RETURNING *),
      scheduled AS (INSERT INTO crm.conversation_work AS work(provider,account,conversation,requested) SELECT provider,account,conversation,generation FROM state ORDER BY provider,account,conversation ON CONFLICT(provider,account,conversation) DO UPDATE SET requested=excluded.requested RETURNING 1)
      SELECT count(*) FROM source_added"#)).bind(&input).bind(s).fetch_one(&mut **tx).await?;
    let conflict:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM jsonb_to_recordset($1) AS x({shape}) JOIN {s}.messages m USING(account,conversation,external_id) WHERE (m.sent_at_ms,m.sender,m.body) IS DISTINCT FROM (x.sent_at_ms,x.sender,x.body))"))
        .bind(input).fetch_one(&mut **tx).await?;
    ensure!(
        !conflict,
        "message identity conflicts with saved fact; use correction path"
    );
    crate::ai::memory::legacy::sync(tx, "message", &memory_ids).await?;
    Ok(count as u64)
}

pub async fn correct_message(
    pool: &PgPool,
    provider: Provider,
    account: &str,
    conversation: &str,
    external: &str,
    event: &str,
    body: Option<&str>,
) -> Result<()> {
    ensure!(!event.is_empty(), "correction needs stable event id");
    let s = provider.schema();
    let id = identity::object_id(
        domain::conversation_id(provider, account, conversation)?,
        "message",
        external,
    );
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM crm.source_accounts WHERE id=$1 FOR SHARE")
        .bind(identity::account_id(provider, conversation)?)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("SELECT id FROM crm.conversations WHERE id=$1 FOR UPDATE")
        .bind(domain::conversation_id(provider, account, conversation)?)
        .fetch_one(&mut *tx)
        .await?;
    let previous=sqlx::query("SELECT tombstone,body,conversation_id,sender,account,occurred_at::text AS occurred_at FROM crm.message_search WHERE message_id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.context("message not found")?;
    let inserted=sqlx::query(&format!("INSERT INTO {s}.message_corrections(account,conversation,external_id,event_id,body,tombstone) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING"))
        .bind(account).bind(conversation).bind(external).bind(event).bind(body).bind(body.is_none()).execute(&mut *tx).await?.rows_affected();
    if inserted == 0 {
        let saved:Option<String>=sqlx::query_scalar(&format!("SELECT body FROM {s}.message_corrections WHERE account=$1 AND conversation=$2 AND external_id=$3 AND event_id=$4")).bind(account).bind(conversation).bind(external).bind(event).fetch_one(&mut *tx).await?;
        ensure!(
            saved.as_deref() == body,
            "correction event reused with different content"
        );
        return Ok(());
    }
    let delta = i64::from(body.is_some()) - i64::from(!previous.get::<bool, _>("tombstone"));
    sqlx::query(
        "UPDATE crm.message_search SET body=coalesce($2,body),tombstone=$3 WHERE message_id=$1",
    )
    .bind(id)
    .bind(body)
    .bind(body.is_none())
    .execute(&mut *tx)
    .await?;
    let conv: Uuid = previous.get("conversation_id");
    sqlx::query("UPDATE crm.conversations SET message_count=message_count+$2,first_message_at=(SELECT min(occurred_at) FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone),last_message_at=(SELECT max(occurred_at) FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone),last_message_id=(SELECT message_id FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1),last_message_preview=(SELECT left(body,80) FROM crm.message_search WHERE conversation_id=$1 AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1) WHERE id=$1").bind(conv).bind(delta).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.message_months SET sent=sent+CASE WHEN $3 THEN $4 ELSE 0 END,received=received+CASE WHEN $3 THEN 0 ELSE $4 END WHERE conversation_id=$1 AND month=to_char($2::timestamptz AT TIME ZONE 'America/Chicago','YYYY-MM')")
        .bind(conv).bind(previous.get::<String,_>("occurred_at")).bind(previous.get::<String,_>("sender")==previous.get::<String,_>("account")).bind(delta).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.message_months_utc SET sent=sent+CASE WHEN $3 THEN $4 ELSE 0 END,received=received+CASE WHEN $3 THEN 0 ELSE $4 END WHERE conversation_id=$1 AND month=to_char($2::timestamptz AT TIME ZONE 'UTC','YYYY-MM')")
        .bind(conv).bind(previous.get::<String,_>("occurred_at")).bind(previous.get::<String,_>("sender")==previous.get::<String,_>("account")).bind(delta).execute(&mut *tx).await?;
    sqlx::query("WITH changed AS (UPDATE crm.conversation_state SET message_count=message_count+$4,last_at_ms=coalesce((SELECT (extract(epoch FROM max(occurred_at))*1000)::bigint FROM crm.message_search WHERE conversation_id=$5 AND NOT tombstone),0),last_id=coalesce((SELECT external_id FROM crm.message_search WHERE conversation_id=$5 AND NOT tombstone ORDER BY occurred_at DESC,external_id DESC LIMIT 1),''),preview=coalesce((SELECT left(body,80) FROM crm.message_search WHERE conversation_id=$5 AND NOT tombstone ORDER BY occurred_at DESC,external_id DESC LIMIT 1),''),generation=generation+1 WHERE provider=$1 AND account=$2 AND conversation=$3 RETURNING *) INSERT INTO crm.conversation_work(provider,account,conversation,requested) SELECT provider,account,conversation,generation FROM changed ON CONFLICT(provider,account,conversation) DO UPDATE SET requested=excluded.requested")
        .bind(s).bind(account).bind(conversation).bind(delta).bind(conv).execute(&mut *tx).await?;
    crate::ai::memory::legacy::sync(&mut tx, "message", &[id]).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn checkpoint(
    pool: &PgPool,
    source: &str,
    stream: &str,
    file: &str,
    checksum: &str,
    offset: i64,
) -> Result<()> {
    ensure!(
        offset >= 0 && !file.is_empty() && !checksum.is_empty(),
        "invalid checkpoint"
    );
    let n=sqlx::query("INSERT INTO crm.source_checkpoints(source,stream,file_id,checksum,record_offset) VALUES($1,$2,$3,$4,$5) ON CONFLICT(source,stream,file_id) DO UPDATE SET record_offset=excluded.record_offset,updated_at=now() WHERE crm.source_checkpoints.checksum=excluded.checksum AND crm.source_checkpoints.record_offset<=excluded.record_offset")
        .bind(source).bind(stream).bind(file).bind(checksum).bind(offset).execute(pool).await?.rows_affected();
    if n != 1 {
        bail!("checkpoint checksum changed or offset moved backwards")
    }
    Ok(())
}

/// Calls/payments are immutable source facts, with no revision/job per event.
pub async fn append_events(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    stream: &str,
    records: &[BatchRecord],
) -> Result<u64> {
    ensure!(records.len() <= 1000, "event batch exceeds 1000 records");
    if records.is_empty() {
        return Ok(0);
    }
    let mut input = Vec::new();
    let mut seen = std::collections::BTreeMap::new();
    for record in records {
        let account = record
            .source_account_external_id
            .as_deref()
            .context("event source account required")?;
        let kind = record
            .entity_type
            .as_deref()
            .or_else(|| record.content.get("event_kind").and_then(Value::as_str))
            .unwrap_or(if stream == "money" { "money" } else { "call" });
        ensure!(
            ["call", "money", "interaction"].contains(&kind),
            "unknown contact event kind"
        );
        identity::validate_json(&record.content)?;
        let content = identity::json_safe(&record.content);
        let hash = identity::observation_hash(&content);
        if let Some(previous) =
            seen.insert((account, kind, record.external_id.as_str()), hash.clone())
        {
            ensure!(previous == hash, "conflicting duplicate events");
            continue;
        }
        let account_id = identity::account_id(provider, account)?;
        input.push(json!({"account":account,"account_id":account_id,"id":identity::object_id(account_id,kind,&record.external_id),"kind":kind,"external_id":record.external_id,"occurred_at":record.occurred_at,"content":content,"hash":hash}));
    }
    let input = json!(input);
    ensure!(
        serde_json::to_vec(&input)?.len() <= 4 * 1024 * 1024,
        "event batch exceeds 4MiB"
    );
    let shape = "account text,account_id uuid,id uuid,kind text,external_id text,occurred_at timestamptz,content jsonb,hash text";
    let s = provider.schema();
    sqlx::query(&format!("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) SELECT DISTINCT account_id,$2,account,CASE WHEN right(account,9)='@chatroom' THEN 'group' ELSE 'person' END,account FROM jsonb_to_recordset($1) AS x({shape}) ORDER BY account_id ON CONFLICT DO NOTHING"))
        .bind(&input).bind(s).execute(&mut **tx).await?;
    sqlx::query(&format!("SELECT id FROM crm.source_accounts WHERE id IN(SELECT account_id FROM jsonb_to_recordset($1) AS x({shape})) ORDER BY id FOR SHARE")).bind(&input).fetch_all(&mut **tx).await?;
    let count:i64=sqlx::query_scalar(&format!("WITH i AS(SELECT * FROM jsonb_to_recordset($1) AS x({shape})),added AS(INSERT INTO {s}.events SELECT account,kind,external_id,occurred_at,content,hash FROM i ORDER BY account,kind,external_id ON CONFLICT DO NOTHING RETURNING *),projected AS(INSERT INTO crm.contact_events(id,provider,source_account_id,profile_id,kind,external_id,occurred_at,content) SELECT i.id,$2,i.account_id,l.profile_id,i.kind,i.external_id,i.occurred_at,i.content FROM i LEFT JOIN crm.source_account_links l ON l.account_id=i.account_id ORDER BY i.id ON CONFLICT DO NOTHING RETURNING 1) SELECT count(*) FROM added"))
        .bind(&input).bind(s).fetch_one(&mut **tx).await?;
    let conflict:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM jsonb_to_recordset($1) AS x({shape}) JOIN {s}.events e USING(account,kind,external_id) WHERE (e.hash,e.occurred_at) IS DISTINCT FROM(x.hash,x.occurred_at))")).bind(input).fetch_one(&mut **tx).await?;
    ensure!(
        !conflict,
        "contact event identity conflicts with saved fact"
    );
    Ok(count as u64)
}

async fn commit_connection_chunk(
    tx: &mut Transaction<'_, Postgres>,
    batch: &Batch,
    cid: Uuid,
    start: usize,
    end: usize,
) -> Result<[i64; 3]> {
    crate::connections::lock_connection(tx, cid).await?;
    let before =
        crate::connections::cursor(tx, batch.source.provider()?, cid, &batch.stream).await?;
    if start == 0 {
        ensure!(
            before.as_str() == batch.cursor_before.as_deref()
                && (batch.cursor_before.is_some() || before.is_null() || before == json!({})),
            "source cursor prerequisite has not committed"
        );
    }
    let mut records = Vec::new();
    let mut bindings = Vec::new();
    for record in &batch.records[start..end] {
        let mut value = if record.content.is_object() {
            record.content.clone()
        } else {
            json!({"value":record.content})
        };
        value["external_id"] = json!(record.external_id);
        let kind = record
            .entity_type
            .as_deref()
            .unwrap_or(match batch.stream.as_str() {
                "messages" | "chat" | "conversations" => "message",
                "contacts" => "contact",
                _ => "record",
            });
        value["kind"] = json!(kind);
        if kind == "message" {
            let thread = record
                .conversation_external_id
                .as_deref()
                .or_else(|| {
                    record
                        .content
                        .get("conversation_external_id")
                        .and_then(Value::as_str)
                })
                .unwrap_or(&record.external_id);
            value["thread_id"] = json!(thread);
            let at = record
                .occurred_at
                .as_deref()
                .or_else(|| record.content.get("occurred_at").and_then(Value::as_str))
                .context("message records require occurred_at")?;
            value["occurred_at"] = json!(at);
            if let Some(message_type) = &record.message_type {
                value["message_type"] = json!(message_type);
            }
            let kind = record
                .conversation_type
                .as_deref()
                .or_else(|| {
                    record
                        .content
                        .get("conversation_type")
                        .and_then(Value::as_str)
                })
                .unwrap_or("direct");
            let kind = if kind == "group" { "group" } else { "direct" };
            let profile = |key: &str| -> Result<Option<Uuid>> {
                record
                    .extra
                    .get(key)
                    .filter(|v| !v.is_null())
                    .map(|v| {
                        Uuid::parse_str(v.as_str().context("profile identifier must be UUID")?)
                            .map_err(Into::into)
                    })
                    .transpose()
            };
            bindings.push(json!({"record_index":records.len(),"external_id":record.external_id,"profile_id":profile("profile_id")?,"sender_profile_id":profile("sender_profile_id")?,"conversation_type":kind}));
        }
        records.push(value);
    }
    let ids: Vec<&str> = records
        .iter()
        .filter_map(|r| r["external_id"].as_str())
        .collect();
    let existing:Vec<(String,i64,String)>=sqlx::query_as(&format!("SELECT external_id,revision,content_hash FROM {}.connection_record_heads WHERE connection_id=$1 AND external_id=ANY($2)",batch.source.schema())).bind(cid).bind(&ids).fetch_all(&mut **tx).await?;
    let inserted = ids
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|id| !existing.iter().any(|old| &old.0 == id))
        .count() as i64;
    let mut heads: std::collections::BTreeMap<String, (i64, String)> = existing
        .into_iter()
        .map(|(id, revision, hash)| (id, (revision, hash)))
        .collect();
    let versions: Vec<i64> = records
        .iter()
        .map(|record| {
            let external = record["external_id"].as_str().unwrap();
            let hash = identity::bytes_hash(&identity::canonical_json(record));
            let previous = heads.get(external);
            let revision = previous.map_or(1, |(revision, old)| {
                if old == &hash {
                    *revision
                } else {
                    revision + 1
                }
            });
            heads.insert(external.to_owned(), (revision, hash));
            revision
        })
        .collect();
    for binding in &mut bindings {
        binding["revision"] = json!(versions[binding["record_index"].as_u64().unwrap() as usize]);
        binding.as_object_mut().unwrap().remove("record_index");
    }
    let cursor = if end == batch.records.len() {
        json!(batch.cursor_after)
    } else {
        before
    };
    let changed = crate::connections::commit_page(
        tx,
        batch.source.provider()?,
        cid,
        &batch.stream,
        &records,
        &cursor,
    )
    .await? as i64;
    if !bindings.is_empty() {
        let bindings = json!(bindings);
        sqlx::query("INSERT INTO crm.connection_import_bindings(connection_id,external_id,revision,profile_id,sender_profile_id,conversation_type) SELECT $2,b.external_id,b.revision,b.profile_id,b.sender_profile_id,b.conversation_type FROM (SELECT DISTINCT ON(value->>'external_id',value->>'revision') value->>'external_id' external_id,(value->>'revision')::bigint revision,(value->>'profile_id')::uuid profile_id,(value->>'sender_profile_id')::uuid sender_profile_id,value->>'conversation_type' conversation_type FROM jsonb_array_elements($1) WITH ORDINALITY x(value,position) ORDER BY value->>'external_id',value->>'revision',position DESC) b ON CONFLICT(connection_id,external_id,revision) DO UPDATE SET profile_id=excluded.profile_id,sender_profile_id=excluded.sender_profile_id,conversation_type=excluded.conversation_type").bind(&bindings).bind(cid).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO crm.connection_relink(evidence_id) SELECT DISTINCT e.id FROM crm.connection_evidence e JOIN jsonb_to_recordset($1) b(external_id text) ON e.connection_id=$2 AND e.external_id=b.external_id ON CONFLICT(evidence_id) DO UPDATE SET requested_at=now()").bind(bindings).bind(cid).execute(&mut **tx).await?;
    }
    Ok([inserted, changed - inserted, (end - start) as i64 - changed])
}

#[path = "imports_notebook.rs"]
mod notebook;

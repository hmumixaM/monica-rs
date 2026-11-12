//! Bounded immutable-message ingestion and coalesced, generation-fenced summaries.
use crate::Provider;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const MAX_RECORDS: usize = 1000;
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Feedback affects only the next batch. Hard row/byte limits always apply.
pub struct BatchBudget {
    pub records: usize,
}
impl Default for BatchBudget {
    fn default() -> Self {
        Self { records: 500 }
    }
}
impl BatchBudget {
    pub fn observe(&mut self, elapsed: std::time::Duration) {
        if elapsed.as_millis() > 200 {
            self.records = (self.records / 2).max(25);
        } else if elapsed.as_millis() < 50 {
            self.records = (self.records + 50).min(MAX_RECORDS);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Message {
    pub account: String,
    pub conversation: String,
    pub external_id: String,
    pub sent_at_ms: i64,
    pub sender: String,
    pub body: String,
}

/// A conflicting stable identity is rejected, never silently treated as an edit.
/// Corrections use the separate audited imports::correct_message revision path.
pub async fn append(pool: &PgPool, provider: Provider, records: &[Message]) -> Result<u64> {
    if records.is_empty() {
        return Ok(0);
    }
    if records.len() > MAX_RECORDS {
        bail!("message batch exceeds record limit");
    }
    let mut unique = BTreeMap::new();
    let mut bytes = 0;
    for record in records {
        if [
            &record.account,
            &record.conversation,
            &record.external_id,
            &record.sender,
        ]
        .iter()
        .any(|x| x.is_empty() || x.len() > 1024)
        {
            bail!("invalid message identity");
        }
        // Bound input before serialization; then enforce actual encoded-byte limit below.
        bytes += record.account.len()
            + record.conversation.len()
            + record.external_id.len()
            + record.sender.len()
            + record.body.len();
        if bytes > MAX_BYTES {
            bail!("message batch exceeds byte limit");
        }
        let key = (&record.account, &record.conversation, &record.external_id);
        if let Some(previous) = unique.insert(key, record)
            && previous != record
        {
            bail!("conflicting identities inside batch");
        }
    }
    let encoded = serde_json::to_vec(&unique.values().collect::<Vec<_>>())?;
    if encoded.len() > MAX_BYTES {
        bail!("encoded message batch exceeds byte limit");
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL lock_timeout='2s'")
        .execute(&mut *tx)
        .await?;
    let inserted = crate::imports::append_messages(&mut tx, provider, records).await?;
    tx.commit().await?;
    Ok(inserted)
}

#[derive(Debug)]
pub struct SummaryLease {
    pub provider: String,
    pub account: String,
    pub conversation: String,
    pub token: Uuid,
}
#[derive(Debug)]
pub struct Summary {
    pub count: i64,
    pub last_at_ms: i64,
    pub last_id: String,
    pub preview: String,
    pub generation: i64,
}

pub async fn claim(pool: &PgPool) -> Result<Option<SummaryLease>> {
    let token = Uuid::new_v4();
    let row=sqlx::query("WITH candidate AS (SELECT provider,account,conversation FROM crm.conversation_work
      WHERE token IS NULL OR lease_until<clock_timestamp() ORDER BY provider,account,conversation
      FOR UPDATE SKIP LOCKED LIMIT 1)
      UPDATE crm.conversation_work w SET token=$1,lease_until=clock_timestamp()+interval '2 minutes'
      FROM candidate c WHERE (w.provider,w.account,w.conversation)=(c.provider,c.account,c.conversation)
      RETURNING w.provider,w.account,w.conversation")
      .bind(token).fetch_optional(pool).await?;
    Ok(row.map(|r| SummaryLease {
        provider: r.get("provider"),
        account: r.get("account"),
        conversation: r.get("conversation"),
        token,
    }))
}

pub async fn snapshot(pool: &PgPool, lease: &SummaryLease) -> Result<Summary> {
    let r = sqlx::query(
        "SELECT * FROM crm.conversation_state WHERE provider=$1 AND account=$2 AND conversation=$3",
    )
    .bind(&lease.provider)
    .bind(&lease.account)
    .bind(&lease.conversation)
    .fetch_one(pool)
    .await?;
    Ok(Summary {
        count: r.get("message_count"),
        last_at_ms: r.get("last_at_ms"),
        last_id: r.get("last_id"),
        preview: r.get("preview"),
        generation: r.get("generation"),
    })
}

pub async fn finish(pool: &PgPool, lease: &SummaryLease, snapshot: &Summary) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT token FROM crm.conversation_work WHERE provider=$1 AND account=$2 AND conversation=$3
       AND token=$4 AND lease_until>clock_timestamp() FOR UPDATE")
      .bind(&lease.provider).bind(&lease.account).bind(&lease.conversation).bind(lease.token)
      .fetch_optional(&mut *tx).await?.context("summary lease lost")?;
    sqlx::query("INSERT INTO crm.conversation_summaries VALUES($1,$2,$3,$4,$5,$6,$7,$8)
       ON CONFLICT(provider,account,conversation) DO UPDATE SET message_count=excluded.message_count,
       last_at_ms=excluded.last_at_ms,last_id=excluded.last_id,preview=excluded.preview,generation=excluded.generation
       WHERE crm.conversation_summaries.generation<excluded.generation")
      .bind(&lease.provider).bind(&lease.account).bind(&lease.conversation).bind(snapshot.count)
      .bind(snapshot.last_at_ms).bind(&snapshot.last_id).bind(&snapshot.preview).bind(snapshot.generation)
      .execute(&mut *tx).await?;
    let n=sqlx::query("UPDATE crm.conversation_work SET token=NULL,lease_until=NULL
       WHERE provider=$1 AND account=$2 AND conversation=$3 AND token=$4 AND lease_until>clock_timestamp()")
      .bind(&lease.provider).bind(&lease.account).bind(&lease.conversation).bind(lease.token)
      .execute(&mut *tx).await?.rows_affected();
    if n != 1 {
        bail!("summary lease expired before commit");
    }
    sqlx::query("DELETE FROM crm.conversation_work WHERE provider=$1 AND account=$2 AND conversation=$3 AND requested<=$4")
      .bind(&lease.provider).bind(&lease.account).bind(&lease.conversation).bind(snapshot.generation)
      .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

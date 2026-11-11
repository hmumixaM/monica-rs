pub mod messages;
// Durable source ingestion and ordered projection. No production credentials or adapters.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Wechat,
    Instagram,
    Linkedin,
}
impl Provider {
    pub fn schema(self) -> &'static str {
        match self {
            Self::Wechat => "wechat",
            Self::Instagram => "instagram",
            Self::Linkedin => "linkedin",
        }
    }
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Record {
    pub provider: Provider,
    pub account: String,
    pub object: String,
    pub event: String,
    pub payload: Value,
}

/// New-runtime identity. Legacy UUID/canonical-JSON compatibility is not yet claimed.
pub fn object_id(record: &Record) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        &serde_json::to_vec(&(record.provider.schema(), &record.account, &record.object)).unwrap(),
    )
}

pub async fn ingest(pool: &PgPool, r: &Record) -> Result<i64> {
    if r.account.is_empty() || r.object.is_empty() || r.event.is_empty() {
        bail!("empty identity");
    }
    let s = r.provider.schema();
    let id = object_id(r);
    let account_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, &serde_json::to_vec(&(s, &r.account))?);
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&r.payload)?));
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '5s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '15s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query(&format!(
        "INSERT INTO {s}.accounts(id,external_id) VALUES($1,$2) ON CONFLICT DO NOTHING"
    ))
    .bind(account_id)
    .bind(&r.account)
    .execute(&mut *tx)
    .await?;
    sqlx::query(&format!(
        "INSERT INTO {s}.objects(id,account_id,external_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING"
    ))
    .bind(id)
    .bind(account_id)
    .bind(&r.object)
    .execute(&mut *tx)
    .await?;
    let previous: i64 = sqlx::query_scalar(&format!(
        "SELECT revision FROM {s}.objects WHERE id=$1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(row) = sqlx::query(&format!(
        "SELECT hash,version FROM {s}.observations WHERE object_id=$1 AND event_id=$2"
    ))
    .bind(id)
    .bind(&r.event)
    .fetch_optional(&mut *tx)
    .await?
    {
        if row.get::<String, _>("hash") != hash {
            bail!("event identity reused with different content");
        }
        return Ok(row.get("version"));
    }
    let last: Option<String> = sqlx::query_scalar(&format!(
        "SELECT hash FROM {s}.revisions WHERE object_id=$1 AND version=$2"
    ))
    .bind(id)
    .bind(previous)
    .fetch_optional(&mut *tx)
    .await?;
    let version = if last.as_deref() == Some(&hash) {
        previous
    } else {
        previous + 1
    };
    if version != previous {
        sqlx::query(&format!("INSERT INTO {s}.revisions VALUES($1,$2,$3,$4)"))
            .bind(id)
            .bind(version)
            .bind(&r.payload)
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        sqlx::query(&format!("UPDATE {s}.objects SET revision=$2 WHERE id=$1"))
            .bind(id)
            .bind(version)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO crm.jobs(provider,object_id,version) VALUES($1,$2,$3)")
            .bind(s)
            .bind(id)
            .bind(version)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(&format!("INSERT INTO {s}.observations VALUES($1,$2,$3,$4)"))
        .bind(id)
        .bind(&r.event)
        .bind(&hash)
        .bind(version)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(version)
}

#[derive(Debug)]
pub struct Lease {
    pub id: i64,
    pub token: Uuid,
    pub provider: Provider,
    pub object: Uuid,
    pub version: i64,
}

pub async fn claim(pool: &PgPool) -> Result<Option<Lease>> {
    let token = Uuid::new_v4();
    let row=sqlx::query("WITH candidate AS (
        SELECT j.id FROM crm.jobs j LEFT JOIN crm.projections p ON p.provider=j.provider AND p.object_id=j.object_id
        WHERE (j.state='queued' OR (j.state='running' AND j.lease_until < now()))
        AND j.version=coalesce(p.version,0)+1
        ORDER BY j.id FOR UPDATE OF j SKIP LOCKED LIMIT 1)
        UPDATE crm.jobs j SET state='running',token=$1,lease_until=now()+interval '2 minutes',attempts=attempts+1
        FROM candidate c WHERE j.id=c.id RETURNING j.*")
        .bind(token).fetch_optional(pool).await?;
    row.map(|r| {
        Ok(Lease {
            id: r.get("id"),
            token,
            provider: serde_json::from_value(Value::String(r.get("provider")))?,
            object: r.get("object_id"),
            version: r.get("version"),
        })
    })
    .transpose()
}

pub async fn heartbeat(pool: &PgPool, lease: &Lease) -> Result<bool> {
    Ok(sqlx::query("UPDATE crm.jobs SET lease_until=now()+interval '2 minutes' WHERE id=$1 AND token=$2 AND state='running' AND lease_until>now()")
        .bind(lease.id).bind(lease.token).execute(pool).await?.rows_affected()==1)
}

pub async fn project(pool: &PgPool, lease: &Lease) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='15s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT id FROM crm.jobs WHERE id=$1 AND token=$2 AND state='running' AND lease_until>clock_timestamp() FOR UPDATE")
        .bind(lease.id).bind(lease.token).fetch_optional(&mut *tx).await?.context("lease lost")?;
    let s = lease.provider.schema();
    let payload: Value = sqlx::query_scalar(&format!(
        "SELECT payload FROM {s}.revisions WHERE object_id=$1 AND version=$2"
    ))
    .bind(lease.object)
    .bind(lease.version)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO crm.projections VALUES($1,$2,$3,$4) ON CONFLICT(provider,object_id) DO UPDATE SET version=excluded.version,payload=excluded.payload WHERE crm.projections.version<excluded.version")
        .bind(s).bind(lease.object).bind(lease.version).bind(payload).execute(&mut *tx).await?;

    let n=sqlx::query("UPDATE crm.jobs SET state='complete',token=NULL,lease_until=NULL WHERE id=$1 AND token=$2 AND lease_until>clock_timestamp()")
        .bind(lease.id).bind(lease.token).execute(&mut *tx).await?.rows_affected();
    if n != 1 {
        bail!("lease expired before commit");
    }
    sqlx::query("WITH removed AS (DELETE FROM crm.jobs WHERE id=$1 AND token IS NULL AND state='complete' RETURNING *) INSERT INTO crm.job_history SELECT * FROM removed")
        .bind(lease.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

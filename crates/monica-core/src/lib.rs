pub mod adapter;
pub mod ai;
pub mod cdc;
pub mod collector;
pub mod connections;
pub mod content;
pub mod credentials;
pub mod domain;
pub mod faces;
pub mod identity;
pub mod import_monitor;
pub mod imports;
pub mod link_matching;
pub mod linking;
pub mod mail;
pub mod mail_oauth;
pub mod media;
pub mod messages;
pub mod pages;
pub mod resources;
pub mod scheduler;
pub mod source_blobs;
pub mod source_time;
pub mod whatsapp;
// Durable source ingestion and ordered projection. No production credentials or adapters.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Wechat,
    Instagram,
    Linkedin,
    Whatsapp,
    #[serde(rename = "apple_messages")]
    AppleMessages,
    #[serde(rename = "apple_contacts")]
    AppleContacts,
    Mail,
}
impl Provider {
    pub fn parse(name: &str) -> Result<Self> {
        serde_json::from_value(Value::String(name.into())).map_err(Into::into)
    }
    pub fn schema(self) -> &'static str {
        match self {
            Self::Wechat => "wechat",
            Self::Instagram => "instagram",
            Self::Linkedin => "linkedin",
            Self::Whatsapp => "whatsapp",
            Self::AppleMessages => "apple_messages",
            Self::AppleContacts => "apple_contacts",
            Self::Mail => "mail",
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
    #[serde(default = "default_object_kind")]
    pub kind: String,
    pub event: String,
    pub payload: Value,
}

fn default_object_kind() -> String {
    "record".into()
}

/// Uses the pinned platform's account namespace and explicit object kind.
pub fn object_id(record: &Record) -> Result<Uuid> {
    Ok(identity::object_id(
        identity::account_id(record.provider, &record.account)?,
        &record.kind,
        &record.object,
    ))
}

pub async fn ingest(pool: &PgPool, r: &Record) -> Result<i64> {
    identity::validate_json(&r.payload)?;
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '5s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '15s'")
        .execute(&mut *tx)
        .await?;
    let observation = domain::observe(&mut tx, r).await?;
    tx.commit().await?;
    Ok(observation.version)
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
    domain::project(&mut tx, lease).await?;
    sqlx::query("INSERT INTO crm.projections(provider,object_id,version) VALUES($1,$2,$3) ON CONFLICT(provider,object_id) DO UPDATE SET version=excluded.version WHERE crm.projections.version<excluded.version")
        .bind(s).bind(lease.object).bind(lease.version).execute(&mut *tx).await?;

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

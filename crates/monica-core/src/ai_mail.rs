//! Header classification and read-only candidate events. Source facts and manual
//! rules retain priority; model output never creates people or actions.
use super::{Lease, Privacy};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;
fn public_headers(source: &Value) -> Value {
    let mut headers = serde_json::Map::new();
    for key in [
        "from",
        "to",
        "cc",
        "bcc",
        "subject",
        "date",
        "message-id",
        "references",
        "in-reply-to",
        "list-id",
        "list-unsubscribe",
        "precedence",
        "auto-submitted",
        "content-type",
    ] {
        if let Some(value) = source["headers"].get(key) {
            headers.insert(key.into(), value.clone());
        }
    }
    Value::Object(headers)
}

pub async fn snapshot(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    subject: &str,
    privacy: &Privacy,
) -> Result<(Value, Value)> {
    let id: Uuid = subject.parse()?;
    if kind == "ai.mail_classify" {
        let batch =
            sqlx::query("SELECT connection_id,external_ids FROM crm.ai_mail_batches WHERE id=$1")
                .bind(id)
                .fetch_one(&mut **tx)
                .await?;
        let connection: Uuid = batch.get("connection_id");
        let ids: Vec<String> = batch.get("external_ids");
        let generation:i64=sqlx::query_scalar("SELECT generation FROM crm.source_connections WHERE id=$1 AND provider='mail' AND enabled FOR SHARE").bind(connection).fetch_optional(&mut **tx).await?.context("mail connection disabled")?;
        let rows=sqlx::query("SELECT e.external_id,e.revision,e.content_hash,e.metadata,x.header_hash FROM crm.connection_evidence e LEFT JOIN crm.mail_classifications x ON x.connection_id=e.connection_id AND x.external_id=e.external_id WHERE e.connection_id=$1 AND e.external_id=ANY($2) AND e.category='pending' AND NOT e.deleted AND coalesce(e.metadata->>'classification_reason','') NOT IN ('model','user_rule') AND x.source_hash IS DISTINCT FROM e.content_hash ORDER BY e.external_id FOR SHARE OF e").bind(connection).bind(ids).fetch_all(&mut **tx).await?;
        let mut records = Vec::new();
        for row in rows {
            let source: Value = row.get("metadata");
            let hash = crate::mail::fingerprint(&source);
            if row.get::<Option<String>, _>("header_hash").as_deref() == Some(&hash) {
                continue;
            }
            records.push(json!({"external_id":row.get::<String,_>("external_id"),"headers":public_headers(&source),"preview":if privacy.include_messages{source["preview"].as_str().unwrap_or_default()}else{""},"header_hash":hash,"source_hash":row.get::<String,_>("content_hash"),"revision":row.get::<i64,_>("revision")}));
        }
        ensure!(!records.is_empty(), "no unclassified mail headers");
        Ok((
            json!({"connection_id":connection,"mail":records}),
            json!({"connection":generation}),
        ))
    } else {
        ensure!(privacy.include_messages, "private mail body excluded");
        let row=sqlx::query("SELECT e.*,c.generation connection_generation FROM crm.connection_evidence e JOIN crm.source_connections c ON c.id=e.connection_id AND c.provider='mail' AND c.enabled WHERE e.id=$1 AND e.included AND NOT e.deleted AND e.category='transactional' AND e.body<>'' FOR SHARE OF e,c").bind(id).fetch_optional(&mut **tx).await?.context("transactional mail no longer retained")?;
        Ok((
            json!({"id":id,"title":row.get::<String,_>("title"),"body":row.get::<String,_>("body")}),
            json!({"revision":row.get::<i64,_>("revision"),"source_hash":row.get::<String,_>("content_hash"),"connection":row.get::<i64,_>("connection_generation")}),
        ))
    }
}
pub fn validate(lease: &Lease, output: Value) -> Result<Value> {
    ensure!(output.is_object(), "mail model result must be object");
    if lease.kind == "ai.mail_classify" {
        ensure!(
            output["items"].is_array(),
            "mail classification items required"
        );
        let mut items = Vec::new();
        for record in lease.input["mail"].as_array().context("mail input array")? {
            let answer = output["items"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|item| item["external_id"] == record["external_id"]);
            let category = answer
                .and_then(|a| a["category"].as_str())
                .filter(|c| ["personal", "transactional", "marketing", "pending"].contains(c))
                .unwrap_or("pending");
            items.push(json!({"external_id":record["external_id"],"category":category}));
        }
        Ok(json!({"items":items}))
    } else {
        ensure!(output["events"].is_array(), "mail event array required");
        let source = format!(
            "{}\n{}",
            lease.input["title"].as_str().unwrap_or_default(),
            lease.input["body"].as_str().unwrap_or_default()
        );
        let mut events = Vec::new();
        for item in output["events"].as_array().into_iter().flatten().take(50) {
            let kind = item["kind"].as_str().unwrap_or_default();
            let title = item["title"].as_str().unwrap_or_default().trim();
            let quote = item["quote"].as_str().unwrap_or_default();
            if ![
                "travel",
                "order",
                "bill",
                "invitation",
                "appointment",
                "commitment",
                "other",
            ]
            .contains(&kind)
                || title.is_empty()
                || title.chars().count() > 1000
                || quote.is_empty()
                || !source.contains(quote)
            {
                continue;
            }
            let mut event = json!({"kind":kind,"title":title,"quote":quote});
            for field in ["date_text", "organization", "location", "reference"] {
                event[field] = item[field]
                    .as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 4000 && source.contains(v))
                    .map_or(Value::Null, |v| json!(v));
            }
            if !events.contains(&event) {
                events.push(event)
            }
        }
        Ok(json!({"events":events}))
    }
}
pub async fn publish(
    tx: &mut Transaction<'_, Postgres>,
    lease: &Lease,
    output: &Value,
    versions: &Value,
) -> Result<()> {
    if lease.kind == "ai.mail_classify" {
        let connection: Uuid = lease.input["connection_id"]
            .as_str()
            .context("mail connection identity")?
            .parse()?;
        for record in lease.input["mail"].as_array().context("mail input")? {
            let answer = output["items"]
                .as_array()
                .context("classification items")?
                .iter()
                .find(|a| a["external_id"] == record["external_id"])
                .context("missing classification")?;
            sqlx::query("INSERT INTO crm.mail_classifications(connection_id,external_id,header_hash,category,source_hash,revision,model) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(connection_id,external_id) DO UPDATE SET header_hash=excluded.header_hash,category=excluded.category,source_hash=excluded.source_hash,revision=excluded.revision,model=excluded.model,classified_at=now()").bind(connection).bind(record["external_id"].as_str()).bind(record["header_hash"].as_str()).bind(answer["category"].as_str()).bind(record["source_hash"].as_str()).bind(record["revision"].as_i64()).bind(&lease.model).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO crm.connection_reprocess(connection_id,external_id) VALUES($1,$2) ON CONFLICT(connection_id,external_id) DO UPDATE SET requested_at=now()").bind(connection).bind(record["external_id"].as_str()).execute(&mut **tx).await?;
        }
        sqlx::query(
            "UPDATE crm.source_connections SET next_sync_at=now(),sync_pending=true WHERE id=$1",
        )
        .bind(connection)
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query("INSERT INTO crm.mail_event_extractions(evidence_id,input_hash,revision,model,events,job_id) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(evidence_id) DO UPDATE SET input_hash=excluded.input_hash,revision=excluded.revision,model=excluded.model,events=excluded.events,job_id=excluded.job_id,generated_at=now()").bind(lease.subject.parse::<Uuid>()?).bind(&lease.input_hash).bind(versions["revision"].as_i64().context("mail revision")?).bind(&lease.model).bind(&output["events"]).bind(lease.id).execute(&mut **tx).await?;
    }
    Ok(())
}
pub async fn enqueue_pending(pool: &PgPool) -> Result<()> {
    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM crm.ai_settings WHERE id")
        .fetch_one(pool)
        .await?;
    if !enabled {
        return Ok(());
    }
    // Header-unchanged body refreshes reuse the existing classification without a
    // call; update the cheap source fence so they cannot starve newer headers.
    let rows=sqlx::query("SELECT e.connection_id,e.external_id,e.content_hash,e.metadata,x.header_hash FROM crm.connection_evidence e JOIN crm.source_connections c ON c.id=e.connection_id AND c.provider='mail' AND c.enabled LEFT JOIN crm.mail_classifications x ON x.connection_id=e.connection_id AND x.external_id=e.external_id WHERE e.category='pending' AND NOT e.deleted AND coalesce(e.metadata->>'classification_reason','') NOT IN ('model','user_rule') AND x.source_hash IS DISTINCT FROM e.content_hash AND NOT EXISTS(SELECT 1 FROM crm.ai_jobs j WHERE j.kind='ai.mail_classify' AND j.status IN ('queued','running') AND j.input @> jsonb_build_object('connection_id',e.connection_id,'mail',jsonb_build_array(jsonb_build_object('external_id',e.external_id,'source_hash',e.content_hash)))) ORDER BY e.occurred_at DESC NULLS LAST,e.id LIMIT 100").fetch_all(pool).await?;
    let mut connections = std::collections::BTreeMap::<Uuid, Vec<(String, String)>>::new();
    for row in rows {
        let connection: Uuid = row.get("connection_id");
        let metadata: Value = row.get("metadata");
        if row.get::<Option<String>, _>("header_hash").as_deref()
            == Some(&crate::mail::fingerprint(&metadata))
        {
            sqlx::query("UPDATE crm.mail_classifications SET source_hash=$3 WHERE connection_id=$1 AND external_id=$2 AND header_hash=$4").bind(connection).bind(row.get::<String,_>("external_id")).bind(row.get::<String,_>("content_hash")).bind(crate::mail::fingerprint(&metadata)).execute(pool).await?;
        } else {
            connections
                .entry(connection)
                .or_default()
                .push((row.get("external_id"), crate::mail::fingerprint(&metadata)));
        }
    }
    for (connection, mut records) in connections {
        records.sort();
        for group in records.chunks(25) {
            let hash = crate::identity::observation_hash(&json!(group));
            let batch = Uuid::new_v5(&connection, hash.as_bytes());
            let ids = group.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
            sqlx::query("INSERT INTO crm.ai_mail_batches(id,connection_id,external_ids) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(batch).bind(connection).bind(ids).execute(pool).await?;
            super::enqueue(pool, "ai.mail_classify", &batch.to_string()).await?;
        }
    }
    let events:Vec<Uuid>=sqlx::query_scalar("SELECT e.id FROM crm.connection_evidence e JOIN crm.source_connections c ON c.id=e.connection_id AND c.provider='mail' AND c.enabled JOIN crm.ai_settings s ON s.id AND s.enabled AND s.include_messages LEFT JOIN crm.mail_event_extractions x ON x.evidence_id=e.id WHERE e.included AND NOT e.deleted AND e.category='transactional' AND e.body<>'' AND (x.evidence_id IS NULL OR x.revision<>e.revision OR x.model<>s.model) ORDER BY e.occurred_at DESC NULLS LAST,e.id LIMIT 50").fetch_all(pool).await?;
    for event in events {
        super::enqueue(pool, "ai.mail_events", &event.to_string()).await?;
    }
    Ok(())
}

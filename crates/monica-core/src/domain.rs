//! Source observations and real, rebuildable CRM projections.
#[path = "domain_facts.rs"]
pub(crate) mod facts;
use crate::{Lease, Provider, Record, identity};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

pub struct Observation {
    pub object: Uuid,
    pub version: i64,
    pub state: &'static str,
}

pub async fn observe(tx: &mut Transaction<'_, Postgres>, r: &Record) -> Result<Observation> {
    ensure!(
        !r.account.trim().is_empty()
            && !r.object.is_empty()
            && !r.kind.is_empty()
            && !r.event.is_empty(),
        "empty identity"
    );
    ensure!(
        serde_json::to_vec(&r.payload)?.len() <= 4 * 1024 * 1024,
        "source record exceeds 4MiB"
    );
    fn bounded(value: &Value, depth: usize, budget: &mut usize) -> Result<()> {
        ensure!(
            depth <= 64 && *budget > 0,
            "source JSON exceeds structural budget"
        );
        *budget -= 1;
        match value {
            Value::Array(values) => {
                for v in values {
                    bounded(v, depth + 1, budget)?
                }
            }
            Value::Object(values) => {
                for v in values.values() {
                    bounded(v, depth + 1, budget)?
                }
            }
            _ => (),
        }
        Ok(())
    }
    bounded(&r.payload, 0, &mut 100000)?;
    let s = r.provider.schema();
    let account = identity::account_id(r.provider, &r.account)?;
    let id = identity::object_id(account, &r.kind, &r.object);
    identity::validate_json(&r.payload)?;
    let payload = identity::json_safe(&r.payload);
    let hash = identity::observation_hash(&payload);
    let kind = if s == "wechat" && r.account.ends_with("@chatroom") {
        "group"
    } else {
        "person"
    };
    sqlx::query(&format!("INSERT INTO {s}.accounts(id,external_id,object_kind) VALUES($1,$2,$3) ON CONFLICT DO NOTHING"))
        .bind(account).bind(&r.account).bind(kind).execute(&mut **tx).await?;
    sqlx::query(&format!("INSERT INTO {s}.objects(id,account_id,external_id,object_kind) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING"))
        .bind(id).bind(account).bind(&r.object).bind(&r.kind).execute(&mut **tx).await?;
    let previous: i64 = sqlx::query_scalar(&format!(
        "SELECT revision FROM {s}.objects WHERE id=$1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if let Some(row) = sqlx::query(&format!(
        "SELECT hash,version FROM {s}.observations WHERE object_id=$1 AND event_id=$2"
    ))
    .bind(id)
    .bind(&r.event)
    .fetch_optional(&mut **tx)
    .await?
    {
        ensure!(
            row.get::<String, _>("hash") == hash,
            "event identity reused with different content"
        );
        return Ok(Observation {
            object: id,
            version: row.get("version"),
            state: "replayed",
        });
    }
    let last: Option<String> = sqlx::query_scalar(&format!(
        "SELECT hash FROM {s}.revisions WHERE object_id=$1 AND version=$2"
    ))
    .bind(id)
    .bind(previous)
    .fetch_optional(&mut **tx)
    .await?;
    let changed = last.as_deref() != Some(hash.as_str());
    let version = previous + i64::from(changed);
    if changed {
        sqlx::query(&format!(
            "INSERT INTO {s}.payloads(hash,value) VALUES($1,$2) ON CONFLICT DO NOTHING"
        ))
        .bind(&hash)
        .bind(&payload)
        .execute(&mut **tx)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {s}.revisions(object_id,version,hash) VALUES($1,$2,$3)"
        ))
        .bind(id)
        .bind(version)
        .bind(&hash)
        .execute(&mut **tx)
        .await?;
        sqlx::query(&format!("UPDATE {s}.objects SET revision=$2 WHERE id=$1"))
            .bind(id)
            .bind(version)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO crm.jobs(provider,object_id,version) VALUES($1,$2,$3)")
            .bind(s)
            .bind(id)
            .bind(version)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query(&format!(
        "INSERT INTO {s}.observations(object_id,event_id,hash,version) VALUES($1,$2,$3,$4)"
    ))
    .bind(id)
    .bind(&r.event)
    .bind(hash)
    .bind(version)
    .execute(&mut **tx)
    .await?;
    Ok(Observation {
        object: id,
        version,
        state: if previous == 0 {
            "new"
        } else if changed {
            "changed"
        } else {
            "unchanged"
        },
    })
}

/// Called inside the worker's fenced projection + acknowledgement transaction.
pub async fn project(tx: &mut Transaction<'_, Postgres>, lease: &Lease) -> Result<()> {
    let s = lease.provider.schema();
    let row = sqlx::query(&format!("SELECT o.account_id,o.object_kind,o.external_id,a.external_id AS account_external,a.object_kind AS account_kind,p.value AS payload,r.hash,m.occurred_at::text AS occurred_at,coalesce(m.observed_at,now())::text AS observed_at FROM {s}.objects o JOIN {s}.accounts a ON a.id=o.account_id JOIN {s}.revisions r ON r.object_id=o.id AND r.version=$2 JOIN {s}.payloads p ON p.hash=r.hash LEFT JOIN {s}.revision_metadata m ON m.object_id=r.object_id AND m.version=r.version WHERE o.id=$1"))
        .bind(lease.object).bind(lease.version).fetch_one(&mut **tx).await?;
    let account: Uuid = row.get("account_id");
    let payload: Value = row.get("payload");
    let external: String = row.get("account_external");
    let object_external: String = row.get("external_id");
    let name = payload
        .get("display_name")
        .or_else(|| payload.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(&external);
    let kind: String = row.get("object_kind");
    let mut timeline_event_id = None;
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,$2,$3,$4,$5) ON CONFLICT(id) DO NOTHING")
        .bind(account).bind(s).bind(&external).bind(row.get::<String,_>("account_kind")).bind(name).execute(&mut **tx).await?;
    let fields_projection = ["profile", "record", "field", "contact"].contains(&kind.as_str());
    if fields_projection && matches!(lease.provider, Provider::Wechat) {
        sqlx::query("SELECT crm.observe_wechat_contact($1,$2,$3::text::timestamptz,$4)")
            .bind(&external)
            .bind(&payload)
            .bind(row.get::<String, _>("observed_at"))
            .bind(format!("object:{}:{:020}", lease.object, lease.version))
            .execute(&mut **tx)
            .await?;
    }
    let membership_person = if kind == "group_membership" {
        let external = payload
            .get("person_external_id")
            .or_else(|| payload.get("member_external_id"))
            .and_then(Value::as_str)
            .context("membership requires person_external_id")?;
        ensure!(
            !external.ends_with("@chatroom"),
            "group member must be a person"
        );
        let id = identity::account_id(lease.provider, external)?;
        sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,$2,$3,'person',$4) ON CONFLICT DO NOTHING")
            .bind(id).bind(s).bind(external).bind(payload.get("person_name").and_then(Value::as_str).unwrap_or(external)).execute(&mut **tx).await?;
        Some(id)
    } else {
        None
    };
    let typed_fact = ["address", "employment", "education", "chat_month"].contains(&kind.as_str());
    let account_lock = if fields_projection || typed_fact {
        "UPDATE"
    } else {
        "SHARE"
    };
    sqlx::query(&format!(
        "SELECT id FROM crm.source_accounts WHERE id=ANY($1) ORDER BY id FOR {account_lock}"
    ))
    .bind(
        [Some(account), membership_person]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
    )
    .fetch_all(&mut **tx)
    .await?;
    let profile: Option<Uuid> =
        sqlx::query_scalar("SELECT profile_id FROM crm.source_account_links WHERE account_id=$1")
            .bind(account)
            .fetch_optional(&mut **tx)
            .await?;
    if let Some(profile) = profile.filter(|_| fields_projection || typed_fact) {
        sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 FOR UPDATE")
            .bind(profile)
            .fetch_one(&mut **tx)
            .await?;
    }
    let occurred: Option<String> = row.get("occurred_at");
    let occurred = occurred.or_else(|| {
        payload
            .get("occurred_at")
            .or_else(|| payload.get("posted_at"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    match kind.as_str() {
        "chat_month" => {
            let month = payload
                .get("month")
                .and_then(Value::as_str)
                .unwrap_or(&object_external);
            ensure!(
                month.len() == 7
                    && chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
                        .is_ok(),
                "invalid chat month"
            );
            let sent = payload.get("sent").and_then(Value::as_i64).unwrap_or(0);
            let received = payload.get("received").and_then(Value::as_i64).unwrap_or(0);
            ensure!(sent >= 0 && received >= 0, "invalid chat month counts");
            let stamp = |v: &Value| {
                v.as_str().map(str::to_owned).or_else(|| {
                    v.as_f64()
                        .and_then(|n| {
                            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                                (n * 1000.) as i64,
                            )
                        })
                        .map(|t| t.to_rfc3339())
                })
            };
            sqlx::query("INSERT INTO crm.source_chat_months(account_id,month,sent,received,total,first_message_at,last_message_at,snapshot_at,timezone) VALUES($1,$2,$3,$4,$5,$6::timestamptz,$7::timestamptz,$8::timestamptz,$9) ON CONFLICT(account_id,month) DO UPDATE SET sent=excluded.sent,received=excluded.received,total=excluded.total,first_message_at=excluded.first_message_at,last_message_at=excluded.last_message_at,snapshot_at=excluded.snapshot_at,timezone=excluded.timezone")
                .bind(account).bind(month).bind(sent).bind(received).bind(payload.get("total").and_then(Value::as_i64).unwrap_or(sent+received)).bind(stamp(&payload["first_message_at"])).bind(stamp(&payload["last_message_at"])).bind(row.get::<String,_>("observed_at")).bind(payload.get("timezone").and_then(Value::as_str).unwrap_or("UTC")).execute(&mut **tx).await?;
        }
        "address" | "employment" | "education" => {}
        "group_membership" => {
            ensure!(
                s == "wechat" && external.ends_with("@chatroom"),
                "membership owner must be a WeChat group"
            );
            let person = payload
                .get("person_external_id")
                .or_else(|| payload.get("member_external_id"))
                .and_then(Value::as_str)
                .context("membership requires person_external_id")?;
            ensure!(
                !person.ends_with("@chatroom"),
                "group member must be a person"
            );
            let person_id = membership_person.context("membership person missing")?;
            sqlx::query("INSERT INTO crm.source_group_memberships(group_account_id,person_account_id,role,joined_at,left_at) VALUES($1,$2,$3,$4::timestamptz,$5::timestamptz) ON CONFLICT(group_account_id,person_account_id) DO UPDATE SET role=excluded.role,joined_at=coalesce(excluded.joined_at,crm.source_group_memberships.joined_at),left_at=excluded.left_at")
                .bind(account).bind(person_id).bind(payload.get("role").and_then(Value::as_str).unwrap_or("member")).bind(payload.get("joined_at").and_then(Value::as_str)).bind(payload.get("left_at").and_then(Value::as_str)).execute(&mut **tx).await?;
        }
        "comment" | "reaction" | "like" | "reply" | "mention" => {
            let post = if let Some(id) = payload.get("post_id").and_then(Value::as_str) {
                Uuid::parse_str(id)?
            } else {
                let author = payload
                    .get("post_account_external_id")
                    .and_then(Value::as_str)
                    .context("interaction requires post author")?;
                let external = payload
                    .get("post_external_id")
                    .and_then(Value::as_str)
                    .context("interaction requires post external_id")?;
                identity::object_id(
                    identity::account_id(lease.provider, author)?,
                    "post",
                    external,
                )
            };
            let post: Uuid = sqlx::query_scalar("SELECT id FROM crm.timeline_events WHERE provider=$1 AND (id=$2 OR object_id=$2) ORDER BY (id=$2) DESC LIMIT 1")
                .bind(s).bind(post).fetch_optional(&mut **tx).await?
                .context("interaction parent post has not been projected")?;
            sqlx::query("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_account_id,author_profile_id,author_external_id,author_name,occurred_at,content,is_deleted) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::timestamptz,$11,$12) ON CONFLICT(post_id,external_id,interaction_type) DO UPDATE SET content=excluded.content,is_deleted=excluded.is_deleted,author_profile_id=excluded.author_profile_id")
                .bind(lease.object).bind(post).bind(s).bind(&object_external).bind(&kind).bind(account).bind(profile).bind(&external).bind(name).bind(&occurred).bind(&payload).bind(payload.get("is_deleted").and_then(Value::as_bool).unwrap_or(false)).execute(&mut **tx).await?;
        }
        "post" | "story" => {
            let _: Option<Uuid> =
                sqlx::query_scalar("SELECT crm.adopt_legacy_timeline_identity($1,$2,$3,$4,$5,$6)")
                    .bind(s)
                    .bind(&kind)
                    .bind(&object_external)
                    .bind(account)
                    .bind(profile)
                    .bind(lease.object)
                    .fetch_one(&mut **tx)
                    .await?;
            let applied: Option<Uuid> = sqlx::query_scalar("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,occurred_at,expires_at,content,content_hash,status,version) VALUES($1,$2,$1,$3,$4,$5,$6,$7::timestamptz,$8::timestamptz,$9,$10,$11,$12) ON CONFLICT(provider,object_id) DO UPDATE SET profile_id=excluded.profile_id,occurred_at=excluded.occurred_at,expires_at=excluded.expires_at,content=excluded.content,content_hash=excluded.content_hash,status=excluded.status,version=excluded.version,last_seen_at=now() WHERE crm.timeline_events.version<excluded.version RETURNING id")
                .bind(lease.object).bind(s).bind(account).bind(profile).bind(&kind).bind(&object_external).bind(&occurred)
                .bind(payload.get("expires_at").and_then(Value::as_str)).bind(&payload).bind(row.get::<String,_>("hash"))
                .bind(if payload.get("is_deleted").and_then(Value::as_bool)==Some(true) {"deleted"} else if kind=="story" {payload.get("coverage_status").and_then(Value::as_str).unwrap_or("active")} else {"active"}).bind(lease.version).fetch_optional(&mut **tx).await?;
            let event_id = match applied {
                Some(id) => id,
                None => {
                    sqlx::query_scalar(
                        "SELECT id FROM crm.timeline_events WHERE provider=$1 AND object_id=$2",
                    )
                    .bind(s)
                    .bind(lease.object)
                    .fetch_one(&mut **tx)
                    .await?
                }
            };
            timeline_event_id = Some(event_id);
            if let Some(items) = payload
                .get("interactions")
                .and_then(Value::as_array)
                .filter(|_| applied.is_some())
            {
                for item in items {
                    let ext = item
                        .get("external_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .context("interaction requires stable external_id")?;
                    let interaction = item
                        .get("interaction_type")
                        .or_else(|| item.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("comment");
                    let id = identity::object_id(lease.object, interaction, ext);
                    let interaction_id: Uuid = sqlx::query_scalar("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_external_id,author_name,occurred_at,content,metadata,is_deleted) VALUES($1,$2,$3,$4,$5,$6,$7,$8::timestamptz,$9,$10,$11) ON CONFLICT(post_id,external_id,interaction_type) DO UPDATE SET content=excluded.content,metadata=excluded.metadata,is_deleted=excluded.is_deleted,author_name=excluded.author_name RETURNING id")
                        .bind(id).bind(event_id).bind(s).bind(ext).bind(interaction).bind(item.get("author_external_id").and_then(Value::as_str))
                        .bind(item.get("author_name").and_then(Value::as_str)).bind(item.get("occurred_at").and_then(Value::as_str))
                        .bind(item.get("content").unwrap_or(item)).bind({ let mut metadata=item.get("metadata").filter(|v|v.is_object()).cloned().unwrap_or(json!({})); for key in ["target_external_id","ref_author_external_id"] { if let Some(v)=item.get(key) { metadata[key]=v.clone(); } } metadata })
                        .bind(item.get("is_deleted").and_then(Value::as_bool).unwrap_or(false)).fetch_one(&mut **tx).await?;
                    sqlx::query("UPDATE crm.social_interactions i SET author_account_id=a.id,author_profile_id=l.profile_id FROM crm.source_accounts a LEFT JOIN crm.source_account_links l ON l.account_id=a.id WHERE i.id=$1 AND a.provider=i.provider AND a.external_id=i.author_external_id")
                        .bind(interaction_id).execute(&mut **tx).await?;
                }
            }
        }
        "call" | "money" | "interaction" => {
            sqlx::query("INSERT INTO crm.contact_events(id,provider,source_account_id,profile_id,kind,external_id,occurred_at,content) VALUES($1,$2,$3,$4,$5,$6,$7::timestamptz,$8) ON CONFLICT(id) DO UPDATE SET profile_id=excluded.profile_id,content=excluded.content,occurred_at=excluded.occurred_at")
                .bind(lease.object).bind(s).bind(account).bind(profile).bind(&kind).bind(&object_external).bind(&occurred).bind(&payload).execute(&mut **tx).await?;
        }
        _ => {
            ensure!(fields_projection, "unsupported source object kind");
            if fields_projection {
                let fields = if let Some(fields) = payload.get("fields").and_then(Value::as_object)
                {
                    fields.clone()
                } else if let Some(key) = payload.get("field_key").and_then(Value::as_str) {
                    [(
                        key.to_owned(),
                        payload.get("value").cloned().unwrap_or(Value::Null),
                    )]
                    .into_iter()
                    .collect()
                } else {
                    payload
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(key, value)| (format!("{s}.{key}"), value))
                        .collect()
                };
                // Include current profile values because rebuilding below may reuse
                // another linked account's fields in this same transaction.
                let mut values = fields.values().cloned().collect::<Vec<_>>();
                if let Some(profile) = profile {
                    let existing: Vec<Value> = sqlx::query_scalar("SELECT f.value FROM crm.source_fields f JOIN crm.source_account_links l ON l.account_id=f.account_id WHERE l.profile_id=$1")
                        .bind(profile).fetch_all(&mut **tx).await?;
                    values.extend(existing);
                }
                let contents = crate::content::register_many(tx, "field_value", &values).await?;
                for ((key, value), content) in fields.into_iter().zip(contents) {
                    let hash = content.hash;
                    sqlx::query("INSERT INTO crm.source_fields(account_id,field_key,value,content_hash,object_id,source_version,observed_at,content_object_id) VALUES($1,$2,$3,$4,$5,$6,$7::timestamptz,$8) ON CONFLICT(account_id,field_key) DO UPDATE SET value=excluded.value,content_hash=excluded.content_hash,content_object_id=excluded.content_object_id,object_id=excluded.object_id,source_version=excluded.source_version,observed_at=excluded.observed_at WHERE (crm.source_fields.object_id=excluded.object_id AND crm.source_fields.source_version<excluded.source_version) OR (crm.source_fields.object_id<>excluded.object_id AND (crm.source_fields.observed_at,crm.source_fields.object_id)<(excluded.observed_at,excluded.object_id))")
                        .bind(account).bind(key).bind(value).bind(hash).bind(lease.object).bind(lease.version).bind(row.get::<String,_>("observed_at")).bind(content.id).execute(&mut **tx).await?;
                }
                sqlx::query("UPDATE crm.source_accounts SET display_name=$2,username=coalesce($3,username),profile_url=coalesce($4,profile_url),last_seen_at=now() WHERE id=$1")
                    .bind(account).bind(name).bind(payload.get("username").and_then(Value::as_str)).bind(payload.get("profile_url").and_then(Value::as_str)).execute(&mut **tx).await?;
            }
        }
    }
    facts::project(
        tx,
        facts::Source {
            account,
            profile: None,
            source: s,
            object: lease.object,
            version: lease.version,
            record: &object_external,
            observed: &row.get::<String, _>("observed_at"),
        },
        &kind,
        &payload,
    )
    .await?;
    if typed_fact && let Some(profile) = profile {
        sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
            .bind(profile)
            .execute(&mut **tx)
            .await?;
    }
    if fields_projection {
        sqlx::query("INSERT INTO crm.identities(id,profile_id,source_account_id,provider,external_id,display_name,status) VALUES($1,$2,$1,$3,$4,$5,$6) ON CONFLICT(provider,external_id) DO UPDATE SET profile_id=excluded.profile_id,source_account_id=excluded.source_account_id,display_name=excluded.display_name,status=excluded.status,last_seen_at=now()")
        .bind(account).bind(profile).bind(s).bind(&external).bind(name).bind(if profile.is_some(){"active"}else{"pending"}).execute(&mut **tx).await?;
        if let Some(profile) = profile {
            crate::linking::rebuild_profile(tx, profile).await?;
        }
    }
    if let Some(event_id) = timeline_event_id {
        crate::ai::memory::legacy::queue_record(tx, "social", event_id).await?;
    }
    Ok(())
}

pub fn conversation_id(provider: Provider, account: &str, conversation: &str) -> Result<Uuid> {
    Ok(identity::object_id(
        identity::account_id(provider, account)?,
        "conversation",
        conversation,
    ))
}

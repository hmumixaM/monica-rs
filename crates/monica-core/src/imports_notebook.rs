//! The `monica` import source owns native CRM facts, never a platform schema.
use super::*;
use std::collections::BTreeMap;
fn text(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
fn present(v: &Value) -> bool {
    !v.is_null()
        && v != &json!("")
        && v.as_array().is_none_or(|a| !a.is_empty())
        && v.as_object().is_none_or(|o| !o.is_empty())
        && v.as_f64() != Some(0.0)
        && v != &json!(false)
}
fn profile_hint(record: &BatchRecord, key: &str) -> Result<Option<Uuid>> {
    record
        .extra
        .get(key)
        .filter(|v| !v.is_null())
        .map(|v| {
            Uuid::parse_str(v.as_str().context("profile identifier must be UUID")?)
                .map_err(Into::into)
        })
        .transpose()
}
fn notebook_object(batch: &Batch, record: &BatchRecord) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("monica:{}:{}", batch.stream, record.external_id).as_bytes(),
    )
}
fn record_kind<'a>(batch: &'a Batch, record: &'a BatchRecord) -> Option<&'a str> {
    record
        .entity_type
        .as_deref()
        .or_else(|| record.content.get("entity_type").and_then(Value::as_str))
        .or(match batch.stream.as_str() {
            "posts" | "moments" => Some("post"),
            "stories" => Some("story"),
            _ => None,
        })
}
fn record_hash(record: &BatchRecord) -> Result<String> {
    Ok(identity::observation_hash(&serde_json::to_value(record)?))
}
async fn changed_records<'a>(
    tx: &mut Transaction<'_, Postgres>,
    batch: &Batch,
    records: Vec<&'a BatchRecord>,
) -> Result<(Vec<&'a BatchRecord>, i64)> {
    let ids: Vec<_> = records.iter().map(|r| notebook_object(batch, r)).collect();
    let rows: Vec<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id,record_hash FROM crm.notebook_objects WHERE id=ANY($1)")
            .bind(ids)
            .fetch_all(&mut **tx)
            .await?;
    let mut heads: BTreeMap<_, _> = rows.into_iter().collect();
    let mut changed = Vec::new();
    let mut skipped = 0;
    for record in records {
        let id = notebook_object(batch, record);
        let hash = record_hash(record)?;
        let unchanged = matches!(record_kind(batch, record), Some("post" | "story"))
            && heads.get(&id).and_then(Option::as_deref) == Some(hash.as_str());
        // Advance the simulated head even inside one batch. A -> B -> A must
        // not mistake the final A for a replay of the pre-batch head.
        heads.insert(id, Some(hash));
        if unchanged {
            skipped += 1;
        } else {
            changed.push(record);
        }
    }
    Ok((changed, skipped))
}
async fn ensure_profile(
    tx: &mut Transaction<'_, Postgres>,
    batch: &Batch,
    record: &BatchRecord,
) -> Result<Option<Uuid>> {
    if let Some(id) = profile_hint(record, "profile_id")? {
        sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .context("profile not found")?;
        return Ok(Some(id));
    }
    if !["contacts", "profiles", "groups", "people", "legacy"].contains(&batch.stream.as_str()) {
        return Ok(None);
    }
    if let Some(id)=sqlx::query_scalar::<_,Uuid>("SELECT profile_id FROM crm.identities WHERE provider='monica' AND external_id=$1 AND status NOT IN ('invalid','merged') AND profile_id IS NOT NULL").bind(&record.external_id).fetch_optional(&mut **tx).await?{sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 FOR UPDATE").bind(id).execute(&mut **tx).await?;return Ok(Some(id))}
    let value = &record.content;
    let name = ["display_name", "full_name", "nickname", "nick", "remark"]
        .iter()
        .find_map(|k| value.get(*k).filter(|v| present(v)))
        .map(text)
        .unwrap_or_else(|| record.external_id.clone());
    let typ = if batch.stream == "groups" || value.get("profile_type") == Some(&json!("group")) {
        "group"
    } else {
        "person"
    };
    let summary = value
        .get("summary")
        .filter(|v| present(v))
        .or_else(|| value.get("headline").filter(|v| present(v)))
        .map(text);
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO crm.profiles(id,profile_type,display_name,summary) VALUES($1,$2,$3,$4)",
    )
    .bind(id)
    .bind(typ)
    .bind(&name)
    .bind(summary)
    .execute(&mut **tx)
    .await?;
    let username = ["username", "alias", "u"]
        .iter()
        .find_map(|k| value.get(*k).filter(|v| present(v)))
        .map(text);
    sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,username,profile_url,display_name,status) VALUES($1,$2,'monica',$3,$4,$5,$6,'pending') ON CONFLICT(provider,external_id) DO UPDATE SET profile_id=coalesce(crm.identities.profile_id,excluded.profile_id),username=coalesce(excluded.username,crm.identities.username),profile_url=coalesce(excluded.profile_url,crm.identities.profile_url),display_name=coalesce(excluded.display_name,crm.identities.display_name),last_seen_at=now()").bind(Uuid::new_v4()).bind(id).bind(&record.external_id).bind(username).bind(value.get("profile_url").and_then(Value::as_str)).bind(name).execute(&mut **tx).await?;
    Ok(Some(id))
}
pub(super) async fn project(
    tx: &mut Transaction<'_, Postgres>,
    batch: &Batch,
    start: usize,
    end: usize,
) -> Result<[i64; 3]> {
    ensure!(
        batch.connection_id.is_none(),
        "Source connection does not match import provider"
    );
    // An identical social snapshot can linearize at this committed read. It
    // does not need to lock its person, reserve content, or rewrite projections.
    let (records, skipped) =
        changed_records(tx, batch, batch.records[start..end].iter().collect()).await?;
    if records.is_empty() {
        return Ok([0, 0, skipped]);
    }
    // Native identities and field revisions share one deterministic transaction lock
    // with other imports of the same source record. No platform account is invented.
    let keys: BTreeSet<String> = records
        .iter()
        .map(|r| format!("notebook:{}", r.external_id))
        .collect();
    for key in keys {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(key)
            .execute(&mut **tx)
            .await?;
    }
    // Another importer may have published the same snapshot while we waited.
    let (records, skipped_locked) = changed_records(tx, batch, records).await?;
    if records.is_empty() {
        return Ok([0, 0, skipped + skipped_locked]);
    }
    // Lock explicit/existing profile owners in UUID order before mutating any row.
    let explicit: Vec<Uuid> = records
        .iter()
        .flat_map(|r| {
            [
                profile_hint(r, "profile_id"),
                profile_hint(r, "sender_profile_id"),
            ]
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    let external: Vec<&str> = records.iter().map(|r| r.external_id.as_str()).collect();
    sqlx::query("SELECT id FROM crm.profiles WHERE id=ANY($1) OR id IN (SELECT profile_id FROM crm.identities WHERE provider='monica' AND external_id=ANY($2) AND status NOT IN ('invalid','merged')) OR id IN (SELECT profile_id FROM crm.notebook_objects WHERE external_id=ANY($2)) ORDER BY id FOR UPDATE").bind(explicit).bind(external).fetch_all(&mut **tx).await?;
    let mut prepared = vec![];
    for record in records {
        let profile = ensure_profile(tx, batch, record).await?;
        prepared.push((record, profile));
    }
    let raw_values: Vec<Value> = prepared.iter().map(|(r, _)| r.content.clone()).collect();
    let raw = crate::content::register_many(tx, "raw_json", &raw_values).await?;
    let mut counts = [0, 0, skipped + skipped_locked];
    for ((record, profile), content) in prepared.into_iter().zip(raw) {
        let object = notebook_object(batch, record);
        let hash = identity::observation_hash(&record.content);
        let previous: Option<(i64, String)> =
            sqlx::query_as("SELECT version,content_hash FROM crm.notebook_objects WHERE id=$1")
                .bind(object)
                .fetch_optional(&mut **tx)
                .await?;
        let changed = previous.as_ref().is_none_or(|(_, h)| h != &hash);
        let version = previous
            .as_ref()
            .map_or(1, |(v, _)| if changed { v + 1 } else { *v });
        counts[if previous.is_none() {
            0
        } else if changed {
            1
        } else {
            2
        }] += 1;
        sqlx::query("INSERT INTO crm.notebook_objects(id,stream,external_id,version,content_hash,profile_id,record_hash) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(id) DO UPDATE SET version=excluded.version,content_hash=excluded.content_hash,profile_id=coalesce(excluded.profile_id,crm.notebook_objects.profile_id),record_hash=excluded.record_hash WHERE (crm.notebook_objects.version,crm.notebook_objects.content_hash,crm.notebook_objects.profile_id,crm.notebook_objects.record_hash) IS DISTINCT FROM (excluded.version,excluded.content_hash,coalesce(excluded.profile_id,crm.notebook_objects.profile_id),excluded.record_hash)").bind(object).bind(&batch.stream).bind(&record.external_id).bind(version).bind(hash).bind(profile).bind(record_hash(record)?).execute(&mut **tx).await?;
        if changed {
            sqlx::query(
                "INSERT INTO crm.notebook_revisions VALUES($1,$2,$3,$4::text::timestamptz)",
            )
            .bind(object)
            .bind(version)
            .bind(content.id)
            .bind(&batch.observed_at)
            .execute(&mut **tx)
            .await?;
        }
        if let Some(profile) = profile {
            if ["contacts", "profiles", "people", "legacy"].contains(&batch.stream.as_str()) {
                crate::domain::facts::project(
                    tx,
                    crate::domain::facts::Source {
                        account: profile,
                        profile: Some(profile),
                        source: "monica",
                        object,
                        version,
                        record: &record.external_id,
                        observed: &batch.observed_at,
                    },
                    "profile",
                    &record.content,
                )
                .await?;
            }
            fields(tx, profile, record, &batch.observed_at).await?;
            sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
                .bind(profile)
                .execute(&mut **tx)
                .await?;
            crate::ai::memory::profile_facts(tx, profile).await?;
        }
        let kind = record_kind(batch, record);
        if kind == Some("message")
            || ["messages", "chat", "conversations"].contains(&batch.stream.as_str())
        {
            message(tx, record, profile).await?;
        }
        if matches!(kind, Some("post" | "story")) {
            social(tx, record, profile, kind.unwrap(), &batch.observed_at).await?;
        }
    }
    Ok(counts)
}
async fn fields(
    tx: &mut Transaction<'_, Postgres>,
    profile: Uuid,
    record: &BatchRecord,
    observed: &str,
) -> Result<()> {
    let values: Vec<(String, Value)> =
        if let Some(key) = record.content.get("field_key").filter(|v| present(v)) {
            vec![(
                text(key),
                record.content.get("value").cloned().unwrap_or(Value::Null),
            )]
        } else {
            record
                .content
                .get("fields")
                .and_then(Value::as_object)
                .map(|v| v.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };
    let contents = crate::content::register_many(
        tx,
        "field_value",
        &values.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>(),
    )
    .await?;
    for ((key, value), content) in values.into_iter().zip(contents) {
        let current:Option<(Uuid,String)>=sqlx::query_as("SELECT r.id,r.source_type FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key=$2").bind(profile).bind(&key).fetch_optional(&mut **tx).await?;
        let latest:Option<(Uuid,String)>=sqlx::query_as("SELECT id,content_hash FROM crm.field_revisions WHERE profile_id=$1 AND field_key=$2 AND source_type='monica' AND source_account_id IS NULL AND source_record_id=$3 ORDER BY revision_seq DESC LIMIT 1").bind(profile).bind(&key).bind(&record.external_id).fetch_optional(&mut **tx).await?;
        if latest
            .as_ref()
            .is_some_and(|(_, hash)| hash == &content.hash)
        {
            continue;
        }
        let manual = current
            .as_ref()
            .is_some_and(|(_, source)| source == "manual");
        let previous = latest
            .map(|(id, _)| id)
            .or_else(|| current.map(|(id, _)| id));
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,observed_at,is_conflict,previous_revision_id,content_object_id,source_record_id) VALUES($1,$2,$3,$4,$5,'monica','import',$6::text::timestamptz,$7,$8,$9,$10)").bind(id).bind(profile).bind(&key).bind(&value).bind(content.hash).bind(observed).bind(manual).bind(previous).bind(content.id).bind(&record.external_id).execute(&mut **tx).await?;
        if !manual {
            sqlx::query("INSERT INTO crm.field_current VALUES($1,$2,$3) ON CONFLICT(profile_id,field_key) DO UPDATE SET revision_id=excluded.revision_id").bind(profile).bind(&key).bind(id).execute(&mut **tx).await?;
            if ["display_name", "summary"].contains(&key.as_str()) && value.is_string() {
                sqlx::query(&format!("UPDATE crm.profiles SET {key}=$2 WHERE id=$1"))
                    .bind(profile)
                    .bind(value.as_str())
                    .execute(&mut **tx)
                    .await?;
            }
        }
    }
    Ok(())
}
async fn message(
    tx: &mut Transaction<'_, Postgres>,
    record: &BatchRecord,
    profile: Option<Uuid>,
) -> Result<()> {
    let value = &record.content;
    crate::content::register(
        tx,
        "message",
        &if value.is_object() {
            value.clone()
        } else {
            json!({"value":value})
        },
    )
    .await?;
    let external = record
        .conversation_external_id
        .as_deref()
        .or_else(|| {
            value
                .get("conversation_external_id")
                .and_then(Value::as_str)
        })
        .unwrap_or(&record.external_id);
    let kind = record
        .conversation_type
        .as_deref()
        .or_else(|| value.get("conversation_type").and_then(Value::as_str))
        .unwrap_or("direct");
    let kind = if kind == "group" { "group" } else { "direct" };
    let at = record
        .occurred_at
        .as_deref()
        .or_else(|| value.get("occurred_at").and_then(Value::as_str))
        .context("message records require occurred_at")?;
    let at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT $1::text::timestamptz")
        .bind(at)
        .fetch_one(&mut **tx)
        .await?;
    let conversation = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("monica:conversation:{kind}:{external}").as_bytes(),
    );
    let id = Uuid::new_v5(
        &conversation,
        format!("message:{}:{}", record.external_id, at.to_rfc3339()).as_bytes(),
    );
    let sender = profile_hint(record, "sender_profile_id")?;
    let old_row=sqlx::query("SELECT m.conversation_id,m.occurred_at,m.direction,m.sender_profile_id,CASE WHEN c.conversation_type='group' THEN m.sender_profile_id ELSE c.profile_id END memory_owner FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE m.message_id=$1 AND NOT m.tombstone").bind(id).fetch_optional(&mut **tx).await?;
    let old = old_row.as_ref().map(|r| {
        (
            r.get("conversation_id"),
            r.get("occurred_at"),
            r.get("direction"),
        )
    });
    let effective_sender = sender.or_else(|| {
        old_row
            .as_ref()
            .and_then(|r| r.get::<Option<Uuid>, _>("sender_profile_id"))
    });
    let effective_profile:Option<Uuid>=sqlx::query_scalar("INSERT INTO crm.conversations(id,provider,account,external_id,profile_id,conversation_type,source_external_id) VALUES($1,'monica','notebook:'||$4,$2,$3,$4,$5) ON CONFLICT(id) DO UPDATE SET profile_id=coalesce(excluded.profile_id,crm.conversations.profile_id) RETURNING profile_id").bind(conversation).bind(external).bind(profile).bind(kind).bind(external).fetch_one(&mut **tx).await?;
    let body = value
        .get("text")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| value.get("body").and_then(Value::as_str))
        .unwrap_or("");
    let direction = if effective_profile.is_some() && effective_sender == effective_profile {
        "received"
    } else {
        "sent"
    };
    sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_profile_id,occurred_at,body,message_type,direction) VALUES($1,$2,'monica','notebook',$3,'',$4,$5,$6,$7,$8) ON CONFLICT(message_id) DO UPDATE SET sender_profile_id=coalesce(excluded.sender_profile_id,crm.message_search.sender_profile_id),body=excluded.body,message_type=excluded.message_type,direction=excluded.direction").bind(id).bind(conversation).bind(format!("{}:{}",record.external_id,at.to_rfc3339())).bind(sender).bind(at).bind(body).bind(record.message_type.as_deref().or_else(||value.get("message_type").and_then(Value::as_str)).unwrap_or("text")).bind(direction).execute(&mut **tx).await?;
    crate::connections::refresh_conversations(
        tx,
        old,
        Some((conversation, at, Some(direction.into()))),
    )
    .await?;
    crate::ai::memory::legacy::sync(tx, "message", &[id]).await?;
    Ok(())
}
async fn social(
    tx: &mut Transaction<'_, Postgres>,
    record: &BatchRecord,
    profile: Option<Uuid>,
    kind: &str,
    observed: &str,
) -> Result<()> {
    let provider = record
        .extra
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("monica");
    let value = if record.content.is_object() {
        record.content.clone()
    } else {
        json!({"value":record.content})
    };
    let content = crate::content::register(
        tx,
        if kind == "post" {
            "post_body"
        } else {
            "story_body"
        },
        &value,
    )
    .await?;
    let id:Uuid = sqlx::query_scalar("SELECT id FROM crm.timeline_events WHERE provider=$1 AND event_type=$2 AND external_id=$3 ORDER BY id LIMIT 1").bind(provider).bind(kind).bind(&record.external_id).fetch_optional(&mut **tx).await?.unwrap_or_else(||Uuid::new_v5(&Uuid::NAMESPACE_URL,format!("notebook:{provider}:{kind}:{}", record.external_id).as_bytes()));
    let previous_row =
        sqlx::query("SELECT content_hash,profile_id FROM crm.timeline_events WHERE id=$1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    let previous: Option<String> = previous_row.as_ref().map(|r| r.get("content_hash"));
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,expires_at,content,content_hash,status,version,source_updated_at) VALUES($1,$2,$1,$3,$4,$5,$6::text::timestamptz,$7::text::timestamptz,$8,$9,$10,1,$11::text::timestamptz) ON CONFLICT(id) DO UPDATE SET profile_id=coalesce(excluded.profile_id,crm.timeline_events.profile_id),occurred_at=coalesce(excluded.occurred_at,crm.timeline_events.occurred_at),expires_at=coalesce(excluded.expires_at,crm.timeline_events.expires_at),content=excluded.content,content_hash=excluded.content_hash,status=excluded.status,version=crm.timeline_events.version+CASE WHEN crm.timeline_events.content_hash<>excluded.content_hash THEN 1 ELSE 0 END,source_updated_at=coalesce(excluded.source_updated_at,crm.timeline_events.source_updated_at),last_seen_at=now()").bind(id).bind(provider).bind(profile).bind(kind).bind(&record.external_id).bind(record.occurred_at.clone().or_else(||timestamp(value.get("occurred_at")))).bind(timestamp(value.get("expires_at"))).bind(&value).bind(&content.hash).bind(if kind=="post"{"active"}else{value.get("coverage_status").and_then(Value::as_str).unwrap_or("partial")}).bind(record.extra.get("source_updated_at").and_then(Value::as_str)).execute(&mut **tx).await?;
    crate::ai::memory::legacy::sync(tx, "social", &[id]).await?;
    if kind == "post" {
        interactions(tx, id, provider, &record.external_id, &value).await?;
    }
    if previous.as_deref() != Some(&content.hash) {
        sqlx::query("INSERT INTO crm.notebook_social_revisions(id,event_id,content_object_id,content_hash,observed_at,source_record_id) VALUES($1,$2,$3,$4,$5::text::timestamptz,$6)").bind(Uuid::new_v4()).bind(id).bind(content.id).bind(content.hash).bind(observed).bind(&record.external_id).execute(&mut **tx).await?;
    }
    Ok(())
}

fn timestamp(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(number) = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()))
    {
        return chrono::DateTime::from_timestamp_millis((number * 1000.0) as i64)
            .map(|d| d.to_rfc3339());
    }
    let text = value.as_str()?.trim();
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|d| d.to_rfc3339())
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|d| d.and_utc().to_rfc3339())
        })
}
async fn interactions(
    tx: &mut Transaction<'_, Postgres>,
    post: Uuid,
    provider: &str,
    external: &str,
    value: &Value,
) -> Result<()> {
    let Some(items) = value.get("interactions").and_then(Value::as_array) else {
        return Ok(());
    };
    for (index, item) in items.iter().enumerate().filter(|(_, v)| v.is_object()) {
        let kind = item
            .get("interaction_type")
            .and_then(Value::as_str)
            .unwrap_or("comment");
        let kind = if ["like", "comment", "reply", "mention"].contains(&kind) {
            kind
        } else {
            "comment"
        };
        let author = item
            .get("author_external_id")
            .filter(|v| present(v))
            .or_else(|| item.get("username").filter(|v| present(v)))
            .map(text);
        let at = timestamp(item.get("occurred_at"));
        let discriminator = item
            .get("create_time")
            .filter(|v| present(v))
            .map(text)
            .or_else(|| at.clone())
            .unwrap_or_else(|| index.to_string());
        let stable = item
            .get("external_id")
            .filter(|v| present(v))
            .map(text)
            .unwrap_or_else(|| {
                format!(
                    "{external}:{kind}:{}:{discriminator}",
                    author.as_deref().unwrap_or("unknown")
                )
            });
        let body = item
            .get("text")
            .filter(|v| present(v))
            .or_else(|| item.get("content").filter(|v| present(v)))
            .map(|v| {
                if v.is_object() {
                    v.clone()
                } else {
                    json!({"text":v})
                }
            });
        if let Some(body) = &body {
            crate::content::register(tx, "social_interaction", body).await?;
        }
        let author_profile:Option<Uuid>=sqlx::query_scalar("SELECT profile_id FROM crm.identities WHERE provider=$1 AND external_id=$2 AND status NOT IN ('invalid','merged')").bind(provider).bind(&author).fetch_optional(&mut **tx).await?.flatten();
        let existing:Option<Uuid>=sqlx::query_scalar("SELECT id FROM crm.social_interactions WHERE provider=$1 AND external_id=$2 AND (interaction_type=$3 OR ($1='wechat' AND interaction_type IN ('comment','reply') AND $3 IN ('comment','reply'))) ORDER BY id LIMIT 1").bind(provider).bind(&stable).bind(kind).fetch_optional(&mut **tx).await?;
        let id = existing.unwrap_or_else(Uuid::new_v4);
        sqlx::query("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_profile_id,author_external_id,author_name,occurred_at,content,metadata) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::timestamptz,$10,$11) ON CONFLICT(id) DO UPDATE SET post_id=excluded.post_id,interaction_type=excluded.interaction_type,author_profile_id=coalesce(excluded.author_profile_id,crm.social_interactions.author_profile_id),author_name=coalesce(excluded.author_name,crm.social_interactions.author_name),occurred_at=coalesce(excluded.occurred_at,crm.social_interactions.occurred_at),content=CASE WHEN excluded.content='null' THEN crm.social_interactions.content ELSE excluded.content END,metadata=excluded.metadata").bind(id).bind(post).bind(provider).bind(stable).bind(kind).bind(author_profile).bind(author).bind(item.get("author_name").filter(|v|present(v)).or_else(||item.get("nickname")).and_then(Value::as_str)).bind(at).bind(body.unwrap_or(Value::Null)).bind(item).execute(&mut **tx).await?;
    }
    Ok(())
}

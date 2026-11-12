//! One page per lease turn. Network calls never hold PostgreSQL transactions.
use super::*;
use crate::{
    Provider,
    connections::{self, Fence},
    credentials::CredentialCipher,
    media::MediaStore,
};
use sqlx::{PgPool, Row};
use std::time::Duration;

/// Body access is gated by deterministic rules or a saved model decision. The
/// model itself is never called from a transport or while holding a source lease.
pub async fn process_page(
    store: &MediaStore,
    cid: Uuid,
    transport: &mut dyn MailTransport,
    page: &MailPage,
) -> Result<Vec<Value>> {
    let mut owners = BTreeSet::new();
    for row in sqlx::query(
        "SELECT external_account,settings FROM crm.source_connections WHERE provider='mail'",
    )
    .fetch_all(&store.pool)
    .await?
    {
        owners.insert(crate::identity::casefold(
            &row.get::<String, _>("external_account"),
        ));
        let settings: Value = row.get("settings");
        for alias in settings["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            owners.insert(crate::identity::casefold(alias));
        }
    }
    let own: Vec<String> = sqlx::query_scalar(
        "SELECT value FROM crm.person_identifiers WHERE kind='email' AND is_self",
    )
    .fetch_all(&store.pool)
    .await?;
    owners.extend(own.into_iter().map(|v| crate::identity::casefold(&v)));
    let method: String =
        sqlx::query_scalar("SELECT connection_method FROM crm.source_connections WHERE id=$1")
            .bind(cid)
            .fetch_one(&store.pool)
            .await?;
    let mut normalized = Vec::new();
    for item in &page.records {
        if item["deleted"] == true {
            normalized.push(item.clone());
            continue;
        }
        let external = item["external_id"]
            .as_str()
            .context("mail external_id required")?;
        let sender = addresses(item)
            .into_iter()
            .find(|p| p["role"] == "sender")
            .and_then(|p| p["id"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let rule:Option<String>=sqlx::query_scalar("SELECT category FROM crm.mail_rules WHERE connection_id=$1 AND ((scope='message' AND target=$2) OR(scope='thread' AND target=$3)OR(scope='sender' AND target=$4)) ORDER BY CASE scope WHEN 'message' THEN 0 WHEN 'thread' THEN 1 ELSE 2 END LIMIT 1").bind(cid).bind(external).bind(item["thread_id"].as_str().unwrap_or("")).bind(&sender).fetch_optional(&store.pool).await?;
        let replied: bool = if let Some(reference) = item["headers"]["in-reply-to"].as_str() {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mail.connection_records WHERE connection_id=$1 AND payload->'headers'->>'message-id'=$2)").bind(cid).bind(reference).fetch_one(&store.pool).await?
        } else {
            false
        };
        let mut decision = classify(item, rule.as_deref(), replied)?;
        if decision.category == "pending"
            && decision.reason != "user_rule"
            && owners.contains(&sender)
        {
            decision = Decision {
                category: "personal".into(),
                reason: "sent_by_self".into(),
            };
        }
        let cached: Option<Value> = sqlx::query_scalar(
            "SELECT payload FROM mail.connection_records WHERE connection_id=$1 AND external_id=$2",
        )
        .bind(cid)
        .bind(external)
        .fetch_optional(&store.pool)
        .await?;
        if decision.category == "pending" && decision.reason != "user_rule" {
            let category:Option<String>=sqlx::query_scalar("SELECT category FROM crm.mail_classifications WHERE connection_id=$1 AND external_id=$2 AND header_hash=$3").bind(cid).bind(external).bind(fingerprint(item)).fetch_optional(&store.pool).await?;
            if let Some(category) = category {
                decision = Decision {
                    category,
                    reason: "model".into(),
                };
            }
        }
        let mut record = item.clone();
        if ["personal", "transactional"].contains(&decision.category.as_str()) {
            let body = if let Some(cached) = cached.as_ref().filter(|v| {
                v.get("original").is_some() && ["gmail", "imap"].contains(&method.as_str())
            }) {
                json!({"headers":cached["headers"],"body":cached.get("raw_body").or(cached.get("body")).cloned().unwrap_or(json!("")),"body_type":cached["body_type"],"attachments":cached["attachments"],"original":cached["original"]})
            } else {
                let raw = transport.body(external).await?;
                archive_mime(store, cid, &raw).await?
            };
            let mut headers = record["headers"].as_object().cloned().unwrap_or_default();
            if let Some(h) = body["headers"].as_object() {
                headers.extend(h.clone());
            }
            record
                .as_object_mut()
                .context("mail record object required")?
                .extend(body.as_object().context("MIME object required")?.clone());
            record["headers"] = json!(headers);
        }
        normalized.push(normalize(record, &owners, &decision)?);
    }
    Ok(normalized)
}

async fn read_cursor(pool: &PgPool, cid: Uuid, stream: &str) -> Result<Value> {
    let mut tx = pool.begin().await?;
    let cursor = connections::cursor(&mut tx, Provider::Mail, cid, stream).await?;
    tx.commit().await?;
    Ok(cursor)
}
struct StagedPage {
    directory: tempfile::TempDir,
    count: usize,
}
async fn stage_page(
    store: &MediaStore,
    cid: Uuid,
    transport: &mut dyn MailTransport,
    page: &MailPage,
) -> Result<StagedPage> {
    let staging = store.root.join(".staging");
    tokio::fs::create_dir_all(&staging).await?;
    let directory = tempfile::Builder::new()
        .prefix("mail-page-")
        .tempdir_in(staging)?;
    for (index, item) in page.records.iter().enumerate() {
        let single = MailPage {
            records: vec![item.clone()],
            cursor: json!({}),
            complete: true,
        };
        let values = process_page(store, cid, transport, &single).await?;
        let bytes = serde_json::to_vec(&values[0])?;
        ensure!(
            bytes.len() <= 48 * 1024 * 1024,
            "normalized mail record exceeds bounded size"
        );
        let _disk = store
            .resources
            .acquire(crate::resources::TaskResources {
                disk: 1,
                ..Default::default()
            })
            .await?;
        tokio::fs::write(directory.path().join(index.to_string()), bytes).await?;
    }
    Ok(StagedPage {
        directory,
        count: page.records.len(),
    })
}
async fn commit_staged(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    fence: &Fence,
    stream: &str,
    cursor: &Value,
    staged: &StagedPage,
) -> Result<()> {
    connections::verify_fence(tx, fence).await?;
    sqlx::query("SET LOCAL statement_timeout='15s'")
        .execute(&mut **tx)
        .await?;
    for index in 0..staged.count {
        let bytes = tokio::fs::read(staged.directory.path().join(index.to_string())).await?;
        let record = serde_json::from_slice(&bytes)?;
        drop(bytes);
        connections::commit_page(
            tx,
            Provider::Mail,
            fence.connection_id,
            stream,
            &[record],
            cursor,
        )
        .await?;
    }
    connections::set_cursor(tx, Provider::Mail, fence.connection_id, stream, cursor).await?;
    Ok(())
}

pub async fn sync_page(
    store: &MediaStore,
    fence: &Fence,
    method: &str,
    settings: &Value,
    transport: &mut dyn MailTransport,
) -> Result<()> {
    let cid = fence.connection_id;
    let mut schedule = read_cursor(&store.pool, cid, "_schedule").await?;
    let turn = schedule["turn"].as_u64().unwrap_or(0);
    let boundary = schedule["boundary"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339());
    schedule["boundary"] = json!(boundary);
    schedule["turn"] = json!(turn + 1);
    // Expired OAuth histories need a full bounded verification pass, while the
    // rebuilt recent/history scans recover additions that may have been missed.
    let mut verify = read_cursor(&store.pool, cid, "_verify").await?;
    if method == "imap" && turn.is_multiple_of(20) && verify["active"] != true {
        verify = json!({"active":true});
    }

    if verify["active"] == true && turn.is_multiple_of(4) {
        let known:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('external_id',external_id,'uid',payload->'uid','folder',payload->'folder') FROM mail.connection_records WHERE connection_id=$1 AND external_id>$2 AND NOT coalesce((payload->>'deleted')::boolean,false) AND ($3!='imap' OR payload ? 'uid') ORDER BY external_id LIMIT 25").bind(cid).bind(verify["after"].as_str().unwrap_or("")).bind(method).fetch_all(&store.pool).await?;
        let mut records = Vec::new();
        if method == "imap" {
            let mut folders = std::collections::BTreeMap::<String, Vec<Value>>::new();
            for old in &known {
                let folder = old["folder"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| {
                        old["external_id"]
                            .as_str()
                            .and_then(|id| id.rsplitn(3, ':').nth(2))
                            .map(str::to_owned)
                    })
                    .context("stored IMAP folder missing")?;
                folders.entry(folder).or_default().push(old.clone());
            }
            for (folder, known) in folders {
                records.extend(transport.reconcile(&folder, &known).await?);
            }
        } else {
            records = transport.reconcile("", &known).await?;
        }
        let page = MailPage {
            records,
            cursor: json!({"active":known.len()==25,"after":known.last().and_then(|v|v["external_id"].as_str()).unwrap_or("")}),
            complete: known.len() < 25,
        };
        let staged = stage_page(store, cid, transport, &page).await?;
        let mut tx = store.pool.begin().await?;
        commit_staged(&mut tx, fence, "_verify", &page.cursor, &staged).await?;
        connections::set_cursor(&mut tx, Provider::Mail, cid, "_schedule", &schedule).await?;
        tx.commit().await?;
        return Ok(());
    }
    // Reprocess requests are acknowledged only after committing the replacement
    // revision; a concurrent rule change keeps its newer request timestamp.
    if turn.is_multiple_of(4) {
        let requests:Vec<(String,String)>=sqlx::query_as("SELECT external_id,requested_at::text FROM crm.connection_reprocess WHERE connection_id=$1 ORDER BY requested_at,external_id LIMIT 25").bind(cid).fetch_all(&store.pool).await?;
        if !requests.is_empty() {
            let mut records = Vec::new();
            for (id, _) in &requests {
                records.push(transport.metadata(id).await?)
            }
            let page = MailPage {
                records,
                cursor: json!({}),
                complete: true,
            };
            let staged = stage_page(store, cid, transport, &page).await?;
            let mut tx = store.pool.begin().await?;
            commit_staged(&mut tx, fence, "_reprocess", &page.cursor, &staged).await?;
            for (id, at) in requests {
                sqlx::query("DELETE FROM crm.connection_reprocess WHERE connection_id=$1 AND external_id=$2 AND requested_at=$3::timestamptz").bind(cid).bind(id).bind(at).execute(&mut *tx).await?;
            }
            connections::set_cursor(&mut tx, Provider::Mail, cid, "_schedule", &schedule).await?;
            tx.commit().await?;
            return Ok(());
        }
    }
    let mut folders: Vec<String> = settings["folders"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if method == "outlook" {
        let mut state = read_cursor(&store.pool, cid, "_folders").await?;
        let last = state["checked_at"].as_i64().unwrap_or(0);
        if turn.is_multiple_of(8)
            && (state["complete"] != true || chrono::Utc::now().timestamp() - last >= 3600)
        {
            let progress = if state["complete"] == true {
                json!({})
            } else {
                state.get("cursor").cloned().unwrap_or(json!({}))
            };
            let page = transport.folders(&progress).await?;
            let mut found: Vec<Value> = state["folders"].as_array().cloned().unwrap_or_default();
            for record in page.records {
                if let Some(id) = record["id"].as_str()
                    && !found.iter().any(|f| f == id)
                {
                    found.push(json!(id));
                }
            }
            ensure!(found.len() <= 10000, "mail folder limit exceeded");
            state = json!({"cursor":page.cursor,"folders":found,"complete":page.complete,"checked_at":chrono::Utc::now().timestamp()});
            let mut tx = store.pool.begin().await?;
            connections::verify_fence(&mut tx, fence).await?;
            connections::set_cursor(&mut tx, Provider::Mail, cid, "_folders", &state).await?;
            connections::set_cursor(&mut tx, Provider::Mail, cid, "_schedule", &schedule).await?;
            tx.commit().await?;
            return Ok(());
        }
        if folders.is_empty() {
            folders = state["folders"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
    }
    if folders.is_empty() {
        folders = if method == "outlook" {
            vec!["inbox".into(), "sentitems".into()]
        } else {
            vec!["INBOX".into()]
        };
    }
    ensure!(folders.len() <= 10000, "mail folder limit exceeded");
    let folder = &folders[turn as usize % folders.len()];
    let mut mode = ["live", "recent", "live", "history"][(turn as usize / folders.len()) % 4];
    let prefix = if method == "gmail" { "mailbox" } else { folder };
    if mode == "history"
        && read_cursor(&store.pool, cid, &format!("{prefix}:recent")).await?["complete"] != true
    {
        mode = "recent";
    }
    let mut stream = format!("{prefix}:{mode}");
    let mut cursor = read_cursor(&store.pool, cid, &stream).await?;
    if mode != "live" && cursor["complete"] == true {
        mode = "live";
        stream = format!("{prefix}:live");
        cursor = read_cursor(&store.pool, cid, &stream).await?;
    }
    if method == "gmail"
        && mode == "live"
        && cursor.get("history_id").is_none()
        && cursor.get("initial_history_id").is_none()
        && let Some(checkpoint) = transport.checkpoint().await?
    {
        cursor = checkpoint;
    }
    if method == "gmail" && mode != "live" {
        let stamp = chrono::DateTime::parse_from_rfc3339(&boundary)?.timestamp();
        cursor["query"] = json!(format!(
            "-in:spam -in:trash {}:{stamp}",
            if mode == "history" { "before" } else { "after" }
        ));
    }
    if method == "outlook"
        && mode != "history"
        && cursor.get("next_link").is_none()
        && cursor.get("delta_link").is_none()
    {
        cursor["since"] = if mode == "live" {
            json!((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339())
        } else {
            json!(boundary)
        };
    }
    if method == "imap" {
        let date = chrono::DateTime::parse_from_rfc3339(&boundary)?
            .format("%d-%b-%Y")
            .to_string();
        if mode == "live" {
            cursor["tail"] = json!(true);
        } else {
            cursor["descending"] = json!(true);
            cursor[if mode == "history" { "before" } else { "since" }] = json!(date);
        }
    }
    let mut page = match transport.scan(folder, &cursor).await {
        Ok(page) => page,
        Err(error) if error.downcast_ref::<ExpiredCursor>().is_some() => {
            let mut tx = store.pool.begin().await?;
            connections::verify_fence(&mut tx, fence).await?;
            for lane in ["live", "recent", "history"] {
                connections::set_cursor(
                    &mut tx,
                    Provider::Mail,
                    cid,
                    &format!("{prefix}:{lane}"),
                    &json!({}),
                )
                .await?;
            }
            connections::set_cursor(
                &mut tx,
                Provider::Mail,
                cid,
                "_verify",
                &json!({"active":true}),
            )
            .await?;
            connections::set_cursor(&mut tx, Provider::Mail, cid, "_schedule", &schedule).await?;
            tx.commit().await?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    ensure!(page.records.len() <= 1000, "mail page exceeds record limit");
    let staged = stage_page(store, cid, transport, &page).await?;
    if mode != "live" {
        page.cursor["complete"] = json!(page.complete);
    }
    let mut tx = store.pool.begin().await?;
    connections::verify_fence(&mut tx, fence).await?;
    // A changed UIDVALIDITY invalidates every lane for the selected folder, in
    // the same transaction as the page observed under the new UID namespace.
    if method == "imap"
        && cursor.get("uidvalidity").is_some()
        && cursor["uidvalidity"] != page.cursor["uidvalidity"]
    {
        for lane in ["live", "recent", "history"] {
            connections::set_cursor(
                &mut tx,
                Provider::Mail,
                cid,
                &format!("{prefix}:{lane}"),
                &json!({}),
            )
            .await?;
        }
        connections::set_cursor(
            &mut tx,
            Provider::Mail,
            cid,
            "_verify",
            &json!({"active":true}),
        )
        .await?;
    }
    commit_staged(&mut tx, fence, &stream, &page.cursor, &staged).await?;
    connections::set_cursor(&mut tx, Provider::Mail, cid, "_schedule", &schedule).await?;
    tx.commit().await?;
    Ok(())
}

/// Claims one explicitly enabled connection. Two fixed scheduler lanes call
/// this function; a lease is fenced against account edits, disable and takeover.
pub async fn run_configured(store: &MediaStore, cipher: &CredentialCipher) -> Result<bool> {
    let recovered = recover_one(&store.pool).await?;
    let owner = Uuid::new_v4();
    let token = Uuid::new_v4();
    let row=sqlx::query("WITH candidate AS(SELECT id FROM crm.source_connections WHERE enabled AND provider IN ('mail','whatsapp') AND(connection_method IN('gmail','outlook','imap','qr')) AND(next_sync_at<=clock_timestamp() OR sync_pending) AND(lease_until IS NULL OR lease_until<=clock_timestamp()) ORDER BY next_sync_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE crm.source_connections c SET lease_owner=$1,lease_token=$2,lease_until=clock_timestamp()+interval '90 seconds',sync_pending=false FROM candidate q WHERE c.id=q.id RETURNING c.*").bind(owner).bind(token).fetch_optional(&store.pool).await?;
    let Some(row) = row else { return Ok(recovered) };
    let cid: Uuid = row.get("id");
    let fence = Fence {
        connection_id: cid,
        owner,
        generation: row.get("generation"),
    };
    let method: String = row.get("connection_method");
    let provider: String = row.get("provider");
    let settings: Value = row.get("settings");
    let account: String = row.get("external_account");
    let encrypted: Option<String> = row.get("credential_ciphertext");
    let operation = async {
        let collected = if provider == "whatsapp" {
            let directory = store
                .root
                .join("sources")
                .join("whatsapp")
                .join(cid.to_string());
            crate::whatsapp::consume_device_page(store, &fence, &directory).await?
        } else {
            let mut transport: Box<dyn MailTransport> = match method.as_str() {
                "imap" => {
                    let credentials = cipher
                        .decrypt(encrypted.as_deref().context("mail credentials missing")?)?;
                    let host = settings["host"]
                        .as_str()
                        .context("IMAP hostname required")?;
                    let port = settings["port"].as_u64().unwrap_or(993);
                    ensure!((1..=65535).contains(&port), "invalid IMAP port");
                    Box::new(
                        imap::Imap::connect(
                            host,
                            port as u16,
                            &account,
                            credentials["password"]
                                .as_str()
                                .context("IMAP password missing")?,
                        )
                        .await?,
                    )
                }
                "gmail" | "outlook" => {
                    let access = crate::mail_oauth::OAuthClient::new()?
                        .access_token(&store.pool, cipher, cid, token, false)
                        .await?;
                    let http = Box::new(
                        http::OAuthHttp::new(
                            access,
                            if method == "gmail" {
                                "gmail.googleapis.com"
                            } else {
                                "graph.microsoft.com"
                            },
                        )?
                        .with_refresh(
                            store.pool.clone(),
                            cipher.clone(),
                            cid,
                            token,
                        ),
                    );
                    if method == "gmail" {
                        Box::new(http::Gmail::new(http))
                    } else {
                        Box::new(http::Outlook::new(http))
                    }
                }
                _ => anyhow::bail!("unsupported mail connection method"),
            };
            sync_page(store, &fence, &method, &settings, &mut *transport).await?;
            true
        };
        connections::deliver(&store.pool, Provider::parse(&provider)?, cid, 1).await?;
        Ok::<_, anyhow::Error>(collected)
    };
    tokio::pin!(operation);
    let mut check = tokio::time::interval(Duration::from_millis(250));
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heartbeat = tokio::time::Instant::now();
    let deadline = tokio::time::sleep(Duration::from_secs(300));
    tokio::pin!(deadline);
    let result = loop {
        tokio::select! {result=&mut operation=>break result,_=&mut deadline=>break Err(anyhow::anyhow!("source page deadline exceeded")),_=check.tick()=>{let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.source_connections WHERE id=$1 AND lease_owner=$2 AND lease_token=$3 AND generation=$4 AND enabled AND lease_until>clock_timestamp())").bind(cid).bind(owner).bind(token).bind(fence.generation).fetch_one(&store.pool).await?;if !valid{break Err(anyhow::anyhow!("connection lease lost"))}
        if heartbeat.elapsed()>=Duration::from_secs(15){sqlx::query("UPDATE crm.source_connections SET lease_until=clock_timestamp()+interval '90 seconds' WHERE id IN(SELECT id FROM crm.source_connections WHERE id=$1 AND lease_owner=$2 AND lease_token=$3 AND generation=$4 AND enabled AND lease_until>clock_timestamp() FOR UPDATE SKIP LOCKED)").bind(cid).bind(owner).bind(token).bind(fence.generation).execute(&store.pool).await?;heartbeat=tokio::time::Instant::now();}}}
    };
    let device_state = if provider == "whatsapp" {
        let path = store
            .root
            .join("sources/whatsapp")
            .join(cid.to_string())
            .join("status.json");
        if tokio::fs::metadata(&path).await.is_ok_and(|m| {
            m.len() <= 64 * 1024
                && m.modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age < Duration::from_secs(60))
        }) {
            tokio::fs::read(path)
                .await
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|v| {
                    v["state"]
                        .as_str()
                        .filter(|s| {
                            [
                                "connected",
                                "connecting",
                                "qr",
                                "needs_login",
                                "paused",
                                "disconnected",
                                "error",
                                "timeout",
                            ]
                            .contains(s)
                        })
                        .map(str::to_owned)
                })
        } else {
            None
        }
    } else {
        None
    };
    // Error messages deliberately omit account addresses, tokens and server bodies.
    sqlx::query("UPDATE crm.source_connections SET lease_owner=NULL,lease_token=NULL,lease_until=NULL,state=CASE WHEN NOT $5 THEN 'error' WHEN $6='whatsapp' THEN coalesce($7,state) ELSE 'connected' END,error=CASE WHEN NOT $5 THEN 'Source page failed; retry scheduled' WHEN $6='whatsapp' AND $7 IS NULL THEN error ELSE NULL END,last_success_at=CASE WHEN $5 AND $8 THEN now() ELSE last_success_at END,next_sync_at=clock_timestamp()+CASE WHEN $5 THEN interval '5 seconds' ELSE interval '60 seconds' END WHERE id=$1 AND lease_owner=$2 AND lease_token=$3 AND generation=$4 AND enabled").bind(cid).bind(owner).bind(token).bind(fence.generation).bind(result.is_ok()).bind(&provider).bind(device_state).bind(result.as_ref().copied().unwrap_or(false)).execute(&store.pool).await?;
    result?;
    Ok(true)
}

/// Recover one committed source revision even when its account is disconnected.
/// Accounts rotate by their last served turn; pages are capped at 4 MiB/1000 rows.
pub async fn recover_one(pool: &PgPool) -> Result<bool> {
    let snapshots =
        connections::finish_native_snapshots_bounded(pool, 1000, 4 * 1024 * 1024).await?;
    let relinked = connections::relink_pending(pool, 1).await?;
    let predicates=["wechat","instagram","linkedin","whatsapp","apple_messages","apple_contacts","mail"].map(|schema|format!("(c.provider='{schema}' AND EXISTS(SELECT 1 FROM {schema}.connection_outbox o WHERE o.connection_id=c.id AND o.delivered_at IS NULL))"));
    let mut claim = pool.begin().await?;
    let candidate:Option<(Uuid,String)>=sqlx::query_as(&format!("SELECT c.id,c.provider FROM crm.source_connections c LEFT JOIN crm.source_recovery_turns t ON t.connection_id=c.id WHERE {} ORDER BY t.served_at ASC NULLS FIRST,c.created_at,c.id LIMIT 1 FOR UPDATE OF c SKIP LOCKED",predicates.join(" OR "))).fetch_optional(&mut *claim).await?;
    if let Some((cid, _)) = &candidate {
        sqlx::query("INSERT INTO crm.source_recovery_turns(connection_id,served_at) VALUES($1,clock_timestamp()) ON CONFLICT(connection_id) DO UPDATE SET served_at=excluded.served_at").bind(cid).execute(&mut *claim).await?;
    }
    claim.commit().await?;
    let delivered = if let Some((cid, provider)) = candidate {
        connections::deliver_bounded(
            pool,
            Provider::parse(&provider)?,
            cid,
            1000,
            4 * 1024 * 1024,
        )
        .await?
    } else {
        0
    };
    Ok(snapshots + relinked + delivered > 0)
}

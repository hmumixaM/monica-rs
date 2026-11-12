//! Up to four independently cancellable IMAP IDLE sockets. Periodic mailbox
//! polling remains the correctness mechanism when IDLE is unsupported/offline.
use super::{Mode, Shared};
use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::Row;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{task::JoinHandle, time::Instant};
use uuid::Uuid;
struct Watcher {
    generation: i64,
    task: JoinHandle<()>,
}
impl Drop for Watcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) async fn run(shared: Arc<Shared>) -> Result<()> {
    let Some(cipher) = shared.credentials.clone() else {
        return Ok(());
    };
    let mut mode = shared.mode.subscribe();
    let mut watchers: HashMap<Uuid, Watcher> = HashMap::new();
    let mut retry: HashMap<Uuid, Instant> = HashMap::new();
    loop {
        let current = *mode.borrow_and_update();
        match current {
            Mode::Stopping => return Ok(()),
            Mode::Paused | Mode::Draining => {
                watchers.clear();
                mode.changed().await?;
                continue;
            }
            Mode::Running => {}
        }
        let rows=sqlx::query("SELECT id,generation,external_account,settings,credential_ciphertext FROM crm.source_connections WHERE enabled AND provider='mail' AND connection_method='imap' ORDER BY id LIMIT 4").fetch_all(&shared.control).await?;
        watchers.retain(|id, w| {
            let keep = rows.iter().any(|r| {
                r.get::<Uuid, _>("id") == *id && r.get::<i64, _>("generation") == w.generation
            }) && !w.task.is_finished();
            if !keep {
                retry.insert(*id, Instant::now() + Duration::from_secs(60));
            }
            keep
        });
        for row in rows {
            let id: Uuid = row.get("id");
            if watchers.contains_key(&id) || retry.get(&id).is_some_and(|t| *t > Instant::now()) {
                continue;
            }
            let generation: i64 = row.get("generation");
            let settings: Value = row.get("settings");
            let account: String = row.get("external_account");
            let encrypted: Option<String> = row.get("credential_ciphertext");
            let cipher = cipher.clone();
            let pool = shared.control.clone();
            let resources = shared.resources.clone();
            let task = tokio::spawn(async move {
                let operation = async {
                    // Sockets are separately capped at four; idle sockets consume no active
                    // network/CPU slot that could starve the page executors on a small NAS.
                    let _memory = resources
                        .acquire(crate::resources::TaskResources {
                            memory_bytes: 1024 * 1024,
                            ..Default::default()
                        })
                        .await?;
                    let secret = cipher
                        .decrypt(encrypted.as_deref().context("IMAP credentials missing")?)?;
                    let port = settings["port"].as_u64().unwrap_or(993);
                    anyhow::ensure!((1..=65535).contains(&port), "invalid IMAP port");
                    let mut imap = crate::mail::imap::Imap::connect(
                        settings["host"].as_str().context("IMAP host missing")?,
                        port as u16,
                        &account,
                        secret["password"]
                            .as_str()
                            .context("IMAP password missing")?,
                    )
                    .await?;
                    let folder = settings["folders"]
                        .as_array()
                        .and_then(|f| f.first())
                        .and_then(Value::as_str)
                        .unwrap_or("INBOX");
                    if imap.idle(folder).await? {
                        sqlx::query("UPDATE crm.source_connections SET sync_pending=true,next_sync_at=clock_timestamp() WHERE id=$1 AND generation=$2 AND enabled").bind(id).bind(generation).execute(&pool).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                };
                let _ = operation.await;
            });
            watchers.insert(id, Watcher { generation, task });
        }
        retry.retain(|_, at| *at > Instant::now());
        tokio::select! {_=tokio::time::sleep(Duration::from_secs(5))=>{},_=mode.changed()=>{}}
    }
}

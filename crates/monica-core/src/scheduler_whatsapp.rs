//! Optional linked-device process. External containers consume the same atomic
//! control file and publish into the connection directory without core spawning.
use super::{Mode, Shared};
use anyhow::Result;
use sqlx::Row;
use std::{collections::HashMap, process::Stdio, sync::Arc, time::Duration};
use tokio::process::{Child, Command};
use uuid::Uuid;
struct Device {
    id: Uuid,
    generation: i64,
    child: Option<Child>,
}
impl Device {
    async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_group(&child);
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}
fn kill_group(child: &Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_group(&child);
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}
pub(super) async fn run(shared: Arc<Shared>) -> Result<()> {
    let binary = shared.whatsapp_binary.clone();
    let Some(media) = &shared.media else {
        return Ok(());
    };
    let mut mode = shared.mode.subscribe();
    let mut device: Option<Device> = None;
    let mut retry_at = tokio::time::Instant::now();
    let mut controls = HashMap::new();
    let mut failed_until = HashMap::new();
    loop {
        let current = *mode.borrow_and_update();
        let rows=sqlx::query("SELECT id,generation,enabled,state FROM crm.source_connections WHERE provider='whatsapp' AND connection_method='qr' ORDER BY created_at,id LIMIT 1000").fetch_all(&shared.control).await?;
        let mut desired = None;
        for row in rows {
            let id: Uuid = row.get("id");
            let generation: i64 = row.get("generation");
            let enabled: bool = row.get("enabled");
            // A connection pause suspends transport but retains device keys. Only
            // explicit disconnect may erase a linked-device session.
            let disconnected = !enabled && row.get::<String, _>("state") == "disconnected";
            let state = (
                generation,
                !disconnected,
                !enabled || current != Mode::Running,
            );
            if failed_until
                .get(&id)
                .is_some_and(|(g, until)| *g == generation && *until > tokio::time::Instant::now())
            {
                continue;
            }
            if controls.get(&id) != Some(&state) {
                let root = media
                    .root
                    .join("sources")
                    .join("whatsapp")
                    .join(id.to_string());
                if crate::whatsapp::write_control(&root, state.1, state.2)
                    .await
                    .is_err()
                {
                    mark_error(
                        &shared,
                        id,
                        generation,
                        "error",
                        "WhatsApp bridge directory unavailable",
                    )
                    .await?;
                    failed_until.insert(
                        id,
                        (
                            generation,
                            tokio::time::Instant::now() + Duration::from_secs(60),
                        ),
                    );
                    continue;
                }
                if disconnected && binary.is_some() {
                    if device.as_ref().is_some_and(|d| d.id == id)
                        && let Some(mut running) = device.take()
                    {
                        running.stop().await;
                    }
                    if crate::whatsapp::clear_local_session(&root).await.is_err() {
                        mark_error(
                            &shared,
                            id,
                            generation,
                            "error",
                            "WhatsApp local session could not be cleared",
                        )
                        .await?;
                        failed_until.insert(
                            id,
                            (
                                generation,
                                tokio::time::Instant::now() + Duration::from_secs(60),
                            ),
                        );
                        continue;
                    }
                }
                controls.insert(id, state);
            }
            if enabled && desired.is_none() {
                desired = Some((id, generation));
            }
        }
        if current != Mode::Running {
            if let Some(mut running) = device.take() {
                running.stop().await;
            }
            if current == Mode::Stopping {
                return Ok(());
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(1))=>{},result=mode.changed()=>{result?;}}
            continue;
        }
        let stopped = device
            .as_mut()
            .and_then(|running| running.child.as_mut())
            .is_some_and(|child| !matches!(child.try_wait(), Ok(None)));
        if device
            .as_ref()
            .is_some_and(|d| Some((d.id, d.generation)) != desired)
            || stopped
        {
            if let Some(mut running) = device.take() {
                running.stop().await;
            }
            retry_at =
                tokio::time::Instant::now() + Duration::from_secs(if stopped { 30 } else { 0 });
        }
        // One embedded process slot. Additional accounts can use isolated external
        // bridge containers; all their ingestion still shares the fixed source lanes.
        if device.is_none()
            && tokio::time::Instant::now() >= retry_at
            && let Some((id, generation)) = desired
            && let Some(binary) = &binary
        {
            let root = media
                .root
                .join("sources")
                .join("whatsapp")
                .join(id.to_string());
            if !binary.is_absolute() || !binary.is_file() {
                mark_error(
                    &shared,
                    id,
                    generation,
                    "needs_bridge",
                    "WhatsApp bridge executable unavailable",
                )
                .await?;
                retry_at = tokio::time::Instant::now() + Duration::from_secs(60);
                continue;
            }
            let mut command = Command::new(binary);
            command
                .arg("--root")
                .arg(root)
                .env_clear()
                .env("GOMEMLIMIT", "32MiB")
                .env("GOMAXPROCS", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(unix)]
            command.process_group(0);
            match command.spawn() {
                Ok(child) => {
                    device = Some(Device {
                        id,
                        generation,
                        child: Some(child),
                    })
                }
                Err(_) => {
                    retry_at = tokio::time::Instant::now() + Duration::from_secs(60);
                    sqlx::query("UPDATE crm.source_connections SET state='error',error='WhatsApp bridge could not start' WHERE id=$1 AND generation=$2 AND enabled").bind(id).bind(generation).execute(&shared.control).await?;
                }
            }
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_secs(1))=>{},_=mode.changed()=>{}}
    }
}

async fn mark_error(
    shared: &Shared,
    id: Uuid,
    generation: i64,
    state: &str,
    error: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE crm.source_connections SET state=$3,error=$4 WHERE id=$1 AND generation=$2",
    )
    .bind(id)
    .bind(generation)
    .bind(state)
    .bind(error)
    .execute(&shared.control)
    .await?;
    Ok(())
}

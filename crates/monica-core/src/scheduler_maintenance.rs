//! Bounded derived work and periodic maintenance are independent of ingestion.
use super::{Active, Mode, Shared, background_request};
use anyhow::Result;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::time::Instant;

pub(super) async fn run(shared: Arc<Shared>) -> Result<()> {
    let mut mode = shared.mode.subscribe();
    let mut housekeeping_at = Instant::now();
    let mut monitor_at = Instant::now() - Duration::from_secs(5);
    loop {
        let current = *mode.borrow_and_update();
        match current {
            Mode::Stopping => return Ok(()),
            Mode::Paused | Mode::Draining => {
                mode.changed().await?;
                continue;
            }
            Mode::Running => {}
        }
        tokio::select! {
            _ = shared.resources.background_turn() => {}
            result = mode.changed() => { result?; continue; }
        }
        let permit = tokio::select! {
            result = shared.resources.acquire(background_request()) => result?,
            result = mode.changed() => { result?; continue; }
        };
        let gate = shared.admission.read().await;
        if *shared.mode.borrow() != Mode::Running {
            drop(gate);
            drop(permit);
            continue;
        }
        let active = Active::new(shared.clone());
        drop(gate);
        let result = tokio::time::timeout(shared.config.task_timeout, async {
            if monitor_at.elapsed() >= Duration::from_secs(5) {
                crate::import_monitor::refresh(&shared.background).await?;
                monitor_at = Instant::now();
            }
            let importing: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.import_batches WHERE status IN ('received','running') AND retry_after<=clock_timestamp())")
                .fetch_one(&shared.background).await?;
            let mut reduced = 0;
            if !importing {
                let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_pending_records) OR EXISTS(SELECT 1 FROM crm.memory_legacy_scopes)")
                    .fetch_one(&shared.background).await?;
                if pending {
                    for _ in 0..4 {
                        let n = crate::ai::memory::legacy::drain(&shared.background).await?;
                        reduced += n;
                        if n == 0 { break; }
                    }
                }
            }
            // Media maintenance reserves its own I/O memory, after this tick's
            // allocation is released, preserving the global acquisition order.
            drop(permit);
            if let Some(media) = &shared.media {
                for _ in 0..25 {
                    if crate::media::consume_record_media(media).await? == 0 { break; }
                }
                crate::media::consume_imports(&shared.background).await?;
                crate::media::resolve_dependencies(&shared.background).await?;
                crate::media::refresh_profiles(&shared.background).await?;
                crate::adapter::consume_media(&shared.background, &media.root, &shared.resources).await?;
                media.recover_publications().await?;
                if !importing && housekeeping_at.elapsed() >= Duration::from_secs(15) {
                    media.expire_uploads().await?;
                    media.collect_orphans(Duration::from_secs(24 * 3600)).await?;
                }
            }
            if !importing && housekeeping_at.elapsed() >= Duration::from_secs(15) {
                sqlx::query("DELETE FROM crm.import_run_events WHERE id IN(SELECT id FROM crm.import_run_events WHERE created_at<now()-interval '90 days' ORDER BY created_at LIMIT 1000)").execute(&shared.background).await?;
                crate::pages::refresh_metrics(&shared.background).await?;
                housekeeping_at = Instant::now();
            }
            Ok::<_, anyhow::Error>(reduced)
        }).await;
        let progressed = match result {
            Ok(Ok(n)) => n > 0,
            _ => {
                shared.failed.fetch_add(1, Ordering::Relaxed);
                false
            }
        };
        drop(active);
        if progressed {
            tokio::task::yield_now().await;
        } else {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = shared.maintenance_wake.notified() => {}
                _ = mode.changed() => {}
            }
        }
    }
}

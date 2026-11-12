//! Three fixed model lanes, sharing reserved background capacity with sources.
use super::{Active, Mode, Shared};
use anyhow::Result;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
pub(super) async fn run(shared: Arc<Shared>) -> Result<()> {
    let Some(media) = &shared.media else {
        return Ok(());
    };
    let mut mode = shared.mode.subscribe();
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
        tokio::select! {_=shared.resources.background_turn()=>{},result=mode.changed()=>{result?;continue}}
        let gate = shared.admission.read().await;
        if *shared.mode.borrow() != Mode::Running {
            drop(gate);
            continue;
        }
        let active = Active::new(shared.clone());
        drop(gate);
        // The provider boundary owns background+global permits, exactly once. Pause
        // also cancels a task waiting for permits or an in-flight network response.
        tokio::select! {
         result=tokio::time::timeout(shared.config.task_timeout,crate::ai::run_configured(&shared.background,&media.root,&shared.resources,shared.credentials.as_ref()))=>{if !matches!(result,Ok(Ok(_))){shared.failed.fetch_add(1,Ordering::Relaxed);}},
         result=mode.changed()=>{result?;}
        }
        drop(active);
        tokio::select! {_=tokio::time::sleep(Duration::from_secs(1))=>{},_=mode.changed()=>{}}
    }
}

//! One fixed external-adapter lane; subprocess lifetime follows runtime control.
use super::{Active, Mode, Shared};
use anyhow::Result;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub(super) async fn run(shared: Arc<Shared>) -> Result<()> {
    run_lane(shared, false).await
}
pub(super) async fn run_messages(shared: Arc<Shared>) -> Result<()> {
    run_lane(shared, true).await
}
async fn run_lane(shared: Arc<Shared>, messages: bool) -> Result<()> {
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
        tokio::select! {
            _=shared.resources.background_turn()=>{},
            result=mode.changed()=>{result?;continue;}
        }
        let gate = shared.admission.read().await;
        if *shared.mode.borrow() != Mode::Running {
            drop(gate);
            continue;
        }
        let active = Active::new(shared.clone());
        drop(gate);
        // run_configured owns its CPU/network/memory permits. Cancellation here
        // drops its child/process-group guard even when stdout has closed and
        // the executable has not exited. Durable leases permit later recovery.
        tokio::select! {
            result=tokio::time::timeout(Duration::from_secs(900),async {
                if messages { crate::adapter::run_messages_configured(&shared.background,&shared.resources).await }
                else { crate::adapter::run_configured(&shared.background,&shared.resources).await }
            })=>{
                if !matches!(result,Ok(Ok(_))) {shared.failed.fetch_add(1,Ordering::Relaxed);}
            },
            result=mode.changed()=>{result?;}
        }
        drop(active);
        tokio::select! {
            _=tokio::time::sleep(Duration::from_secs(1))=>{},
            _=mode.changed()=>{}
        }
    }
}

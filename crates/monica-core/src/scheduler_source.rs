//! Fixed source page lanes. Cancelling a lane drops TLS/HTTP futures immediately.
use super::{Active, Mode, Shared};
use crate::resources::TaskResources;
use anyhow::Result;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
pub(super) fn request() -> TaskResources {
    TaskResources {
        cpu: 1,
        network: 1,
        memory_bytes: 192 * 1024 * 1024,
        ..Default::default()
    }
}
pub(super) async fn run(shared: Arc<Shared>) -> Result<()> {
    let (Some(media), Some(cipher)) = (&shared.media, &shared.credentials) else {
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
        tokio::select! {_=shared.resources.background_turn()=>{},r=mode.changed()=>{r?;continue}}
        let permit = tokio::select! {p=shared.resources.acquire_background(request())=>p?,r=mode.changed()=>{r?;continue}};
        let gate = shared.admission.read().await;
        if *shared.mode.borrow() != Mode::Running {
            drop(gate);
            drop(permit);
            continue;
        }
        let active = Active::new(shared.clone());
        drop(gate);
        let worked = tokio::select! {result=crate::mail::sync::run_configured(media,cipher)=>{match result{Ok(worked)=>worked,Err(_)=>{shared.failed.fetch_add(1,Ordering::Relaxed);false}}},r=mode.changed()=>{r?;false}};
        drop(active);
        drop(permit);
        if worked {
            tokio::task::yield_now().await;
        } else {
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(1))=>{},_=mode.changed()=>{}}
        }
    }
}

//! Optional fixed WAL capture lane. Slot initialization is an explicit CLI action.
use super::{Active, Mode, Shared};
use crate::resources::TaskResources;
use anyhow::Result;
use std::{sync::Arc, time::Duration};

pub(super) async fn run(shared: Arc<Shared>) -> Result<()> {
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
        let mut progress = false;
        if crate::cdc::enabled().unwrap_or(false) {
            let work = async {
                shared.resources.background_turn().await;
                let _permit = shared
                    .resources
                    .acquire_background(TaskResources {
                        cpu: 1,
                        memory_bytes: 128 * 1024 * 1024,
                        ..TaskResources::default()
                    })
                    .await?;
                let gate = shared.admission.read().await;
                if *shared.mode.borrow() != Mode::Running {
                    return Ok(0);
                }
                let _active = Active::new(shared.clone());
                drop(gate);
                crate::cdc::capture(&shared.background).await
            };
            tokio::select! {
                result=tokio::time::timeout(shared.config.task_timeout,work)=>match result {
                    Ok(Ok(n))=>progress=n>0,
                    _=>{let _=tokio::time::timeout(Duration::from_secs(2),crate::cdc::mark_error(&shared.background)).await;}
                },
                changed=mode.changed()=>{changed?;}
            }
        }
        if progress {
            tokio::task::yield_now().await;
        } else {
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(30))=>{},changed=mode.changed()=>{changed?;}}
        }
    }
}

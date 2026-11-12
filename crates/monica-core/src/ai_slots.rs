//! Four model slots, with independent reserved interactive/background capacity.
use anyhow::{Context, Result};
use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
struct Slots {
    interactive: Arc<Semaphore>,
    background: Arc<Semaphore>,
    waiters: Arc<Semaphore>,
}
fn slots() -> &'static Slots {
    static SLOTS: OnceLock<Slots> = OnceLock::new();
    SLOTS.get_or_init(|| Slots {
        interactive: Arc::new(Semaphore::new(1)),
        background: Arc::new(Semaphore::new(3)),
        waiters: Arc::new(Semaphore::new(8)),
    })
}
pub async fn background() -> Result<OwnedSemaphorePermit> {
    slots()
        .background
        .clone()
        .acquire_owned()
        .await
        .context("model background capacity closed")
}
pub async fn interactive() -> Result<OwnedSemaphorePermit> {
    let _waiter = slots()
        .waiters
        .clone()
        .try_acquire_owned()
        .context("interactive model queue full")?;
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        slots().interactive.clone().acquire_owned(),
    )
    .await
    .context("interactive model queue deadline")?
    .context("interactive model capacity closed")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn background_cannot_consume_reserved_interactive_and_drop_releases() {
        let (a, b, c) = tokio::join!(background(), background(), background());
        let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), background())
                .await
                .is_err()
        );
        let interactive = interactive().await.unwrap();
        assert_eq!(slots().interactive.available_permits(), 0);
        drop(interactive);
        assert_eq!(slots().interactive.available_permits(), 1);
        drop((a, b, c));
        assert_eq!(slots().background.available_permits(), 3);
    }
}

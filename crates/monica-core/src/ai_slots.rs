//! Separate concurrency budget for batch model work.
use anyhow::{Context, Result};
use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
fn background_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(3)))
}
pub async fn background() -> Result<OwnedSemaphorePermit> {
    background_slots()
        .clone()
        .acquire_owned()
        .await
        .context("model background capacity closed")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn background_capacity_is_bounded_and_drop_releases() {
        let (a, b, c) = tokio::join!(background(), background(), background());
        let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), background())
                .await
                .is_err()
        );
        drop((a, b, c));
        assert_eq!(background_slots().available_permits(), 3);
    }
}

//! Content-addressed source originals. Public APIs accept hashes, never paths.
use crate::{
    media::{Asset, MediaStore, hash_file},
    resources::TaskResources,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlobRef {
    pub blob: String,
    pub size: u64,
}

pub fn path(root: &Path, hash: &str) -> Result<PathBuf> {
    Ok(root.join(crate::media::object_path(hash)?))
}

pub async fn archive_bytes(
    store: &MediaStore,
    provider: &str,
    connection: Uuid,
    bytes: &[u8],
) -> Result<BlobRef> {
    ensure!(
        bytes.len() as u64 <= crate::media::MAX_FILE,
        "source original exceeds storage limit"
    );
    let staging = store.root.join(".staging");
    tokio::fs::create_dir_all(&staging).await?;
    let temporary = tempfile::NamedTempFile::new_in(staging)?;
    {
        let _disk = store
            .resources
            .acquire(TaskResources {
                disk: 1,
                ..Default::default()
            })
            .await?;
        let mut output = tokio::fs::File::create(temporary.path()).await?;
        output.write_all(bytes).await?;
        output.sync_all().await?;
    }
    archive_path(store, provider, connection, temporary.path()).await
}

pub async fn archive_path(
    store: &MediaStore,
    provider: &str,
    connection: Uuid,
    input: &Path,
) -> Result<BlobRef> {
    ensure!(
        ["mail", "whatsapp", "apple_messages", "apple_contacts"].contains(&provider),
        "unsupported source blob provider"
    );
    let _disk = store
        .resources
        .acquire(TaskResources {
            disk: 1,
            ..Default::default()
        })
        .await?;
    let (hash, size) = hash_file(input).await?;
    let asset = Asset {
        id: Uuid::new_v4(),
        sha256: hash.clone(),
        media_type: "application/octet-stream".into(),
        byte_length: size as i64,
        object_path: String::new(),
        file_name: None,
        width: None,
        height: None,
        duration_ms: None,
    };
    let saved = store
        .publish(
            input,
            asset,
            provider,
            Some(&format!("{connection}:{hash}")),
        )
        .await?;
    sqlx::query("INSERT INTO crm.source_blob_refs(connection_id,provider,sha256,media_id,byte_length) VALUES($1,$2,$3,$4,$5) ON CONFLICT(connection_id,sha256) DO NOTHING").bind(connection).bind(provider).bind(&hash).bind(saved.id).bind(size as i64).execute(&store.pool).await?;
    Ok(BlobRef { blob: hash, size })
}

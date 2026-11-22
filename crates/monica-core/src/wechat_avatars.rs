//! Download only avatar URLs observed in a WeChat contact snapshot.
use super::{Asset, MediaStore};
use anyhow::{Context, Result, ensure};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

fn avatar_url(raw: &str) -> Option<url::Url> {
    let mut url = url::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(
            url.host_str()?,
            "wx.qlogo.cn"
                | "thirdwx.qlogo.cn"
                | "wx.qpic.cn"
                | "wework.qpic.cn"
                | "mmhead.c2c.wechat.com"
                | "mmhead.hk.wechat.com"
                | "mmhead.qpic.cn"
        )
    {
        return None;
    }
    url.set_scheme("https").ok()?;
    Some(url)
}

async fn download(
    client: &reqwest::Client,
    url: url::Url,
    root: &Path,
) -> Result<tempfile::NamedTempFile> {
    let mut response = client.get(url).send().await?;
    ensure!(response.status().is_success(), "avatar CDN unavailable");
    let temporary = tempfile::NamedTempFile::new_in(root)?;
    let mut file = tokio::fs::File::from_std(temporary.reopen()?);
    let mut length = 0;
    while let Some(chunk) = response.chunk().await? {
        length += chunk.len();
        ensure!(length <= 4 * 1024 * 1024, "avatar exceeds size limit");
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    ensure!(length > 0, "empty avatar");
    Ok(temporary)
}

async fn fetch(store: &MediaStore, urls: &[String], account: Uuid) -> Result<Asset> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    tokio::fs::create_dir_all(&store.root).await?;
    for url in urls.iter().filter_map(|v| avatar_url(v)) {
        let file = {
            let _permit = store
                .resources
                .acquire(crate::resources::TaskResources {
                    network: 1,
                    disk: 1,
                    ..Default::default()
                })
                .await?;
            download(&client, url, &store.root).await
        };
        if let Ok(file) = file {
            let kind = infer::get_from_path(file.path())?;
            if kind.is_some_and(|v| {
                matches!(
                    v.mime_type(),
                    "image/jpeg" | "image/png" | "image/webp" | "image/gif"
                )
            }) && let Ok(asset) = store
                .ingest_path(
                    file.path(),
                    None,
                    "wechat",
                    Some(&account.to_string()),
                    None,
                    None,
                )
                .await
                && asset.media_type.starts_with("image/")
            {
                return Ok(asset);
            }
        }
    }
    anyhow::bail!("no usable WeChat avatar")
}

pub async fn refresh_wechat_avatar(store: &MediaStore) -> Result<()> {
    // The due time doubles as a crash-recoverable lease. A new contact
    // observation invalidates the token even while the old download is in flight.
    let request:Option<(Uuid,Uuid,Vec<String>)> = sqlx::query_as(
        "WITH due AS (SELECT q.account_id FROM crm.wechat_avatar_requests q
         JOIN crm.source_account_links l USING(account_id) WHERE next_attempt<=now()
         ORDER BY next_attempt,q.account_id FOR UPDATE OF q SKIP LOCKED LIMIT 1)
         UPDATE crm.wechat_avatar_requests q SET next_attempt=now()+interval '2 minutes',token=gen_random_uuid()
         FROM due WHERE due.account_id=q.account_id RETURNING q.account_id,q.token,q.urls"
    ).fetch_optional(&store.pool).await?;
    let Some((account, token, urls)) = request else {
        return Ok(());
    };
    let result = fetch(store, &urls, account).await;
    finish(&store.pool, account, token, result).await
}

async fn finish(
    pool: &sqlx::PgPool,
    account: Uuid,
    token: Uuid,
    result: Result<Asset>,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let valid:Option<Uuid> = sqlx::query_scalar("SELECT account_id FROM crm.wechat_avatar_requests WHERE account_id=$1 AND token=$2 FOR UPDATE")
        .bind(account).bind(token).fetch_optional(&mut *tx).await?;
    if valid.is_none() {
        return Ok(());
    }
    match result {
        Ok(asset) => {
            // Record ownership of a migrated source-selected avatar while its
            // old link still exists. Manual selections remain authoritative.
            sqlx::query("INSERT INTO crm.source_avatar_projections(profile_id,media_id,source_account_id)
                SELECT p.id,p.avatar_media_id,$1 FROM crm.source_account_links l JOIN crm.profiles p ON p.id=l.profile_id
                WHERE l.account_id=$1 AND EXISTS(SELECT 1 FROM crm.media_links m WHERE m.media_id=p.avatar_media_id
                    AND m.entity_type='source_account' AND m.entity_id=$1 AND m.role LIKE 'wechat_avatar%')
                ON CONFLICT(profile_id) DO NOTHING").bind(account).execute(&mut *tx).await?;
            sqlx::query(
                "UPDATE crm.media_assets SET verified_at=now() WHERE id=$1 AND verified_at IS NULL",
            )
            .bind(asset.id)
            .execute(&mut *tx)
            .await?;
            super::set_slot_tx(
                &mut tx,
                "source_account",
                account,
                "wechat_avatar_small",
                asset.id,
                Some(account),
            )
            .await?;
            sqlx::query("UPDATE crm.wechat_avatar_requests SET next_attempt=NULL,attempts=0 WHERE account_id=$1")
                .bind(account).execute(&mut *tx).await?;
        }
        Err(_) => {
            sqlx::query("UPDATE crm.wechat_avatar_requests SET attempts=attempts+1,next_attempt=now()+interval '1 minute'*(1<<least(attempts,10)) WHERE account_id=$1")
                .bind(account).execute(&mut *tx).await.context("reschedule WeChat avatar")?;
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{ResourceBudget, ResourceLimits};

    #[sqlx::test(migrations = "../../migrations")]
    async fn only_current_photo_reaches_profile(pool: sqlx::PgPool) {
        let root = tempfile::tempdir().unwrap();
        let store = MediaStore::new(
            pool.clone(),
            root.path().into(),
            ResourceBudget::new(ResourceLimits::nas()).unwrap(),
        )
        .unwrap();
        let account = Uuid::new_v4();
        async fn observe(pool: &sqlx::PgPool, account: Uuid, url: &str) -> Uuid {
            sqlx::query("SELECT crm.wechat_directory_contact('wxid_peer','Peer',$1,NULL,$2)")
                .bind(serde_json::json!({"big_head_url":url}))
                .bind(account)
                .execute(pool)
                .await
                .unwrap();
            sqlx::query_scalar("SELECT token FROM crm.wechat_avatar_requests WHERE account_id=$1")
                .bind(account)
                .fetch_one(pool)
                .await
                .unwrap()
        }
        let old = observe(&pool, account, "https://wx.qlogo.cn/old").await;
        let mut shown = None;
        for color in [1, 2] {
            let file = root.path().join("fixture.png");
            image::RgbImage::from_pixel(2, 2, image::Rgb([color, 0, 0]))
                .save(&file)
                .unwrap();
            let (sha256, size) = crate::media::hash_file(&file).await.unwrap();
            let asset = Asset {
                id: Uuid::new_v4(),
                sha256,
                media_type: "image/png".into(),
                byte_length: size as i64,
                object_path: String::new(),
                file_name: None,
                width: Some(2),
                height: Some(2),
                duration_ms: None,
            };
            let saved = store.publish(&file, asset, "wechat", None).await.unwrap();
            let token = if color == 1 {
                old
            } else {
                observe(&pool, account, "https://wx.qlogo.cn/new").await
            };
            finish(&pool, account, token, Ok(saved.clone()))
                .await
                .unwrap();
            if color == 2 {
                finish(&pool, account, old, Err(anyhow::anyhow!("stale download")))
                    .await
                    .unwrap();
            }
            crate::media::refresh_profiles(&pool).await.unwrap();
            let current:Uuid=sqlx::query_scalar("SELECT p.avatar_media_id FROM crm.source_account_links l JOIN crm.profiles p ON p.id=l.profile_id WHERE l.account_id=$1").bind(account).fetch_one(&pool).await.unwrap();
            assert_eq!(current, saved.id);
            assert_ne!(shown, Some(current));
            assert_eq!(
                std::fs::read(root.path().join(&saved.object_path)).unwrap(),
                std::fs::read(file).unwrap()
            );
            shown = Some(current);
        }
    }
}

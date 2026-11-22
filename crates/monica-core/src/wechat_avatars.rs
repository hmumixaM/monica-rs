//! Download only avatar URLs observed in a WeChat contact snapshot.
use super::{Asset, MediaStore};
use anyhow::{Result, ensure};
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
         WHERE next_attempt<=now() AND
         ((q.native AND EXISTS(SELECT 1 FROM crm.connection_evidence_people ep WHERE ep.evidence_id=q.account_id))
          OR (NOT q.native AND EXISTS(SELECT 1 FROM crm.source_account_links l WHERE l.account_id=q.account_id)))
         ORDER BY next_attempt,q.account_id FOR UPDATE OF q SKIP LOCKED LIMIT 1)
         UPDATE crm.wechat_avatar_requests q SET next_attempt=now()+interval '2 minutes',token=gen_random_uuid()
         FROM due WHERE due.account_id=q.account_id RETURNING q.account_id,q.token,q.urls"
    ).fetch_optional(&store.pool).await?;
    let Some((account, token, urls)) = request else {
        return Ok(());
    };
    let result = if urls.is_empty() {
        Ok(None)
    } else {
        fetch(store, &urls, account).await.map(Some)
    };
    finish(&store.pool, account, token, result).await
}

async fn finish(
    pool: &sqlx::PgPool,
    account: Uuid,
    token: Uuid,
    result: Result<Option<Asset>>,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    if let Some(cid) = sqlx::query_scalar::<_, Uuid>(
        "SELECT connection_id FROM crm.connection_evidence WHERE id=$1",
    )
    .bind(account)
    .fetch_optional(&mut *tx)
    .await?
    {
        crate::connections::lock_connection(&mut tx, cid).await?;
    }
    let request:Option<(bool,chrono::DateTime<chrono::Utc>)>=sqlx::query_as(
        "SELECT native,observed_at FROM crm.wechat_avatar_requests WHERE account_id=$1 AND token=$2 FOR UPDATE")
        .bind(account).bind(token).fetch_optional(&mut *tx).await?;
    let Some((native, observed)) = request else {
        return Ok(());
    };
    let Ok(asset) = result else {
        sqlx::query("UPDATE crm.wechat_avatar_requests SET attempts=attempts+1,next_attempt=now()+interval '1 minute'*(1<<least(attempts,10)) WHERE account_id=$1")
            .bind(account).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(());
    };
    let (entity, role) = if native {
        ("wechat_native_contact", "wechat_avatar")
    } else {
        ("source_account", "wechat_avatar_small")
    };
    // A clear affects all WeChat sizes. Serialize with conventional manifest
    // writers using their existing per-role locks, in one deterministic order.
    let roles: &[&str] = if native {
        &["wechat_avatar"]
    } else {
        &["wechat_avatar", "wechat_avatar_big", "wechat_avatar_small"]
    };
    for locked_role in roles {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("media-slot:{entity}:{account}:{locked_role}"))
            .execute(&mut *tx)
            .await?;
    }
    if !native && sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM crm.media_slots WHERE entity_type='source_account' AND entity_id=$1 AND role=ANY($2) AND source_observed_at>$3)")
        .bind(account).bind(roles).bind(observed).fetch_one(&mut *tx).await? {
        sqlx::query("UPDATE crm.wechat_avatar_requests SET next_attempt=NULL WHERE account_id=$1")
            .bind(account).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(());
    }
    if native {
        sqlx::query("SELECT id FROM crm.connection_evidence WHERE id=$1 FOR UPDATE")
            .bind(account)
            .fetch_one(&mut *tx)
            .await?;
    } else {
        sqlx::query("INSERT INTO crm.source_avatar_projections(profile_id,media_id,source_account_id)
            SELECT p.id,p.avatar_media_id,$1 FROM crm.source_account_links l JOIN crm.profiles p ON p.id=l.profile_id
            WHERE l.account_id=$1 AND EXISTS(SELECT 1 FROM crm.media_links m WHERE m.media_id=p.avatar_media_id
            AND ((m.entity_type='source_account' AND m.entity_id=$1) OR (m.entity_type='profile' AND m.entity_id=p.id AND m.source_account_id=$1))
            AND m.role IN('wechat_avatar','wechat_avatar_small','wechat_avatar_big'))
            ON CONFLICT(profile_id) DO NOTHING").bind(account).execute(&mut *tx).await?;
    }
    if let Some(asset) = asset {
        if native {
            sqlx::query("INSERT INTO crm.avatar_observations(evidence_id,role,previous_media_id,media_id,observed_at,evidence_key)
                SELECT $1,'wechat_avatar',prior.media_id,$2,$3,$4 FROM (SELECT (SELECT media_id FROM crm.media_slots WHERE entity_type='wechat_native_contact' AND entity_id=$1 AND role='wechat_avatar') media_id) prior
                WHERE prior.media_id IS DISTINCT FROM $2 ON CONFLICT(evidence_key) DO NOTHING")
                .bind(account).bind(asset.id).bind(observed).bind(format!("wechat-native:{token}")).execute(&mut *tx).await?;
        }

        sqlx::query(
            "UPDATE crm.media_assets SET verified_at=now() WHERE id=$1 AND verified_at IS NULL",
        )
        .bind(asset.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO crm.media_slots AS s(entity_type,entity_id,role,media_id,source_observed_at) VALUES($1,$2,$3,$4,$5)
            ON CONFLICT(entity_type,entity_id,role) DO UPDATE SET media_id=excluded.media_id,generation=s.generation+CASE WHEN s.media_id<>excluded.media_id THEN 1 ELSE 0 END,
            source_batch_id=NULL,source_ordinal=NULL,source_observed_at=excluded.source_observed_at,updated_at=now()
            WHERE s.media_id<>excluded.media_id OR s.source_observed_at IS DISTINCT FROM excluded.source_observed_at").bind(entity).bind(account).bind(role).bind(asset.id).bind(observed).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM crm.media_links WHERE entity_type=$1 AND entity_id=$2 AND role=$3 AND media_id<>$4")
            .bind(entity).bind(account).bind(role).bind(asset.id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO crm.media_links(entity_type,entity_id,role,media_id,source_account_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(entity).bind(account).bind(role).bind(asset.id).bind((!native).then_some(account)).execute(&mut *tx).await?;
    } else {
        sqlx::query("INSERT INTO crm.avatar_observations(account_id,evidence_id,role,previous_media_id,media_id,observed_at,evidence_key)
            SELECT $1,$2,'wechat_avatar',prior.media_id,NULL,$3,$4 FROM (
                SELECT media_id FROM (
                  SELECT media_id,role,1 source,source_observed_at observed FROM crm.media_slots WHERE entity_type=$5 AND entity_id=$6
                  UNION ALL SELECT media_id,role,2,created_at FROM crm.media_links WHERE (entity_type=$5 AND entity_id=$6) OR (entity_type='profile' AND source_account_id=$1)
                ) candidates JOIN crm.media_assets a ON a.id=candidates.media_id
                WHERE role IN('wechat_avatar_small','wechat_avatar','wechat_avatar_big')
                ORDER BY source,(a.verified_at IS NULL),observed DESC NULLS LAST,CASE role WHEN 'wechat_avatar_small' THEN 0 WHEN 'wechat_avatar' THEN 1 ELSE 2 END,media_id LIMIT 1
            ) prior ON CONFLICT(evidence_key) DO NOTHING")
            .bind((!native).then_some(account)).bind(native.then_some(account)).bind(observed)
            .bind(format!("wechat-clear:{token}")).bind(entity).bind(account).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM crm.media_slots WHERE entity_type=$1 AND entity_id=$2 AND role IN('wechat_avatar','wechat_avatar_small','wechat_avatar_big')")
            .bind(entity).bind(account).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM crm.media_links WHERE ((entity_type=$1 AND entity_id=$2) OR (entity_type='profile' AND source_account_id=$3)) AND role IN('wechat_avatar','wechat_avatar_small','wechat_avatar_big')")
            .bind(entity).bind(account).bind((!native).then_some(account)).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO crm.memory_profile_refresh SELECT profile_id FROM crm.connection_evidence_people WHERE $2 AND evidence_id=$1 AND role='contact' UNION SELECT profile_id FROM crm.source_account_links WHERE NOT $2 AND account_id=$1 ON CONFLICT DO NOTHING")
        .bind(account).bind(native).execute(&mut *tx).await?;
    if native {
        let owners:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM crm.profiles WHERE id IN(SELECT profile_id FROM crm.connection_evidence_people WHERE evidence_id=$1) ORDER BY id FOR UPDATE")
            .bind(account).fetch_all(&mut *tx).await?;
        for profile in owners {
            crate::linking::rebuild_avatar(&mut tx, profile).await?;
        }
    } else {
        sqlx::query("INSERT INTO crm.media_profile_refresh(account_id) VALUES($1) ON CONFLICT(account_id) DO UPDATE SET generation=crm.media_profile_refresh.generation+1")
            .bind(account).execute(&mut *tx).await?;
    }
    sqlx::query(
        "UPDATE crm.wechat_avatar_requests SET next_attempt=NULL,attempts=0 WHERE account_id=$1",
    )
    .bind(account)
    .execute(&mut *tx)
    .await?;
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
        let mut old_asset = None;
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
            finish(&pool, account, token, Ok(Some(saved.clone())))
                .await
                .unwrap();
            if color == 1 {
                old_asset = Some(saved.clone());
            } else {
                finish(&pool, account, old, Ok(old_asset.clone()))
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
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM crm.avatar_observations WHERE account_id=$1 AND observed_at IS NOT NULL")
            .bind(account).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 2);
        // A more recent observation of the same bytes must outrank a different
        // size that an intervening manifest supplied.
        let saved = store.asset(shown.unwrap()).await.unwrap();
        crate::media::set_slot(
            &pool,
            "source_account",
            account,
            "wechat_avatar_big",
            old_asset.as_ref().unwrap().id,
            Some(account),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE crm.media_slots SET source_observed_at=clock_timestamp() WHERE entity_id=$1 AND role='wechat_avatar_big'")
            .bind(account).execute(&pool).await.unwrap();
        let same_photo = observe(&pool, account, "https://wx.qlogo.cn/rotated").await;
        finish(&pool, account, same_photo, Ok(Some(saved)))
            .await
            .unwrap();
        crate::media::refresh_profiles(&pool).await.unwrap();
        let selected:Option<Uuid>=sqlx::query_scalar("SELECT p.avatar_media_id FROM crm.source_account_links l JOIN crm.profiles p ON p.id=l.profile_id WHERE l.account_id=$1")
            .bind(account).fetch_one(&pool).await.unwrap();
        assert_eq!(selected, shown);
        sqlx::query("UPDATE crm.media_slots SET source_observed_at=clock_timestamp() WHERE entity_id=$1 AND role='wechat_avatar_big'")
            .bind(account).execute(&pool).await.unwrap();
        crate::media::refresh_profiles(&pool).await.unwrap();
        let before_clear:Uuid=sqlx::query_scalar("SELECT p.avatar_media_id FROM crm.source_account_links l JOIN crm.profiles p ON p.id=l.profile_id WHERE l.account_id=$1")
            .bind(account).fetch_one(&pool).await.unwrap();
        assert_eq!(before_clear, old_asset.as_ref().unwrap().id);
        observe(&pool, account, "").await;
        refresh_wechat_avatar(&store).await.unwrap();
        crate::media::refresh_profiles(&pool).await.unwrap();
        let cleared:Option<Uuid>=sqlx::query_scalar("SELECT p.avatar_media_id FROM crm.source_account_links l JOIN crm.profiles p ON p.id=l.profile_id WHERE l.account_id=$1")
            .bind(account).fetch_one(&pool).await.unwrap();
        assert!(cleared.is_none());
        let prior:Uuid=sqlx::query_scalar("SELECT previous_media_id FROM crm.avatar_observations WHERE account_id=$1 AND media_id IS NULL")
            .bind(account).fetch_one(&pool).await.unwrap();
        assert_eq!(prior, before_clear);

        let delayed = observe(&pool, account, "https://wx.qlogo.cn/delayed").await;
        let newer = shown.unwrap();
        crate::media::set_slot(
            &pool,
            "source_account",
            account,
            "wechat_avatar_small",
            newer,
            Some(account),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE crm.media_slots SET source_observed_at=clock_timestamp()+interval '1 second' WHERE entity_id=$1 AND role='wechat_avatar_small'").bind(account).execute(&pool).await.unwrap();
        finish(&pool, account, delayed, Ok(old_asset))
            .await
            .unwrap();
        let kept:Uuid=sqlx::query_scalar("SELECT media_id FROM crm.media_slots WHERE entity_id=$1 AND role='wechat_avatar_small'")
            .bind(account).fetch_one(&pool).await.unwrap();
        assert_eq!(kept, newer);
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn native_photo_and_roster_follow_connection_owner(pool: sqlx::PgPool) {
        use crate::{Provider, connections};
        use serde_json::json;
        let provider = Provider::Wechat;
        let cid = connections::connection_id(provider, "fixture");
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method) VALUES($1,'wechat','fixture','Fixture','local')")
            .bind(cid).execute(&pool).await.unwrap();
        let mut contact = json!({"kind":"contact","external_id":"contact:peer","directory_version":1,"fields":{"alias":"original","big_head_url":"https://wx.qlogo.cn/same"},"avatar_checksum":"old","participants":[{"id":"peer","name":"Friend","role":"contact"}]});
        let mut member = json!({"kind":"group_membership","external_id":"membership:room:peer","participants":[{"id":"r@chatroom","role":"room"},{"id":"peer","name":"Friend","role":"member"}]});
        let eid = connections::evidence_id(cid, "contact:peer");
        let mut previous = None;
        for index in 0..2 {
            if index == 1 {
                contact["avatar_checksum"] = json!("changed");
                contact["fields"]["alias"] = json!("current");
                // The roster is scanned before contact after a nickname change.
                contact["participants"][0]["name"] = json!("Renamed");
                member["participants"][1]["name"] = json!("Renamed");
                member["participants"][0]["name"] = json!("Renamed room");
            }
            let mut tx = pool.begin().await.unwrap();
            connections::commit_page(
                &mut tx,
                provider,
                cid,
                "chatlog",
                &[member.clone(), contact.clone()],
                &json!({"position":index}),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            connections::deliver(&pool, provider, cid, 10)
                .await
                .unwrap();
            let token = sqlx::query_scalar(
                "SELECT token FROM crm.wechat_avatar_requests WHERE account_id=$1",
            )
            .bind(eid)
            .fetch_one(&pool)
            .await
            .unwrap();
            let asset = Asset {
                id: Uuid::new_v4(),
                sha256: format!("{index:064x}"),
                media_type: "image/png".into(),
                byte_length: 12,
                object_path: format!("fixture/{index}"),
                file_name: None,
                width: Some(2),
                height: Some(2),
                duration_ms: None,
            };
            sqlx::query("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) VALUES($1,$2,'image/png',12,$3)")
                .bind(asset.id).bind(&asset.sha256).bind(&asset.object_path).execute(&pool).await.unwrap();
            finish(&pool, eid, token, Ok(Some(asset.clone())))
                .await
                .unwrap();
            let row:(Uuid,Option<Uuid>)=sqlx::query_as("SELECT p.avatar_media_id,o.previous_media_id FROM crm.current_group_memberships m JOIN crm.profiles p ON p.id=m.person_profile_id JOIN crm.connection_evidence_people ep ON ep.profile_id=p.id AND ep.role='contact' JOIN crm.avatar_observations o ON o.evidence_id=ep.evidence_id WHERE o.media_id=$1")
                .bind(asset.id).fetch_one(&pool).await.unwrap();
            assert_eq!(row, (asset.id, previous));
            previous = Some(asset.id);
        }
        let native_owner: Uuid = sqlx::query_scalar(
            "SELECT profile_id FROM crm.connection_evidence_people WHERE evidence_id=$1",
        )
        .bind(eid)
        .fetch_one(&pool)
        .await
        .unwrap();
        let title: String = sqlx::query_scalar("SELECT display_name FROM crm.profiles WHERE id=$1")
            .bind(native_owner)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(title, "Renamed");
        let group_title:String=sqlx::query_scalar("SELECT display_name FROM crm.profiles WHERE id=(SELECT group_profile_id FROM crm.current_group_memberships WHERE person_profile_id=$1)")
            .bind(native_owner).fetch_one(&pool).await.unwrap();
        assert_eq!(group_title, "Renamed room");
        let source_account = Uuid::new_v4();
        sqlx::query("SELECT crm.wechat_directory_contact('peer','Friend','{}',NULL,$1)")
            .bind(source_account)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Uuid>(
                "SELECT profile_id FROM crm.source_account_links WHERE account_id=$1"
            )
            .bind(source_account)
            .fetch_one(&pool)
            .await
            .unwrap(),
            native_owner
        );
        sqlx::query("UPDATE crm.connection_identity_links SET blocked=true,profile_id=NULL WHERE connection_id=$1 AND external_id='peer'").bind(cid).execute(&pool).await.unwrap();
        // Reproject the same records, as the connection-person unlink endpoint does.
        sqlx::query("INSERT INTO crm.connection_relink(evidence_id) SELECT id FROM crm.connection_evidence WHERE connection_id=$1 ON CONFLICT DO NOTHING").bind(cid).execute(&pool).await.unwrap();
        connections::relink_pending(&pool, 10).await.unwrap();
        let visible: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM crm.profiles WHERE avatar_media_id IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(visible, 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.current_group_memberships")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }
}

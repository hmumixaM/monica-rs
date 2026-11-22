//! Account ownership changes use deterministic scoped locks, never a global lock.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LinkChange {
    pub account_id: Uuid,
    pub profile_id: Option<Uuid>,
    #[serde(default)]
    pub expected_profile_id: Option<Uuid>,
}

pub async fn rebuild_profile(tx: &mut Transaction<'_, Postgres>, profile: Uuid) -> Result<()> {
    // Manual values (including explicit null deletions) remain authoritative.
    sqlx::query("DELETE FROM crm.field_current c USING crm.field_revisions r WHERE c.revision_id=r.id AND c.profile_id=$1 AND r.source_account_id IS NOT NULL AND r.source_type<>'manual'")
        .bind(profile).execute(&mut **tx).await?;
    let rows=sqlx::query("SELECT DISTINCT ON(f.field_key) f.*,a.provider,CASE WHEN protected.source_type IN ('manual','monica') THEN protected.content_hash END AS protected_hash FROM crm.source_fields f JOIN crm.source_account_links l ON l.account_id=f.account_id JOIN crm.source_accounts a ON a.id=f.account_id LEFT JOIN crm.field_current c ON c.profile_id=l.profile_id AND c.field_key=f.field_key LEFT JOIN crm.field_revisions protected ON protected.id=c.revision_id WHERE l.profile_id=$1 ORDER BY f.field_key,f.observed_at DESC,f.account_id")
        .bind(profile).fetch_all(&mut **tx).await?;
    let values = rows
        .iter()
        .map(|r| r.get::<Value, _>("value"))
        .collect::<Vec<_>>();
    let contents = crate::content::register_many(tx, "field_value", &values).await?;
    for (row, content) in rows.into_iter().zip(contents) {
        let key: String = row.get("field_key");
        let account: Uuid = row.get("account_id");
        let value: Value = row.get("value");
        let hash = content.hash;
        let is_conflict = row
            .get::<Option<String>, _>("protected_hash")
            .is_some_and(|current| current != hash);
        let revision=sqlx::query("SELECT id,content_hash,value FROM crm.field_revisions WHERE profile_id=$1 AND source_account_id=$2 AND field_key=$3 ORDER BY revision_seq DESC LIMIT 1")
            .bind(profile).bind(account).bind(&key).fetch_optional(&mut **tx).await?;
        let id = if let Some(revision) = revision.filter(|r| {
            r.get::<String, _>("content_hash") == hash && r.get::<Value, _>("value") == value
        }) {
            revision.get("id")
        } else {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,source_account_id,content_object_id,is_conflict) VALUES($1,$2,$3,$4,$5,$6,'import',$7,$8,$9)")
                .bind(id).bind(profile).bind(&key).bind(value).bind(hash).bind(row.get::<String,_>("provider")).bind(account).bind(content.id).bind(is_conflict).execute(&mut **tx).await?;
            id
        };
        sqlx::query("INSERT INTO crm.field_current(profile_id,field_key,revision_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(profile).bind(key).bind(id).execute(&mut **tx).await?;
    }
    rebuild_avatar(tx, profile).await?;
    sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
        .bind(profile)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Called after a durable media slot commit. Same account -> profile lock order
/// as link changes; only the avatar projection is touched by this recovery job.
pub async fn refresh_account(pool: &PgPool, account: Uuid) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='2s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT id FROM crm.source_accounts WHERE id=$1 FOR SHARE")
        .bind(account)
        .fetch_one(&mut *tx)
        .await?;
    let profile: Option<Uuid> =
        sqlx::query_scalar("SELECT profile_id FROM crm.source_account_links WHERE account_id=$1")
            .bind(account)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(profile) = profile {
        sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 FOR UPDATE")
            .bind(profile)
            .fetch_one(&mut *tx)
            .await?;
        rebuild_avatar(&mut tx, profile).await?;
    }
    tx.commit().await?;
    Ok(())
}
async fn rebuild_avatar(tx: &mut Transaction<'_, Postgres>, profile: Uuid) -> Result<()> {
    let current: Option<Uuid> =
        sqlx::query_scalar("SELECT avatar_media_id FROM crm.profiles WHERE id=$1")
            .bind(profile)
            .fetch_one(&mut **tx)
            .await?;
    let previous: Option<Uuid> = sqlx::query_scalar(
        "SELECT media_id FROM crm.source_avatar_projections WHERE profile_id=$1",
    )
    .bind(profile)
    .fetch_optional(&mut **tx)
    .await?;
    let manual:Option<Uuid>=sqlx::query_scalar("SELECT media_id FROM crm.media_slots WHERE entity_type='profile' AND entity_id=$1 AND role='avatar'").bind(profile).fetch_optional(&mut **tx).await?;
    let proven_source: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.media_links m WHERE m.media_id=$2 AND m.role IN ('wechat_avatar','wechat_avatar_small','wechat_avatar_big','instagram_avatar','linkedin_avatar') AND ((m.entity_type='profile' AND m.entity_id=$1 AND m.source_account_id IS NOT NULL) OR (m.entity_type='source_account' AND EXISTS(SELECT 1 FROM crm.source_account_links l WHERE l.account_id=m.entity_id AND l.profile_id=$1))))")
        .bind(profile).bind(current).fetch_one(&mut **tx).await?;
    if manual.is_some() || (current != previous && !proven_source) {
        return Ok(());
    }
    // Any avatar a linked account has is better than initials. Archived
    // avatars were copied without the import-time byte check, so they count
    // too; a checked one is preferred when the person has both.
    let selected:Option<(Uuid,Uuid)>=sqlx::query_as("SELECT m.media_id,l.account_id FROM crm.source_account_links l JOIN crm.media_slots m ON m.entity_type='source_account' AND m.entity_id=l.account_id JOIN crm.media_assets a ON a.id=m.media_id WHERE l.profile_id=$1 AND m.role IN ('wechat_avatar_small','wechat_avatar','wechat_avatar_big','instagram_avatar','linkedin_avatar') ORDER BY (a.verified_at IS NULL),CASE m.role WHEN 'wechat_avatar_small' THEN 0 WHEN 'wechat_avatar' THEN 1 WHEN 'wechat_avatar_big' THEN 2 WHEN 'instagram_avatar' THEN 3 ELSE 4 END,l.account_id LIMIT 1").bind(profile).fetch_optional(&mut **tx).await?;
    if selected.is_none() && current.is_some() {
        let still_owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.source_account_links l JOIN crm.media_links m ON m.entity_type='source_account' AND m.entity_id=l.account_id WHERE l.profile_id=$1 AND m.media_id=$2)")
            .bind(profile).bind(current).fetch_one(&mut **tx).await?;
        if still_owned {
            return Ok(());
        }
    }
    let media = selected.map(|(m, _)| m);
    sqlx::query("UPDATE crm.profiles SET avatar_media_id=$2,generation=generation+1 WHERE id=$1 AND avatar_media_id IS DISTINCT FROM $2").bind(profile).bind(media).execute(&mut **tx).await?;
    if let Some((media, account)) = selected {
        sqlx::query("INSERT INTO crm.source_avatar_projections(profile_id,media_id,source_account_id) VALUES($1,$2,$3) ON CONFLICT(profile_id) DO UPDATE SET media_id=excluded.media_id,source_account_id=excluded.source_account_id").bind(profile).bind(media).bind(account).execute(&mut **tx).await?;
    } else {
        sqlx::query("DELETE FROM crm.source_avatar_projections WHERE profile_id=$1")
            .bind(profile)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub async fn set_links(
    tx: &mut Transaction<'_, Postgres>,
    changes: &[LinkChange],
    actor: Option<Uuid>,
    evidence: &Value,
) -> Result<Vec<Value>> {
    ensure!(
        !changes.is_empty() && changes.len() <= 1000,
        "invalid number of links"
    );
    let ids = changes
        .iter()
        .map(|x| x.account_id)
        .collect::<BTreeSet<_>>();
    ensure!(
        ids.len() == changes.len(),
        "duplicate account in link batch"
    );
    let mut profiles = BTreeSet::new();
    for account in &ids {
        sqlx::query("SELECT id FROM crm.source_accounts WHERE id=$1 FOR UPDATE")
            .bind(account)
            .fetch_optional(&mut **tx)
            .await?
            .context("source account not found")?;
        let old: Option<Uuid> = sqlx::query_scalar(
            "SELECT profile_id FROM crm.source_account_links WHERE account_id=$1",
        )
        .bind(account)
        .fetch_optional(&mut **tx)
        .await?;
        let change = changes.iter().find(|x| x.account_id == *account).unwrap();
        ensure!(
            old == change.expected_profile_id,
            "account link changed; refresh before confirming"
        );
        if let Some(old) = old {
            profiles.insert(old);
        }
        if let Some(new) = change.profile_id {
            profiles.insert(new);
        }
    }
    for profile in &profiles {
        let archived: bool = sqlx::query_scalar(
            "SELECT archived_at IS NOT NULL FROM crm.profiles WHERE id=$1 FOR UPDATE",
        )
        .bind(profile)
        .fetch_optional(&mut **tx)
        .await?
        .context("profile not found")?;
        if archived {
            let restored=sqlx::query("DELETE FROM crm.profile_redirects WHERE old_profile_id=$1 AND evidence->>'kind'='empty-after-account-move'").bind(profile).execute(&mut **tx).await?.rows_affected()>0;
            ensure!(restored, "profile is archived");
            sqlx::query(
                "UPDATE crm.profiles SET archived_at=NULL,generation=generation+1 WHERE id=$1",
            )
            .bind(profile)
            .execute(&mut **tx)
            .await?;
            sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) VALUES($1,'profile.auto_restored','profile',$2)").bind(actor).bind(profile).execute(&mut **tx).await?;
        }
    }
    let blocked = link_blockers(tx, &ids.iter().copied().collect::<Vec<_>>()).await?;
    for change in changes {
        if let Some(reason) = blocked.get(&change.account_id) {
            ensure!(
                reason != "quarantined"
                    && (change.profile_id.is_none() || evidence["kind"] == "explicit-person-merge"),
                "source account ownership blocked: {reason}"
            );
        }
    }
    let mut output = Vec::new();
    for account in ids {
        let change = changes.iter().find(|x| x.account_id == account).unwrap();
        if change.profile_id == change.expected_profile_id {
            output
                .push(json!({"account_id":account,"profile_id":change.profile_id,"changed":false}));
            continue;
        }
        if let Some(profile) = change.profile_id {
            let compatible:bool=sqlx::query_scalar("SELECT a.object_kind=p.profile_type FROM crm.source_accounts a CROSS JOIN crm.profiles p WHERE a.id=$1 AND p.id=$2")
                .bind(account).bind(profile).fetch_one(&mut **tx).await?;
            ensure!(
                compatible,
                "people link to person accounts; groups link to group accounts"
            );
            sqlx::query("INSERT INTO crm.source_account_links(account_id,profile_id,linked_by) VALUES($1,$2,$3) ON CONFLICT(account_id) DO UPDATE SET profile_id=excluded.profile_id,generation=crm.source_account_links.generation+1,linked_by=excluded.linked_by,linked_at=now()")
                .bind(account).bind(profile).bind(actor).execute(&mut **tx).await?;
        } else {
            sqlx::query("DELETE FROM crm.source_account_links WHERE account_id=$1")
                .bind(account)
                .execute(&mut **tx)
                .await?;
        }
        sqlx::query("INSERT INTO crm.source_account_link_revisions(account_id,old_profile_id,new_profile_id,actor_user_id,evidence) VALUES($1,$2,$3,$4,$5)")
            .bind(account).bind(change.expected_profile_id).bind(change.profile_id).bind(actor).bind(evidence).execute(&mut **tx).await?;
        for table in ["timeline_events", "contact_events", "conversations"] {
            sqlx::query(&format!("UPDATE crm.{table} SET profile_id=$2 WHERE source_account_id=$1 AND profile_id IS DISTINCT FROM $2"))
                .bind(account).bind(change.profile_id).execute(&mut **tx).await?;
        }
        sqlx::query("UPDATE crm.identities SET profile_id=$2,status=CASE WHEN $2::uuid IS NULL THEN 'pending' ELSE 'active' END WHERE source_account_id=$1")
            .bind(account).bind(change.profile_id).execute(&mut **tx).await?;
        if change.profile_id.is_some() {
            sqlx::query("INSERT INTO crm.identities(id,profile_id,source_account_id,provider,external_id,username,profile_url,display_name,status) SELECT a.id,$2,a.id,CASE WHEN a.object_kind='group' THEN 'wechat_group' ELSE a.provider END,a.external_id,a.username,a.profile_url,a.display_name,'active' FROM crm.source_accounts a WHERE a.id=$1 AND NOT EXISTS(SELECT 1 FROM crm.identities i WHERE i.source_account_id=a.id) ON CONFLICT DO NOTHING").bind(account).bind(change.profile_id).execute(&mut **tx).await?;
        }
        sqlx::query(
            "UPDATE crm.social_interactions i SET author_profile_id=$2,author_account_id=$1 FROM crm.source_accounts a WHERE a.id=$1 AND (i.author_account_id=$1 OR (i.provider=a.provider AND i.author_external_id=a.external_id))",
        )
        .bind(account)
        .bind(change.profile_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'account.link.changed','source_account',$2,$3)")
            .bind(actor).bind(account).bind(json!({"old":change.expected_profile_id,"new":change.profile_id})).execute(&mut **tx).await?;
        crate::ai::memory::legacy::queue_account(
            tx,
            account,
            &[change.expected_profile_id, change.profile_id]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
        )
        .await?;
        output.push(json!({"account_id":account,"profile_id":change.profile_id,"changed":true}));
    }
    if output.iter().all(|r| r["changed"] == false) {
        return Ok(output);
    }
    // Reserve the union before rebuilding multiple profiles: reversed link batches
    // must never acquire shared content addresses in opposite profile order.
    let values: Vec<Value> = sqlx::query_scalar("SELECT f.value FROM crm.source_fields f JOIN crm.source_account_links l ON l.account_id=f.account_id WHERE l.profile_id=ANY($1)")
        .bind(profiles.iter().copied().collect::<Vec<_>>()).fetch_all(&mut **tx).await?;
    crate::content::register_many(tx, "field_value", &values).await?;
    for profile in &profiles {
        rebuild_profile(tx, *profile).await?;
    }
    if evidence["kind"] != "explicit-person-merge" {
        reconcile_profiles(tx, &profiles, actor).await?;
    }
    Ok(output)
}

pub async fn set_link(pool: &PgPool, change: LinkChange, actor: Option<Uuid>) -> Result<Value> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='3s'")
        .execute(&mut *tx)
        .await?;
    let result = set_links(
        &mut tx,
        &[change],
        actor,
        &json!({"kind":"manual-confirmation"}),
    )
    .await?;
    tx.commit().await?;
    Ok(result.into_iter().next().unwrap())
}

/// Inspect independent CRM facts before retiring an empty account-move origin.
/// Source facts are never deleted, moved, or rewritten by retirement.
pub async fn retirement_candidate(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value> {
    let row = sqlx::query(
        "SELECT profile_type,display_name,summary,avatar_media_id FROM crm.profiles WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    let mut blockers = BTreeSet::new();
    if row.get::<String, _>("profile_type") != "person" {
        blockers.insert("not-person");
    }
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM crm.source_account_links WHERE profile_id=$1)",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?
    {
        blockers.insert("linked-accounts");
    }
    if !blockers.is_empty() {
        return Ok(json!({"profile_id":id,"target_id":null,"accounts":[],"blockers":blockers}));
    }
    let accounts=sqlx::query("SELECT DISTINCT r.account_id,l.profile_id,p.archived_at IS NOT NULL AS archived FROM crm.source_account_link_revisions r LEFT JOIN crm.source_account_links l ON l.account_id=r.account_id LEFT JOIN crm.profiles p ON p.id=l.profile_id WHERE r.old_profile_id=$1 AND r.old_profile_id IS DISTINCT FROM r.new_profile_id").bind(id).fetch_all(&mut **tx).await?;
    let targets = accounts
        .iter()
        .filter(|a| !a.get::<bool, _>("archived"))
        .filter_map(|a| a.get::<Option<Uuid>, _>("profile_id"))
        .collect::<BTreeSet<_>>();
    let target = if accounts.is_empty()
        || targets.len() != 1
        || targets.contains(&id)
        || accounts.iter().any(|a| {
            a.get::<Option<Uuid>, _>("profile_id").is_none() || a.get::<bool, _>("archived")
        }) {
        blockers.insert("no-unique-current-target");
        None
    } else {
        targets.first().copied()
    };
    if row
        .get::<Option<String>, _>("summary")
        .is_some_and(|s| !s.trim().is_empty())
        || row.get::<Option<Uuid>, _>("avatar_media_id").is_some()
    {
        blockers.insert("summary-or-avatar");
    }
    for (reason, guard) in [
        (
            "manual-profile",
            "SELECT 1 FROM crm.audit_events WHERE entity_id=$1 AND entity_type='profile' AND action NOT IN ('profile.auto_archived','profile.auto_restored')",
        ),
        (
            "manual-history",
            "SELECT 1 FROM crm.field_revisions WHERE profile_id=$1 AND source_type='manual'",
        ),
        (
            "identity",
            "SELECT 1 FROM crm.identities WHERE profile_id=$1 AND provider<>'monica' AND status NOT IN ('invalid','merged')",
        ),
        (
            "structured-data",
            "SELECT 1 FROM crm.profile_structured_current WHERE profile_id=$1",
        ),
        (
            "dates",
            "SELECT 1 FROM crm.profile_dates WHERE profile_id=$1",
        ),
        (
            "activities",
            "SELECT 1 FROM crm.activity_participants WHERE profile_id=$1",
        ),
        (
            "faces",
            "SELECT 1 FROM crm.faces WHERE profile_id=$1 AND status='confirmed'",
        ),
        (
            "media",
            "SELECT 1 FROM crm.media_links WHERE entity_type='profile' AND entity_id=$1",
        ),
        (
            "relationships",
            "SELECT 1 FROM crm.presented_relationship_edges WHERE from_profile_id=$1 OR to_profile_id=$1",
        ),
        (
            "groups",
            "SELECT 1 FROM crm.group_memberships WHERE person_profile_id=$1 OR group_profile_id=$1",
        ),
        (
            "tags",
            "SELECT 1 FROM crm.tag_memberships WHERE profile_id=$1",
        ),
        (
            "posts",
            "SELECT 1 FROM crm.timeline_events WHERE profile_id=$1 AND event_type='post'",
        ),
        (
            "stories",
            "SELECT 1 FROM crm.timeline_events WHERE profile_id=$1 AND event_type='story'",
        ),
        (
            "conversations",
            "SELECT 1 FROM crm.conversations WHERE profile_id=$1",
        ),
    ] {
        let exists: bool = sqlx::query_scalar(&format!("SELECT EXISTS({guard})"))
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        if exists {
            blockers.insert(reason);
        }
    }
    let fields=sqlx::query("SELECT c.field_key,r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1").bind(id).fetch_all(&mut **tx).await?;
    for field in fields {
        let key: String = field.get("field_key");
        let value: Value = field.get("value");
        if !meaningful(&value)
            || (key == "monica.name" && value == row.get::<String, _>("display_name"))
        {
            continue;
        }
        let same:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id JOIN crm.source_account_links l ON l.account_id=r.source_account_id AND l.profile_id=c.profile_id WHERE c.profile_id=$1 AND c.field_key=$2 AND r.value=$3)").bind(target).bind(&key).bind(&value).fetch_one(&mut **tx).await?;
        if !same {
            blockers.insert("independent-field");
        }
    }
    Ok(
        json!({"profile_id":id,"target_id":target,"accounts":accounts.iter().map(|a|a.get::<Uuid,_>("account_id")).collect::<Vec<_>>(),"blockers":blockers}),
    )
}
fn meaningful(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(v) => v.iter().any(meaningful),
        Value::Object(v) => v.values().any(meaningful),
        _ => true,
    }
}
/// Reconciliation is called with the sorted profile locks held by the link move.
pub async fn reconcile_profiles(
    tx: &mut Transaction<'_, Postgres>,
    ids: &BTreeSet<Uuid>,
    actor: Option<Uuid>,
) -> Result<Vec<Value>> {
    let mut reports = vec![];
    for id in ids {
        let report = retirement_candidate(tx, *id).await?;
        let blocked = !report["blockers"].as_array().unwrap().is_empty();
        if blocked {
            let restored=sqlx::query("DELETE FROM crm.profile_redirects WHERE old_profile_id=$1 AND evidence->>'kind'='empty-after-account-move'").bind(id).execute(&mut **tx).await?.rows_affected()>0;
            if restored {
                sqlx::query(
                    "UPDATE crm.profiles SET archived_at=NULL,generation=generation+1 WHERE id=$1",
                )
                .bind(id)
                .execute(&mut **tx)
                .await?;
                sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) VALUES($1,'profile.auto_restored','profile',$2)").bind(actor).bind(id).execute(&mut **tx).await?;
            }
        } else {
            let target: Uuid = serde_json::from_value(report["target_id"].clone())?;
            let evidence = json!({"kind":"empty-after-account-move","version":1,"accounts":report["accounts"]});
            sqlx::query("INSERT INTO crm.profile_redirects(old_profile_id,new_profile_id,evidence) VALUES($1,$2,$3) ON CONFLICT(old_profile_id) DO UPDATE SET new_profile_id=excluded.new_profile_id,evidence=excluded.evidence").bind(id).bind(target).bind(&evidence).execute(&mut **tx).await?;
            let changed=sqlx::query("UPDATE crm.profiles SET archived_at=now(),generation=generation+1 WHERE id=$1 AND archived_at IS NULL").bind(id).execute(&mut **tx).await?.rows_affected()>0;
            if changed {
                sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'profile.auto_archived','profile',$2,$3)").bind(actor).bind(id).bind(evidence).execute(&mut **tx).await?;
            }
        }
        reports.push(report);
    }
    Ok(reports)
}
pub async fn restore(pool: &PgPool, id: Uuid, actor: Uuid) -> Result<bool> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let restored=sqlx::query("DELETE FROM crm.profile_redirects WHERE old_profile_id=$1 AND evidence->>'kind'='empty-after-account-move'").bind(id).execute(&mut *tx).await?.rows_affected()>0;
    if restored {
        sqlx::query("UPDATE crm.profiles SET archived_at=NULL,generation=generation+1 WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id) VALUES($1,'profile.restored','profile',$2)").bind(actor).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(restored)
}
pub async fn resolve_profile(pool: &PgPool, id: Uuid) -> Result<Uuid> {
    let mut current = id;
    let mut seen = BTreeSet::new();
    for _ in 0..32 {
        ensure!(seen.insert(current), "profile redirect cycle");
        let next:Option<Uuid>=sqlx::query_scalar("SELECT r.new_profile_id FROM crm.profile_redirects r JOIN crm.profiles p ON p.id=r.old_profile_id WHERE r.old_profile_id=$1 AND p.archived_at IS NOT NULL").bind(current).fetch_optional(pool).await?;
        if let Some(next) = next {
            current = next
        } else {
            return Ok(current);
        }
    }
    anyhow::bail!("too many profile redirects")
}

/// Platform facts without account provenance cannot be silently left behind by
/// an ownership move. Detach remains possible except for quarantined identities.
pub async fn link_blockers(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<std::collections::BTreeMap<Uuid, String>> {
    let rows:Vec<(Uuid,String)>=sqlx::query_as(r#"SELECT a.id,CASE
 WHEN EXISTS(SELECT 1 FROM crm.identities i WHERE i.source_account_id=a.id GROUP BY i.source_account_id HAVING bool_or(i.status='invalid') AND bool_and(i.status IN ('invalid','merged'))) THEN 'quarantined'
 WHEN EXISTS(SELECT 1 FROM crm.media_links m WHERE m.entity_type='profile' AND m.entity_id=l.profile_id AND m.source_account_id IS NULL AND m.role IN ('cover','wechat_avatar','instagram_avatar','linkedin_avatar') AND CASE m.role WHEN 'instagram_avatar' THEN 'instagram' WHEN 'linkedin_avatar' THEN 'linkedin' ELSE 'wechat' END=a.provider) THEN 'unowned_media'
 WHEN EXISTS(SELECT 1 FROM crm.tag_memberships t WHERE t.profile_id=l.profile_id AND t.source_type=a.provider) THEN 'unowned_tags'
 WHEN EXISTS(SELECT 1 FROM crm.relationship_edges e WHERE (e.from_profile_id=l.profile_id OR e.to_profile_id=l.profile_id) AND e.source_type=a.provider AND NOT EXISTS(SELECT 1 FROM crm.legacy_relationship_owners o WHERE o.edge_id=e.id)) THEN 'unowned_relationships'
 END reason FROM crm.source_accounts a LEFT JOIN crm.source_account_links l ON l.account_id=a.id WHERE a.id=ANY($1)"#).bind(ids).fetch_all(&mut **tx).await?.into_iter().filter_map(|(id,reason):(Uuid,Option<String>)|reason.map(|reason|(id,reason))).collect();
    Ok(rows.into_iter().collect())
}

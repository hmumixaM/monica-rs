//! Explicit same-person merge. Ordinary single-account relinking stays separate.
//! Immutable facts remain addressable through the recorded canonical redirect.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub operation_id: Uuid,
    pub target_id: Uuid,
    pub source_generation: i64,
    pub target_generation: i64,
}

pub async fn preview(
    tx: &mut Transaction<'_, Postgres>,
    source: Uuid,
    target: Uuid,
) -> Result<Value> {
    ensure!(source != target, "different people required");
    let people: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM crm.profiles p WHERE id=ANY($1) ORDER BY id")
            .bind(vec![source, target])
            .fetch_all(&mut **tx)
            .await?;
    ensure!(
        people.len() == 2
            && people
                .iter()
                .all(|p| p["profile_type"] == "person" && p["archived_at"].is_null()),
        "active people required"
    );
    let source_person = people
        .iter()
        .find(|p| p["id"] == source.to_string())
        .unwrap();
    let target_person = people
        .iter()
        .find(|p| p["id"] == target.to_string())
        .unwrap();
    let mut conflicts = Vec::<String>::new();
    for (name, sql) in [
        (
            "structured_fields",
            "SELECT EXISTS(SELECT 1 FROM crm.notebook_structured_current a JOIN crm.notebook_structured_current b ON b.profile_id=$2 AND b.fact_type=a.fact_type AND b.fact_key=a.fact_key JOIN crm.source_structured_revisions ar ON ar.id=a.revision_id JOIN crm.source_structured_revisions br ON br.id=b.revision_id WHERE a.profile_id=$1 AND ar.value IS DISTINCT FROM br.value)",
        ),
        (
            "link_decisions",
            "SELECT EXISTS(SELECT 1 FROM crm.platform_link_decisions a JOIN crm.platform_link_decisions b ON b.account_id=a.account_id AND b.profile_id=$2 WHERE a.profile_id=$1 AND a.status<>b.status)",
        ),
        (
            "relationship_values",
            "SELECT EXISTS(SELECT 1 FROM crm.relationship_edges a JOIN crm.relationship_edges b ON b.id<>a.id AND b.from_profile_id=CASE WHEN a.from_profile_id=$1 THEN $2 ELSE a.from_profile_id END AND b.to_profile_id=CASE WHEN a.to_profile_id=$1 THEN $2 ELSE a.to_profile_id END AND b.relationship_type=a.relationship_type WHERE (a.from_profile_id=$1 OR a.to_profile_id=$1) AND a.evidence_kind='relationship' AND b.evidence_kind='relationship' AND (a.note,a.valid_from,a.valid_to) IS DISTINCT FROM (b.note,b.valid_from,b.valid_to))",
        ),
        (
            "manual_fields",
            "SELECT EXISTS(SELECT 1 FROM crm.field_current a JOIN crm.field_revisions ar ON ar.id=a.revision_id JOIN crm.field_current b ON b.profile_id=$2 AND b.field_key=a.field_key JOIN crm.field_revisions br ON br.id=b.revision_id WHERE a.profile_id=$1 AND a.field_key<>'monica.name' AND ar.source_type IN ('manual','monica') AND br.source_type IN ('manual','monica') AND ar.value IS DISTINCT FROM br.value)",
        ),
        (
            "birthday",
            "SELECT EXISTS(SELECT 1 FROM crm.profile_dates a JOIN crm.profile_dates b ON b.profile_id=$2 AND b.kind='birthday' WHERE a.profile_id=$1 AND a.kind='birthday' AND (a.date_precision,a.year,a.month,a.day) IS DISTINCT FROM (b.date_precision,b.year,b.month,b.day))",
        ),
        (
            "face_decisions",
            "SELECT EXISTS(SELECT 1 FROM crm.faces WHERE (profile_id=ANY($1::uuid[]) OR suggested_profile_id=ANY($1::uuid[])) AND rejected_profile_id=ANY($1::uuid[]))",
        ),
    ] {
        let exists = if name == "face_decisions" {
            sqlx::query_scalar::<_, bool>(sql)
                .bind(vec![source, target])
                .fetch_one(&mut **tx)
                .await?
        } else {
            sqlx::query_scalar::<_, bool>(sql)
                .bind(source)
                .bind(target)
                .fetch_one(&mut **tx)
                .await?
        };
        if exists {
            conflicts.push(name.into());
        }
    }
    if source_person["summary"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
        && target_person["summary"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && source_person["summary"] != target_person["summary"]
    {
        conflicts.push("summary".into());
    }
    let counts:Value=sqlx::query_scalar("SELECT jsonb_build_object('accounts',(SELECT count(*) FROM crm.source_account_links WHERE profile_id=$1),'fields',(SELECT count(*) FROM crm.field_current WHERE profile_id=$1),'confirmed_faces',(SELECT count(*) FROM crm.faces WHERE profile_id=$1 AND status='confirmed'),'suggested_faces',(SELECT count(*) FROM crm.faces WHERE suggested_profile_id=$1),'media_links',(SELECT count(*) FROM crm.media_links WHERE entity_type='profile' AND entity_id=$1),'dates',(SELECT count(*) FROM crm.profile_dates WHERE profile_id=$1))")
        .bind(source).fetch_one(&mut **tx).await?;
    Ok(
        json!({"source":source_person,"target":target_person,"counts":counts,"conflicts":conflicts,"can_merge":conflicts.is_empty()}),
    )
}

pub async fn apply(
    pool: &PgPool,
    source: Uuid,
    request: &Request,
    actor: Option<Uuid>,
) -> Result<Value> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='3s'")
        .execute(&mut *tx)
        .await?;
    // The operation receipt makes retries observable and prevents a second move.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,713820))")
        .bind(request.operation_id.to_string())
        .execute(&mut *tx)
        .await?;
    if let Some(row) = sqlx::query(
        "SELECT source_id,target_id,result FROM crm.profile_merges WHERE operation_id=$1",
    )
    .bind(request.operation_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        ensure!(
            row.get::<Uuid, _>("source_id") == source
                && row.get::<Uuid, _>("target_id") == request.target_id,
            "operation identity changed"
        );
        return Ok(row.get("result"));
    }
    let target = request.target_id;
    let accounts:Vec<(Uuid,Uuid)>=sqlx::query_as("SELECT a.id,l.profile_id FROM crm.source_accounts a JOIN crm.source_account_links l ON l.account_id=a.id WHERE l.profile_id=ANY($1) ORDER BY a.id FOR UPDATE OF a")
        .bind(vec![source,target]).fetch_all(&mut *tx).await?;
    sqlx::query("SELECT id FROM crm.profiles WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(vec![source, target])
        .fetch_all(&mut *tx)
        .await?;
    let report = preview(&mut tx, source, target).await?;
    ensure!(
        report["source"]["generation"] == request.source_generation
            && report["target"]["generation"] == request.target_generation,
        "profile changed; preview again"
    );
    ensure!(
        report["can_merge"] == true,
        "merge conflicts; resolve before merging"
    );
    let mut retained = serde_json::Map::new();
    for (table, predicate) in [
        ("field_current", "profile_id=$1"),
        ("notebook_structured_current", "profile_id=$1"),
        ("profile_dates", "profile_id=$1"),
        ("tag_memberships", "profile_id=$1"),
        ("tag_membership_removals", "profile_id=$1"),
        ("group_memberships", "person_profile_id=$1"),
        ("person_identifiers", "profile_id=$1"),
        ("platform_link_decisions", "profile_id=$1"),
        (
            "faces",
            "profile_id=$1 OR suggested_profile_id=$1 OR rejected_profile_id=$1",
        ),
        (
            "relationship_edges",
            "from_profile_id=$1 OR to_profile_id=$1",
        ),
        ("media_slots", "entity_type='profile' AND entity_id=$1"),
    ] {
        let values: Vec<Value> = sqlx::query_scalar(&format!(
            "SELECT to_jsonb(t) FROM crm.{table} t WHERE {predicate}"
        ))
        .bind(source)
        .fetch_all(&mut *tx)
        .await?;
        retained.insert(table.into(), json!(values));
    }
    let changes = accounts
        .into_iter()
        .filter(|(_, p)| *p == source)
        .map(|(id, _)| crate::linking::LinkChange {
            account_id: id,
            profile_id: Some(target),
            expected_profile_id: Some(source),
        })
        .collect::<Vec<_>>();
    if !changes.is_empty() {
        crate::linking::set_links(
            &mut tx,
            &changes,
            actor,
            &json!({"kind":"explicit-person-merge","operation_id":request.operation_id}),
        )
        .await?;
    }
    // Keep source revisions immutable. Current pointers may refer to their IDs.
    sqlx::query("INSERT INTO crm.field_current SELECT $2,field_key,revision_id FROM crm.field_current WHERE profile_id=$1 ON CONFLICT(profile_id,field_key) DO UPDATE SET revision_id=excluded.revision_id WHERE field_current.field_key<>'monica.name' AND EXISTS(SELECT 1 FROM crm.field_revisions a JOIN crm.field_revisions b ON b.id=field_current.revision_id WHERE a.id=excluded.revision_id AND a.source_type IN ('manual','monica') AND b.source_type NOT IN ('manual','monica'))").bind(source).bind(target).execute(&mut *tx).await?;
    // PK duplicates with identical membership meaning collapse. Original values
    // remain in the immutable merge receipt, and revision/audit tables are untouched.
    for sql in [
        "INSERT INTO crm.notebook_structured_current SELECT $2,fact_type,fact_key,revision_id FROM crm.notebook_structured_current WHERE profile_id=$1 ON CONFLICT DO NOTHING",
        "INSERT INTO crm.platform_link_decisions SELECT account_id,$2,status,evidence,actor_user_id,updated_at FROM crm.platform_link_decisions WHERE profile_id=$1 ON CONFLICT DO NOTHING",
        "INSERT INTO crm.tag_memberships SELECT tag_id,$2,source_type FROM crm.tag_memberships WHERE profile_id=$1 ON CONFLICT DO NOTHING",
        "INSERT INTO crm.tag_membership_removals SELECT tag_id,$2,removed_by,removed_at FROM crm.tag_membership_removals WHERE profile_id=$1 ON CONFLICT DO NOTHING",
        "INSERT INTO crm.activity_participants SELECT activity_id,$2 FROM crm.activity_participants WHERE profile_id=$1 ON CONFLICT DO NOTHING",
        "INSERT INTO crm.person_identifiers SELECT $2,kind,value,confirmed,is_self FROM crm.person_identifiers WHERE profile_id=$1 ON CONFLICT(profile_id,kind,value) DO UPDATE SET confirmed=person_identifiers.confirmed OR excluded.confirmed,is_self=person_identifiers.is_self OR excluded.is_self",
        "INSERT INTO crm.connection_evidence_people SELECT evidence_id,$2,role FROM crm.connection_evidence_people WHERE profile_id=$1 ON CONFLICT DO NOTHING",
        "INSERT INTO crm.connection_identity_candidates SELECT connection_id,external_id,$2,reason FROM crm.connection_identity_candidates WHERE profile_id=$1 ON CONFLICT DO NOTHING",
        "INSERT INTO crm.media_links SELECT entity_type,$2,media_id,role,created_at,source_account_id FROM crm.media_links WHERE entity_type='profile' AND entity_id=$1 ON CONFLICT DO NOTHING",
    ] {
        sqlx::query(sql)
            .bind(source)
            .bind(target)
            .execute(&mut *tx)
            .await?;
    }
    for table in [
        "field_current",
        "notebook_structured_current",
        "platform_link_decisions",
        "tag_memberships",
        "tag_membership_removals",
        "activity_participants",
        "person_identifiers",
        "connection_evidence_people",
        "connection_identity_candidates",
    ] {
        sqlx::query(&format!("DELETE FROM crm.{table} WHERE profile_id=$1"))
            .bind(source)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM crm.media_links WHERE entity_type='profile' AND entity_id=$1")
        .bind(source)
        .execute(&mut *tx)
        .await?;
    // Same birthdays/dates deduplicate; any different birthday was rejected above.
    sqlx::query("DELETE FROM crm.profile_dates a USING crm.profile_dates b WHERE a.profile_id=$1 AND b.profile_id=$2 AND a.kind=b.kind AND (a.kind='birthday' OR (coalesce(a.label,''),a.date_precision,coalesce(a.year,0),coalesce(a.month,0),coalesce(a.day,0))=(coalesce(b.label,''),b.date_precision,coalesce(b.year,0),coalesce(b.month,0),coalesce(b.day,0)))").bind(source).bind(target).execute(&mut *tx).await?;
    for (table, column) in [
        ("notebook_objects", "profile_id"),
        ("identities", "profile_id"),
        ("timeline_events", "profile_id"),
        ("contact_events", "profile_id"),
        ("conversations", "profile_id"),
        ("message_search", "sender_profile_id"),
        ("social_interactions", "author_profile_id"),
        ("faces", "profile_id"),
        ("faces", "suggested_profile_id"),
        ("faces", "rejected_profile_id"),
        ("connection_identifiers", "profile_id"),
        ("connection_identity_links", "profile_id"),
        ("connection_people", "resolved_profile_id"),
        ("profile_dates", "profile_id"),
        ("identity_candidates", "candidate_profile_id"),
    ] {
        sqlx::query(&format!(
            "UPDATE crm.{table} SET {column}=$2 WHERE {column}=$1"
        ))
        .bind(source)
        .bind(target)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("INSERT INTO crm.group_memberships SELECT group_profile_id,$2,role,joined_at,left_at,observed_at,source_type,source_record_id FROM crm.group_memberships WHERE person_profile_id=$1 ON CONFLICT DO NOTHING").bind(source).bind(target).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM crm.group_memberships WHERE person_profile_id=$1")
        .bind(source)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE crm.relationship_edges SET evidence_kind='merged_self' WHERE evidence_kind='relationship' AND ((from_profile_id=$1 AND to_profile_id=$2) OR (from_profile_id=$2 AND to_profile_id=$1))")
        .bind(source).bind(target).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.relationship_edges a SET evidence_kind='merged_duplicate' FROM crm.relationship_edges b WHERE a.evidence_kind='relationship' AND b.evidence_kind='relationship' AND a.id<>b.id AND (a.from_profile_id=$1 OR a.to_profile_id=$1) AND b.from_profile_id=CASE WHEN a.from_profile_id=$1 THEN $2 ELSE a.from_profile_id END AND b.to_profile_id=CASE WHEN a.to_profile_id=$1 THEN $2 ELSE a.to_profile_id END AND a.relationship_type=b.relationship_type")
        .bind(source).bind(target).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.relationship_edges SET from_profile_id=CASE WHEN from_profile_id=$1 THEN $2 ELSE from_profile_id END,to_profile_id=CASE WHEN to_profile_id=$1 THEN $2 ELSE to_profile_id END WHERE evidence_kind='relationship' AND (from_profile_id=$1 OR to_profile_id=$1)")
        .bind(source).bind(target).execute(&mut *tx).await?;
    // Preserve per-source relationship decisions and immutable revisions; the
    // existing presented view retains account ownership and hides collapsed edges.
    sqlx::query("UPDATE crm.profiles SET summary=coalesce(nullif(summary,''),$2),avatar_media_id=coalesce(avatar_media_id,$3),generation=generation+1 WHERE id=$1")
        .bind(target).bind(report["source"]["summary"].as_str()).bind(report["source"]["avatar_media_id"].as_str().and_then(|s|s.parse::<Uuid>().ok())).execute(&mut *tx).await?;
    // Preserve profile slots as separate candidates; conflicting manual avatar
    // choices stay with their recorded origin instead of replacing the target.
    sqlx::query("INSERT INTO crm.media_slots SELECT entity_type,$2,role,media_id,generation,source_observed_at,source_batch_id,source_ordinal,updated_at FROM crm.media_slots WHERE entity_type='profile' AND entity_id=$1 AND (role<>'avatar' OR NOT EXISTS(SELECT 1 FROM crm.profiles p WHERE p.id=$2 AND p.avatar_media_id IS DISTINCT FROM media_slots.media_id)) ON CONFLICT DO NOTHING").bind(source).bind(target).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.profile_redirects(old_profile_id,new_profile_id,evidence) VALUES($1,$2,$3)").bind(source).bind(target).bind(json!({"kind":"explicit-person-merge","operation_id":request.operation_id})).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.profiles SET archived_at=now(),generation=generation+1 WHERE id=$1")
        .bind(source)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO crm.memory_profile_refresh SELECT unnest($1::uuid[]) ON CONFLICT DO NOTHING",
    )
    .bind(vec![source, target])
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO crm.memory_legacy_scopes(scope,id,profiles) SELECT 'profile',id,$1::uuid[] FROM unnest($1::uuid[]) id ON CONFLICT(scope,id) DO UPDATE SET after_kind='',after_id='00000000-0000-0000-0000-000000000000',profiles=excluded.profiles,updated_at=now()")
        .bind(vec![source,target]).execute(&mut *tx).await?;
    let result = json!({"operation_id":request.operation_id,"source_id":source,"target_id":target,"archived":true,"counts":report["counts"],"pre_merge":report,"retained_current_facts":retained});
    sqlx::query("INSERT INTO crm.profile_merges(operation_id,source_id,target_id,actor_user_id,result) VALUES($1,$2,$3,$4,$5)").bind(request.operation_id).bind(source).bind(target).bind(actor).bind(&result).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'profile.merged','profile',$2,$3)").bind(actor).bind(source).bind(json!({"operation_id":request.operation_id,"target_id":target})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(result)
}

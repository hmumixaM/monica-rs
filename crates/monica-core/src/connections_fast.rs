//! Stable, first-seen messages project as bounded sets. Mutable revisions retain
//! the ordered per-record path, with exactly the same identity rules.
use super::*;
use std::collections::{BTreeMap, HashMap};
pub(super) async fn project_new_messages(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    cid: Uuid,
    rows: &[Revision],
) -> Result<bool> {
    if rows.is_empty() {
        return Ok(true);
    }
    if rows.iter().any(|r| {
        r.revision != 1
            || string(&r.payload, "kind", "message") != "message"
            || !truth(r.payload.get("included"), true)
            || truth(r.payload.get("deleted"), false)
            || r.payload
                .get("occurred_at")
                .and_then(Value::as_str)
                .is_none()
    }) {
        return Ok(false);
    }
    let ids: Vec<Uuid> = rows
        .iter()
        .map(|r| evidence_id(cid, &r.external_id))
        .collect();
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM crm.connection_evidence WHERE id=ANY($1))",
    )
    .bind(&ids)
    .fetch_one(&mut **tx)
    .await?
    {
        return Ok(false);
    }
    let participants: Vec<&Value> = rows
        .iter()
        .flat_map(|r| {
            r.payload
                .get("participants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .collect();
    if participants.iter().any(|p| string(p, "id", "").is_empty()) {
        bail!("participant id required")
    }
    let keys: BTreeSet<_> = participants.iter().flat_map(|p| identifiers(p)).collect();
    let lock_keys: BTreeSet<String> = keys
        .iter()
        .map(|(k, v)| format!("identifier:{k}:{v}"))
        .chain(
            participants
                .iter()
                .map(|p| format!("identifier:source:{}", string(p, "id", ""))),
        )
        .collect();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(k,0)) FROM unnest($1::text[]) k ORDER BY k",
    )
    .bind(lock_keys.into_iter().collect::<Vec<_>>())
    .execute(&mut **tx)
    .await?;
    let owned = owned_identifiers(tx).await?;
    let values: Vec<Value> = rows
        .iter()
        .map(|r| apply_ownership(r.payload.clone(), &owned))
        .collect();
    let kinds: Vec<String> = keys.iter().map(|r| r.0.clone()).collect();
    let identifiers_values: Vec<String> = keys.iter().map(|r| r.1.clone()).collect();
    let matches:Vec<(String,String,Uuid)>=sqlx::query_as("WITH wanted AS(SELECT * FROM unnest($1::text[],$2::text[]) w(kind,value)) SELECT i.kind,i.value,i.profile_id FROM crm.person_identifiers i JOIN wanted USING(kind,value) UNION SELECT i.kind,i.value,i.profile_id FROM crm.connection_identifiers i JOIN wanted USING(kind,value) WHERE i.active").bind(kinds).bind(identifiers_values).fetch_all(&mut **tx).await?;
    let mut by_identifier: HashMap<(String, String), BTreeSet<Uuid>> = HashMap::new();
    for (kind, value, profile) in matches {
        by_identifier
            .entry((kind, value))
            .or_default()
            .insert(profile);
    }
    let external: Vec<String> = participants
        .iter()
        .map(|p| string(p, "id", "").to_owned())
        .collect();
    let explicit:Vec<(String,Option<Uuid>,bool)>=sqlx::query_as("SELECT external_id,profile_id,blocked FROM crm.connection_identity_links WHERE connection_id=$1 AND external_id=ANY($2) AND confirmed").bind(cid).bind(&external).fetch_all(&mut **tx).await?;
    let links: HashMap<String, (Option<Uuid>, bool)> = explicit
        .into_iter()
        .map(|(id, p, b)| (id, (p, b)))
        .collect();
    let mut profiles = BTreeSet::new();
    let mut people = BTreeMap::new();
    let mut candidates: BTreeMap<String, BTreeSet<Uuid>> = BTreeMap::new();
    let mut evidence_people = vec![];
    let mut evidence = vec![];
    let mut messages = vec![];
    let mut conversations = BTreeMap::new();
    for ((row, value), eid) in rows.iter().zip(&values).zip(&ids) {
        let ps = value
            .get("participants")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut resolved = vec![];
        for p in &ps {
            let external = string(p, "id", "");
            let name = string(p, "name", external);
            let own = truth(p.get("is_self"), false);
            let profile = if let Some((profile, blocked)) = links.get(external) {
                if *blocked { None } else { *profile }
            } else {
                let found: BTreeSet<Uuid> = identifiers(p)
                    .iter()
                    .flat_map(|key| by_identifier.get(key).into_iter().flatten().copied())
                    .collect();
                let result = if found.len() == 1 {
                    found.first().copied()
                } else {
                    None
                };
                candidates.insert(
                    external.to_owned(),
                    if result.is_none() {
                        found
                    } else {
                        BTreeSet::new()
                    },
                );
                result
            };
            people.insert(external.to_owned(),json!({"external_id":external,"display_name":name,"is_self":own,"profile_id":profile}));
            if let Some(profile) = profile {
                profiles.insert(profile);
                let role = string(p, "role", "participant");
                resolved.push((profile, role, own));
                evidence_people.push(json!({"evidence_id":eid,"profile_id":profile,"role":role}));
            }
        }
        let external_people: BTreeSet<&str> = ps
            .iter()
            .filter(|p| !truth(p.get("is_self"), false))
            .map(|p| string(p, "id", ""))
            .collect();
        let direct = row.binding.as_ref().map_or(
            external_people.len() == 1 && !truth(value.get("is_group"), false),
            |b| b.conversation_type == "direct",
        );
        let typ = if direct { "direct" } else { "group" };
        let thread = string(value, "thread_id", &row.external_id);
        let conversation = Uuid::new_v5(&cid, format!("conversation:{typ}:{thread}").as_bytes());
        let counterparts: BTreeSet<Uuid> = resolved
            .iter()
            .filter(|(_, _, own)| !*own)
            .map(|(p, _, _)| *p)
            .collect();
        let profile = row.binding.as_ref().map_or_else(
            || {
                if direct && counterparts.len() == 1 {
                    counterparts.first().copied()
                } else {
                    None
                }
            },
            |b| b.profile_id,
        );
        conversations.insert(conversation,json!({"id":conversation,"external_id":format!("{typ}:{thread}"),"source_external_id":thread,"profile_id":profile,"conversation_type":typ}));
        let sender = ps
            .iter()
            .find(|p| string(p, "role", "") == "sender")
            .map_or("", |p| string(p, "id", ""));
        let sender_profile = row.binding.as_ref().map_or_else(
            || {
                resolved
                    .iter()
                    .find(|(_, role, _)| *role == "sender")
                    .map(|(p, _, _)| *p)
            },
            |b| b.sender_profile_id,
        );
        if let Some(binding) = &row.binding {
            for (profile, role) in [
                (binding.profile_id, "participant"),
                (binding.sender_profile_id, "sender"),
            ] {
                if let Some(profile) = profile {
                    profiles.insert(profile);
                    evidence_people
                        .push(json!({"evidence_id":eid,"profile_id":profile,"role":role}));
                }
            }
        }
        messages.push(json!({"message_id":eid,"conversation_id":conversation,"external_id":row.external_id,"sender":sender,"sender_profile_id":sender_profile,"occurred_at":value["occurred_at"],"body":message_text(value),"message_type":string(value,"message_type","text"),"direction":message_direction(value,row.binding.as_ref())}));
        let mut metadata = value.clone();
        for key in ["body", "raw_body", "raw_message", "raw_revoke"] {
            metadata
                .as_object_mut()
                .context("record must be object")?
                .remove(key);
        }
        evidence.push(json!({"id":eid,"external_id":row.external_id,"content_hash":row.content_hash,"occurred_at":value["occurred_at"],"title":string(value,"title",""),"body":string(value,"body",""),"category":value.get("category"),"metadata":metadata,"dedup_key":if provider.schema()=="mail"{value.get("dedup_key")}else{None},"conversation_id":conversation}));
    }
    let profiles: Vec<Uuid> = profiles.into_iter().collect();
    sqlx::query("SELECT id FROM crm.profiles WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&profiles)
        .fetch_all(&mut **tx)
        .await?;
    let conversations = json!(conversations.into_values().collect::<Vec<_>>());
    sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,profile_id,conversation_type,connection_id,source_external_id) SELECT id,$2,$3,external_id,profile_id,conversation_type,$4,source_external_id FROM jsonb_to_recordset($1) r(id uuid,external_id text,profile_id uuid,conversation_type text,source_external_id text) ON CONFLICT(id) DO UPDATE SET profile_id=excluded.profile_id").bind(conversations).bind(provider.schema()).bind(cid.to_string()).bind(cid).execute(&mut **tx).await?;
    let evidence = json!(evidence);
    sqlx::query("INSERT INTO crm.connection_evidence(id,connection_id,external_id,revision,content_hash,kind,occurred_at,title,body,category,included,metadata,dedup_key,message_id,conversation_id) SELECT id,$2,external_id,1,content_hash,'message',occurred_at,title,body,category,true,metadata,dedup_key,id,conversation_id FROM jsonb_to_recordset($1) r(id uuid,external_id text,content_hash text,occurred_at timestamptz,title text,body text,category text,metadata jsonb,dedup_key text,conversation_id uuid)").bind(&evidence).bind(cid).execute(&mut **tx).await?;
    sqlx::query("WITH names AS(SELECT (e.value->>'conversation_id')::uuid conversation_id,(e.value->>'occurred_at')::timestamptz at,e.position,crm.conversation_source_name(c.conversation_type,e.value->'metadata') label FROM jsonb_array_elements($1) WITH ORDINALITY e(value,position) JOIN crm.conversations c ON c.id=(e.value->>'conversation_id')::uuid),latest AS(SELECT DISTINCT ON(conversation_id) conversation_id,at,label FROM names WHERE label IS NOT NULL ORDER BY conversation_id,at DESC,position DESC) UPDATE crm.conversations c SET source_display_name=l.label,source_display_at=l.at FROM latest l WHERE c.id=l.conversation_id AND (c.source_display_at IS NULL OR c.source_display_at<=l.at)").bind(&evidence).execute(&mut **tx).await?;
    let messages = json!(messages);
    sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,sender_profile_id,occurred_at,body,message_type,direction) SELECT message_id,conversation_id,$2,$3,external_id,sender,sender_profile_id,occurred_at,body,message_type,direction FROM jsonb_to_recordset($1) r(message_id uuid,conversation_id uuid,external_id text,sender text,sender_profile_id uuid,occurred_at timestamptz,body text,message_type text,direction text)").bind(&messages).bind(provider.schema()).bind(cid.to_string()).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO crm.connection_people(connection_id,external_id,display_name,is_self,resolved_profile_id) SELECT $2,external_id,display_name,is_self,profile_id FROM jsonb_to_recordset($1) r(external_id text,display_name text,is_self bool,profile_id uuid) ON CONFLICT(connection_id,external_id) DO UPDATE SET display_name=excluded.display_name,is_self=excluded.is_self,resolved_profile_id=excluded.resolved_profile_id").bind(json!(people.into_values().collect::<Vec<_>>())).bind(cid).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO crm.connection_evidence_people SELECT DISTINCT evidence_id,profile_id,role FROM jsonb_to_recordset($1) r(evidence_id uuid,profile_id uuid,role text) ON CONFLICT DO NOTHING").bind(json!(evidence_people)).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM crm.connection_identity_candidates WHERE connection_id=$1 AND external_id=ANY($2)").bind(cid).bind(candidates.keys().cloned().collect::<Vec<_>>()).execute(&mut **tx).await?;
    let candidates: Vec<Value> = candidates
        .into_iter()
        .flat_map(|(external, ids)| {
            ids.into_iter()
                .map(move |id| json!({"external_id":external,"profile_id":id}))
        })
        .collect();
    sqlx::query("INSERT INTO crm.connection_identity_candidates SELECT $2,external_id,profile_id,'conflicting_identifier' FROM jsonb_to_recordset($1) r(external_id text,profile_id uuid) ON CONFLICT DO NOTHING").bind(json!(candidates)).bind(cid).execute(&mut **tx).await?;
    sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=ANY($1)")
        .bind(profiles)
        .execute(&mut **tx)
        .await?;
    sqlx::query("WITH added AS(SELECT * FROM jsonb_to_recordset($1) r(conversation_id uuid,occurred_at timestamptz)),totals AS(SELECT conversation_id,count(*) n,min(occurred_at) first,max(occurred_at) last FROM added GROUP BY conversation_id) UPDATE crm.conversations c SET message_count=c.message_count+t.n,first_message_at=least(c.first_message_at,t.first),last_message_at=greatest(c.last_message_at,t.last),last_message_id=(SELECT message_id FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1),last_message_preview=(SELECT left(btrim(regexp_replace(body,'[[:space:]]+',' ','g')),80) FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone AND btrim(body)<>'' ORDER BY occurred_at DESC,message_id DESC LIMIT 1) FROM totals t WHERE c.id=t.conversation_id").bind(&messages).execute(&mut **tx).await?;
    for (table, zone) in [
        ("message_months_utc", "UTC"),
        ("message_months", "America/Chicago"),
    ] {
        sqlx::query(&format!("INSERT INTO crm.{table} SELECT conversation_id,to_char(occurred_at AT TIME ZONE $2,'YYYY-MM'),count(*) FILTER(WHERE direction='sent'),count(*) FILTER(WHERE direction IS DISTINCT FROM 'sent'),min(occurred_at),max(occurred_at) FROM jsonb_to_recordset($1) r(conversation_id uuid,occurred_at timestamptz,direction text) GROUP BY conversation_id,to_char(occurred_at AT TIME ZONE $2,'YYYY-MM') ON CONFLICT(conversation_id,month) DO UPDATE SET sent=crm.{table}.sent+excluded.sent,received=crm.{table}.received+excluded.received,first_message_at=least(crm.{table}.first_message_at,excluded.first_message_at),last_message_at=greatest(crm.{table}.last_message_at,excluded.last_message_at)")).bind(&messages).bind(zone).execute(&mut **tx).await?;
    }
    Ok(true)
}

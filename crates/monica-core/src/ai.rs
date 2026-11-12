//! Durable, privacy-scoped model work. No provider runs from an HTTP request.
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[path = "ai_mail.rs"]
pub mod mail;
#[path = "ai_memory.rs"]
pub mod memory;
#[path = "ai_provider.rs"]
pub mod provider;
#[path = "ai_slots.rs"]
pub mod slots;
#[path = "ai_weekly.rs"]
pub mod weekly;

pub const PROMPT_VERSION: &str = "monica-evidence-v2-attribution";
pub const KINDS: &[&str] = &[
    "ai.profile",
    "ai.person_week",
    "ai.digest",
    "ai.community",
    "ai.reconnect_one",
    "ai.image_ocr",
    "ai.post_media",
    "ai.mail_classify",
    "ai.mail_events",
];
pub const SCHEDULED: &[&str] = &[
    "ai.profiles",
    "ai.person_weeks",
    "ai.digest",
    "ai.community_names",
    "ai.reconnect",
    "ai.post_media",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Privacy {
    pub include_private_notes: bool,
    pub include_messages: bool,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub input: Value,
    pub versions: Value,
    pub privacy: Privacy,
    pub model: String,
    pub provider: String,
    pub hash: String,
}
fn hash(value: &Value) -> Result<String> {
    crate::identity::validate_json(value)?;
    Ok(crate::identity::observation_hash(value))
}

/// Every field has an explicit allow-list. Bodies and credentials never enter by
/// copying an entire database row; notes/messages require separate consent.
pub async fn snapshot(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    subject: &str,
) -> Result<Snapshot> {
    ensure!(KINDS.contains(&kind), "unknown model job");
    let settings = sqlx::query("SELECT * FROM crm.ai_settings WHERE id=true FOR SHARE")
        .fetch_one(&mut **tx)
        .await?;
    ensure!(settings.get::<bool, _>("enabled"), "AI is disabled");
    let privacy = Privacy {
        include_private_notes: settings.get("include_private_notes"),
        include_messages: settings.get("include_messages"),
    };
    let mut model: String = settings.get("model");
    let provider: String = settings.get("provider");
    let mut input = json!({"evidence":[],"prompt_version":PROMPT_VERSION});
    let mut versions = json!({});
    match kind {
        "ai.mail_classify" | "ai.mail_events" => {
            let (payload, source) = mail::snapshot(tx, kind, subject, &privacy).await?;
            input = payload;
            versions = source;
        }
        "ai.profile" | "ai.person_week" | "ai.reconnect_one" => {
            let mut parts = subject.split(':');
            let id: Uuid = parts.next().context("missing profile")?.parse()?;
            let row = sqlx::query("SELECT id,display_name,summary,generation FROM crm.profiles WHERE id=$1 AND archived_at IS NULL AND profile_type='person' FOR SHARE").bind(id).fetch_optional(&mut **tx).await?.context("inactive profile")?;
            input["profile"] = json!({"id":id,"name":row.get::<String,_>("display_name"),"summary":row.get::<Option<String>,_>("summary")});
            versions["profile"] = json!(row.get::<i64, _>("generation"));
            let week = if kind == "ai.person_week" {
                Some(parts.next().context("missing week")?.parse::<NaiveDate>()?)
            } else {
                None
            };
            ensure!(parts.next().is_none(), "invalid profile subject");
            input["week_start"] = json!(week);
            let fields:Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',r.id,'kind','profile','text',r.value,'field_key',c.field_key,'at',r.observed_at) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key=ANY($2) ORDER BY c.field_key LIMIT 80")
                .bind(id).bind(vec!["residence","hometown","company","job","school","email","phone","food","interests","family","personal.residence","personal.hometown","employment.company","employment.title","education.school","contact.email","contact.phone","personal.food","personal.interests","personal.family","monica.residence","monica.hometown","monica.company","monica.job","monica.school","monica.email","monica.phone","monica.food_preferences","monica.interests","monica.family"]).fetch_all(&mut **tx).await?;
            let posts:Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'kind',event_type,'text',coalesce(content->>'text',content->>'caption',''),'at',occurred_at,'version',version) FROM crm.timeline_events WHERE profile_id=$1 AND status='active' AND ($2::date IS NULL OR (occurred_at>=($2::timestamp AT TIME ZONE 'America/Chicago') AND occurred_at<(($2+7)::timestamp AT TIME ZONE 'America/Chicago'))) ORDER BY occurred_at DESC NULLS LAST,id LIMIT 80").bind(id).bind(week).fetch_all(&mut **tx).await?;
            let mut evidence = if week.is_some() { Vec::new() } else { fields };
            evidence.extend(posts);
            if privacy.include_private_notes {
                let notes:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',a.id,'kind','note','text',left(coalesce(a.body,a.title),8000),'at',a.occurred_at,'version',a.content_hash) FROM crm.activities a JOIN crm.activity_participants p ON p.activity_id=a.id WHERE p.profile_id=$1 AND a.activity_type='note' AND ($2::date IS NULL OR (a.occurred_at>=($2::timestamp AT TIME ZONE 'America/Chicago') AND a.occurred_at<(($2+7)::timestamp AT TIME ZONE 'America/Chicago'))) ORDER BY a.occurred_at DESC,a.id LIMIT 40").bind(id).bind(week).fetch_all(&mut **tx).await?;
                evidence.extend(notes);
            }
            if privacy.include_messages {
                let messages:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',m.message_id,'conversation_id',m.conversation_id,'kind',c.conversation_type,'text',left(m.body,4000),'at',m.occurred_at,'sender',m.sender,'sender_profile_id',coalesce(m.sender_profile_id,l.profile_id),'direction',coalesce(m.direction,CASE WHEN m.sender=m.account THEN 'sent' ELSE 'received' END),'speaker',CASE WHEN c.conversation_type='group' THEN 'TA' WHEN m.direction='unknown' THEN '未知' WHEN m.direction='sent' OR (m.direction IS NULL AND m.sender=m.account) THEN '我' ELSE 'TA' END,'group',CASE WHEN c.conversation_type='group' THEN coalesce(c.source_display_name,c.external_id) END) FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id LEFT JOIN crm.source_account_links l ON l.account_id=m.sender_account_id WHERE ((c.conversation_type='direct' AND c.profile_id=$1) OR (c.conversation_type='group' AND coalesce(m.sender_profile_id,l.profile_id)=$1)) AND NOT m.tombstone AND ($2::date IS NULL OR (m.occurred_at>=($2::timestamp AT TIME ZONE 'America/Chicago') AND m.occurred_at<(($2+7)::timestamp AT TIME ZONE 'America/Chicago'))) ORDER BY m.occurred_at DESC,m.message_id LIMIT 120").bind(id).bind(week).fetch_all(&mut **tx).await?;
                evidence.extend(messages);
            }
            if week.is_some() {
                ensure!(!evidence.is_empty(), "no events occurred in this week");
            }
            input["evidence"] = json!(evidence);
        }
        "ai.digest" => {
            let week: NaiveDate = subject.parse()?;
            ensure!(
                week.weekday().num_days_from_monday() == 0,
                "week must begin Monday"
            );
            let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',t.id,'kind',t.event_type,'profile_id',t.profile_id,'text',coalesce(t.content->>'text',t.content->>'caption',''),'at',t.occurred_at,'version',t.version) FROM crm.timeline_events t JOIN crm.profiles p ON p.id=t.profile_id WHERE p.archived_at IS NULL AND t.status='active' AND t.occurred_at>=($1::date::timestamp AT TIME ZONE 'America/Chicago') AND t.occurred_at<(($1::date+7)::timestamp AT TIME ZONE 'America/Chicago') ORDER BY t.occurred_at,t.id LIMIT 160").bind(week).fetch_all(&mut **tx).await?;
            input["week_start"] = json!(week);
            input["evidence"] = json!(rows);
            input["facts"] = weekly::facts(tx, week).await?;
        }
        "ai.community" => {
            let row = sqlx::query(
                "SELECT input,version_hash FROM crm.ai_community_inputs WHERE community_id=$1",
            )
            .bind(subject)
            .fetch_optional(&mut **tx)
            .await?
            .context("unknown community snapshot")?;
            input = row.get("input");
            versions["community"] = json!(row.get::<String, _>("version_hash"));
        }
        "ai.image_ocr" => {
            let id: Uuid = subject.parse()?;
            let ocr = sqlx::query("SELECT * FROM crm.image_ocr_settings WHERE id FOR SHARE")
                .fetch_one(&mut **tx)
                .await?;
            ensure!(ocr.get::<bool, _>("enabled"), "OCR is disabled");
            model = ocr.get("model");
            let media:Value=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'sha256',sha256,'media_type',media_type,'byte_length',byte_length) FROM crm.media_assets WHERE id=$1 AND media_type LIKE 'image/%'").bind(id).fetch_optional(&mut **tx).await?.context("unknown image")?;
            if !ocr.get::<bool, _>("include_chat") {
                let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.media_links WHERE media_id=$1 AND entity_type IN ('social_post','social_story','post','story','profile'))").bind(id).fetch_one(&mut **tx).await?;
                ensure!(allowed, "private chat image excluded");
            }
            input["media"] = media;
            versions["ocr_controls"] = json!({"model":ocr.get::<String,_>("model"),"include_chat":ocr.get::<bool,_>("include_chat")});
        }
        "ai.post_media" => {
            let id: Uuid = subject.parse()?;
            let post:Value=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'kind',event_type,'text',coalesce(content->>'text',content->>'caption',''),'version',version) FROM crm.timeline_events WHERE id=$1 AND status='active'").bind(id).fetch_optional(&mut **tx).await?.context("unknown post")?;
            let readings:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',r.media_id,'kind','image','text',r.text,'description',r.description,'summary',r.summary,'version',r.input_hash) FROM crm.media_links l JOIN crm.image_ocr_readings r ON r.media_id=l.media_id WHERE l.entity_id=$1 AND r.status='complete' ORDER BY l.media_id LIMIT 20").bind(id).fetch_all(&mut **tx).await?;
            input["post"] = post;
            input["evidence"] = json!(readings);
        }
        _ => unreachable!(),
    }
    // Bound serialized input independently of row count and never truncate JSON bytes.
    ensure!(
        serde_json::to_vec(&input)?.len() <= 512 * 1024,
        "model input exceeds budget"
    );
    let digest = hash(&json!([
        PROMPT_VERSION,
        kind,
        subject,
        provider,
        model,
        input,
        versions,
        privacy
    ]))?;
    Ok(Snapshot {
        input,
        versions,
        privacy,
        model,
        provider,
        hash: digest,
    })
}

pub async fn enqueue(pool: &PgPool, kind: &str, subject: &str) -> Result<Uuid> {
    let mut tx = pool.begin().await?;
    let data = snapshot(&mut tx, kind, subject).await?;
    let id:Uuid=sqlx::query_scalar("INSERT INTO crm.ai_jobs(id,kind,subject,model,input_hash,input,source_versions,privacy) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(kind,subject,model,input_hash) DO UPDATE SET status=CASE WHEN ai_jobs.status IN ('failed','cancelled') OR (ai_jobs.status='complete' AND NOT EXISTS(SELECT 1 FROM crm.ai_outputs o WHERE o.kind=ai_jobs.kind AND o.subject=ai_jobs.subject AND o.input_hash=ai_jobs.input_hash)) THEN 'queued' ELSE ai_jobs.status END,error=NULL,attempts=CASE WHEN ai_jobs.status IN ('failed','cancelled','complete') THEN 0 ELSE ai_jobs.attempts END RETURNING id")
        .bind(Uuid::new_v4()).bind(kind).bind(subject).bind(&data.model).bind(&data.hash).bind(&data.input).bind(&data.versions).bind(json!({"provider":data.provider,"policy":data.privacy})).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.ai_subject_heads VALUES($1,$2,$3) ON CONFLICT(kind,subject) DO UPDATE SET job_id=excluded.job_id").bind(kind).bind(subject).bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,error='superseded',completed_at=now() WHERE kind=$1 AND subject=$2 AND id<>$3 AND status IN ('queued','running')").bind(kind).bind(subject).bind(id).execute(&mut *tx).await?;
    if kind == "ai.image_ocr" {
        sqlx::query("INSERT INTO crm.image_ocr_readings(media_id,job_id,input_hash) VALUES($1,$2,$3) ON CONFLICT(media_id) DO UPDATE SET status=CASE WHEN image_ocr_readings.input_hash IS DISTINCT FROM excluded.input_hash THEN 'pending' ELSE image_ocr_readings.status END,job_id=excluded.job_id,input_hash=excluded.input_hash").bind(subject.parse::<Uuid>()?).bind(id).bind(&data.hash).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(id)
}

#[derive(Clone, Debug)]
pub struct Lease {
    pub id: Uuid,
    pub token: Uuid,
    pub kind: String,
    pub subject: String,
    pub model: String,
    pub input: Value,
    pub input_hash: String,
    pub privacy: Value,
}
pub async fn claim(pool: &PgPool) -> Result<Option<Lease>> {
    let token = Uuid::new_v4();
    let row=sqlx::query("WITH candidate AS (SELECT j.id FROM crm.ai_jobs j JOIN crm.ai_subject_heads h ON h.job_id=j.id JOIN crm.ai_settings s ON s.id AND s.enabled WHERE j.attempts<3 AND (j.kind<>'ai.image_ocr' OR EXISTS(SELECT 1 FROM crm.image_ocr_settings o WHERE o.id AND o.enabled AND o.daily_limit>(SELECT count(*) FROM crm.ai_call_reservations WHERE day=(now() AT TIME ZONE 'America/Chicago')::date))) AND (j.status='queued' OR (j.status='running' AND j.lease_until<clock_timestamp())) ORDER BY j.created_at,j.id FOR UPDATE OF j SKIP LOCKED LIMIT 1) UPDATE crm.ai_jobs j SET status='running',token=$1,lease_until=clock_timestamp()+interval '90 seconds',attempts=attempts+1 FROM candidate c WHERE j.id=c.id RETURNING j.*").bind(token).fetch_optional(pool).await?;
    Ok(row.map(|r| Lease {
        id: r.get("id"),
        token,
        kind: r.get("kind"),
        subject: r.get("subject"),
        model: r.get("model"),
        input: r.get("input"),
        input_hash: r.get("input_hash"),
        privacy: r.get("privacy"),
    }))
}
pub async fn heartbeat(pool: &PgPool, lease: &Lease) -> Result<bool> {
    Ok(sqlx::query("UPDATE crm.ai_jobs SET lease_until=clock_timestamp()+interval '90 seconds' WHERE id=$1 AND token=$2 AND status='running' AND lease_until>clock_timestamp()").bind(lease.id).bind(lease.token).execute(pool).await?.rows_affected()==1)
}
pub async fn cancel(pool: &PgPool, id: Uuid) -> Result<bool> {
    Ok(sqlx::query("UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,completed_at=now() WHERE id=$1 AND status IN ('queued','running')").bind(id).execute(pool).await?.rows_affected()==1)
}

/// Model output cannot invent an evidence pointer. Resolution uses the exact
/// snapshot submitted to the provider and retains the immutable evidence text.
pub fn resolve_evidence(input: &Value, refs: &Value) -> Vec<Value> {
    let evidence = input["evidence"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    refs.as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str())
        .filter(|id| seen.insert((*id).to_owned()))
        .filter_map(|id| {
            evidence
                .iter()
                .find(|e| e["id"].as_str() == Some(id))
                .cloned()
                .map(|mut value| {
                    value["quote"] = value["text"].clone();
                    value["discovered"] = json!(false);
                    let source = value["id"].as_str().unwrap_or_default();
                    let profile = input["profile"]["id"].as_str().unwrap_or(source);
                    let citation = match value["kind"].as_str() {
                        Some("post" | "story") => format!("/timeline?event={source}"),
                        Some("profile" | "note") => format!("/profiles/{profile}"),
                        _ => format!(
                            "/messages?conversation={}&message={source}",
                            value["conversation_id"].as_str().unwrap_or_default()
                        ),
                    };
                    value["citation"] = json!(citation);
                    value
                })
        })
        .take(8)
        .collect()
}
pub fn fingerprint(kind: &str, value: &str, date: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let compact = value
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    let normal = crate::collector::casefold(&compact);
    format!(
        "{:x}",
        Sha256::digest(format!("{kind}|{normal}|{}", date.unwrap_or_default()).as_bytes())
    )
}
pub fn validate_output(lease: &Lease, output: Value) -> Result<Value> {
    ensure!(
        serde_json::to_vec(&output)?.len() <= 256 * 1024,
        "model output exceeds budget"
    );
    ensure!(output.is_object(), "model output must be object");
    if lease.kind == "ai.mail_classify" || lease.kind == "ai.mail_events" {
        return mail::validate(lease, output);
    }
    let summary = output["summary"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(4000)
        .collect::<String>();
    if lease.kind == "ai.image_ocr" {
        return Ok(
            json!({"summary":summary,"description":output["description"].as_str().unwrap_or_default().chars().take(1000).collect::<String>(),"text":output["text"].as_str().unwrap_or_default().chars().take(8000).collect::<String>(),"category":output["category"].as_str().unwrap_or("其他")}),
        );
    }
    if lease.kind == "ai.community" {
        let label = output["label"].as_str().unwrap_or_default().trim();
        ensure!(
            !label.is_empty() && label.chars().count() <= 40,
            "invalid community label"
        );
        return Ok(json!({"label":label,"reason":output["reason"].as_str().unwrap_or_default()}));
    }
    if lease.kind == "ai.digest" {
        let evidence = lease.input["evidence"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let life_events = output["life_events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|event| {
                ["new_job","move","travel","wedding","graduation","new_baby","other"].contains(&event["kind"].as_str().unwrap_or_default())
                    && event["text"].is_string() && evidence
                    .iter()
                    .any(|e| e["id"] == event["post_id"] && e["profile_id"] == event["profile_id"])
            })
            .take(30)
            .map(|event| json!({"profile_id":event["profile_id"],"post_id":event["post_id"],"kind":event["kind"],"text":event["text"].as_str().unwrap_or_default().chars().take(2000).collect::<String>()}))
            .collect::<Vec<_>>();
        let fact_people: std::collections::BTreeSet<&str> = [
            "top_contacts",
            "new_people",
            "new_links",
            "upcoming_dates",
            "newly_dormant",
        ]
        .into_iter()
        .flat_map(|key| lease.input["facts"][key].as_array().into_iter().flatten())
        .filter_map(|row| row["profile_id"].as_str())
        .collect();
        let suggestions = output["suggestions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| {
                item["reason"].is_string() && (fact_people.contains(item["profile_id"].as_str().unwrap_or_default())
                    || evidence
                        .iter()
                        .any(|e| e["profile_id"] == item["profile_id"]))
            })
            .take(5)
            .map(|item| json!({"profile_id":item["profile_id"],"reason":item["reason"].as_str().unwrap_or_default().chars().take(2000).collect::<String>()}))
            .collect::<Vec<_>>();
        return Ok(
            json!({"summary":summary,"life_events":life_events,"suggestions":suggestions,"facts":lease.input["facts"]}),
        );
    }
    if lease.kind == "ai.reconnect_one" {
        return Ok(
            json!({"reason":output["reason"].as_str().unwrap_or_default().chars().take(1000).collect::<String>(),"opener":output["opener"].as_str().unwrap_or_default().chars().take(1000).collect::<String>(),"sources":resolve_evidence(&lease.input,&output["sources"])}),
        );
    }
    if lease.kind == "ai.post_media" {
        let evidence = lease.input["evidence"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let items = output["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["id"].is_string() && evidence.iter().any(|e| e["id"] == item["id"]))
            .take(20)
            .cloned()
            .collect::<Vec<_>>();
        return Ok(json!({"summary":summary,"items":items}));
    }
    let mut items = Vec::new();
    for item in output["items"].as_array().into_iter().flatten().take(30) {
        let kind = item["kind"].as_str().unwrap_or_default();
        if ![
            "birthday",
            "residence",
            "hometown",
            "company",
            "job",
            "school",
            "email",
            "phone",
            "food",
            "interests",
            "family",
            "meetup",
        ]
        .contains(&kind)
        {
            continue;
        }
        let value = item["value"].as_str().unwrap_or_default().trim();
        if value.is_empty() || value.chars().count() > 2000 {
            continue;
        }
        let evidence = resolve_evidence(&lease.input, &item["evidence"]);
        if evidence.is_empty()
            || !["high", "medium"].contains(&item["confidence"].as_str().unwrap_or_default())
        {
            continue;
        }
        let date = if kind == "meetup" {
            let Some(day) = item["date"]
                .as_str()
                .and_then(|s| s.parse::<NaiveDate>().ok())
            else {
                continue;
            };
            let today = Utc::now()
                .with_timezone(&chrono_tz::America::Chicago)
                .date_naive();
            if day < today - chrono::Duration::days(120) || day > today + chrono::Duration::days(60)
            {
                continue;
            }
            Some(day.to_string())
        } else {
            None
        };
        if kind == "birthday" && birthday(value).is_none() {
            continue;
        }
        let key = fingerprint(kind, value, date.as_deref());
        if items.iter().any(|i: &Value| i["fingerprint"] == key) {
            continue;
        }
        items.push(json!({"fingerprint":key,"kind":kind,"value":value,"date":date,"evidence":evidence,"quote":item["quote"].as_str().unwrap_or_default().chars().take(80).collect::<String>(),"confidence":item["confidence"]}));
    }
    Ok(
        json!({"summary":summary,"facts":output["facts"].as_array().map(|a|a.iter().take(15).cloned().collect::<Vec<_>>()).unwrap_or_default(),"gaps":output["gaps"].as_array().map(|a|a.iter().filter(|v|v.is_string()).take(20).cloned().collect::<Vec<_>>()).unwrap_or_default(),"sources":resolve_evidence(&lease.input,&output["sources"]),"items":items}),
    )
}
pub fn birthday(value: &str) -> Option<(Option<i32>, u32, u32)> {
    birthday_at(
        value,
        Utc::now()
            .with_timezone(&chrono_tz::America::Chicago)
            .date_naive(),
    )
}
pub fn birthday_at(value: &str, today: NaiveDate) -> Option<(Option<i32>, u32, u32)> {
    let parts = value
        .split('-')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    let (year, month, day) = match parts.as_slice() {
        [m, d] => (None, *m, *d),
        [y, m, d] => (Some(*y as i32), *m, *d),
        _ => return None,
    };
    let date = NaiveDate::from_ymd_opt(year.unwrap_or(2000), month, day)?;
    if year.is_some_and(|y| y < 1900 || date > today) {
        return None;
    }
    Some((year, month, day))
}

pub async fn finish(pool: &PgPool, lease: &Lease, output: Value) -> Result<bool> {
    let memory_generation = output.get("_memory_generation").and_then(Value::as_i64);
    let output = validate_output(lease, output)?;
    let mut tx = pool.begin().await?;
    // Snapshot locks the same subject rows as enqueue and manual writes.
    let current = snapshot(&mut tx, &lease.kind, &lease.subject).await;
    let head: Option<Uuid> = sqlx::query_scalar(
        "SELECT job_id FROM crm.ai_subject_heads WHERE kind=$1 AND subject=$2 FOR UPDATE",
    )
    .bind(&lease.kind)
    .bind(&lease.subject)
    .fetch_optional(&mut *tx)
    .await?;
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.ai_jobs WHERE id=$1 AND token=$2 AND status='running' AND lease_until>clock_timestamp())").bind(lease.id).bind(lease.token).fetch_one(&mut *tx).await?;
    if !valid {
        tx.rollback().await?;
        return Ok(false);
    }
    let data = match current {
        Ok(s) if s.hash == lease.input_hash && head == Some(lease.id) => s,
        _ => {
            sqlx::query("UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,error='input or privacy policy changed',completed_at=now() WHERE id=$1 AND token=$2").bind(lease.id).bind(lease.token).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(false);
        }
    };
    if let Some(expected) = memory_generation {
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM crm.memory_legacy_pending WHERE profile_id=$1)",
        )
        .bind(lease.subject.parse::<Uuid>()?)
        .fetch_one(&mut *tx)
        .await?;
        if pending {
            sqlx::query("UPDATE crm.ai_jobs SET status='queued',token=NULL,lease_until=NULL,attempts=greatest(0,attempts-1) WHERE id=$1 AND token=$2").bind(lease.id).bind(lease.token).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(false);
        }
        let generation: Option<i64> = sqlx::query_scalar(
            "SELECT generation FROM crm.memory_heads WHERE profile_id=$1 FOR SHARE",
        )
        .bind(lease.subject.parse::<Uuid>()?)
        .fetch_optional(&mut *tx)
        .await?;
        if generation != Some(expected) {
            sqlx::query("UPDATE crm.ai_jobs SET status='queued',token=NULL,lease_until=NULL,attempts=greatest(0,attempts-1) WHERE id=$1 AND token=$2").bind(lease.id).bind(lease.token).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(false);
        }
    }
    sqlx::query("INSERT INTO crm.ai_outputs(kind,subject,output,model,input_hash,source_versions,job_id) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(kind,subject) DO UPDATE SET output=excluded.output,model=excluded.model,input_hash=excluded.input_hash,source_versions=excluded.source_versions,job_id=excluded.job_id,generated_at=now()")
        .bind(&lease.kind).bind(&lease.subject).bind(&output).bind(&lease.model).bind(&lease.input_hash).bind(&data.versions).bind(lease.id).execute(&mut *tx).await?;
    match lease.kind.as_str() {
        "ai.mail_classify" | "ai.mail_events" => {
            mail::publish(&mut tx, lease, &output, &data.versions).await?;
        }
        "ai.profile" => {
            let id: Uuid = lease.subject.parse()?;
            sqlx::query("DELETE FROM crm.profile_suggestions WHERE profile_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            for item in output["items"].as_array().into_iter().flatten() {
                sqlx::query("INSERT INTO crm.profile_suggestions(profile_id,fingerprint,kind,value,date,evidence,quote,confidence,input_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING").bind(id).bind(item["fingerprint"].as_str()).bind(item["kind"].as_str()).bind(item["value"].as_str()).bind(item["date"].as_str().map(str::parse::<NaiveDate>).transpose()?).bind(&item["evidence"]).bind(item["quote"].as_str()).bind(item["confidence"].as_str()).bind(&lease.input_hash).execute(&mut *tx).await?;
            }
        }
        "ai.person_week" => {
            let (id, week) = lease.subject.split_once(':').context("week subject")?;
            sqlx::query("INSERT INTO crm.person_weekly_updates(profile_id,week_start,output,input_hash) VALUES($1,$2,$3,$4) ON CONFLICT(profile_id,week_start) DO UPDATE SET output=excluded.output,input_hash=excluded.input_hash,generated_at=now()").bind(id.parse::<Uuid>()?).bind(week.parse::<NaiveDate>()?).bind(&output).bind(&lease.input_hash).execute(&mut *tx).await?;
        }
        "ai.digest" => {
            sqlx::query("INSERT INTO crm.weekly_digests(week_start,output,input_hash) VALUES($1,$2,$3) ON CONFLICT(week_start) DO UPDATE SET output=excluded.output,input_hash=excluded.input_hash,generated_at=now()").bind(lease.subject.parse::<NaiveDate>()?).bind(&output).bind(&lease.input_hash).execute(&mut *tx).await?;
        }
        "ai.image_ocr" => {
            sqlx::query("UPDATE crm.image_ocr_readings SET status='complete',summary=$3,description=$4,category=$5,text=$6,model=$7,generated_at=now(),error=NULL WHERE media_id=$1 AND job_id=$2").bind(lease.subject.parse::<Uuid>()?).bind(lease.id).bind(output["summary"].as_str()).bind(output["description"].as_str()).bind(output["category"].as_str()).bind(output["text"].as_str()).bind(&lease.model).execute(&mut *tx).await?;
        }
        "ai.post_media" => {
            sqlx::query("INSERT INTO crm.media_readings(entity_id,entity_type,status,summary,items,model,generated_at,input_hash,job_id) VALUES($1,$2,'complete',$3,$4,$5,now(),$6,$7) ON CONFLICT(entity_id,entity_type) DO UPDATE SET status='complete',summary=excluded.summary,items=excluded.items,model=excluded.model,generated_at=now(),input_hash=excluded.input_hash,job_id=excluded.job_id").bind(lease.subject.parse::<Uuid>()?).bind(if lease.input["post"]["kind"]=="story"{"social_story"}else{"social_post"}).bind(output["summary"].as_str()).bind(&output["items"]).bind(&lease.model).bind(&lease.input_hash).bind(lease.id).execute(&mut *tx).await?;
        }
        _ => {}
    }
    let changed=sqlx::query("UPDATE crm.ai_jobs SET status='complete',token=NULL,lease_until=NULL,completed_at=now(),error=NULL WHERE id=$1 AND token=$2 AND lease_until>clock_timestamp()").bind(lease.id).bind(lease.token).execute(&mut *tx).await?.rows_affected();
    if changed != 1 {
        tx.rollback().await?;
        return Ok(false);
    }
    tx.commit().await?;
    Ok(true)
}

pub fn background_resources() -> crate::resources::TaskResources {
    crate::resources::TaskResources {
        cpu: 1,
        network: 1,
        memory_bytes: 128 * 1024 * 1024,
        ..Default::default()
    }
}
pub async fn run_one<P: provider::Provider>(pool: &PgPool, provider: &P) -> Result<bool> {
    run_with_media(pool, provider, None, None).await
}
pub async fn run_with_media<P: provider::Provider>(
    pool: &PgPool,
    provider: &P,
    media_root: Option<&std::path::Path>,
    resources: Option<&crate::resources::ResourceBudget>,
) -> Result<bool> {
    let _resources = match resources {
        Some(resources) => Some(resources.acquire_background(background_resources()).await?),
        None => None,
    };
    let _model_slot = slots::background().await?;
    let Some(lease) = claim(pool).await? else {
        return Ok(false);
    };
    if lease.privacy["provider"].as_str() != Some(provider.name()) {
        fail(pool, &lease, "provider configuration unavailable").await?;
        return Ok(true);
    }
    if lease.kind == "ai.image_ocr" && !reserve_ocr(pool, &lease).await? {
        return Ok(false);
    }
    let mut submitted = lease.clone();
    let mut memory = None;
    if lease.kind == "ai.profile" {
        let mut tx = pool.begin().await?;
        memory::profile_facts(&mut tx, lease.subject.parse()?).await?;
        tx.commit().await?;
        match memory::refresh(pool, provider, &lease).await? {
            memory::Refresh::NoInputs => {}
            memory::Refresh::Pending => {
                sqlx::query("UPDATE crm.ai_jobs SET status='queued',token=NULL,lease_until=NULL,attempts=greatest(0,attempts-1) WHERE id=$1 AND token=$2 AND lease_until>clock_timestamp()").bind(lease.id).bind(lease.token).execute(pool).await?;
                return Ok(true);
            }
            memory::Refresh::Ready { output, generation } => {
                submitted.input["memory"] = output.clone();
                memory = Some((output, generation));
            }
        }
    }
    if lease.kind == "ai.image_ocr"
        && provider.name() == "openai"
        && hydrate_image(&mut submitted, media_root, resources)
            .await
            .is_err()
    {
        fail(pool, &lease, "image unavailable or exceeds vision budget").await?;
        return Ok(true);
    }
    if let Some((ref overview, generation)) = memory {
        let saved:Option<Value>=sqlx::query_scalar("SELECT output FROM crm.ai_visible_outputs WHERE kind='ai.profile' AND subject=$1 AND input_hash=$2").bind(&lease.subject).bind(&lease.input_hash).fetch_optional(pool).await?;
        if let Some(mut output) = saved {
            for key in ["summary", "facts", "gaps"] {
                output[key] = overview[key].clone();
            }
            output["_memory_generation"] = json!(generation);
            finish(pool, &lease, output).await?;
            return Ok(true);
        }
    }
    let call = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        provider.complete(&submitted),
    );
    tokio::pin!(call);
    let result = loop {
        tokio::select! {
            result=&mut call=>break result,
            _=tokio::time::sleep(std::time::Duration::from_secs(20))=>{if !heartbeat(pool,&lease).await?{return Ok(true)}}
        }
    };
    match result {
        Ok(Ok(mut output)) => {
            if let Some((memory, generation)) = memory
                && output.is_object()
            {
                for key in ["summary", "facts", "gaps"] {
                    output[key] = memory[key].clone();
                }
                output["_memory_generation"] = json!(generation);
            }
            if let Err(_error) = finish(pool, &lease, output).await {
                fail(pool, &lease, "invalid model result").await?;
            }
        }
        _ => {
            fail(pool, &lease, "model call failed").await?;
        }
    }
    Ok(true)
}
pub async fn fail(pool: &PgPool, lease: &Lease, message: &'static str) -> Result<()> {
    // Never persist arbitrary provider errors: upstream bodies can echo secrets.
    let mut tx = pool.begin().await?;
    let n=sqlx::query("UPDATE crm.ai_jobs SET status='failed',error=$3,token=NULL,lease_until=NULL,completed_at=now() WHERE id=$1 AND token=$2 AND status='running' AND lease_until>clock_timestamp()").bind(lease.id).bind(lease.token).bind(message).execute(&mut *tx).await?.rows_affected();
    if n == 1 && lease.kind == "ai.image_ocr" {
        sqlx::query("UPDATE crm.image_ocr_readings SET status='failed',error=$2 WHERE job_id=$1")
            .bind(lease.id)
            .bind(message)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn reserve_ocr(pool: &PgPool, lease: &Lease) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let limit: i32 = sqlx::query_scalar(
        "SELECT daily_limit FROM crm.image_ocr_settings WHERE id AND enabled FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(0);
    let used:i64=sqlx::query_scalar("SELECT count(*) FROM crm.ai_call_reservations WHERE day=(now() AT TIME ZONE 'America/Chicago')::date").fetch_one(&mut *tx).await?;
    if used >= limit as i64 {
        sqlx::query("UPDATE crm.ai_jobs SET status='queued',attempts=greatest(0,attempts-1),token=NULL,lease_until=NULL WHERE id=$1 AND token=$2").bind(lease.id).bind(lease.token).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query("INSERT INTO crm.ai_call_reservations(job_id,day) VALUES($1,(now() AT TIME ZONE 'America/Chicago')::date) ON CONFLICT DO NOTHING").bind(lease.id).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.image_ocr_readings SET status='running' WHERE job_id=$1")
        .bind(lease.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}
async fn hydrate_image(
    lease: &mut Lease,
    root: Option<&std::path::Path>,
    resources: Option<&crate::resources::ResourceBudget>,
) -> Result<()> {
    use base64::Engine;
    let root = root.context("media root is required")?;
    let hash = lease.input["media"]["sha256"]
        .as_str()
        .context("missing media hash")?;
    let path = root.join(crate::media::object_path(hash)?);
    let meta = tokio::fs::metadata(&path).await?;
    ensure!(meta.len() <= 10 * 1024 * 1024, "vision image too large");
    let _disk = match resources {
        Some(r) => Some(
            r.acquire(crate::resources::TaskResources {
                disk: 1,
                ..Default::default()
            })
            .await?,
        ),
        None => None,
    };
    let bytes = tokio::fs::read(path).await?;
    drop(_disk);
    ensure!(
        crate::identity::bytes_hash(&bytes) == hash,
        "image content changed"
    );
    lease.input["image_data"] = json!(format!(
        "data:{};base64,{}",
        lease.input["media"]["media_type"]
            .as_str()
            .context("media type")?,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ));
    Ok(())
}
/// The scheduler owns this future. Disabled settings or absent runtime keys do
/// no external work; injected FakeProvider is used only by explicit tests.
pub async fn run_configured(
    pool: &PgPool,
    media_root: &std::path::Path,
    resources: &crate::resources::ResourceBudget,
    cipher: Option<&crate::credentials::CredentialCipher>,
) -> Result<bool> {
    let Some(provider) = configured_provider(pool, cipher).await? else {
        return Ok(false);
    };
    schedule_due(pool).await?;
    dispatch_one(pool).await?;
    mail::enqueue_pending(pool).await?;
    memory::enqueue_pending(pool).await?;
    run_with_media(pool, &provider, Some(media_root), Some(resources)).await
}

/// Environment configuration may override an installation's encrypted settings.
/// Resolving configuration never probes or calls the external model service.
pub async fn configured_provider(
    pool: &PgPool,
    cipher: Option<&crate::credentials::CredentialCipher>,
) -> Result<Option<provider::OpenAiProvider>> {
    let row = sqlx::query("SELECT base_url,credential_ciphertext FROM crm.ai_settings WHERE id AND enabled AND provider='openai'")
        .fetch_optional(pool).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let key = match std::env::var("MONICA_AI_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(key) => Some(key),
        None => match (
            cipher,
            row.get::<Option<String>, _>("credential_ciphertext"),
        ) {
            (Some(cipher), Some(encrypted)) => cipher.decrypt(&encrypted)?["api_key"]
                .as_str()
                .map(str::to_owned),
            _ => None,
        },
    };
    let Some(key) = key else {
        return Ok(None);
    };
    let base = std::env::var("MONICA_AI_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or(row.get::<Option<String>, _>("base_url"))
        .context("model base URL required")?;
    Ok(Some(provider::OpenAiProvider::new(&base, key)?))
}

pub async fn current_period(pool: &PgPool, kind: &str, force: bool) -> Result<Option<String>> {
    ensure!(
        SCHEDULED.contains(&kind) || kind == "ai.image_ocr",
        "unknown model schedule"
    );
    let local: chrono::NaiveDateTime =
        sqlx::query_scalar("SELECT now() AT TIME ZONE 'America/Chicago'")
            .fetch_one(pool)
            .await?;
    let local = if force {
        local.date().and_hms_opt(23, 59, 0).context("local time")?
    } else {
        local
    };
    let (hour, weekday, hourly) = match kind {
        "ai.community_names" => (3, None, false),
        "ai.reconnect" => (6, None, false),
        "ai.digest" | "ai.person_weeks" => (8, Some(0), false),
        "ai.profiles" => (1, Some(6), false),
        "ai.post_media" => (20, None, true),
        "ai.image_ocr" => (0, None, true),
        _ => unreachable!(),
    };
    if hourly {
        return Ok((local.minute() >= hour).then(|| local.format("%Y-%m-%dT%H").to_string()));
    }
    if let Some(day) = weekday {
        let date = local.date()
            - chrono::Duration::days(i64::from(
                (local.weekday().num_days_from_monday() + 7 - day) % 7,
            ));
        if local.date() == date && local.hour() < hour {
            return Ok(None);
        }
        return Ok(Some(date.to_string()));
    }
    Ok((local.hour() >= hour).then(|| local.date().to_string()))
}
pub async fn enqueue_dispatch(
    pool: &PgPool,
    kind: &str,
    period: &str,
    refresh: bool,
) -> Result<Uuid> {
    ensure!(
        SCHEDULED.contains(&kind) || kind == "ai.image_ocr",
        "unknown model schedule"
    );
    if kind == "ai.post_media" || kind == "ai.image_ocr" {
        chrono::NaiveDateTime::parse_from_str(&format!("{period}:00:00"), "%Y-%m-%dT%H:%M:%S")?;
    } else {
        period.parse::<NaiveDate>()?;
    }
    Ok(sqlx::query_scalar("INSERT INTO crm.ai_dispatches(id,kind,period) VALUES($1,$2,$3) ON CONFLICT(kind,period) DO UPDATE SET status=CASE WHEN $4 AND ai_dispatches.status='complete' THEN 'queued' ELSE ai_dispatches.status END,cursor=CASE WHEN $4 AND ai_dispatches.status='complete' THEN NULL ELSE ai_dispatches.cursor END RETURNING id").bind(Uuid::new_v4()).bind(kind).bind(period).bind(refresh).fetch_one(pool).await?)
}
pub async fn schedule_due(pool: &PgPool) -> Result<()> {
    for kind in SCHEDULED
        .iter()
        .copied()
        .chain(std::iter::once("ai.image_ocr"))
    {
        if kind == "ai.image_ocr"
            && !sqlx::query_scalar::<_, bool>("SELECT enabled FROM crm.image_ocr_settings WHERE id")
                .fetch_one(pool)
                .await?
        {
            continue;
        }
        if let Some(period) = current_period(pool, kind, false).await? {
            enqueue_dispatch(pool, kind, &period, false).await?;
        }
    }
    Ok(())
}
/// Dispatch at most25 subjects per transaction-free turn. Cursor and child
/// enqueue are replay-safe; there is no1000-person truncation or long DB lock.
pub async fn dispatch_one(pool: &PgPool) -> Result<bool> {
    let token = Uuid::new_v4();
    let row=sqlx::query("WITH candidate AS (SELECT d.id FROM crm.ai_dispatches d JOIN crm.ai_settings s ON s.id AND s.enabled WHERE d.status='queued' OR (d.status='running' AND d.lease_until<clock_timestamp()) ORDER BY d.created_at,d.id FOR UPDATE OF d SKIP LOCKED LIMIT 1) UPDATE crm.ai_dispatches d SET status='running',token=$1,lease_until=clock_timestamp()+interval '2 minutes' FROM candidate c WHERE d.id=c.id RETURNING d.*").bind(token).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(false) };
    let id: Uuid = row.get("id");
    let kind: String = row.get("kind");
    let period: String = row.get("period");
    let cursor: Option<String> = row.get("cursor");
    let (child,subjects):(String,Vec<String>)=match kind.as_str(){
        "ai.digest"=>{let week=period.parse::<NaiveDate>()?-chrono::Duration::days(7);(kind.clone(),if cursor.is_none(){vec![week.to_string()]}else{vec![]})},
        "ai.community_names"=>("ai.community".into(),sqlx::query_scalar("SELECT community_id FROM crm.ai_community_inputs WHERE ($1::text IS NULL OR community_id>$1) ORDER BY community_id LIMIT 25").bind(&cursor).fetch_all(pool).await?),
        "ai.image_ocr"=>(kind.clone(),sqlx::query_scalar("SELECT id::text FROM crm.media_assets m WHERE media_type LIKE 'image/%' AND ($1::text IS NULL OR id::text>$1) AND EXISTS(SELECT 1 FROM crm.media_links l WHERE l.media_id=m.id AND l.entity_type IN ('profile','social_post','social_story','post','story')) ORDER BY id::text LIMIT 25").bind(&cursor).fetch_all(pool).await?),
        "ai.post_media"=>(kind.clone(),sqlx::query_scalar("SELECT id::text FROM crm.timeline_events WHERE status='active' AND ($1::text IS NULL OR id::text>$1) ORDER BY id::text LIMIT 25").bind(&cursor).fetch_all(pool).await?),
        _=>{let child=if kind=="ai.person_weeks"{"ai.person_week"}else if kind=="ai.reconnect"{"ai.reconnect_one"}else{"ai.profile"};let ids:Vec<String>=sqlx::query_scalar("SELECT id::text FROM crm.profiles WHERE profile_type='person' AND archived_at IS NULL AND ($1::text IS NULL OR id::text>$1) ORDER BY id::text LIMIT 25").bind(cursor.as_ref().map(|c|c.split(':').next().unwrap_or(c))).fetch_all(pool).await?;let week=if kind=="ai.person_weeks"{Some(period.parse::<NaiveDate>()?-chrono::Duration::days(7))}else{None};(child.into(),ids.into_iter().map(|id|week.map_or(id.clone(),|week|format!("{id}:{week}"))).collect())}
    };
    let mut errors = 0i64;
    for subject in &subjects {
        if enqueue(pool, &child, subject).await.is_err() {
            errors += 1;
        }
    }
    let complete = subjects.len() < 25;
    let last = subjects.last().or(cursor.as_ref());
    sqlx::query("UPDATE crm.ai_dispatches SET status=$3,cursor=$4,processed_count=processed_count+$5,error_count=error_count+$6,token=NULL,lease_until=NULL,completed_at=CASE WHEN $7 THEN now() ELSE NULL END WHERE id=$1 AND token=$2 AND lease_until>clock_timestamp()").bind(id).bind(token).bind(if complete{"complete"}else{"queued"}).bind(last).bind(subjects.len() as i64).bind(errors).bind(complete).execute(pool).await?;
    Ok(true)
}

pub fn previous_week() -> NaiveDate {
    let today = Utc::now()
        .with_timezone(&chrono_tz::America::Chicago)
        .date_naive();
    today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64 + 7)
}
pub async fn today(pool: &PgPool) -> Result<NaiveDate> {
    Ok(
        sqlx::query_scalar("SELECT (now() AT TIME ZONE 'America/Chicago')::date")
            .fetch_one(pool)
            .await?,
    )
}
pub async fn previous_week_for(pool: &PgPool) -> Result<NaiveDate> {
    let today = today(pool).await?;
    Ok(today - chrono::Duration::days(i64::from(today.weekday().num_days_from_monday()) + 7))
}

pub(crate) fn optional_bool(name: &str) -> Result<Option<bool>> {
    match std::env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "" => Ok(None),
            "true" | "1" => Ok(Some(true)),
            "false" | "0" => Ok(Some(false)),
            _ => anyhow::bail!("{name} must be true or false"),
        },
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => anyhow::bail!("invalid configuration encoding for {name}"),
    }
}
/// Apply only explicitly supplied configuration. Absent variables preserve
/// persisted user choices across native restarts; environment keys are not persisted.
pub async fn configure_from_env(pool: &PgPool) -> Result<()> {
    let enabled = optional_bool("MONICA_AI_ENABLED")?;
    let notes = optional_bool("MONICA_AI_INCLUDE_PRIVATE_NOTES")?;
    let messages = optional_bool("MONICA_AI_INCLUDE_MESSAGES")?;
    let model = std::env::var("MONICA_AI_MODEL")
        .ok()
        .filter(|s| !s.is_empty());
    let base = std::env::var("MONICA_AI_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty());
    let provider = std::env::var("MONICA_AI_PROVIDER")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| (enabled == Some(true)).then(|| "openai".into()));
    if let Some(value) = &model {
        ensure!(
            !value.trim().is_empty() && value.len() <= 255,
            "invalid MONICA_AI_MODEL"
        )
    }
    if let Some(value) = &provider {
        ensure!(
            ["openai", "fake"].contains(&value.as_str()),
            "invalid MONICA_AI_PROVIDER"
        )
    }
    if let Some(value) = &base {
        provider::OpenAiProvider::new(value, "configuration-validation-only".into())?;
    }
    let ocr_enabled = optional_bool("MONICA_OCR_ENABLED")?;
    let chat = optional_bool("MONICA_OCR_INCLUDE_CHAT")?;
    let ocr_model = std::env::var("MONICA_OCR_MODEL")
        .ok()
        .filter(|s| !s.is_empty());
    let limit = std::env::var("MONICA_OCR_DAILY_LIMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .transpose()?;
    if let Some(value) = limit {
        ensure!(
            (1..=100000).contains(&value),
            "invalid MONICA_OCR_DAILY_LIMIT"
        )
    }
    if let Some(value) = &ocr_model {
        ensure!(
            !value.trim().is_empty() && value.len() <= 255,
            "invalid MONICA_OCR_MODEL"
        )
    }
    let mut tx = pool.begin().await?;
    if enabled.is_some()
        || notes.is_some()
        || messages.is_some()
        || model.is_some()
        || base.is_some()
        || provider.is_some()
    {
        sqlx::query("UPDATE crm.ai_settings SET enabled=coalesce($1,enabled),provider=coalesce($2,provider),model=coalesce($3,model),base_url=coalesce($4,base_url),include_private_notes=coalesce($5,include_private_notes),include_messages=coalesce($6,include_messages),updated_at=now() WHERE id").bind(enabled).bind(provider).bind(model).bind(base).bind(notes).bind(messages).execute(&mut *tx).await?;
    }
    if ocr_enabled.is_some() || chat.is_some() || ocr_model.is_some() || limit.is_some() {
        sqlx::query("UPDATE crm.image_ocr_settings SET enabled=coalesce($1,enabled),include_chat=coalesce($2,include_chat),model=coalesce($3,model),daily_limit=coalesce($4,daily_limit),updated_at=now() WHERE id").bind(ocr_enabled).bind(chat).bind(ocr_model).bind(limit).execute(&mut *tx).await?;
    }
    let qa = optional_bool("MONICA_AI_QA_ENABLED")?;
    let context = std::env::var("MONICA_AI_QA_CONTEXT_WINDOW")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .transpose()?;
    let max_tokens = std::env::var("MONICA_AI_QA_MAX_TOKENS")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .transpose()?;
    ensure!(
        context.is_none_or(|n| (1024..=2097152).contains(&n))
            && max_tokens.is_none_or(|n| (1..=131072).contains(&n)),
        "invalid QA model limits"
    );
    if qa.is_some() || context.is_some() || max_tokens.is_some() {
        sqlx::query("UPDATE crm.ai_settings SET qa_enabled=coalesce($1,qa_enabled),context_window=coalesce($2,context_window),max_tokens=coalesce($3,max_tokens),updated_at=now() WHERE id").bind(qa).bind(context).bind(max_tokens).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

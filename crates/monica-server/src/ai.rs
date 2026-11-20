//! Authenticated AI reads and durable dispatch. No model call occurs in routes.
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch, post},
};
use chrono::NaiveDate;
use monica_core::ai;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;
#[path = "ai_browser.rs"]
mod browser;
#[path = "ai_settings.rs"]
mod settings;
type Result<T> = std::result::Result<T, ApiError>;
fn invalid(message: &'static str) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message)
}
fn missing(message: &'static str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, message)
}
fn service(error: anyhow::Error) -> ApiError {
    tracing::warn!(%error,"model operation rejected");
    ApiError(
        StatusCode::CONFLICT,
        "Model work unavailable for this input or policy",
    )
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(browser::routes())
        .merge(settings::routes())
        .route("/api/v1/ai/status", get(status))
        .route("/api/v1/ai/jobs/{kind}", post(dispatch))
        .route("/api/v1/jobs/status", get(jobs))
        .route("/api/v1/ai/image-ocr", patch(ocr_controls))
        .route("/api/v1/ai/profiles/{id}", get(profile))
        .route("/api/v1/ai/profiles/{id}/refresh", post(refresh))
        .route("/api/v1/ai/suggestions", get(suggestions))
        .route(
            "/api/v1/ai/suggestions/{id}/{key}",
            post(decide).delete(undo),
        )
        .route("/api/v1/digests", get(digests))
        .route("/api/v1/digests/{week}", get(digest))
        .route("/api/v1/ai/weekly-people", get(weekly))
        .route("/api/v1/ai/weekly-highlights", get(highlights))
        .route("/api/v1/reconnect", get(reconnect))
        .route(
            "/api/v1/reconnect/{id}",
            post(reconnect_action).delete(reconnect_undo),
        )
}
async fn enabled(s: &AppState) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT enabled FROM crm.ai_settings WHERE id")
            .fetch_one(&s.pool)
            .await?,
    )
}
async fn ocr_progress(s: &AppState) -> Result<Value> {
    Ok(sqlx::query_scalar("WITH counts AS (SELECT status,count(*) n FROM crm.image_ocr_readings GROUP BY status), attempts AS (SELECT count(*) n FROM crm.ai_call_reservations WHERE day=(now() AT TIME ZONE 'America/Chicago')::date) SELECT jsonb_build_object('enabled',s.enabled,'include_chat',s.include_chat,'model',s.model,'daily_limit',s.daily_limit,'target_days',0,'baseline_images',coalesce((SELECT sum(n) FROM counts),0),'counts',coalesce((SELECT jsonb_object_agg(status,n) FROM counts),'{}'::jsonb),'unlimited',false,'attempted_today',a.n,'daily_remaining',greatest(0,s.daily_limit-a.n),'estimated_days_remaining',ceil(coalesce((SELECT sum(n) FROM counts WHERE status IN ('pending','running')),0)::numeric/s.daily_limit)) FROM crm.image_ocr_settings s CROSS JOIN attempts a WHERE id").fetch_one(&s.pool).await?)
}
async fn status(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Ok(Json(json!({"enabled":false})));
    }
    let config = sqlx::query("SELECT model,model_routes,provider,execution_enabled,base_url,credential_ciphertext IS NOT NULL AS has_key FROM crm.ai_settings WHERE id")
        .fetch_one(&s.pool)
        .await?;
    let model: String = config.get("model");
    let routes: Value = config.get("model_routes");
    let vision: String = sqlx::query_scalar("SELECT model FROM crm.image_ocr_settings WHERE id")
        .fetch_one(&s.pool)
        .await?;
    let mut rows = Vec::new();
    for kind in ai::SCHEDULED {
        let row:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('job_type',kind,'period',period,'status',status,'completed_at',completed_at,'error',CASE WHEN error_count>0 THEN 'Some subjects could not be queued' END) FROM crm.ai_dispatches WHERE kind=$1 ORDER BY created_at DESC LIMIT 1").bind(kind).fetch_optional(&s.pool).await?;
        rows.push(row.unwrap_or(json!({"job_type":kind})));
    }
    // Reachability means a configured transport has completed, not a probe to a
    // paid/remote endpoint merely because a settings page was opened.
    let reachable: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.ai_jobs WHERE status='complete' AND privacy->>'provider'='openai')")
            .fetch_one(&s.pool)
            .await?;
    let configured = config.get::<String, _>("provider") == "openai"
        && (config.get::<Option<String>, _>("base_url").is_some()
            || std::env::var("MONICA_AI_BASE_URL").is_ok_and(|s| !s.is_empty()))
        && (config.get::<bool, _>("has_key")
            || std::env::var("MONICA_AI_API_KEY").is_ok_and(|s| !s.is_empty()));
    Ok(Json(
        json!({"enabled":true,"execution_enabled":config.get::<bool,_>("execution_enabled"),"configured":configured,"reachable":reachable,"image_ocr":ocr_progress(&s).await?,"models":{"batch":ai::models::resolve(&model,&routes,"ai.memory_chunk"),"writer":ai::models::resolve(&model,&routes,"ai.digest"),"person":ai::models::resolve(&model,&routes,"ai.profile"),"qa":ai::models::resolve(&model,&routes,"ai.qa"),"vision":vision},"jobs":rows}),
    ))
}
async fn jobs(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let queues:Vec<Value>=sqlx::query_scalar("WITH work AS (SELECT kind job_type,'ai' resource_class,status,created_at,lease_until,completed_at FROM crm.ai_jobs UNION ALL SELECT kind,'ai',status,created_at,lease_until,completed_at FROM crm.ai_dispatches UNION ALL SELECT 'source.project.'||provider,'cpu',state,NULL,lease_until,NULL FROM crm.jobs UNION ALL SELECT 'message.summary','cpu',CASE WHEN token IS NULL THEN 'queued' ELSE 'running' END,NULL,lease_until,NULL FROM crm.conversation_work UNION ALL SELECT 'adapter.'||provider,'network',status,created_at,lease_until,completed_at FROM crm.adapter_runs) SELECT jsonb_build_object('job_type',job_type,'resource_class',resource_class,'status',status,'count',count(*),'oldest_due_at',min(created_at) FILTER(WHERE status='queued'),'oldest_heartbeat_at',min(lease_until)-interval '90 seconds','last_success_at',max(completed_at) FILTER(WHERE status='complete'),'expired_leases',count(*) FILTER(WHERE status='running' AND lease_until<now())) FROM work GROUP BY job_type,resource_class,status ORDER BY job_type,status").fetch_all(&s.pool).await?;
    let outputs:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('kind',kind,'last_generated_at',max(generated_at),'generated_last_24h',count(*) FILTER(WHERE generated_at>now()-interval '24 hours')) FROM crm.ai_visible_outputs GROUP BY kind ORDER BY kind").fetch_all(&s.pool).await?;
    let workers:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('worker_id',instance_id,'resource_class','scheduler','heartbeat_at',heartbeat_at,'job_id',NULL,'draining',state IN ('draining','stopping'),'stale',heartbeat_at<now()-interval '2 minutes','active_jobs',active_jobs) FROM crm.scheduler_runs WHERE stopped_at IS NULL ORDER BY started_at").fetch_all(&s.pool).await?;
    let sources:Vec<Value>=sqlx::query_scalar("SELECT DISTINCT ON(provider) jsonb_build_object('source',provider,'phase','collector','status',status,'started_at',started_at,'heartbeat_at',heartbeat_at,'details',progress,'stale',status='running' AND heartbeat_at<now()-interval '2 minutes','progress_stalled',status='running' AND coalesce((progress->>'last_progress_at')::timestamptz,started_at)<now()-interval '15 minutes') FROM crm.adapter_runs ORDER BY provider,created_at DESC").fetch_all(&s.pool).await?;
    let watermarks:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('source',source,'stream',stream,'last_import_at',updated_at) FROM crm.source_cursors ORDER BY source,stream").fetch_all(&s.pool).await?;
    Ok(Json(
        json!({"queues":queues,"workers":workers,"sources":sources,"watermarks":watermarks,"outputs":outputs,"scheduled_status_meaning":"Parent completion means dispatch completed; inspect child queues separately."}),
    ))
}
#[derive(Deserialize)]
struct OcrControls {
    daily_limit: Option<i32>,
    enabled: Option<bool>,
    include_chat: Option<bool>,
}
async fn ocr_controls(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(body): Json<OcrControls>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    if !enabled(&s).await? {
        return Err(missing("AI is disabled"));
    }
    if body.daily_limit.is_some_and(|v| !(1..=100000).contains(&v)) {
        return Err(invalid("invalid daily limit"));
    }
    let mut tx = s.pool.begin().await?;
    sqlx::query("UPDATE crm.image_ocr_settings SET daily_limit=coalesce($1,daily_limit),enabled=coalesce($2,enabled),include_chat=coalesce($3,include_chat),updated_at=now() WHERE id").bind(body.daily_limit).bind(body.enabled).bind(body.include_chat).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,metadata) VALUES($1,'ai.ocr.controls','settings',$2)").bind(actor).bind(json!({"daily_limit":body.daily_limit,"enabled":body.enabled,"include_chat":body.include_chat})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(ocr_progress(&s).await?))
}
async fn refresh(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>)> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Err(missing("Unknown profile or model features are off"));
    }
    ai::enqueue(&s.pool, "ai.profile", &id.to_string())
        .await
        .map_err(service)?;
    ai::enqueue(
        &s.pool,
        "ai.person_week",
        &format!(
            "{id}:{}",
            ai::previous_week_for(&s.pool).await.map_err(service)?
        ),
    )
    .await
    .map_err(service)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"profile_id":id,"queued":true})),
    ))
}
async fn dispatch(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(kind): Path<String>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? || !ai::SCHEDULED.contains(&kind.as_str()) {
        return Err(missing("Unknown or disabled model job"));
    }
    let period = ai::current_period(&kind, true)
        .map_err(service)?
        .ok_or(missing("Unknown or disabled model job"))?;
    let id = ai::enqueue_dispatch(&s.pool, &kind, &period, true)
        .await
        .map_err(service)?;
    Ok(Json(json!({"job_id":id,"job_type":kind,"period":period})))
}

fn normal(value: &str) -> String {
    monica_core::identity::casefold(
        &value
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>(),
    )
}
fn label(kind: &str) -> &str {
    match kind {
        "birthday" => "生日",
        "residence" => "常驻地",
        "hometown" => "家乡",
        "company" => "公司",
        "job" => "职位",
        "school" => "学校",
        "email" => "邮箱",
        "phone" => "电话",
        "food" => "饮食偏好",
        "interests" => "兴趣",
        "family" => "家人",
        "meetup" => "见面 / 聚会",
        _ => kind,
    }
}
fn field_key(kind: &str) -> String {
    if kind == "food" {
        "monica.food_preferences".into()
    } else {
        format!("monica.{kind}")
    }
}
async fn pending(s: &AppState, id: Option<Uuid>) -> Result<Vec<Value>> {
    let mut rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'generated_at',max(v.generated_at),'current',(SELECT jsonb_object_agg(c.field_key,r.value) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=p.id),'birthday',(SELECT jsonb_build_object('month',month,'day',day) FROM crm.profile_dates WHERE profile_id=p.id AND kind='birthday'),'items',jsonb_agg(jsonb_build_object('fingerprint',v.fingerprint,'kind',v.kind,'value',v.value,'date',v.date,'evidence',v.evidence,'quote',v.quote,'confidence',v.confidence) ORDER BY v.generated_at DESC,v.fingerprint)) FROM crm.profile_suggestions v JOIN crm.profiles p ON p.id=v.profile_id WHERE p.archived_at IS NULL AND ($1::uuid IS NULL OR p.id=$1) AND EXISTS(SELECT 1 FROM crm.ai_visible_outputs o WHERE o.kind IN ('ai.profile','ai.profile_updates') AND o.subject=p.id::text AND o.input_hash=v.input_hash) AND NOT EXISTS(SELECT 1 FROM crm.profile_suggestion_decisions d WHERE d.profile_id=p.id AND d.fingerprint=v.fingerprint) GROUP BY p.id ORDER BY max(v.generated_at) DESC,p.id").bind(id).fetch_all(&s.pool).await?;
    let profiles: Vec<Uuid> = rows
        .iter()
        .filter_map(|p| p["profile_id"].as_str()?.parse().ok())
        .collect();
    let structured = ai::known::structured(&s.pool, &profiles).await?;
    for person in &mut rows {
        let profile = person["profile_id"]
            .as_str()
            .and_then(|id| id.parse::<Uuid>().ok());
        let facts = profile
            .as_ref()
            .and_then(|id| structured.get(id))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let known = person
            .as_object_mut()
            .unwrap()
            .remove("current")
            .unwrap_or(Value::Null);
        let birthday = person
            .as_object_mut()
            .unwrap()
            .remove("birthday")
            .unwrap_or(Value::Null);
        let items = person["items"].as_array_mut().expect("SQL array");
        items.retain_mut(|item| {
            let kind = item["kind"].as_str().unwrap_or_default().to_string();
            let value = item["value"].as_str().unwrap_or_default();
            let duplicate = if kind == "meetup" {
                false
            } else if kind == "birthday" {
                ai::birthday(value).is_some_and(|(_, m, d)| {
                    birthday["month"].as_u64() == Some(m as u64)
                        && birthday["day"].as_u64() == Some(d as u64)
                })
            } else {
                known[field_key(&kind)]
                    .as_str()
                    .is_some_and(|old| normal(old) == normal(value))
            };
            item["label"] = json!(label(&kind));
            !duplicate && !ai::known::represented(item, facts)
        });
    }
    rows.retain(|p| p["items"].as_array().is_some_and(|items| !items.is_empty()));
    Ok(rows)
}
async fn profile(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Ok(Json(json!({"enabled":false})));
    }
    let memory:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('summary',output->'summary','facts',output->'facts','gaps',output->'gaps','generated_at',generated_at) FROM crm.ai_visible_outputs WHERE kind IN ('ai.profile','ai.profile_overview') AND subject=$1 ORDER BY (kind='ai.profile') DESC LIMIT 1").bind(id.to_string()).fetch_optional(&s.pool).await?;
    let queued:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.ai_subject_heads h JOIN crm.ai_jobs j ON j.id=h.job_id WHERE (h.subject=$1 OR (h.subject~>=~($1||':') AND h.subject~<~($1||';'))) AND j.status IN ('queued','running'))").bind(id.to_string()).fetch_one(&s.pool).await?;
    let weeks:Vec<Value>=sqlx::query_scalar("SELECT output||jsonb_build_object('week_start',week_start,'generated_at',generated_at) FROM crm.person_weekly_updates w WHERE profile_id=$1 AND EXISTS(SELECT 1 FROM crm.ai_visible_outputs o WHERE o.kind='ai.person_week' AND o.subject=w.profile_id::text||':'||w.week_start::text AND o.input_hash=w.input_hash) ORDER BY week_start DESC LIMIT 12").bind(id).fetch_all(&s.pool).await?;
    let suggestions = pending(&s, Some(id))
        .await?
        .first()
        .map(|p| p["items"].clone())
        .unwrap_or(json!([]));
    Ok(Json(
        json!({"enabled":true,"queued":queued,"memory":memory,"weekly_updates":weeks,"suggestions":suggestions}),
    ))
}
#[derive(Deserialize)]
struct SuggestionQuery {
    kind: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}
async fn suggestions(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<SuggestionQuery>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Ok(Json(
            json!({"enabled":false,"total":0,"items":0,"kinds":{},"people":[]}),
        ));
    }
    let mut people = pending(&s, None).await?;
    let mut counts = serde_json::Map::new();
    let mut items = 0;
    for person in &mut people {
        let list = person["items"].as_array_mut().expect("SQL array");
        for item in list.iter() {
            let key = item["kind"].as_str().unwrap_or_default().to_string();
            let count = counts.entry(key).or_insert(json!(0));
            *count = json!(count.as_u64().unwrap_or(0) + 1);
        }
        list.retain(|item| q.kind.as_ref().is_none_or(|k| item["kind"] == *k));
        items += list.len();
    }
    people.retain(|p| p["items"].as_array().is_some_and(|a| !a.is_empty()));
    let total = people.len();
    let people = people
        .into_iter()
        .skip(q.offset.unwrap_or(0))
        .take(q.limit.unwrap_or(30).min(100))
        .collect::<Vec<_>>();
    Ok(Json(
        json!({"enabled":true,"total":total,"items":items,"kinds":counts,"people":people}),
    ))
}
#[derive(Deserialize)]
struct Decision {
    action: String,
    value: Option<String>,
    date: Option<String>,
}
async fn decide(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, key)): Path<(Uuid, String)>,
    Json(body): Json<Decision>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    if !["accept", "ignore"].contains(&body.action.as_str()) {
        return Err(invalid("invalid action"));
    }
    let today = ai::today(&s.pool).await.map_err(service)?;
    let mut tx = s.pool.begin().await?;
    sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 AND archived_at IS NULL FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(missing("profile not found"))?;
    let item=sqlx::query("SELECT * FROM crm.profile_suggestions v WHERE profile_id=$1 AND fingerprint=$2 AND EXISTS(SELECT 1 FROM crm.ai_visible_outputs o WHERE o.kind IN ('ai.profile','ai.profile_updates') AND o.subject=v.profile_id::text AND o.input_hash=v.input_hash) AND NOT EXISTS(SELECT 1 FROM crm.profile_suggestion_decisions d WHERE d.profile_id=v.profile_id AND d.fingerprint=v.fingerprint) FOR UPDATE").bind(id).bind(&key).fetch_optional(&mut *tx).await?.ok_or(missing("No pending suggestion"))?;
    let kind: String = item.get("kind");
    let suggested: String = item.get("value");
    let already_known = if kind == "meetup" {
        false
    } else if kind == "birthday" {
        let parts: Option<(Option<i32>, Option<i32>)> = sqlx::query_as(
            "SELECT month,day FROM crm.profile_dates WHERE profile_id=$1 AND kind='birthday'",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        ai::birthday_at(&suggested, today)
            .is_some_and(|(_, m, d)| parts == Some((Some(m as i32), Some(d as i32))))
    } else {
        let known:Option<Value>=sqlx::query_scalar("SELECT r.value FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key=$2").bind(id).bind(field_key(&kind)).fetch_optional(&mut *tx).await?;
        known
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|old| normal(old) == normal(&suggested))
    };
    if already_known {
        return Err(missing("No pending suggestion"));
    }
    let value = body
        .value
        .unwrap_or_else(|| item.get("value"))
        .trim()
        .to_owned();
    let applied = if body.action == "ignore" {
        Value::Null
    } else {
        if value.is_empty() || value.chars().count() > 2000 {
            return Err(invalid("empty or oversized value"));
        }
        if kind == "birthday" {
            let (year, month, day) = ai::birthday_at(&value, today)
                .ok_or(invalid("birthday must be MM-DD or YYYY-MM-DD"))?;
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.profile_dates WHERE profile_id=$1 AND kind='birthday')").bind(id).fetch_one(&mut *tx).await?;
            if exists {
                return Err(ApiError(StatusCode::CONFLICT, "birthday_exists"));
            }
            let date_id = Uuid::new_v4();
            let precision = if year.is_some() { "exact" } else { "month_day" };
            sqlx::query("INSERT INTO crm.profile_dates(id,profile_id,kind,date_precision,year,month,day,updated_by) VALUES($1,$2,'birthday',$3,$4,$5,$6,$7)").bind(date_id).bind(id).bind(precision).bind(year).bind(month as i32).bind(day as i32).bind(actor).execute(&mut *tx).await?;
            sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            json!({"date_id":date_id,"precision":precision,"year":year,"month":month,"day":day})
        } else if kind == "meetup" {
            let day = body
                .date
                .or_else(|| {
                    item.get::<Option<NaiveDate>, _>("date")
                        .map(|d| d.to_string())
                })
                .and_then(|d| d.parse::<NaiveDate>().ok())
                .ok_or(invalid("meetup date required"))?;
            if day < today - chrono::Duration::days(120) || day > today + chrono::Duration::days(60)
            {
                return Err(invalid("meetup date outside suggestion window"));
            }
            let aid = Uuid::new_v4();
            let title = value.chars().take(255).collect::<String>();
            let quote: String = item.get("quote");
            let hash = monica_core::identity::content_hash(
                &json!({"title":title,"body":quote,"date":day}),
            );
            sqlx::query("INSERT INTO crm.activities(id,activity_type,occurred_at,title,body,source_type,created_by,content_hash) VALUES($1,'activity',($2::date+time '12:00') AT TIME ZONE 'America/Chicago',$3,$4,'manual',$5,$6)").bind(aid).bind(day).bind(&title).bind(quote).bind(actor).bind(hash).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO crm.activity_participants VALUES($1,$2)")
                .bind(aid)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO crm.activity_revisions(activity_id,operation,payload,content_hash,actor_user_id) SELECT id,'created',to_jsonb(a),content_hash,$2 FROM crm.activities a WHERE id=$1").bind(aid).bind(actor).execute(&mut *tx).await?;
            sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            json!({"activity_id":aid,"date":day,"title":value})
        } else {
            let field_key = if kind == "food" {
                "monica.food_preferences".to_string()
            } else {
                format!("monica.{kind}")
            };
            // Same transaction as the decision. A direct manual edit still wins
            // until the user explicitly accepts this suggestion.
            let content = monica_core::content::register(&mut tx, "field_value", &json!(value))
                .await
                .map_err(service)?;
            let previous: Option<Uuid> = sqlx::query_scalar(
                "SELECT revision_id FROM crm.field_current WHERE profile_id=$1 AND field_key=$2",
            )
            .bind(id)
            .bind(&field_key)
            .fetch_optional(&mut *tx)
            .await?;
            let revision = Uuid::new_v4();
            sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,previous_revision_id,content_object_id) VALUES($1,$2,$3,$4,$5,'manual','manual_edit',$6,$7)").bind(revision).bind(id).bind(&field_key).bind(json!(value)).bind(content.hash).bind(previous).bind(content.id).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO crm.field_current VALUES($1,$2,$3) ON CONFLICT(profile_id,field_key) DO UPDATE SET revision_id=excluded.revision_id").bind(id).bind(&field_key).bind(revision).execute(&mut *tx).await?;
            sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            json!({"field_key":field_key,"value":value})
        }
    };
    let decision = if applied.is_null() {
        "ignored"
    } else {
        "accepted"
    };
    sqlx::query("INSERT INTO crm.profile_suggestion_decisions(profile_id,fingerprint,kind,decision,applied) VALUES($1,$2,$3,$4,$5)").bind(id).bind(&key).bind(&kind).bind(decision).bind(&applied).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,$2,'profile',$3,$4)").bind(actor).bind(format!("profile.suggestion.{decision}")).bind(id).bind(json!({"kind":kind,"fingerprint":key})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"profile_id":id,"fingerprint":key,"decision":decision,"applied":applied}),
    ))
}
async fn undo(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, key)): Path<(Uuid, String)>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    let n=sqlx::query("DELETE FROM crm.profile_suggestion_decisions WHERE profile_id=$1 AND fingerprint=$2 AND decision='ignored'").bind(id).bind(&key).execute(&s.pool).await?.rows_affected();
    if n == 0 {
        return Err(missing("No ignored suggestion"));
    }
    Ok(Json(
        json!({"profile_id":id,"fingerprint":key,"decision":null}),
    ))
}
async fn digests(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Ok(Json(json!({"enabled":false,"digests":[]})));
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('week_start',w.week_start,'generated_at',w.generated_at,'summary',o.output->>'summary','life_events',coalesce(jsonb_array_length(o.output->'life_events'),0)) FROM crm.weekly_digests w LEFT JOIN crm.ai_visible_outputs o ON o.kind='ai.digest' AND o.subject=w.week_start::text AND o.input_hash=w.input_hash ORDER BY w.week_start DESC LIMIT 104").fetch_all(&s.pool).await?;
    Ok(Json(json!({"enabled":true,"digests":rows})))
}
async fn digest(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(week): Path<NaiveDate>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Err(missing("AI is disabled"));
    }
    let mut tx = s.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let mut row:Value=sqlx::query_scalar("SELECT jsonb_build_object('week_start',w.week_start,'generated_at',w.generated_at,'facts',w.output->'facts','narrative',o.output-'facts','model',o.model) FROM crm.weekly_digests w LEFT JOIN crm.ai_visible_outputs o ON o.kind='ai.digest' AND o.subject=w.week_start::text AND o.input_hash=w.input_hash WHERE w.week_start=$1").bind(week).fetch_optional(&mut *tx).await?.ok_or(missing("No digest for this week"))?;
    // Older isolated builds stored only source text under facts. Supply the
    // complete deterministic contract while preserving frozen facts on new runs.
    if !row["facts"]["totals"].is_object() {
        row["facts"] = ai::weekly::facts(&mut tx, week).await.map_err(service)?;
    }
    let mut people = std::collections::BTreeSet::new();
    let mut posts = std::collections::BTreeSet::new();
    for key in ["life_events", "suggestions"] {
        for item in row["narrative"][key].as_array().into_iter().flatten() {
            if let Some(id) = item["profile_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
            {
                people.insert(id);
            }
            if let Some(id) = item["post_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
            {
                posts.insert(id);
            }
        }
    }
    row["people"]=sqlx::query_scalar::<_,Value>("SELECT coalesce(jsonb_object_agg(id::text,jsonb_build_object('name',display_name,'avatar_media_id',avatar_media_id)),'{}') FROM crm.profiles WHERE id=ANY($1)").bind(people.into_iter().collect::<Vec<_>>()).fetch_one(&mut *tx).await?;
    row["post_dates"]=sqlx::query_scalar::<_,Value>("SELECT coalesce(jsonb_object_agg(id::text,occurred_at),'{}') FROM crm.timeline_events WHERE id=ANY($1)").bind(posts.into_iter().collect::<Vec<_>>()).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(row))
}
#[derive(Deserialize)]
struct WeekQuery {
    week: Option<NaiveDate>,
    after: Option<Uuid>,
    limit: Option<i64>,
}
async fn weekly(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<WeekQuery>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Ok(Json(
            json!({"enabled":false,"week_start":null,"weeks":[],"total":0,"people":[],"next_cursor":null}),
        ));
    }
    let limit = q.limit.unwrap_or(12);
    if !(1..=50).contains(&limit) {
        return Err(invalid("limit must be 1..50"));
    }
    let week = match q.week {
        Some(week) => week,
        None => ai::previous_week_for(&s.pool).await.map_err(service)?,
    };
    let mut weeks:Vec<NaiveDate>=sqlx::query_scalar("SELECT DISTINCT w.week_start FROM crm.person_weekly_updates w JOIN crm.profiles p ON p.id=w.profile_id WHERE p.archived_at IS NULL AND p.profile_type='person' AND EXISTS(SELECT 1 FROM crm.ai_visible_outputs o WHERE o.kind='ai.person_week' AND o.subject=w.profile_id::text||':'||w.week_start::text AND o.input_hash=w.input_hash) ORDER BY w.week_start DESC").fetch_all(&s.pool).await?;
    if !weeks.contains(&week) {
        weeks.push(week);
        weeks.sort_by(|a, b| b.cmp(a));
    }
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM crm.person_weekly_updates w JOIN crm.profiles p ON p.id=w.profile_id WHERE w.week_start=$1 AND EXISTS(SELECT 1 FROM crm.ai_visible_outputs o WHERE o.kind='ai.person_week' AND o.subject=w.profile_id::text||':'||w.week_start::text AND o.input_hash=w.input_hash) AND p.archived_at IS NULL AND p.profile_type='person'").bind(week).fetch_one(&s.pool).await?;
    let mut people:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'week_start',w.week_start,'generated_at',w.generated_at,'summary',w.output->>'summary','sources',w.output->'sources') FROM crm.person_weekly_updates w JOIN crm.profiles p ON p.id=w.profile_id WHERE w.week_start=$1 AND EXISTS(SELECT 1 FROM crm.ai_visible_outputs o WHERE o.kind='ai.person_week' AND o.subject=w.profile_id::text||':'||w.week_start::text AND o.input_hash=w.input_hash) AND p.archived_at IS NULL AND p.profile_type='person' AND ($2::uuid IS NULL OR p.id>$2) ORDER BY p.id LIMIT $3").bind(week).bind(q.after).bind(limit+1).fetch_all(&s.pool).await?;
    let cursor = if people.len() > limit as usize {
        people.truncate(limit as usize);
        people.last().map(|p| p["profile_id"].clone())
    } else {
        None
    };
    Ok(Json(
        json!({"enabled":true,"week_start":week,"weeks":weeks,"total":total,"people":people,"next_cursor":cursor}),
    ))
}
async fn highlights(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Ok(Json(json!({"enabled":false,"week_start":null,"people":[]})));
    }
    let requested = ai::previous_week_for(&s.pool).await.map_err(service)?;
    // Home needs eight saved summaries, not the full weekly index, source
    // evidence, population count and pagination contract of the archive page.
    let result: Value = sqlx::query_scalar(r#"WITH chosen AS MATERIALIZED(
 SELECT w.week_start FROM crm.person_weekly_updates w JOIN crm.profiles p ON p.id=w.profile_id
 WHERE w.week_start<=$1 AND p.archived_at IS NULL AND p.profile_type='person'
 AND EXISTS(SELECT 1 FROM crm.ai_visible_outputs o WHERE o.kind='ai.person_week' AND o.subject=w.profile_id::text||':'||w.week_start::text AND o.input_hash=w.input_hash)
 ORDER BY w.week_start DESC,w.profile_id LIMIT 1),people AS(
 SELECT p.id,p.display_name,p.avatar_media_id,w.week_start,w.output->>'summary' summary
 FROM chosen c JOIN crm.person_weekly_updates w ON w.week_start=c.week_start JOIN crm.profiles p ON p.id=w.profile_id
 WHERE p.archived_at IS NULL AND p.profile_type='person'
 AND EXISTS(SELECT 1 FROM crm.ai_visible_outputs o WHERE o.kind='ai.person_week' AND o.subject=w.profile_id::text||':'||w.week_start::text AND o.input_hash=w.input_hash)
 ORDER BY p.id LIMIT 8)
 SELECT jsonb_build_object('enabled',true,'week_start',(SELECT week_start FROM chosen),'people',coalesce((SELECT jsonb_agg(jsonb_build_object('profile_id',id,'name',display_name,'avatar_media_id',avatar_media_id,'week_start',week_start,'summary',summary) ORDER BY id) FROM people),'[]'::jsonb))"#)
        .bind(requested).fetch_one(&s.pool).await?;
    Ok(Json(result))
}
async fn reconnect(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    user(&s, &h).await?;
    if !enabled(&s).await? {
        return Ok(Json(json!({"enabled":false,"people":[]})));
    }
    let people:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'avatar_media_id',p.avatar_media_id,'last_contact_at',c.last_contact_at,'suggestion',o.output) FROM crm.profiles p JOIN crm.weekly_contact_stats c ON c.profile_id=p.id LEFT JOIN crm.ai_visible_outputs o ON o.kind='ai.reconnect_one' AND o.subject=p.id::text WHERE p.archived_at IS NULL AND p.profile_type='person' AND c.last_contact_at<now()-interval '90 days' AND NOT EXISTS(SELECT 1 FROM crm.ai_reconnect_decisions d WHERE d.profile_id=p.id AND (d.until_at IS NULL OR d.until_at>now())) ORDER BY c.last_contact_at DESC,p.id LIMIT 12").fetch_all(&s.pool).await?;
    Ok(Json(json!({"enabled":true,"people":people})))
}
#[derive(Deserialize)]
struct ReconnectAction {
    state: String,
    snooze: Option<String>,
}
async fn reconnect_action(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReconnectAction>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    if !["done", "snoozed", "dismissed"].contains(&body.state.as_str())
        || (body.state == "snoozed") != body.snooze.is_some()
    {
        return Err(invalid("snooze is required exactly when state is snoozed"));
    }
    let days = match body.snooze.as_deref() {
        Some("2w") => Some(14),
        Some("1m") => Some(30),
        None => None,
        _ => return Err(invalid("invalid snooze")),
    };
    let until = days.map(|d| chrono::Utc::now() + chrono::Duration::days(d));
    let action = match body.state.as_str() {
        "done" => "contacted",
        "snoozed" => "snooze",
        _ => "dismiss",
    };
    let mut tx = s.pool.begin().await?;
    sqlx::query("SELECT id FROM crm.profiles WHERE id=$1 AND archived_at IS NULL FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(missing("profile not found"))?;
    sqlx::query("INSERT INTO crm.ai_reconnect_decisions(profile_id,action,until_at) VALUES($1,$2,$3) ON CONFLICT(profile_id) DO UPDATE SET action=excluded.action,until_at=excluded.until_at,created_at=now()").bind(id).bind(action).bind(until).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'reconnect.updated','profile',$2,$3)").bind(actor).bind(id).bind(json!({"state":body.state})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"profile_id":id,"state":body.state,"until":until}),
    ))
}
async fn reconnect_undo(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    user(&s, &h).await?;
    sqlx::query("DELETE FROM crm.ai_reconnect_decisions WHERE profile_id=$1")
        .bind(id)
        .execute(&s.pool)
        .await?;
    Ok(Json(json!({"profile_id":id,"state":null})))
}

#[cfg(test)]
#[path = "ai_tests.rs"]
mod tests;

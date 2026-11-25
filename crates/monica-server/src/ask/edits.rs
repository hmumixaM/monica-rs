//! Model calls can propose typed changes. Only the chat owner's confirmation
//! can apply the saved payload, and the receipt commits with the actual edit.
use super::{Result, invalid, missing};
use crate::{
    auth::{ApiError, AppState, session_user},
    profiles::{self, activities, dates, relationships},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::BTreeSet;
use uuid::Uuid;

const MAX_PEOPLE: usize = 50;
const MAX_PENDING: i64 = 20;
const MAX_ACTION_BYTES: usize = 12_000;
pub(super) const NAME: &str = "propose_monica_edit";

#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    AddRecord {
        profile_ids: Vec<Uuid>,
        record_type: RecordType,
        title: String,
        body: Option<String>,
        occurred_at: Option<DateTime<Utc>>,
    },
    AddTag {
        profile_ids: Vec<Uuid>,
        tag: String,
    },
    CreateTag {
        #[serde(default)]
        profile_ids: Vec<Uuid>,
        tag: String,
    },
    SetProfileField {
        profile_id: Uuid,
        field: ProfileField,
        value: String,
    },
    AddDate {
        profile_id: Uuid,
        date: dates::DateWrite,
    },
    AddRelationship {
        profile_id: Uuid,
        related_profile_id: Uuid,
        relationship: String,
        note: Option<String>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecordType {
    Note,
    Activity,
    Call,
}

impl RecordType {
    fn display(&self) -> (&'static str, &'static str) {
        match self {
            Self::Note => ("note", "新增笔记"),
            Self::Activity => ("activity", "新增活动记录"),
            Self::Call => ("call", "新增通话记录"),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProfileField {
    Summary,
    Nickname,
    City,
    Company,
    Job,
    School,
    Hometown,
    Interests,
}

impl ProfileField {
    fn display(&self) -> (&'static str, &'static str) {
        match self {
            Self::Summary => ("summary", "简介"),
            Self::Nickname => ("monica.nickname", "联系人备注"),
            Self::City => ("monica.residence", "常驻地"),
            Self::Company => ("monica.company", "公司"),
            Self::Job => ("monica.job", "职位"),
            Self::School => ("monica.school", "学校"),
            Self::Hometown => ("monica.hometown", "家乡"),
            Self::Interests => ("monica.interests", "兴趣"),
        }
    }
}

pub(super) fn tool() -> Value {
    json!({"type":"function","function":{"name":NAME,
        "description":"仅当用户明确要求修改 Monica 时，为真实确认界面生成一项待确认的编辑。它只生成预览。add_record 需要 profile_ids、record_type、title，可选 body；活动和通话须提供 occurred_at，普通笔记省略则使用提案创建时间；add_tag 和 create_tag 需要 tag 和 profile_ids（仅创建标签时可为空）；set_profile_field 需要 profile_id、field、value；add_date 需要 profile_id 及 date 对象（kind、precision 和已知的年月日，可选 label）；add_relationship 需要 profile_id、related_profile_id、relationship，可选 note。所有人物用查询或用户选中的真实 UUID，单次最多 50 人。",
        "parameters":{"type":"object","required":["action"],"additionalProperties":false,"properties":{
            "action":{"type":"string","enum":["add_record","add_tag","create_tag","set_profile_field","add_date","add_relationship"]},
            "profile_id":{"type":"string"}, "related_profile_id":{"type":"string"},
            "profile_ids":{"type":"array","items":{"type":"string"},"maxItems":MAX_PEOPLE},
            "record_type":{"type":"string","enum":["note","activity","call"]},
            "title":{"type":"string"}, "body":{"type":"string"},
            "occurred_at":{"type":"string","description":"带时区的 ISO 8601 时间"},
            "tag":{"type":"string"},
            "field":{"type":"string","enum":["summary","nickname","city","company","job","school","hometown","interests"]},
            "value":{"type":"string"},
            "date":{"type":"object","required":["kind","precision"],"additionalProperties":false,"properties":{
                "kind":{"type":"string","enum":["birthday","anniversary","other"]},
                "precision":{"type":"string","enum":["exact","month_day","approximate"]},
                "year":{"type":"integer"}, "month":{"type":"integer"}, "day":{"type":"integer"}, "label":{"type":"string"}
            }},
            "relationship":{"type":"string","description":"用户明确给出的关系，从 profile_id 指向 related_profile_id"},
            "note":{"type":"string"}
        }}
    }})
}

fn text(value: &mut String, max: usize) -> Result<()> {
    *value = value.trim().to_owned();
    if !(1..=max).contains(&value.chars().count()) || value.contains('\0') {
        return Err(invalid("编辑内容为空、太长或包含无效字符。"));
    }
    Ok(())
}

fn optional_text(value: &mut Option<String>, max: usize) -> Result<()> {
    if let Some(value) = value {
        text(value, max)?;
    }
    Ok(())
}

impl Action {
    fn normalize(self) -> Result<Self> {
        let mut action = match self {
            Self::AddDate {
                profile_id,
                mut date,
            } => {
                optional_text(&mut date.label, 60)?;
                let now = Utc::now();
                let date = date.normalize((now.year(), now.month() as i32, now.day() as i32))?;
                return Ok(Self::AddDate { profile_id, date });
            }
            other => other,
        };
        match &mut action {
            Self::AddRecord {
                record_type,
                title,
                body,
                occurred_at,
                ..
            } => {
                text(title, 255)?;
                optional_text(body, 1800)?;
                if occurred_at.is_none() {
                    if !matches!(record_type, RecordType::Note) {
                        return Err(invalid("请为活动或通话提供发生时间。"));
                    }
                    *occurred_at = Some(Utc::now());
                }
            }
            Self::AddTag { tag, .. } | Self::CreateTag { tag, .. } => text(tag, 120)?,
            Self::SetProfileField { field, value, .. } => {
                text(
                    value,
                    if matches!(field, ProfileField::Summary) {
                        1200
                    } else {
                        300
                    },
                )?;
            }
            Self::AddDate { .. } => unreachable!(),
            Self::AddRelationship {
                relationship,
                note,
                profile_id,
                related_profile_id,
            } => {
                text(relationship, 120)?;
                optional_text(note, 600)?;
                if profile_id == related_profile_id
                    || ["group", "member", "owner", "群"]
                        .iter()
                        .any(|word| relationship.to_lowercase().contains(word))
                {
                    return Err(invalid("关系需要关联两个不同的人。"));
                }
            }
        }
        let count = action.people().len();
        if count > MAX_PEOPLE || (count == 0 && !matches!(action, Self::CreateTag { .. })) {
            return Err(invalid("请选择 1 至 50 位联系人。"));
        }
        Ok(action)
    }

    fn people(&self) -> Vec<Uuid> {
        let ids = match self {
            Self::AddRecord { profile_ids, .. }
            | Self::AddTag { profile_ids, .. }
            | Self::CreateTag { profile_ids, .. } => profile_ids.clone(),
            Self::SetProfileField { profile_id, .. } | Self::AddDate { profile_id, .. } => {
                vec![*profile_id]
            }
            Self::AddRelationship {
                profile_id,
                related_profile_id,
                ..
            } => vec![*profile_id, *related_profile_id],
        };
        ids.into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[derive(Serialize, sqlx::FromRow)]
pub(super) struct EditView {
    id: Uuid,
    status: String,
    preview: Value,
    result: Option<Value>,
}

pub(super) async fn list(
    tx: &mut Transaction<'_, Postgres>,
    chat: Uuid,
) -> sqlx::Result<Vec<EditView>> {
    sqlx::query_as(
        "SELECT e.id,CASE WHEN r.status='cancelled' AND e.status='pending' THEN 'dismissed' ELSE e.status END status,e.preview,e.result
         FROM crm.ask_runs r JOIN crm.ask_edits e ON e.run_id=r.id WHERE r.chat_id=$1
         ORDER BY (e.status='pending' AND r.status<>'cancelled') DESC,e.created_at DESC,e.id LIMIT 30",
    ).bind(chat).fetch_all(&mut **tx).await
}

fn stale() -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "资料和预览时不同，请重新提出这项编辑。",
    )
}

async fn preview(tx: &mut Transaction<'_, Postgres>, action: &Action, lock: bool) -> Result<Value> {
    let ids = action.people();
    let mut sql = String::from(
        "SELECT id,display_name,profile_type FROM crm.profiles
         WHERE id=ANY($1) AND archived_at IS NULL ORDER BY id",
    );
    if lock {
        sql.push_str(" FOR UPDATE");
    }
    let people: Vec<(Uuid, String, String)> =
        sqlx::query_as(&sql).bind(&ids).fetch_all(&mut **tx).await?;
    if people.len() != ids.len() || people.iter().any(|(_, _, kind)| kind != "person") {
        return Err(invalid("编辑对象必须是仍存在的联系人。"));
    }
    let people: Vec<Value> = people
        .into_iter()
        .map(|(id, name, _)| json!({"id":id,"name":name}))
        .collect();
    let mut state = Value::Null;
    let (title, before, after) = match action {
        Action::AddRecord {
            record_type,
            title,
            body,
            occurred_at,
            ..
        } => (
            record_type.display().1,
            Value::Null,
            json!({"标题":title,"内容":body,"发生时间":occurred_at}),
        ),
        Action::AddTag { tag, .. } | Action::CreateTag { tag, .. } => {
            let existing: Vec<(Uuid, String)> = sqlx::query_as(
                "SELECT id,name FROM crm.tags WHERE lower(name)=lower($1) ORDER BY name LIMIT 2",
            )
            .bind(tag)
            .fetch_all(&mut **tx)
            .await?;
            if existing.len() > 1 {
                return Err(invalid("有多个同名标签，请先处理重名。"));
            }
            let existing = existing.first();
            if existing.is_none() && matches!(action, Action::AddTag { .. }) {
                return Err(invalid(
                    "这个标签不存在。请先找到已有标签，或在用户要求时提出创建。",
                ));
            }
            let members: Vec<Uuid> = if let Some((id, _)) = existing {
                sqlx::query_scalar("SELECT DISTINCT profile_id FROM crm.presented_tag_memberships WHERE tag_id=$1 AND profile_id=ANY($2) ORDER BY profile_id")
                    .bind(id).bind(&ids).fetch_all(&mut **tx).await?
            } else {
                Vec::new()
            };
            state = json!({"tagId":existing.map(|(id, _)| id),"memberIds":members});
            let names = people
                .iter()
                .filter(|p| members.iter().any(|id| p["id"] == json!(id)))
                .map(|p| p["name"].clone())
                .collect::<Vec<_>>();
            (
                if existing.is_none() {
                    "创建标签"
                } else {
                    "添加已有标签"
                },
                json!({"已带标签":names}),
                json!({"标签":existing.map(|(_, name)| name.as_str()).unwrap_or(tag)}),
            )
        }
        Action::SetProfileField {
            profile_id,
            field,
            value,
        } => {
            let (key, label) = field.display();
            let entry: Option<(Value, String)> = sqlx::query_as(
                "SELECT r.value,r.source_type FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key=$2",
            ).bind(profile_id).bind(key).fetch_optional(&mut **tx).await?;
            let (old, source) = match entry {
                Some((old, source)) => {
                    if !old.is_null()
                        && (!old.is_string() || !matches!(source.as_str(), "manual" | "monica"))
                    {
                        return Err(invalid("这项资料来自导入记录，请在联系人页面核对。"));
                    }
                    (old, json!(source))
                }
                None if key == "summary" => {
                    let value: Option<String> =
                        sqlx::query_scalar("SELECT summary FROM crm.profiles WHERE id=$1")
                            .bind(profile_id)
                            .fetch_one(&mut **tx)
                            .await?;
                    (json!(value), Value::Null)
                }
                None => (Value::Null, Value::Null),
            };
            (
                "补录联系人资料",
                json!({(label):old,"来源":source}),
                json!({(label):value}),
            )
        }
        Action::AddDate { profile_id, date } => {
            let existing: Option<Value> = if date.kind == "birthday" {
                sqlx::query_scalar("SELECT value - 'next' - 'days_until' - 'years' - 'years_exact' FROM crm.profile_dates_presented WHERE profile_id=$1 AND kind='birthday'")
                    .bind(profile_id).fetch_optional(&mut **tx).await?
            } else {
                None
            };
            if existing.is_some() {
                return Err(invalid("这个联系人已有生日，请在联系人页面核对。"));
            }
            let parts = [
                date.year.map(|v| format!("{v} 年")),
                date.month.map(|v| format!("{v} 月")),
                date.day.map(|v| format!("{v} 日")),
            ];
            let value = parts.into_iter().flatten().collect::<Vec<_>>().join("");
            let kind = match date.kind.as_str() {
                "birthday" => "生日",
                "anniversary" => "纪念日",
                _ => "其他日期",
            };
            let precision = match date.precision.as_str() {
                "exact" => "完整日期",
                "month_day" => "仅月日",
                _ => "大致日期",
            };
            (
                "补录重要日期",
                Value::Null,
                json!({"类型":kind,"名称":date.label,"日期":value,"精度":precision}),
            )
        }
        Action::AddRelationship {
            profile_id,
            related_profile_id,
            relationship,
            note,
        } => {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM crm.presented_relationship_edges WHERE
                 from_profile_id=$1 AND to_profile_id=$2 AND relationship_type=$3)",
            )
            .bind(profile_id)
            .bind(related_profile_id)
            .bind(relationship)
            .fetch_one(&mut **tx)
            .await?;
            let name = |id: &Uuid| {
                people
                    .iter()
                    .find(|p| p["id"] == json!(id))
                    .map(|p| p["name"].clone())
            };
            (
                "记录人物关系",
                json!({"已有此关系":exists}),
                json!({"人物":name(profile_id),"相关人物":name(related_profile_id),"关系":relationship,"备注":note}),
            )
        }
    };
    Ok(json!({"title":title,"people":people,"before":before,"after":after,"state":state}))
}

pub(super) async fn propose(
    pool: &PgPool,
    chat: Uuid,
    run: Uuid,
    lease: Uuid,
    call: &str,
    args: &Value,
) -> Result<Uuid> {
    if args.to_string().len() > MAX_ACTION_BYTES {
        return Err(invalid("本次编辑太长，请分成几项。"));
    }
    let action = serde_json::from_value::<Action>(args.clone())
        .map_err(|_| invalid("编辑参数无效，请检查操作类型和必填项。"))?
        .normalize()?;
    let mut tx = pool.begin().await?;
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM crm.ask_chats WHERE id=$1 FOR UPDATE")
        .bind(chat)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(missing)?;
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM crm.ask_runs WHERE id=$1 AND chat_id=$2 AND lease_id=$3 AND status='running' AND lease_until>now() FOR UPDATE")
        .bind(run).bind(chat).bind(lease).fetch_optional(&mut *tx).await?.ok_or_else(missing)?;
    if let Some(id) =
        sqlx::query_scalar("SELECT id FROM crm.ask_edits WHERE run_id=$1 AND tool_call_id=$2")
            .bind(run)
            .bind(call)
            .fetch_optional(&mut *tx)
            .await?
    {
        return Ok(id);
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crm.ask_edits e JOIN crm.ask_runs r ON r.id=e.run_id
         WHERE r.chat_id=$1 AND e.status='pending' AND r.status<>'cancelled'",
    )
    .bind(chat)
    .fetch_one(&mut *tx)
    .await?;
    if pending >= MAX_PENDING {
        return Err(invalid("会话里待确认的编辑太多。请先处理已有编辑。"));
    }
    let preview = preview(&mut tx, &action, false).await?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO crm.ask_edits(id,run_id,tool_call_id,action,preview) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(run)
    .bind(call)
    .bind(sqlx::types::Json(action))
    .bind(preview)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

#[derive(Deserialize)]
pub(super) struct Decision {
    confirm: bool,
}

pub(super) async fn decide(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((chat, id)): Path<(Uuid, Uuid)>,
    Json(decision): Json<Decision>,
) -> Result<Json<EditView>> {
    let (owner, _) = session_user(&s, &headers).await?;
    Ok(Json(
        resolve(&s.pool, owner, chat, id, decision.confirm).await?,
    ))
}

async fn resolve(
    pool: &PgPool,
    owner: Uuid,
    chat: Uuid,
    id: Uuid,
    confirm: bool,
) -> Result<EditView> {
    let mut tx = pool.begin().await?;
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM crm.ask_chats WHERE id=$1 AND owner_id=$2 FOR KEY SHARE",
    )
    .bind(chat)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(missing)?;
    let (run_status, status, action, saved, result): (
        String,
        String,
        sqlx::types::Json<Action>,
        Value,
        Option<Value>,
    ) = sqlx::query_as(
        "SELECT r.status,e.status,e.action,e.preview,e.result FROM crm.ask_edits e
            JOIN crm.ask_runs r ON r.id=e.run_id WHERE e.id=$1 AND r.chat_id=$2 FOR UPDATE OF r,e",
    )
    .bind(id)
    .bind(chat)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(missing)?;
    if status != "pending" {
        return Ok(EditView {
            id,
            status,
            preview: saved,
            result,
        });
    }
    let (status, result) = if !confirm || run_status == "cancelled" {
        ("dismissed", None)
    } else {
        let current = match preview(&mut tx, &action.0, true).await {
            Ok(current) => current,
            Err(error) if error.0.is_client_error() => Value::Null,
            Err(error) => return Err(error),
        };
        if current != saved {
            ("stale", Some(json!({"message":stale().1})))
        } else {
            (
                "applied",
                Some(apply(&mut tx, owner, action.0, &current).await?),
            )
        }
    };
    sqlx::query("UPDATE crm.ask_edits SET status=$2,result=$3 WHERE id=$1")
        .bind(id)
        .bind(status)
        .bind(&result)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(EditView {
        id,
        status: status.into(),
        preview: saved,
        result,
    })
}

async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    owner: Uuid,
    action: Action,
    current: &Value,
) -> Result<Value> {
    let ids = action.people();
    match action {
        Action::AddRecord {
            record_type,
            title,
            body,
            occurred_at,
            ..
        } => {
            let (id, _) = activities::save_locked(
                tx,
                None,
                owner,
                activities::Write {
                    activity_type: record_type.display().0.into(),
                    occurred_at: occurred_at.ok_or_else(stale)?.to_rfc3339(),
                    title,
                    body,
                    participant_profile_ids: ids,
                    due_at: None,
                    media_ids: None,
                },
            )
            .await?;
            Ok(json!({"link":format!("/timeline?event={id}")}))
        }
        Action::AddTag { .. } | Action::CreateTag { .. } => {
            let tag = current["after"]["标签"].as_str().ok_or_else(stale)?;
            let (id, created) = if let Some(id) = current["state"]["tagId"].as_str() {
                (Uuid::parse_str(id).map_err(|_| stale())?, false)
            } else {
                let created =
                    sqlx::query("INSERT INTO crm.tags VALUES($1,$2) ON CONFLICT(name) DO NOTHING")
                        .bind(Uuid::new_v4())
                        .bind(tag)
                        .execute(&mut **tx)
                        .await?
                        .rows_affected()
                        > 0;
                let id = sqlx::query_scalar("SELECT id FROM crm.tags WHERE name=$1")
                    .bind(tag)
                    .fetch_one(&mut **tx)
                    .await?;
                (id, created)
            };
            let already: Vec<Uuid> = serde_json::from_value(current["state"]["memberIds"].clone())
                .map_err(|_| stale())?;
            let mut added = 0;
            for profile in ids.into_iter().filter(|id| !already.contains(id)) {
                profiles::membership_locked(tx, owner, profile, id, true).await?;
                added += 1;
            }
            let message = match (created, added) {
                (true, 0) => format!("已创建标签「{tag}」。"),
                (true, _) => format!("已创建标签「{tag}」，并添加到 {added} 位联系人。"),
                (false, 0) => format!("标签「{tag}」已存在，没有需要新增的成员。"),
                (false, _) => format!("已给 {added} 位联系人添加标签「{tag}」。"),
            };
            Ok(json!({"message":message}))
        }
        Action::SetProfileField {
            profile_id,
            field,
            value,
        } => {
            profiles::change_locked(
                tx,
                profile_id,
                owner,
                profiles::Field {
                    field_key: field.display().0.into(),
                    value: json!(value),
                    source_type: None,
                },
                "manual_edit",
            )
            .await?;
            Ok(json!({"link":format!("/profiles/{profile_id}")}))
        }
        Action::AddDate { profile_id, date } => {
            dates::save_locked(tx, profile_id, None, owner, date).await?;
            Ok(json!({"link":format!("/profiles/{profile_id}")}))
        }
        Action::AddRelationship {
            profile_id,
            related_profile_id,
            relationship,
            note,
        } => {
            if current["before"]["已有此关系"] == true {
                return Ok(json!({"message":"已有此关系。"}));
            }
            relationships::save_locked(
                tx,
                profile_id,
                owner,
                relationships::Edge {
                    to_profile_id: related_profile_id,
                    relationship_type: relationship,
                    note,
                    valid_from: None,
                    valid_to: None,
                },
            )
            .await?;
            Ok(json!({"link":format!("/profiles/{profile_id}")}))
        }
    }
}

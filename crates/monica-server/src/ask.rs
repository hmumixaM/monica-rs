mod agent;
mod edits;
mod lookup;
mod sandbox;
mod script_worker;

pub(crate) use script_worker::entry as script_worker_entry;

use crate::{
    ai::browser::model_settings,
    auth::{ApiError, AppState, user},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRequest, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

type Result<T> = std::result::Result<T, ApiError>;
type Actor = (Uuid, String);
const IMPORT_BYTES: usize = 24 * 1024 * 1024;
const MESSAGE_PAGE: i64 = 100;
const MESSAGE_PAGE_BYTES: i64 = 2 * 1024 * 1024;
const SCRIPT_RESULT_BYTES: usize = 96_000;
const MAX_MESSAGE_BYTES: usize = 1536 * 1024;

fn message_bytes(message: &Value) -> usize {
    message.to_string().len()
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ask/chats", get(list).post(create))
        .route(
            "/api/v1/ask/chats/import",
            post(import).layer(DefaultBodyLimit::max(IMPORT_BYTES)),
        )
        .route(
            "/api/v1/ask/chats/{id}",
            get(read).patch(edit).delete(delete),
        )
        .route("/api/v1/ask/chats/{id}/runs", post(start))
        .route("/api/v1/ask/chats/{id}/edits/{edit}", post(edits::decide))
        .route(
            "/api/v1/ask/chats/{id}/runs/{run}",
            axum::routing::delete(cancel),
        )
}

pub(crate) fn launch(state: AppState, control: PgPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(agent::work(state, control))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Person {
    id: Uuid,
    display_name: String,
    avatar_media_id: Option<Uuid>,
}

#[derive(Serialize, sqlx::FromRow)]
struct Chat {
    id: Uuid,
    title: String,
    people: sqlx::types::Json<Vec<Person>>,
    #[serde(rename = "updatedAt")]
    updated_at: DateTime<Utc>,
    running: bool,
}
const CHATS: &str = "SELECT c.id,c.title,c.people,c.updated_at,
    EXISTS(SELECT 1 FROM crm.ask_runs r WHERE r.chat_id=c.id AND r.status IN ('queued','running')) running
    FROM crm.ask_chats c";

fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "这个会话已被删除或不存在。")
}
fn invalid(detail: &'static str) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, detail)
}
fn validate_people(people: &[Person]) -> Result<()> {
    if people.len() > 40
        || people
            .iter()
            .any(|person| person.display_name.chars().count() > 300)
    {
        return Err(invalid("选中的人物太多或名称太长。"));
    }
    Ok(())
}

async fn list(State(s): State<AppState>, headers: HeaderMap) -> Result<Json<Vec<Chat>>> {
    let (owner, _) = user(&s, &headers).await?;
    Ok(Json(
        sqlx::query_as(&format!(
            "{CHATS} WHERE c.owner_id=$1 ORDER BY c.updated_at DESC,c.id"
        ))
        .bind(owner)
        .fetch_all(&s.pool)
        .await?,
    ))
}

async fn chat(pool: &PgPool, owner: Uuid, id: Uuid) -> Result<Chat> {
    sqlx::query_as(&format!("{CHATS} WHERE c.id=$1 AND c.owner_id=$2"))
        .bind(id)
        .bind(owner)
        .fetch_optional(pool)
        .await?
        .ok_or_else(missing)
}

#[derive(Deserialize)]
struct NewChat {
    id: Uuid,
}
async fn create(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<NewChat>,
) -> Result<Json<Chat>> {
    let (owner, _) = user(&s, &headers).await?;
    sqlx::query("INSERT INTO crm.ask_chats(id,owner_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(input.id)
        .bind(owner)
        .execute(&s.pool)
        .await?;
    Ok(Json(chat(&s.pool, owner, input.id).await?))
}

#[derive(Deserialize)]
struct Edit {
    people: Vec<Person>,
}
async fn edit(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(edit): Json<Edit>,
) -> Result<Json<Chat>> {
    let (owner, _) = user(&s, &headers).await?;
    validate_people(&edit.people)?;
    sqlx::query("UPDATE crm.ask_chats SET people=$3,updated_at=now() WHERE id=$1 AND owner_id=$2")
        .bind(id)
        .bind(owner)
        .bind(sqlx::types::Json(edit.people))
        .execute(&s.pool)
        .await?;
    Ok(Json(chat(&s.pool, owner, id).await?))
}

async fn delete(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    let (owner, _) = user(&s, &headers).await?;
    let deleted = sqlx::query("DELETE FROM crm.ask_chats WHERE id=$1 AND owner_id=$2")
        .bind(id)
        .bind(owner)
        .execute(&s.pool)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(missing());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Default, Deserialize)]
struct Cursor {
    #[serde(default)]
    after: i64,
}
#[derive(Serialize)]
struct SavedMessage {
    seq: i64,
    message: Value,
}
#[derive(Serialize, sqlx::FromRow)]
struct RunView {
    id: Uuid,
    status: String,
    progress: String,
    error: String,
    model: String,
    draft: Option<Value>,
}
async fn read(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(cursor): Query<Cursor>,
) -> Result<Response> {
    let (owner, _) = user(&s, &headers).await?;
    let mut tx = s.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let chat: Chat = sqlx::query_as(&format!("{CHATS} WHERE c.id=$1 AND c.owner_id=$2"))
        .bind(id)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(missing)?;
    // Fetch one extra cursor as a sentinel. PostgreSQL decides which rows
    // fit before SQLx decodes their potentially large JSON values.
    let entries: Vec<(i64, Option<Value>)> = sqlx::query_as(
        "WITH recent AS MATERIALIZED (
            SELECT seq,encoded_bytes FROM crm.ask_messages WHERE chat_id=$1 AND seq>$2 ORDER BY seq LIMIT $3
         ), sized AS (
            SELECT seq,row_number() OVER ordered AS position,
                sum(encoded_bytes) OVER ordered AS bytes
            FROM recent WINDOW ordered AS (ORDER BY seq)
         ) SELECT sized.seq,m.message FROM sized
         LEFT JOIN crm.ask_messages m ON m.seq=sized.seq
            AND (position=1 OR (position<$3 AND bytes<=$4))
         ORDER BY sized.seq",
    )
    .bind(id).bind(cursor.after.max(0)).bind(MESSAGE_PAGE + 1).bind(MESSAGE_PAGE_BYTES)
    .fetch_all(&mut *tx).await?;
    let more = entries.last().is_some_and(|(_, message)| message.is_none());
    let rows = entries
        .into_iter()
        .filter_map(|(seq, message)| {
            Some(SavedMessage {
                seq,
                message: message?,
            })
        })
        .collect::<Vec<_>>();
    let run: Option<RunView> = sqlx::query_as(
        "SELECT id,status,progress,error,model,draft FROM crm.ask_runs WHERE chat_id=$1 ORDER BY created_at DESC,id DESC LIMIT 1",
    )
    .bind(id).fetch_optional(&mut *tx).await?;
    let edits = edits::list(&mut tx, id).await?;
    tx.commit().await?;
    #[derive(Serialize)]
    struct Page {
        chat: Chat,
        rows: Vec<SavedMessage>,
        run: Option<RunView>,
        edits: Vec<edits::EditView>,
        more: bool,
    }
    Ok((
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        Json(Page {
            chat,
            rows,
            run,
            edits,
            more,
        }),
    )
        .into_response())
}

#[derive(Deserialize)]
struct Question {
    request_id: Uuid,
    question: String,
    #[serde(default)]
    people: Vec<Person>,
    model: Option<String>,
    #[serde(default)]
    timezone: Option<chrono_tz::Tz>,
}
async fn start(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<Question>,
) -> Result<StatusCode> {
    let (owner, _) = user(&s, &headers).await?;
    let question = input.question.trim();
    if question.is_empty() || question.len() > 32_000 {
        return Err(invalid("请输入问题，长度最多 32 KB。"));
    }
    validate_people(&input.people)?;
    let settings = model_settings(&s).await?;
    let mut tx = s.pool.begin().await?;
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM crm.ask_chats WHERE id=$1 AND owner_id=$2 FOR UPDATE",
    )
    .bind(id)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(missing)?;
    let already_started: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.ask_runs WHERE id=$1 AND chat_id=$2)")
            .bind(input.request_id)
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if already_started {
        return Ok(StatusCode::ACCEPTED);
    }
    if settings["enabled"] != true {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "请在「设置 → 模型」里启用模型服务和问 Monica。",
        ));
    }
    let model = input
        .model
        .filter(|name| {
            settings["models"]
                .as_array()
                .is_some_and(|models| models.iter().any(|item| item == name))
        })
        .unwrap_or_else(|| settings["model"].as_str().unwrap_or_default().to_owned());
    let inserted = sqlx::query(
        "INSERT INTO crm.ask_runs(id,chat_id,model,timezone,people) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
    )
    .bind(input.request_id).bind(id).bind(model).bind(input.timezone.unwrap_or(chrono_tz::UTC).name())
    .bind(sqlx::types::Json(&input.people)).execute(&mut *tx).await?.rows_affected();
    if inserted == 0 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "这个会话正在回答，请稍后再提问。",
        ));
    }
    let message =
        json!({"role":"user","content":question,"timestamp":Utc::now().timestamp_millis()});
    sqlx::query("INSERT INTO crm.ask_messages(chat_id,run_id,message) VALUES($1,$2,$3)")
        .bind(id)
        .bind(input.request_id)
        .bind(message)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE crm.ask_chats SET updated_at=now(),people=$2,title=CASE
                WHEN NOT EXISTS(SELECT 1 FROM crm.ask_messages WHERE chat_id=$1 AND run_id IS DISTINCT FROM $3)
                THEN $4 ELSE title END WHERE id=$1")
        .bind(id).bind(sqlx::types::Json(&input.people)).bind(input.request_id)
        .bind(question.chars().take(60).collect::<String>()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::ACCEPTED)
}

async fn cancel(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((chat_id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let (owner, _) = user(&s, &headers).await?;
    let mut tx = s.pool.begin().await?;
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM crm.ask_chats WHERE id=$1 AND owner_id=$2 FOR KEY SHARE",
    )
    .bind(chat_id)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(missing)?;
    let draft: Option<Option<Value>> = sqlx::query_scalar(
        "SELECT draft FROM crm.ask_runs
        WHERE id=$1 AND chat_id=$2 AND status IN ('queued','running') FOR UPDATE",
    )
    .bind(run_id)
    .bind(chat_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(draft) = draft {
        sqlx::query("UPDATE crm.ask_runs SET status='cancelled',progress='',draft=NULL,lease_id=NULL,lease_until=NULL WHERE id=$1")
            .bind(run_id).execute(&mut *tx).await?;
        if let Some(mut message) = draft {
            message["stopReason"] = json!("aborted");
            sqlx::query("INSERT INTO crm.ask_messages(chat_id,run_id,message) VALUES($1,$2,$3)")
                .bind(chat_id)
                .bind(run_id)
                .bind(message)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Imported {
    version: u32,
    conversation: Option<ImportChat>,
    messages: Vec<Value>,
}
#[derive(Deserialize)]
struct ImportChat {
    title: Option<String>,
    #[serde(default)]
    people: Vec<Person>,
}
async fn import(State(s): State<AppState>, request: Request) -> Result<Response> {
    let (owner, _) = user(&s, request.headers()).await?;
    let _permit = tokio::time::timeout(
        Duration::from_millis(250),
        s.resources
            .acquire_background(monica_core::resources::TaskResources {
                memory_bytes: 128 * 1024 * 1024,
                ..Default::default()
            }),
    )
    .await
    .ok()
    .and_then(std::result::Result::ok)
    .ok_or(ApiError(
        StatusCode::TOO_MANY_REQUESTS,
        "会话导入暂时繁忙，请稍后再试。",
    ))?;
    let Json(input) = match Json::<Imported>::from_request(request, &s).await {
        Ok(input) => input,
        Err(error) => return Ok(error.into_response()),
    };
    if input.version != 1 || input.messages.len() > 12_000 {
        return Err(invalid("无法导入这个版本或大小的会话文件。"));
    }
    let id = Uuid::new_v4();
    let info = input.conversation.unwrap_or(ImportChat {
        title: None,
        people: vec![],
    });
    validate_people(&info.people)?;
    let title: String = info
        .title
        .as_deref()
        .unwrap_or("导入的会话")
        .chars()
        .take(60)
        .collect();
    let messages = input
        .messages
        .into_iter()
        .filter_map(agent::imported_message)
        .collect::<Vec<_>>();
    if messages
        .iter()
        .any(|message| message_bytes(message) > MAX_MESSAGE_BYTES)
    {
        return Err(invalid("导入文件中有一条会话记录过长。"));
    }
    let mut tx = s.pool.begin().await?;
    sqlx::query("INSERT INTO crm.ask_chats(id,owner_id,title,people) VALUES($1,$2,$3,$4)")
        .bind(id)
        .bind(owner)
        .bind(title)
        .bind(sqlx::types::Json(info.people))
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.ask_messages(chat_id,message) SELECT $1,value FROM jsonb_array_elements($2) WITH ORDINALITY AS entry(value,position) ORDER BY position")
        .bind(id).bind(Value::Array(messages)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(chat(&s.pool, owner, id).await?).into_response())
}

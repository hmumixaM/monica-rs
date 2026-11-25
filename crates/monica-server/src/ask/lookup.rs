//! The only bridge reachable by a generated script. Paths, methods and response
//! projections are selected here, never supplied by the script.
use super::{Actor, sandbox};
use crate::auth::{AppState, delegated_routes};
use anyhow::{Context, Result, anyhow, bail, ensure};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::Request,
    http::StatusCode,
};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::{OnceCell, mpsc, oneshot};
use tower::ServiceExt;
use uuid::Uuid;

const MAX_LOOKUPS: usize = 240;
const LOOKUP_BYTES: usize = 256 * 1024;
const DIRECTORY_BYTES: usize = 32 * 1024 * 1024;
const TRACE_BYTES: usize = 64 * 1024;
const MAX_BATCH: usize = 6;

const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "directory",
        "query?, type?, tags?, company?, school?, provider?, sort?, limit?, offset?",
        "按筛选条件列出联系人。tags 是标签名称或 ID 数组，必须全部匹配。type 默认 person，也支持 group、all。sort 可用 relevance、name、last_contact、messages、birthday。按 next_offset 翻页，limit 最大 500。",
    ),
    (
        "directory_facets",
        "kind, query?, limit?, offset?",
        "列出标签、公司或学校。kind 可用 tags、companies、schools。",
    ),
    (
        "find_people",
        "query",
        "按关键词查联系人，返回 profile_id。",
    ),
    (
        "get_person",
        "profile_id",
        "查人物资料、标签、工作教育经历、近况、来源和往来统计。",
    ),
    (
        "search_messages",
        "query, profile_id?, chat?, since?, before?",
        "搜索私聊和群聊，查询词用 | 分隔，最多 6 个，每项按原文匹配。可按人物、direct/group 或时间筛选。",
    ),
    (
        "read_messages",
        "profile_id, since?, until?, before?",
        "按时间阅读人物的私聊和群里发言。用返回的 earlier 继续往前翻。",
    ),
    (
        "get_posts",
        "profile_id, cursor?",
        "查人物的朋友圈、Instagram 和 LinkedIn 动态，按 next 翻页。",
    ),
    ("search_records", "query", "搜索动态、笔记和人物资料。"),
    (
        "search_mail",
        "query?, profile_id?, before?",
        "按关键词或人物搜索收录的邮件、短信等来源记录，按 earlier 翻页。",
    ),
    (
        "read_record",
        "record_id, offset?",
        "读一条邮件或来源记录的正文，按 next_offset 翻页。",
    ),
    (
        "relationship_overview",
        "",
        "查看最近联系、许久未联系、生日纪念日和近况回顾。",
    ),
];

pub(super) fn tool() -> Value {
    let descriptions = COMMANDS
        .iter()
        .map(|(name, args, description)| format!("{name}({args}) {description}"))
        .collect::<Vec<_>>()
        .join("\n");
    json!({"type":"function","function":{
        "name":"monica_cli",
        "description":format!("运行 JavaScript 查询 Monica。用同步函数 cli(command, args) 读取，再 return 需要的信息。可用 cli(\"batch\", [{{command,args}}], (result, index) => ...) 分批并行查阅并提取数据，结果顺序与输入一致。程序最多执行 240 项只读查询。cli(\"help\", {{command}}) 查看说明。\n{descriptions}"),
        "parameters":{"type":"object","properties":{
            "title":{"type":"string","description":"用简短中文概括这段脚本要查什么，如「筛选 QD 标签并读取近况」"},
            "script":{"type":"string","description":"JavaScript 函数体，最后 return 查询结果"}},
            "required":["title","script"],"additionalProperties":false}
    }})
}

pub(super) fn title(value: &Value) -> String {
    let value = value
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        "运行 JavaScript 查询".into()
    } else {
        value.chars().take(60).collect()
    }
}

struct Lookup {
    router: Router,
    zone: Tz,
    directory: OnceCell<Value>,
    attempts: AtomicUsize,
    good: AtomicUsize,
    failed: AtomicUsize,
}
struct ScriptRequest {
    input: String,
    reply: oneshot::Sender<String>,
}

pub(super) async fn run(
    state: &AppState,
    owner: Actor,
    zone: Tz,
    source: String,
) -> (String, bool, Value) {
    // Reserve space for the guest and one directory snapshot. The blocking
    // task keeps its permit until the guest exits, including after cancellation.
    let permit = match state
        .resources
        .acquire_background(monica_core::resources::TaskResources {
            cpu: 1,
            memory_bytes: 96 * 1024 * 1024,
            ..Default::default()
        })
        .await
    {
        Ok(permit) => permit,
        Err(_) => {
            return (
                error_value(anyhow!("JavaScript 查询暂时无法分配资源。")).to_string(),
                true,
                Value::Null,
            );
        }
    };
    let lookup = Lookup {
        router: delegated_routes(state.clone(), owner),
        zone,
        directory: OnceCell::new(),
        attempts: AtomicUsize::new(0),
        good: AtomicUsize::new(0),
        failed: AtomicUsize::new(0),
    };
    let (sender, mut requests) = mpsc::channel::<ScriptRequest>(1);
    let mut task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        sandbox::execute(&source, &mut |input| {
            let (reply, answer) = oneshot::channel();
            sender
                .blocking_send(ScriptRequest {
                    input: input.to_owned(),
                    reply,
                })
                .map_err(|_| anyhow!("查询已中止。"))?;
            answer.blocking_recv().context("查询已中止。")
        })
    });
    let mut queries = Vec::new();
    let mut trace_bytes = 0;
    let mut omitted = 0;
    let result = match tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(request) = requests.recv().await {
            let mut record = |name: &str, args: &Value| {
                let line = format!("cli({}, {})", json!(name), args);
                if trace_bytes + line.len() <= TRACE_BYTES {
                    trace_bytes += line.len();
                    queries.push(line);
                } else {
                    omitted += 1;
                }
            };
            let result = lookup
                .dispatch(&request.input, &mut record)
                .await
                .unwrap_or_else(error_value);
            let _ = request.reply.send(result.to_string());
        }
        (&mut task).await?
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow!("JavaScript 查询超时。请缩小范围或分批查询。")),
    };
    let details = json!({"queries":queries,"omittedQueries":omitted});
    match result {
        Ok(text) => (
            text,
            lookup.good.load(Ordering::Relaxed) == 0 && lookup.failed.load(Ordering::Relaxed) > 0,
            details,
        ),
        Err(error) => (error_value(error).to_string(), true, details),
    }
}

fn error_value(error: anyhow::Error) -> Value {
    json!({"error":error.to_string().chars().take(400).collect::<String>()})
}

impl Lookup {
    async fn dispatch(
        &self,
        input: &str,
        record: &mut (impl FnMut(&str, &Value) + Send),
    ) -> Result<Value> {
        let (name, args): (String, Value) =
            serde_json::from_str(input).context("查询参数无效。")?;
        if name != "batch" {
            record(&name, &args);
            return Ok(self.checked(&name, &args).await);
        }
        let entries = args
            .as_array()
            .filter(|items| items.len() <= MAX_BATCH)
            .context("batch 分片最多六项。")?;
        let entries = entries
            .iter()
            .map(|item| {
                (
                    item["command"].as_str().unwrap_or_default(),
                    item.get("args").cloned().unwrap_or(json!({})),
                )
            })
            .collect::<Vec<_>>();
        for (name, args) in &entries {
            record(name, args);
        }
        let rows = futures_util::future::join_all(
            entries.iter().map(|(name, args)| self.checked(name, args)),
        )
        .await;
        Ok(Value::Array(rows))
    }

    async fn checked(&self, name: &str, args: &Value) -> Value {
        let result = async {
            ensure!(
                self.attempts.fetch_add(1, Ordering::Relaxed) < MAX_LOOKUPS,
                "本脚本的查询次数已用完。请分批查询。"
            );
            ensure!(args.is_object(), "查询参数必须是对象。");
            let value = self.command(name, args).await?;
            ensure!(
                serde_json::to_vec(&value)?.len() <= LOOKUP_BYTES,
                "单次查询结果过长。请缩小范围。"
            );
            Ok(value)
        }
        .await;
        let value = result.unwrap_or_else(error_value);
        if value.get("error").is_some() {
            self.failed.fetch_add(1, Ordering::Relaxed);
        } else if name != "help" {
            self.good.fetch_add(1, Ordering::Relaxed);
        }
        value
    }

    async fn read(&self, path: &str, params: &[(&str, &str)], limit: usize) -> Result<Value> {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params.iter().filter(|(_, v)| !v.is_empty()))
            .finish();
        let uri = if query.is_empty() {
            path.to_owned()
        } else {
            format!("{path}?{query}")
        };
        for attempt in 0..=2 {
            let request = Request::builder().uri(&uri).body(Body::empty())?;
            let response = tokio::time::timeout(
                Duration::from_secs(12),
                self.router.clone().oneshot(request),
            )
            .await;
            match response {
                Ok(Ok(response)) if response.status().is_success() => {
                    return Ok(serde_json::from_slice(
                        &to_bytes(response.into_body(), limit)
                            .await
                            .context("资料超出单次查询大小。")?,
                    )?);
                }
                Ok(Ok(response))
                    if attempt == 2
                        || !response.status().is_server_error()
                            && response.status() != StatusCode::TOO_MANY_REQUESTS =>
                {
                    bail!("查询资料失败（HTTP {}）。", response.status().as_u16())
                }
                _ if attempt >= 2 => bail!("查询资料超时。"),
                _ => tokio::time::sleep(Duration::from_millis(250 << attempt)).await,
            }
        }
        unreachable!()
    }

    async fn get(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        self.read(path, params, 4 * 1024 * 1024).await
    }

    fn instant(&self, args: &Value, key: &str) -> Result<String> {
        let value = arg(args, key)?;
        if value.is_empty() {
            return Ok(String::new());
        }
        if let Ok(time) = DateTime::parse_from_rfc3339(value) {
            return Ok(time.to_rfc3339());
        }
        let day = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .context("日期请使用 YYYY-MM-DD 或带时区的时间。")?;
        let utc = self
            .zone
            .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .context("该时区没有这个日期。")?;
        Ok(utc.to_rfc3339())
    }

    fn date(&self, value: &Value, minute: bool) -> Value {
        value
            .as_str()
            .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
            .map(|time| {
                json!(
                    time.with_timezone(&self.zone)
                        .format(if minute { "%Y-%m-%d %H:%M" } else { "%Y-%m-%d" })
                        .to_string()
                )
            })
            .unwrap_or(Value::Null)
    }

    async fn command(&self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "help" => {
                let command = arg(args, "command")?;
                if command.is_empty() {
                    return Ok(json!(
                        COMMANDS
                            .iter()
                            .map(|(name, _, _)| *name)
                            .chain(["batch"])
                            .collect::<Vec<_>>()
                    ));
                }
                let (_, parameters, description) = COMMANDS
                    .iter()
                    .find(|(name, _, _)| *name == command)
                    .context("未知命令。")?;
                Ok(json!({"description":description,"parameters":parameters}))
            }
            "directory" | "directory_facets" => {
                let directory = self
                    .directory
                    .get_or_try_init(|| self.read("/api/v1/directory", &[], DIRECTORY_BYTES))
                    .await?;
                directory_command(
                    directory,
                    name,
                    args,
                    Utc::now()
                        .with_timezone(&self.zone)
                        .format("%m-%d")
                        .to_string()
                        .as_str(),
                )
            }
            "find_people" => {
                let q = required(args, "query")?;
                let rows = self
                    .get(
                        "/api/v1/profiles",
                        &[("q", q), ("profile_type", "person"), ("limit", "10")],
                    )
                    .await?;
                Ok(json!(array(&rows).iter().map(|row| json!({"profile_id":row["id"],"name":row["display_name"],"summary":cut(&row["summary"],120)})).collect::<Vec<_>>()))
            }
            "get_person" => self.person(id(args, "profile_id")?).await,
            "search_messages" | "read_messages" => self.messages(name, args).await,
            "get_posts" => {
                let id = id(args, "profile_id")?.to_string();
                let rows = self
                    .get(
                        "/api/v1/timeline",
                        &[
                            ("profile_id", &id),
                            ("limit", "20"),
                            ("cursor", arg(args, "cursor")?),
                        ],
                    )
                    .await?;
                Ok(page(&rows, "posts", "next", "cursor", 20, |row| {
                    json!({
                        "at":self.date(&row["occurred_at"],true),"provider":row["provider"],"text":cut(&row["summary"],500),
                        "images":array(&row["media"]).iter().filter_map(|media| media["description"].as_str()).take(4).map(|text| truncate(text,800)).collect::<Vec<_>>(),
                        "place":row["location"].get("name").unwrap_or(&row["location"]["poiName"]),"likes":row["like_count"],"comments":row["comment_count"],
                        "link":format!("/timeline?event={}",s(&row["id"]))
                    })
                }))
            }
            "search_records" => {
                let rows = self
                    .get("/api/v1/search", &[("q", required(args, "query")?)])
                    .await?;
                Ok(json!(array(&rows).iter().take(30).map(|row| json!({
                    "kind":row["kind"],"at":self.date(&row["occurred_at"],false),"person":row["profile_name"],"profile_id":row["profile_id"],
                    "title":cut(&row["title"],80),"text":cut(&row["summary"],300),
                    "link":if row["entity_type"]=="timeline" { Some(format!("/timeline?event={}",s(&row["id"]))) }
                        else { row["profile_id"].as_str().map(|id| format!("/profiles/{id}")) }
                })).collect::<Vec<_>>()))
            }
            "search_mail" => {
                let profile = optional_id(args, "profile_id")?
                    .map(|x| x.to_string())
                    .unwrap_or_default();
                let rows = self
                    .get(
                        "/api/v1/evidence",
                        &[
                            ("q", arg(args, "query")?),
                            ("profile_id", &profile),
                            ("before", &self.instant(args, "before")?),
                            ("limit", "20"),
                        ],
                    )
                    .await?;
                Ok(page(
                    &rows,
                    "records",
                    "earlier",
                    "occurred_at",
                    20,
                    |row| {
                        json!({
                            "record_id":row["id"],"at":self.date(&row["occurred_at"],true),"source":row["provider"],"account":row["account_name"],
                            "title":cut(&row["title"],120),"text":cut(&row["body"],400),
                            "from":array(&row["metadata"]["participants"]).iter().filter(|p| p["role"]=="sender")
                                .map(|p| if p["is_self"]==true { "我" } else { p["name"].as_str().or(p["id"].as_str()).unwrap_or("") }).collect::<Vec<_>>().join("、"),
                            "link":evidence(&row["id"], &row["revision"])
                        })
                    },
                ))
            }
            "read_record" => {
                let record = id(args, "record_id")?;
                let offset = args["offset"]
                    .as_u64()
                    .or_else(|| s(&args["offset"]).parse().ok())
                    .unwrap_or(0)
                    .to_string();
                let row = self
                    .get(
                        &format!("/api/v1/evidence/{record}"),
                        &[("body_limit", "8000"), ("body_offset", &offset)],
                    )
                    .await?;
                Ok(
                    json!({"title":row["title"],"at":self.date(&row["occurred_at"],true),"source":row["provider"],"account":row["account_name"],
                    "body":row["body"],"next_offset":row["next_body_offset"],"events":row["events"],"link":evidence(&row["id"],&row["revision"])}),
                )
            }
            "relationship_overview" => self.relationships().await,
            _ => bail!("未知的 Monica 查询命令。"),
        }
    }

    async fn person(&self, id: Uuid) -> Result<Value> {
        let path = format!("/api/v1/profiles/{id}");
        let overview = format!("{path}/overview");
        let ai = format!("/api/v1/ai/profiles/{id}");
        let (overview, profile, ai) = tokio::join!(
            self.get(&overview, &[]),
            self.get(&path, &[]),
            self.get(&ai, &[])
        );
        let (overview, profile, ai) = (overview?, profile?, ai.unwrap_or(Value::Null));
        let header = &overview["header"];
        let sources = profile["presentation"]["sources"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(provider, fields)| {
                let fields = fields
                    .as_object()?
                    .iter()
                    .filter(|(key, value)| {
                        !key.contains("url")
                            && !key.ends_with("_id")
                            && value
                                .as_str()
                                .is_some_and(|text| !text.is_empty() && text.len() <= 400)
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<serde_json::Map<_, _>>();
                (!fields.is_empty()).then(|| (provider.clone(), Value::Object(fields)))
            })
            .collect::<serde_json::Map<_, _>>();
        let facts = array(&profile["presentation"]["facts"])
            .iter()
            .take(12)
            .map(|fact| {
                let value = &fact["value"];
                let company = [
                    &fact["company_name"],
                    &fact["school_name"],
                    &value["company_name"],
                    &value["school_name"],
                ]
                .into_iter()
                .find_map(Value::as_str)
                .unwrap_or("");
                let title = value["title"]
                    .as_str()
                    .or(value["degree"].as_str())
                    .unwrap_or("");
                truncate(
                    &format!(
                        "{} · {company} · {title} · {}—{}",
                        if fact["fact_type"] == "education" {
                            "教育"
                        } else {
                            "工作"
                        },
                        s(&value["start_date"]),
                        value["end_date"]
                            .as_str()
                            .unwrap_or(if value["is_current"] == true {
                                "至今"
                            } else {
                                ""
                            })
                    ),
                    200,
                )
            })
            .collect::<Vec<_>>();
        let contact = &overview["messages"];
        Ok(
            json!({"profile_id":id,"name":header["name"],"link":format!("/profiles/{id}"),
                "note":header["note"],"city":header["city"],"summary":cut(&header["summary"],400),
                "tags":array(&overview["tags"]).iter().map(|row| &row["name"]).collect::<Vec<_>>(),
                "chat":{"total":contact["total"],"sent_by_me":contact["sent"],"received":contact["received"],"first":self.date(&contact["first_contact_at"],false),"last":self.date(&contact["last_contact_at"],false)},
                "memory":if ai["memory"].is_object() { json!({"summary":ai["memory"]["summary"],"facts":ai["memory"]["facts"]}) } else { Value::Null },
                "recent_weeks":array(&ai["weekly_updates"]).iter().filter(|row| row["summary"].is_string()).take(4).map(|row| json!({"week":row["week_start"],"summary":row["summary"]})).collect::<Vec<_>>(),
                "career":facts,"sources":sources,
                "relationships":array(&overview["relationships"]).iter().take(20).map(|row| format!("{}：{}",s(&row["relationship_type"]),s(&row["name"]))).collect::<Vec<_>>(),
                "groups":array(&overview["groups"]).iter().take(15).map(|row| &row["name"]).collect::<Vec<_>>()
            }),
        )
    }

    async fn messages(&self, name: &str, args: &Value) -> Result<Value> {
        let profile = optional_id(args, "profile_id")?;
        let path = profile
            .map(|id| format!("/api/v1/profiles/{id}/messages"))
            .unwrap_or_else(|| "/api/v1/messages/search".into());
        let since = self.instant(args, "since")?;
        let before = self.instant(args, "before")?;
        let until = self.instant(args, "until")?;
        let chat = arg(args, "chat")?;
        ensure!(
            matches!(chat, "" | "direct" | "group"),
            "chat 可用 direct 或 group。"
        );
        let filters = [
            ("since", since.as_str()),
            ("before", before.as_str()),
            ("until", until.as_str()),
            ("conversation_type", chat),
            (
                "include_group_speech",
                if profile.is_some() { "true" } else { "" },
            ),
        ];
        if name == "read_messages" {
            ensure!(profile.is_some(), "请提供 profile_id。");
            let rows = self
                .get(&path, &[&filters[..], &[("limit", "60")]].concat())
                .await?;
            return Ok(page(
                &rows,
                "messages",
                "earlier",
                "occurred_at",
                60,
                |row| self.message(row, &[]),
            ));
        }
        let mut unique = HashSet::new();
        let terms = required(args, "query")?
            .split(['|', '｜'])
            .map(str::trim)
            .filter(|q| !q.is_empty() && unique.insert(*q))
            .take(6)
            .collect::<Vec<_>>();
        ensure!(!terms.is_empty(), "请提供要搜索的关键词。");
        let mut pages = Vec::new();
        let mut failures = Vec::new();
        for term in &terms {
            match self
                .get(
                    &path,
                    &[&filters[..], &[("q", *term), ("limit", "40")]].concat(),
                )
                .await
            {
                Ok(rows) => pages.push(rows),
                Err(error) => failures.push(json!({"term":term,"error":error.to_string()})),
            }
        }
        let cursor = pages
            .iter()
            .filter_map(|page| {
                let rows = array(page);
                (rows.len() == 40).then(|| &rows[39]["occurred_at"])
            })
            .max_by_key(|time| s(time))
            .unwrap_or(&Value::Null);
        let mut seen = HashSet::new();
        let mut messages = pages
            .iter()
            .flat_map(array)
            .filter(|row| {
                (cursor.is_null() || s(&row["occurred_at"]) >= s(cursor))
                    && seen.insert(s(&row["id"]))
            })
            .collect::<Vec<_>>();
        messages.sort_by(|a, b| s(&b["occurred_at"]).cmp(s(&a["occurred_at"])));
        let mut result = json!({"messages":messages.into_iter().map(|row| self.message(row,&terms)).collect::<Vec<_>>(),"earlier":cursor});
        if !failures.is_empty() {
            result["not_searched"] = json!(failures);
        }
        if pages.is_empty() {
            result["error"] = json!("所有关键词都查询失败。");
        }
        Ok(result)
    }

    fn message(&self, row: &Value, terms: &[&str]) -> Value {
        let body = message_text(s(&row["text"]));
        let text = if terms.is_empty() {
            truncate(&body, 600)
        } else {
            around(&body, terms)
        };
        let mut output = json!({"at":self.date(&row["occurred_at"],true),
            "from":if row["direction"]=="sent" { "我" } else { row["sender_name"].as_str().unwrap_or("TA") },
            "text":text,"link":if row["evidence_id"].is_string() { evidence(&row["evidence_id"],&row["evidence_revision"]) }
                else { format!("/messages?conversation={}&message={}",s(&row["conversation_id"]), s(&row["id"])) }});
        if row["conversation_type"] == "group" {
            output["group"] = row["conversation_name"].clone();
        } else if row["conversation_name"].is_string() {
            output["chat_with"] = row["conversation_name"].clone();
            output["profile_id"] = row["profile_id"].clone();
        }
        output
    }

    async fn relationships(&self) -> Result<Value> {
        let (insights, home, digests) = tokio::join!(
            self.get("/api/v1/insights", &[]),
            self.get("/api/v1/home", &[]),
            self.get("/api/v1/digests", &[])
        );
        let (insights, home, digests) = (insights?, home?, digests.unwrap_or(Value::Null));
        let contacts = |key, limit| {
            array(&insights[key]).iter().take(limit).map(|row| {
            let recent = array(&row["months"]).iter().rev().take(3).rev().map(|month|
                format!("{} {}条",s(&month["month"]), month["sent"].as_u64().unwrap_or(0)+month["received"].as_u64().unwrap_or(0))
            ).collect::<Vec<_>>();
            json!({"profile_id":row["profile_id"],"name":row["name"],"last_contact":self.date(&row["last_contact_at"],false),"direct_messages":row["direct_message_count"],"recent_months":recent})
        }).collect::<Vec<_>>()
        };
        let week = &digests["digests"][0];
        Ok(
            json!({"totals":home["key_numbers"],"recent_contacts":contacts("recent_contacts",20),"dormant_contacts":contacts("dormant_contacts",15),
                "upcoming_dates":array(&insights["upcoming_dates"]).iter().take(12).collect::<Vec<_>>(),
                "recent_posters":array(&insights["recent_posters"]).iter().take(12).map(|row| json!({"profile_id":row["profile_id"],"name":row["name"],"posts":row["posts"],"latest":self.date(&row["latest_at"],false)})).collect::<Vec<_>>(),
                "latest_weekly_review":if week.is_null() { Value::Null } else { json!({"week":week["week_start"],"summary":week["summary"]}) }
            }),
        )
    }
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}
fn s(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    match args.get(key) {
        None => Ok(""),
        Some(value) => value
            .as_str()
            .map(str::trim)
            .ok_or_else(|| anyhow!("{key} 必须是字符串。")),
    }
}
fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let text = arg(args, key)?;
    ensure!(!text.is_empty(), "请提供 {key}。");
    Ok(text)
}
fn optional_id(args: &Value, key: &str) -> Result<Option<Uuid>> {
    args.get(key)
        .map(|value| {
            Uuid::parse_str(s(value)).with_context(|| format!("{key} 必须是 Monica 记录的 UUID。"))
        })
        .transpose()
}
fn id(args: &Value, key: &str) -> Result<Uuid> {
    optional_id(args, key)?.with_context(|| format!("请提供 {key}。"))
}
fn truncate(value: &str, limit: usize) -> String {
    let text = value.trim();
    text.char_indices()
        .nth(limit)
        .map(|(at, _)| format!("{}…", &text[..at]))
        .unwrap_or_else(|| text.to_owned())
}
fn cut(value: &Value, limit: usize) -> String {
    truncate(s(value), limit)
}
fn evidence(id: &Value, revision: &Value) -> String {
    format!("/evidence/{}?revision={revision}", s(id))
}
fn page(
    rows: &Value,
    key: &str,
    next: &str,
    via: &str,
    limit: usize,
    project: impl Fn(&Value) -> Value,
) -> Value {
    let rows = array(rows);
    json!({ (key): rows.iter().map(project).collect::<Vec<_>>(),
        (next): if rows.len() == limit { &rows[limit-1][via] } else { &Value::Null }
    })
}
fn message_text(text: &str) -> String {
    static XML: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"<\?xml|<msg[\s>]").unwrap());
    static TITLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<title>(?:<!\[CDATA\[)?([^<\]]+)").unwrap()
    });
    static TAG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"<[^>]*>").unwrap());
    let Some(start) = XML.find(text) else {
        return text.to_owned();
    };
    let rest = TITLE
        .captures(text)
        .map(|captures| format!("[分享] {}", captures[1].trim()))
        .unwrap_or_else(|| {
            TAG.replace_all(&text[start.start()..], " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
    format!("{}{rest}", &text[..start.start()])
        .trim()
        .to_owned()
}
fn around(text: &str, terms: &[&str]) -> String {
    let lower = text.to_lowercase();
    let at = terms
        .iter()
        .filter_map(|term| {
            lower
                .find(&term.to_lowercase())
                .map(|i| lower[..i].chars().count())
        })
        .min()
        .unwrap_or(0);
    let skip = at.saturating_sub(80);
    if text.chars().count() <= 240 || skip == 0 {
        return truncate(text, 240);
    }
    format!(
        "…{}",
        truncate(&text.chars().skip(skip).take(241).collect::<String>(), 240)
    )
}

fn directory_command(root: &Value, name: &str, args: &Value, today: &str) -> Result<Value> {
    let integer = |key: &str, fallback: usize, min: usize, max: usize| -> Result<usize> {
        let Some(value) = args.get(key) else {
            return Ok(fallback);
        };
        value
            .as_u64()
            .and_then(|x| usize::try_from(x).ok())
            .filter(|x| (min..=max).contains(x))
            .with_context(|| format!("{key} 分页参数无效。"))
    };
    let offset = integer("offset", 0, 0, usize::MAX)?;
    let limit = integer("limit", 100, 1, 500)?;
    let query = arg(args, "query")?.to_lowercase();
    if name == "directory_facets" {
        let kind = required(args, "kind")?;
        ensure!(
            matches!(kind, "tags" | "companies" | "schools"),
            "kind 可用 tags、companies 或 schools。"
        );
        let matches = array(&root[kind])
            .iter()
            .filter(|entry| s(&entry[1]).to_lowercase().contains(&query))
            .collect::<Vec<_>>();
        return Ok(
            json!({"total":matches.len(),"next_offset":next(offset,limit,matches.len()),
            "rows":matches.iter().skip(offset).take(limit).map(|entry| json!({"id":entry[0],"name":entry[1]})).collect::<Vec<_>>()}),
        );
    }
    let resolve = |dictionary: &str, term: &str| -> Result<usize> {
        let entries = array(&root[dictionary]);
        if let Some(index) = entries.iter().position(|entry| entry[0] == term) {
            return Ok(index);
        }
        let matches = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| s(&entry[1]).to_lowercase() == term.to_lowercase())
            .collect::<Vec<_>>();
        ensure!(
            matches.len() == 1,
            "筛选项「{term}」不存在或重名。用 directory_facets 查看名称和 ID。"
        );
        Ok(matches[0].0)
    };
    let tags = match args.get("tags") {
        None => vec![],
        Some(tags) => tags
            .as_array()
            .filter(|tags| tags.len() <= 20)
            .context("tags 必须是最多 20 项的名称或 ID 数组。")?
            .iter()
            .map(|tag| resolve("tags", s(tag).trim()))
            .collect::<Result<Vec<_>>>()?,
    };
    let company = args
        .get("company")
        .map(|value| resolve("companies", s(value).trim()))
        .transpose()?;
    let school = args
        .get("school")
        .map(|value| resolve("schools", s(value).trim()))
        .transpose()?;
    let provider = arg(args, "provider")?.to_lowercase();
    let kind = arg(args, "type")?;
    let kind = if kind.is_empty() { "person" } else { kind };
    let sort = arg(args, "sort")?;
    ensure!(
        matches!(kind, "person" | "group" | "all"),
        "type 可用 person、group 或 all。"
    );
    ensure!(
        matches!(
            sort,
            "" | "relevance" | "name" | "last_contact" | "messages" | "birthday"
        ),
        "未知排序字段。"
    );
    let columns = array(&root["columns"]);
    let index = |key: &str| {
        columns
            .iter()
            .position(|column| column == key)
            .unwrap_or(usize::MAX)
    };
    let [
        id,
        ty,
        display,
        summary,
        tag,
        company_col,
        school_col,
        aliases,
        providers,
        headline,
        city,
        birth,
        contact,
        messages,
        members,
        group_names,
    ] = [
        "id",
        "type",
        "name",
        "summary",
        "tag_ids",
        "company_ids",
        "school_ids",
        "aliases",
        "providers",
        "headline",
        "city",
        "birthday_md",
        "last_contact_at",
        "direct_message_count",
        "member_count",
        "group_names",
    ]
    .map(index);
    let has = |values: &Value, index: usize| {
        array(values)
            .iter()
            .any(|v| v.as_u64() == Some(index as u64))
    };
    let terms = query.split_whitespace().collect::<Vec<_>>();
    let mut rows = array(&root["profiles"])
        .iter()
        .filter_map(|row| {
            if (kind != "all" && row[ty] != kind)
                || !tags.iter().all(|t| has(&row[tag], *t))
                || company.is_some_and(|c| !has(&row[company_col], c))
                || school.is_some_and(|c| !has(&row[school_col], c))
                || (!provider.is_empty()
                    && !array(&row[providers]).iter().any(|item| item == &provider))
            {
                return None;
            }
            let display_name = s(&row[display]).to_lowercase();
            if terms.is_empty() {
                return Some((row, display_name, 0));
            }
            let searchable = [&row[display], &row[summary], &row[headline], &row[city]]
                .into_iter()
                .chain(array(&row[aliases]))
                .chain(array(&row[group_names]))
                .filter_map(Value::as_str)
                .map(|text| {
                    format!(
                        "{} {} {}",
                        text.to_lowercase(),
                        monica_core::link_matching::pinyin_compact(text),
                        monica_core::link_matching::pinyin_initials(text)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            terms.iter().all(|term| searchable.contains(term)).then(|| {
                let score = if display_name == query {
                    3
                } else if display_name.starts_with(&query) {
                    2
                } else {
                    i32::from(display_name.contains(&query))
                };
                (row, display_name, score)
            })
        })
        .collect::<Vec<_>>();
    let birthday = |row: &Value| {
        let date = s(&row[birth]);
        (date.is_empty(), date < today, date.to_owned())
    };
    rows.sort_by(|(a, an, ar), (b, bn, br)| {
        let order = match sort {
            "last_contact" => s(&b[contact]).cmp(s(&a[contact])),
            "messages" => b[messages]
                .as_i64()
                .unwrap_or(0)
                .cmp(&a[messages].as_i64().unwrap_or(0)),
            "birthday" => birthday(a).cmp(&birthday(b)),
            "name" => std::cmp::Ordering::Equal,
            _ => br.cmp(ar),
        };
        order
            .then_with(|| an.cmp(bn))
            .then_with(|| s(&a[id]).cmp(s(&b[id])))
    });
    let labels = |row: &Value, key: &str| {
        array(row)
            .iter()
            .filter_map(|item| {
                array(&root[key])
                    .get(item.as_u64()? as usize)?
                    .get(1)
                    .cloned()
            })
            .collect::<Vec<_>>()
    };
    Ok(
        json!({"total":rows.len(),"next_offset":next(offset,limit,rows.len()),
            "rows":rows.iter().skip(offset).take(limit).map(|(r,_,_)| json!({
                "profile_id":r[id],"name":r[display],"type":r[ty],"city":r[city],"summary":cut(&r[summary],240),"headline":cut(&r[headline],240),
                "providers":r[providers],"tags":labels(&r[tag],"tags"),"companies":labels(&r[company_col],"companies"),"schools":labels(&r[school_col],"schools"),
                "birthday":r[birth],"last_contact":r[contact],"messages":r[messages],"members":r[members],"link":format!("/profiles/{}",s(&r[id]))
            })).collect::<Vec<_>>()
        }),
    )
}
fn next(offset: usize, limit: usize, total: usize) -> Option<usize> {
    let end = offset.saturating_add(limit);
    (end < total).then_some(end)
}

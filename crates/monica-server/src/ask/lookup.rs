//! The only bridge reachable by a generated script. Paths, methods and response
//! projections are selected here, never supplied by the script.
use super::{Actor, image_lookup, script_worker};
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
use tokio::sync::OnceCell;
use tower::ServiceExt;
use uuid::Uuid;

#[path = "aggregations.rs"]
mod aggregations;
#[path = "contact_insights.rs"]
mod contact_insights;

const LOOKUP_BYTES: usize = 256 * 1024;
const DIRECTORY_BYTES: usize = 32 * 1024 * 1024;
const TRACE_BYTES: usize = 64 * 1024;
const MAX_BATCH: usize = 6;
const FIELDS_HELP: &str = "每个查询可用 fields 指定返回字段，如 [\"profile_id\",\"headline\"] 或 [\"memory.summary\"]。列表按条筛字段，保留总数、分页和查询失败信息。batch 的每项可在 args 中指定 fields。";

struct CommandSpec {
    name: &'static str,
    parameters: &'static str,
    description: &'static str,
    list: Option<&'static str>,
    fields: &'static str,
}

impl CommandSpec {
    fn parameters(&self) -> String {
        if self.parameters.is_empty() {
            "fields?".into()
        } else {
            format!("{}, fields?", self.parameters)
        }
    }
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "directory",
        parameters: "query?, type?, tags?, company?, school?, provider?, sort?, limit?, offset?",
        description: "按筛选条件列出联系人。tags 是标签名称或 ID 数组，必须全部匹配。type 默认 person，也支持 group、all。sort 可用 relevance、name、last_contact、messages、birthday。按 next_offset 翻页，limit 最大 500。",
        list: Some("rows"),
        fields: "profile_id name headline companies schools tags city providers type summary birthday last_contact messages members link",
    },
    CommandSpec {
        name: "directory_facets",
        parameters: "kind, query?, limit?, offset?",
        description: "列出标签、公司或学校。kind 可用 tags、companies、schools。",
        list: Some("rows"),
        fields: "id name",
    },
    CommandSpec {
        name: "find_people",
        parameters: "query",
        description: "按关键词查联系人，返回 profile_id。",
        list: None,
        fields: "profile_id name summary",
    },
    CommandSpec {
        name: "get_person",
        parameters: "profile_id",
        description: "查人物资料、标签、工作教育经历、来源和往来统计。只需要人物总结或每周近况时，用 profile_summary。",
        list: None,
        fields: "profile_id name city note summary tags chat ai_enabled ai_error memory.summary memory.facts recent_weeks career sources relationships groups link",
    },
    CommandSpec {
        name: "profile_merge_preview",
        parameters: "source_profile_id, target_profile_id",
        description: "为同一人的两份资料提出合并建议。source 是要合入的资料，target 是保留的资料。返回影响范围、冲突和打开确认界面的链接。",
        list: None,
        fields: "source target counts conflicts can_merge link",
    },
    CommandSpec {
        name: "profile_summary",
        parameters: "profile_id, week?",
        description: "优先读取已保存的 AI 人物总结（Memory）和每周近况。默认最近 12 份已保存周报，也可用 week（YYYY-MM-DD，通常为周一）指定一周。含生成时间和周报来源链接。",
        list: None,
        fields: "profile_id enabled memory.summary memory.facts memory.gaps memory.generated_at recent_weeks.week recent_weeks.summary recent_weeks.sources link",
    },
    CommandSpec {
        name: "weekly_review",
        parameters: "week?, limit?, offset?",
        description: "查看整体周报。省略 week 时分页列出最近 104 份已保存的周报（limit 最大 30）；指定 week（YYYY-MM-DD，通常为周一）读取该周统计、重点联系人、重要近况和建议。",
        list: None,
        fields: "enabled digests week generated_at facts narrative link",
    },
    CommandSpec {
        name: "weekly_people",
        parameters: "week?, after?, limit?",
        description: "按周分页读取已保存的人物近况，默认上一完整周。week 为 YYYY-MM-DD，通常为周一。limit 最大 25。继续时传返回的 week 和 next（作为 after）。",
        list: Some("people"),
        fields: "profile_id name week summary generated_at sources link",
    },
    CommandSpec {
        name: "places",
        parameters: "profile_id?, query?, near?, provider?, year?, kind?, limit?, offset?",
        description: "查询足迹地图。query 匹配地点名称。near 接受 {city, radius_km} 或 {latitude, longitude, radius_km}，按距中心的直线距离筛选并从近到远排序。重名城市用逗号补充州或国家。全局还支持 provider（wechat、instagram、linkedin）、year 和 kind（post、profile、all）；查某年动态地点时用 kind=post。指定 profile_id 时查该人物的记录坐标。按 next_offset 翻页时保留相同筛选，limit 最大 50。",
        list: Some("points"),
        fields: "label count latitude longitude distance_km precision source_kind sources people_count people first_seen last_seen links",
    },
    CommandSpec {
        name: "relationship_network",
        parameters: "profile_id?, limit?",
        description: "查看关系图中的互动、已记录关系、共同小群和共同受众。指定 profile_id 聚焦某人。返回权重较高的关系样本及范围，limit 最大 50。",
        list: Some("edges"),
        fields: "from to interactions_a_to_b interactions_b_to_a shared_groups shared_audience relationship_types last_at",
    },
    CommandSpec {
        name: "chat_statistics",
        parameters: "profile_id, from_month?, to_month?",
        description: "读取人物已导入且未删除的私聊月统计，含收发、系统和方向未知的记录。用 YYYY-MM 筛选月份，含起止月。只看数量和趋势时优先用此命令。",
        list: Some("months"),
        fields: "month sent received total system unknown records",
    },
    CommandSpec {
        name: "contact_calendar",
        parameters: "year? 或 day?, limit?, offset?",
        description: "按年看每天联系了几个人，或用 day（YYYY-MM-DD）查看当天联系的对象、私聊条数与通话次数。按 America/Chicago 归日，私聊来自每周统计快照。可用 next_offset 翻页。",
        list: Some("rows"),
        fields: "date people profile_id name messages calls link",
    },
    CommandSpec {
        name: "contact_activity",
        parameters: "profile_id, from_month?, to_month?",
        description: "查看某人的私聊收发、通话和社交互动月趋势，按 America/Chicago 归月。用 YYYY-MM 指定起止月（含起止）。",
        list: Some("months"),
        fields: "month sent received calls interactions",
    },
    CommandSpec {
        name: "contact_events_summary",
        parameters: "profile_id",
        description: "读取人物已收录通话与收支的全部汇总。通话有语音、视频、方向和时长，收支金额按币种区分。",
        list: None,
        fields: "profile_id calls money money_amount_unit link",
    },
    CommandSpec {
        name: "source_coverage",
        parameters: "profile_id, provider?",
        description: "查看人物各已关联来源的已收录记录数量、时间范围和同步状态。可选 provider；覆盖范围只描述已收录记录。",
        list: Some("sources"),
        fields: "connection_id provider name state enabled last_success_at records earliest_at latest_at",
    },
    CommandSpec {
        name: "import_daily",
        parameters: "provider?, connection_id?, days?, limit?, offset?",
        description: "查看最近每天的导入记录、处理量、变化和失败数。可按来源或连接筛选，days 默认 7，最大 366；按 next_offset 翻页。",
        list: Some("records"),
        fields: "date provider connection_id stream records processed inserted changed unchanged failures",
    },
    CommandSpec {
        name: "group_members",
        parameters: "group_id, limit?, offset?",
        description: "列出群的已记录现任成员、角色和加入时间。群 ID 从 directory 的 group 结果取得；按 next_offset 翻页。",
        list: Some("members"),
        fields: "profile_id name role joined_at link",
    },
    CommandSpec {
        name: "social_statistics",
        parameters: "profile_id?, provider?, from_month?, to_month?",
        description: "查看个人或全局按平台汇总的发帖、获赞和评论量及发帖月趋势。provider 可用 wechat、instagram、linkedin；起止月仅筛月趋势，totals 为全部时间。UTC 归月。",
        list: Some("providers"),
        fields: "provider totals monthly first_at latest_at",
    },
    CommandSpec {
        name: "profile_history",
        parameters: "profile_id, field_key?, before?, limit?",
        description: "浏览人物资料字段历史版本，含资料来源、观测时间、冲突和删除状态。可用 field_key（如 monica.company）筛选；把 next 原样作为 before 继续。值过长会截短。",
        list: Some("revisions"),
        fields: "id field_key value value_truncated source_type source_account_id source_record_id operation observed_at is_conflict link",
    },
    CommandSpec {
        name: "profile_fact_history",
        parameters: "profile_id, fact_type?, before?, before_id?, limit?",
        description: "浏览人物结构化经历的来源历史，包括 employment（工作）、education（教育）和 address（地址）。保留原始观测时间和来源。翻页时把 next.before 和 next.before_id 原样传回。",
        list: Some("revisions"),
        fields: "id fact_type fact_key organization value value_truncated source_type source_account_id source_record_id observed_at is_conflict link",
    },
    CommandSpec {
        name: "search_messages",
        parameters: "query, profile_id?, chat?, since?, before?",
        description: "搜索私聊和群聊，查询词用 | 分隔，每项按原文匹配。可按人物、direct/group 或时间筛选。",
        list: Some("messages"),
        fields: "at from text link group chat_with profile_id image_ids",
    },
    CommandSpec {
        name: "read_messages",
        parameters: "profile_id, since?, until?, before?",
        description: "按时间阅读人物的私聊和群里发言。用返回的 earlier 继续往前翻。",
        list: Some("messages"),
        fields: "at from text link group chat_with profile_id image_ids",
    },
    CommandSpec {
        name: "get_posts",
        parameters: "profile_id?, provider?, since?, before?, media?, cursor?, limit?",
        description: "浏览全局时间线或人物时间线。可按来源（如 wechat、instagram、linkedin、manual）和起止时间筛选，media=ready 仅看有可用媒体的记录。按 next 传回 cursor，保留相同筛选，limit 最大 30。",
        list: Some("posts"),
        fields: "post_id at provider title name profile_id text images image_ids place likes comments link",
    },
    CommandSpec {
        name: "image_notes",
        parameters: "post_id? 或 image_id?",
        description: "读取已保存的图像识别文字和描述。动态传 get_posts 返回的 post_id，单张图片传 image_id，二选一。优先使用已保存识别结果，只有确需核实图像细节时才识图。",
        list: Some("notes"),
        fields: "image_id summary description text link",
    },
    CommandSpec {
        name: "profile_suggestions",
        parameters: "profile_id?, kind?, limit?, offset?",
        description: "读取待处理的 AI 资料建议，可按人物和类型筛选。保留 fingerprint 与来源；只有用户要求处理建议时才用 propose_monica_edit 提出操作。",
        list: Some("people"),
        fields: "profile_id name generated_at items items.fingerprint items.kind items.value items.date items.quote items.evidence link",
    },
    CommandSpec {
        name: "birthdays",
        parameters: "month?, limit?, offset?",
        description: "分页读取已记录生日的完整联系人列表，可按月份筛选。保留近似或仅月日的精度；按 next_offset 继续，limit 最大 100。",
        list: Some("rows"),
        fields: "profile_id name year month day precision next days_until link",
    },
    CommandSpec {
        name: "search_records",
        parameters: "query",
        description: "搜索动态、笔记和人物资料。",
        list: None,
        fields: "kind at person profile_id title text link",
    },
    CommandSpec {
        name: "search_mail",
        parameters: "query?, profile_id?, before?",
        description: "按关键词或人物搜索收录的邮件、短信等来源记录，按 earlier 翻页。",
        list: Some("records"),
        fields: "record_id at source account title from text link",
    },
    CommandSpec {
        name: "read_record",
        parameters: "record_id, offset?",
        description: "读一条邮件或来源记录的正文，按 next_offset 翻页。",
        list: None,
        fields: "title at source account body events link",
    },
    CommandSpec {
        name: "relationship_overview",
        parameters: "",
        description: "查看最近联系、许久未联系、生日纪念日和近况回顾。",
        list: None,
        fields: "totals recent_contacts dormant_contacts upcoming_dates recent_posters latest_weekly_review",
    },
];

fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|command| command.name == name)
}

pub(super) fn tool() -> Value {
    let descriptions = COMMANDS
        .iter()
        .map(|command| {
            format!(
                "{}({}) {} 可选字段：{}。",
                command.name,
                command.parameters(),
                command.description,
                command.fields
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    json!({"type":"function","function":{
        "name":"monica_cli",
        "description":format!("运行 JavaScript 查询 Monica。用同步函数 cli(command, args) 读取，再 return 需要的信息。可用 cli(\"batch\", [{{command,args}}], (result, index) => ...) 分批并行查阅并提取数据，结果顺序与输入一致。cli(\"help\", {{command}}) 查看说明。{FIELDS_HELP}\n{descriptions}"),
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
    good: AtomicUsize,
    failed: AtomicUsize,
}
pub(super) async fn run(
    state: &AppState,
    owner: Actor,
    zone: Tz,
    source: String,
) -> (String, bool, Value) {
    // Reserve CPU and space for the guest and one directory snapshot.
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
        good: AtomicUsize::new(0),
        failed: AtomicUsize::new(0),
    };
    let mut bridge = QueryBridge {
        lookup,
        queries: Vec::new(),
        trace_bytes: 0,
        omitted: 0,
    };
    let result =
        script_worker::run(&source, script_worker::SCRIPT_TIMEOUT, permit, &mut bridge).await;
    let details = json!({"queries":bridge.queries,"omittedQueries":bridge.omitted});
    match result {
        Ok(text) => (
            text,
            bridge.lookup.good.load(Ordering::Relaxed) == 0
                && bridge.lookup.failed.load(Ordering::Relaxed) > 0,
            details,
        ),
        Err(error) => (error_value(error).to_string(), true, details),
    }
}

struct QueryBridge {
    lookup: Lookup,
    queries: Vec<String>,
    trace_bytes: usize,
    omitted: usize,
}

impl script_worker::Bridge for QueryBridge {
    async fn reply(&mut self, input: String) -> String {
        let mut record = |name: &str, args: &Value| {
            let line = format!("cli({}, {})", json!(name), args);
            if self.trace_bytes + line.len() <= TRACE_BYTES {
                self.trace_bytes += line.len();
                self.queries.push(line);
            } else {
                self.omitted += 1;
            }
        };
        self.lookup
            .dispatch(&input, &mut record)
            .await
            .unwrap_or_else(error_value)
            .to_string()
    }
}

fn error_value(error: anyhow::Error) -> Value {
    json!({"error":error.to_string().chars().take(400).collect::<String>()})
}

fn requested_fields(args: &Value) -> Result<Option<Vec<&str>>> {
    let Some(fields) = args.get("fields") else {
        return Ok(None);
    };
    let fields = fields
        .as_array()
        .filter(|fields| !fields.is_empty() && fields.len() <= 32)
        .context("fields 必须是包含 1 到 32 条字段路径的数组。")?;
    let fields = fields
        .iter()
        .map(|field| {
            field
                .as_str()
                .filter(|field| {
                    field.len() <= 96
                        && field.split('.').all(|part| {
                            !part.is_empty()
                                && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                        })
                })
                .context("fields 必须使用字段名或点分路径。")
        })
        .collect::<Result<_>>()?;
    Ok(Some(fields))
}

fn dehydrate(command: &str, value: &mut Value, fields: &[&str]) {
    let list = command_spec(command).and_then(|spec| spec.list);
    if let Some(rows) = list.and_then(|key| value.get_mut(key)) {
        project(rows, fields, false);
    } else {
        project(value, fields, true);
    }
}

fn project(value: &mut Value, fields: &[&str], keep_metadata: bool) {
    match value {
        Value::Object(object) => object.retain(|key, value| {
            if fields.contains(&key.as_str())
                || (keep_metadata
                    && matches!(
                        key.as_str(),
                        "total" | "next" | "next_offset" | "earlier" | "error" | "not_searched"
                    ))
            {
                return true;
            }
            let nested = fields
                .iter()
                .filter_map(|path| path.strip_prefix(key.as_str())?.strip_prefix('.'))
                .collect::<Vec<_>>();
            if nested.is_empty() || !(value.is_object() || value.is_array()) {
                return false;
            }
            project(value, &nested, false);
            true
        }),
        Value::Array(rows) => {
            for row in rows {
                project(row, fields, false);
            }
        }
        _ => {}
    }
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
            ensure!(args.is_object(), "查询参数必须是对象。");
            let fields = requested_fields(args)?;
            let mut value = self.command(name, args).await?;
            if let Some(fields) = fields {
                dehydrate(name, &mut value, &fields);
            }
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
                            .map(|command| command.name)
                            .chain(["batch"])
                            .collect::<Vec<_>>()
                    ));
                }
                let spec = command_spec(command).context("未知命令。")?;
                Ok(json!({
                    "description":spec.description,"parameters":spec.parameters(),
                    "fields":FIELDS_HELP,"result_fields":spec.fields.split_whitespace().collect::<Vec<_>>(),
                    "list_key":spec.list
                }))
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
            "profile_merge_preview" => {
                let source = id(args, "source_profile_id")?;
                let target = id(args, "target_profile_id")?;
                let report = self
                    .get(
                        &format!("/api/v1/profiles/{source}/merge-preview"),
                        &[("target_id", &target.to_string())],
                    )
                    .await?;
                Ok(json!({
                    "source":{"id":source,"name":report["source"]["display_name"]},
                    "target":{"id":target,"name":report["target"]["display_name"]},
                    "counts":report["counts"],"conflicts":report["conflicts"],"can_merge":report["can_merge"],
                    "link":format!("/profiles/{source}?merge_with={target}")
                }))
            }
            "profile_summary" => self.profile_summary(args).await,
            "weekly_review" => self.weekly_review(args).await,
            "weekly_people" => self.weekly_people(args).await,
            "places" => self.places(args).await,
            "relationship_network" => self.relationship_network(args).await,
            "chat_statistics" => self.chat_statistics(args).await,
            "contact_calendar" => self.contact_calendar(args).await,
            "contact_activity" => self.contact_activity(args).await,
            "contact_events_summary" => self.contact_events_summary(args).await,
            "source_coverage" => self.source_coverage(args).await,
            "import_daily" => self.import_daily(args).await,
            "group_members" => self.group_members(args).await,
            "social_statistics" => self.social_statistics(args).await,
            "profile_history" => self.profile_history(args, false).await,
            "profile_fact_history" => self.profile_history(args, true).await,
            "birthdays" => self.birthdays(args).await,
            "profile_suggestions" => self.profile_suggestions(args).await,
            "image_notes" => self.image_notes(args).await,
            "search_messages" | "read_messages" => self.messages(name, args).await,
            "get_posts" => {
                let profile = optional_id(args, "profile_id")?
                    .map(|id| id.to_string())
                    .unwrap_or_default();
                let limit = integer(args, "limit", 20, 1, 30)?;
                let limit_str = limit.to_string();
                let rows = self
                    .get(
                        "/api/v1/timeline",
                        &[
                            ("profile_id", &profile),
                            ("provider", arg(args, "provider")?),
                            ("after", &self.instant(args, "since")?),
                            ("before", &self.instant(args, "before")?),
                            ("media", arg(args, "media")?),
                            ("limit", &limit_str),
                            ("cursor", arg(args, "cursor")?),
                        ],
                    )
                    .await?;
                Ok(page(&rows, "posts", "next", "cursor", limit, |row| {
                    json!({
                        "at":self.date(&row["occurred_at"],true),"provider":row["provider"],"text":cut(&row["summary"],500),
                        "post_id":row["id"],"profile_id":row["profile_id"],"name":row["profile_name"],"title":row["title"],
                        "images":array(&row["media"]).iter().filter_map(|media| media["description"].as_str()).take(4).map(|text| truncate(text,800)).collect::<Vec<_>>(),
                        "image_ids":image_lookup::image_ids(&row["media"]),
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
        let ai = format!("/api/v1/ai/profiles/{id}/summary");
        let (overview, profile, ai) = tokio::join!(
            self.get(&overview, &[]),
            self.get(&path, &[]),
            self.get(&ai, &[])
        );
        let (overview, profile) = (overview?, profile?);
        let (ai, ai_error) = match ai {
            Ok(value) => (value, None),
            Err(error) => (Value::Null, Some(error.to_string())),
        };
        let header = &overview["header"];
        let mut sources = profile["presentation"]["sources"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(provider, fields)| {
                let fields = fields
                    .as_object()?
                    .iter()
                    .filter(|(_, value)| {
                        value
                            .as_str()
                            .is_some_and(|text| !text.is_empty() && text.len() <= 400)
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<serde_json::Map<_, _>>();
                (!fields.is_empty()).then(|| (provider.clone(), Value::Object(fields)))
            })
            .collect::<serde_json::Map<_, _>>();
        let linkedin_urls = array(&profile["identities"])
            .iter()
            .filter(|identity| identity["provider"] == "linkedin")
            .filter_map(|identity| {
                identity["profile_url"]
                    .as_str()
                    .filter(|url| !url.is_empty())
            })
            .collect::<Vec<_>>();
        if let Some(first) = linkedin_urls.first() {
            let linkedin = sources
                .entry("linkedin")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap();
            let previous = linkedin.get("profile_url").and_then(Value::as_str);
            if !previous.is_some_and(|url| linkedin_urls.contains(&url))
                && let Some(previous) = linkedin.insert("profile_url".into(), json!(first))
            {
                linkedin.insert("source_profile_url".into(), previous);
            }
            linkedin.insert("linked_profile_urls".into(), json!(linkedin_urls));
        }
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
                "ai_enabled":ai["enabled"],"ai_error":ai_error,
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
        let images = image_lookup::image_ids(&row["media"]);
        if !images.is_empty() {
            output["image_ids"] = json!(images);
        }
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
fn integer(args: &Value, key: &str, fallback: usize, min: usize, max: usize) -> Result<usize> {
    let Some(value) = args.get(key) else {
        return Ok(fallback);
    };
    value
        .as_u64()
        .and_then(|x| usize::try_from(x).ok())
        .filter(|x| (min..=max).contains(x))
        .with_context(|| format!("{key} 必须是 {min} 到 {max} 之间的整数。"))
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
    let offset = integer(args, "offset", 0, 0, usize::MAX)?;
    let limit = integer(args, "limit", 100, 1, 500)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_preserve_page_and_partial_failure_metadata() {
        let mut value = json!({
            "messages":[{"text":"sample","link":"https://example.invalid","details":{"when":"today","other":"omit"}}],
            "earlier":"cursor",
            "not_searched":[{"term":"second","error":"offline"}]
        });
        dehydrate("search_messages", &mut value, &["text", "details.when"]);
        assert_eq!(
            value,
            json!({
                "messages":[{"text":"sample","details":{"when":"today"}}],
                "earlier":"cursor",
                "not_searched":[{"term":"second","error":"offline"}]
            })
        );
        let mut record = json!({"body":"sample","next_offset":3,"events":[]});
        dehydrate("read_record", &mut record, &["body"]);
        assert_eq!(record, json!({"body":"sample","next_offset":3}));
    }
}

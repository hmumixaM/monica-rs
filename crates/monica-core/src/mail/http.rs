//! OAuth HTTP transports; opaque Graph URLs are checked before attaching a token.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{collections::BTreeSet, time::Duration};
#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    pub query: Vec<(String, String)>,
    pub prefer: Option<String>,
}
pub struct Response {
    pub status: u16,
    pub bytes: Vec<u8>,
}
pub trait MailHttp: Send {
    fn get<'a>(&'a mut self, request: Request) -> MailFuture<'a, Response>;
}
pub struct OAuthHttp {
    client: reqwest::Client,
    token: String,
    host: &'static str,
    refresh: Option<(
        sqlx::PgPool,
        crate::credentials::CredentialCipher,
        Uuid,
        Uuid,
    )>,
}
impl OAuthHttp {
    pub fn with_refresh(
        mut self,
        pool: sqlx::PgPool,
        cipher: crate::credentials::CredentialCipher,
        cid: Uuid,
        lease: Uuid,
    ) -> Self {
        self.refresh = Some((pool, cipher, cid, lease));
        self
    }
    pub fn new(token: String, host: &'static str) -> Result<Self> {
        ensure!(
            ["gmail.googleapis.com", "graph.microsoft.com"].contains(&host),
            "invalid mail OAuth host"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()?,
            token,
            host,
            refresh: None,
        })
    }
}
fn allowed(url: &str, host: &str) -> Result<()> {
    let u = url::Url::parse(url)?;
    ensure!(
        u.scheme() == "https"
            && u.host_str() == Some(host)
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none(),
        "invalid mail HTTP origin"
    );
    Ok(())
}
impl MailHttp for OAuthHttp {
    fn get<'a>(&'a mut self, r: Request) -> MailFuture<'a, Response> {
        Box::pin(async move {
            allowed(&r.url, self.host)?;
            for attempt in 0..2 {
                let mut request = self
                    .client
                    .get(&r.url)
                    .bearer_auth(&self.token)
                    .query(&r.query);
                if let Some(prefer) = &r.prefer {
                    request = request.header("Prefer", prefer)
                }
                let mut response = request.send().await?;
                let status = response.status().as_u16();
                if status == 401
                    && attempt == 0
                    && let Some((pool, cipher, cid, lease)) = &self.refresh
                {
                    drop(response);
                    self.token = crate::mail_oauth::OAuthClient::new()?
                        .access_token(pool, cipher, *cid, *lease, true)
                        .await?;
                    continue;
                }

                ensure!(
                    response
                        .content_length()
                        .is_none_or(|n| n <= MAX_BODY as u64),
                    "mail response exceeds 16 MiB"
                );
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await? {
                    ensure!(
                        bytes.len() + chunk.len() <= MAX_BODY,
                        "mail response exceeds 16 MiB"
                    );
                    bytes.extend_from_slice(&chunk)
                }
                return Ok(Response { status, bytes });
            }
            anyhow::bail!("mail OAuth retry exhausted")
        })
    }
}
fn request(url: String) -> Request {
    Request {
        url,
        query: vec![],
        prefer: None,
    }
}
fn text(v: &Value, k: &str) -> String {
    v[k].as_str()
        .map(str::to_owned)
        .or_else(|| v[k].as_u64().map(|v| v.to_string()))
        .unwrap_or_default()
}
fn id(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
fn decode(r: Response) -> Result<Value> {
    ensure!(
        (200..300).contains(&r.status),
        "mail HTTP status {}",
        r.status
    );
    Ok(serde_json::from_slice(&r.bytes)?)
}
const GMAIL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/";
const GRAPH: &str = "https://graph.microsoft.com/v1.0/me/";
const SELECT: &str = "id,parentFolderId,conversationId,internetMessageId,internetMessageHeaders,subject,from,toRecipients,ccRecipients,receivedDateTime,bodyPreview,hasAttachments";
pub struct Gmail {
    pub http: Box<dyn MailHttp>,
}
impl Gmail {
    pub fn new(http: Box<dyn MailHttp>) -> Self {
        Self { http }
    }
    pub async fn history_id(&mut self) -> Result<String> {
        let v = decode(self.http.get(request(format!("{GMAIL}profile"))).await?)?;
        let history = text(&v, "historyId");
        ensure!(!history.is_empty(), "Gmail profile history ID missing");
        Ok(history)
    }
}
impl MailTransport for Gmail {
    fn checkpoint(&mut self) -> MailFuture<'_, Option<Value>> {
        Box::pin(async move { Ok(Some(json!({"history_id":self.history_id().await?}))) })
    }
    fn scan<'a>(&'a mut self, _folder: &'a str, cursor: &'a Value) -> MailFuture<'a, MailPage> {
        Box::pin(async move {
            let history = text(cursor, "history_id");
            let initial = if history.is_empty() {
                let old = text(cursor, "initial_history_id");
                if old.is_empty() {
                    self.history_id().await?
                } else {
                    old
                }
            } else {
                String::new()
            };
            let mut r = request(format!(
                "{GMAIL}{}",
                if history.is_empty() {
                    "messages"
                } else {
                    "history"
                }
            ));
            r.query.push(("maxResults".into(), "25".into()));
            if history.is_empty() {
                r.query.push((
                    "q".into(),
                    cursor["query"]
                        .as_str()
                        .unwrap_or("-in:spam -in:trash")
                        .into(),
                ));
            } else {
                r.query.push(("startHistoryId".into(), history.clone()));
            }
            if let Some(token) = cursor["page_token"].as_str() {
                r.query.push(("pageToken".into(), token.into()));
            }
            let response = self.http.get(r).await?;
            if response.status == 404 && !history.is_empty() {
                return Err(ExpiredCursor.into());
            }
            let page = decode(response)?;
            let mut ids = BTreeSet::new();
            let mut removed = BTreeSet::new();
            if history.is_empty() {
                for m in page["messages"].as_array().into_iter().flatten() {
                    ids.insert(text(m, "id"));
                }
            } else {
                for event in page["history"].as_array().into_iter().flatten() {
                    for change in ["messagesAdded", "labelsAdded", "labelsRemoved"] {
                        for m in event[change].as_array().into_iter().flatten() {
                            ids.insert(text(&m["message"], "id"));
                        }
                    }
                    for m in event["messagesDeleted"].as_array().into_iter().flatten() {
                        removed.insert(text(&m["message"], "id"));
                    }
                }
            }
            ensure!(
                ids.len() + removed.len() <= 1000,
                "Gmail history page exceeds bounded record limit"
            );
            let mut records = Vec::new();
            for mid in ids {
                if !mid.is_empty() && !removed.contains(&mid) {
                    records.push(self.metadata(&mid).await?);
                }
            }
            for mid in removed {
                records.push(json!({"external_id":mid,"deleted":true}));
            }
            let token = text(&page, "nextPageToken");
            let next = if history.is_empty() {
                if token.is_empty() {
                    json!({"history_id":initial})
                } else {
                    let mut n = json!({"initial_history_id":initial,"page_token":token});
                    if let Some(q) = cursor.get("query") {
                        n["query"] = q.clone()
                    }
                    n
                }
            } else if token.is_empty() {
                let h = text(&page, "historyId");
                ensure!(!h.is_empty(), "Gmail history ID missing");
                json!({"history_id":h})
            } else {
                json!({"history_id":history,"page_token":token})
            };
            Ok(MailPage {
                records,
                cursor: next,
                complete: token.is_empty(),
            })
        })
    }
    fn metadata<'a>(&'a mut self, mid: &'a str) -> MailFuture<'a, Value> {
        Box::pin(async move {
            let mut r = request(format!("{GMAIL}messages/{}", id(mid)));
            r.query.push(("format".into(), "metadata".into()));
            let response = self.http.get(r).await?;
            if response.status == 404 {
                return Ok(json!({"external_id":mid,"deleted":true}));
            }
            let v = decode(response)?;
            let mut headers = serde_json::Map::new();
            for h in v["payload"]["headers"].as_array().into_iter().flatten() {
                headers.insert(text(h, "name").to_lowercase(), h["value"].clone());
            }
            Ok(
                json!({"external_id":v["id"],"thread_id":v["threadId"],"labels":v.get("labelIds").cloned().unwrap_or(json!([])),"headers":headers,"preview":v["snippet"].as_str().unwrap_or(""),"timestamp_ms":text(&v,"internalDate").parse::<i64>()?}),
            )
        })
    }
    fn body<'a>(&'a mut self, mid: &'a str) -> MailFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let mut r = request(format!("{GMAIL}messages/{}", id(mid)));
            r.query.push(("format".into(), "raw".into()));
            let v = decode(self.http.get(r).await?)?;
            Ok(URL_SAFE_NO_PAD.decode(
                v["raw"]
                    .as_str()
                    .context("Gmail raw MIME missing")?
                    .trim_end_matches('='),
            )?)
        })
    }
}
pub struct Outlook {
    pub http: Box<dyn MailHttp>,
}
impl Outlook {
    pub fn new(http: Box<dyn MailHttp>) -> Self {
        Self { http }
    }
    fn record(v: &Value) -> Value {
        let mut headers = serde_json::Map::new();
        for h in v["internetMessageHeaders"].as_array().into_iter().flatten() {
            headers.insert(text(h, "name").to_lowercase(), h["value"].clone());
        }
        headers.insert("message-id".into(), json!(text(v, "internetMessageId")));
        headers.insert("subject".into(), json!(text(v, "subject")));
        json!({"external_id":v["id"],"thread_id":v["conversationId"],"headers":headers,"sender":v["from"],"to":v.get("toRecipients").cloned().unwrap_or(json!([])),"cc":v.get("ccRecipients").cloned().unwrap_or(json!([])),"occurred_at":v["receivedDateTime"],"preview":text(v,"bodyPreview"),"parent_folder_id":v["parentFolderId"],"has_attachments":v["hasAttachments"].as_bool().unwrap_or(false)})
    }
}
impl MailTransport for Outlook {
    fn scan<'a>(&'a mut self, folder: &'a str, cursor: &'a Value) -> MailFuture<'a, MailPage> {
        Box::pin(async move {
            let opaque = cursor["next_link"]
                .as_str()
                .or(cursor["delta_link"].as_str());
            let url = opaque
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{GRAPH}mailFolders/{}/messages/delta", id(folder)));
            allowed(&url, "graph.microsoft.com")?;
            let mut r = request(url);
            r.prefer = Some("IdType=\"ImmutableId\", odata.maxpagesize=25".into());
            if opaque.is_none() {
                r.query = vec![
                    ("$select".into(), SELECT.into()),
                    ("$orderby".into(), "receivedDateTime desc".into()),
                ];
                if let Some(since) = cursor["since"].as_str() {
                    r.query
                        .push(("$filter".into(), format!("receivedDateTime ge {since}")));
                }
            }
            let response = self.http.get(r).await?;
            if response.status == 410 {
                return Err(ExpiredCursor.into());
            }
            let page = decode(response)?;
            let values = page["value"]
                .as_array()
                .context("Graph delta value missing")?;
            ensure!(values.len() <= 1000, "Graph page exceeds bounded limit");
            let mut records = Vec::new();
            for v in values {
                records.push(if v.get("@removed").is_some() {
                    self.metadata(v["id"].as_str().context("Graph ID missing")?)
                        .await?
                } else {
                    Self::record(v)
                })
            }
            let following = page["@odata.nextLink"].as_str();
            let next = if let Some(next) = following {
                allowed(next, "graph.microsoft.com")?;
                json!({"next_link":next})
            } else {
                let delta = page["@odata.deltaLink"]
                    .as_str()
                    .context("Graph delta link missing")?;
                allowed(delta, "graph.microsoft.com")?;
                json!({"delta_link":delta})
            };
            Ok(MailPage {
                records,
                cursor: next,
                complete: following.is_none(),
            })
        })
    }
    fn metadata<'a>(&'a mut self, mid: &'a str) -> MailFuture<'a, Value> {
        Box::pin(async move {
            let mut r = request(format!("{GRAPH}messages/{}", id(mid)));
            r.query.push(("$select".into(), SELECT.into()));
            r.prefer = Some("IdType=\"ImmutableId\"".into());
            let response = self.http.get(r).await?;
            if response.status == 404 {
                return Ok(json!({"external_id":mid,"deleted":true}));
            }
            Ok(Self::record(&decode(response)?))
        })
    }
    fn body<'a>(&'a mut self, mid: &'a str) -> MailFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let mut r = request(format!("{GRAPH}messages/{}/$value", id(mid)));
            r.prefer = Some("IdType=\"ImmutableId\"".into());
            let response = self.http.get(r).await?;
            ensure!(
                (200..300).contains(&response.status),
                "Graph MIME status {}",
                response.status
            );
            Ok(response.bytes)
        })
    }
    fn folders<'a>(&'a mut self, cursor: &'a Value) -> MailFuture<'a, MailPage> {
        Box::pin(async move {
            let mut queue = cursor["queue"]
                .as_array()
                .cloned()
                .unwrap_or(vec![json!("root")]);
            if queue.is_empty() {
                return Ok(MailPage {
                    records: vec![],
                    cursor: json!({"queue":[]}),
                    complete: true,
                });
            }
            let parent = queue[0].as_str().context("Graph folder queue invalid")?;
            let next = cursor["next_link"].as_str();
            let url = next.map(str::to_owned).unwrap_or_else(|| {
                if parent == "root" {
                    format!("{GRAPH}mailFolders")
                } else {
                    format!("{GRAPH}mailFolders/{}/childFolders", id(parent))
                }
            });
            allowed(&url, "graph.microsoft.com")?;
            let mut r = request(url);
            r.prefer = Some("IdType=\"ImmutableId\"".into());
            if next.is_none() {
                r.query = vec![
                    ("$top".into(), "50".into()),
                    ("$select".into(), "id,displayName,childFolderCount".into()),
                ];
            }
            let page = decode(self.http.get(r).await?)?;
            let records: Vec<_> = page["value"]
                .as_array()
                .context("Graph folder value missing")?
                .iter()
                .filter(|v| v["@odata.type"] != "#microsoft.graph.mailSearchFolder")
                .cloned()
                .collect();
            for v in &records {
                if v["childFolderCount"].as_u64().unwrap_or(0) > 0 && !queue.contains(&v["id"]) {
                    queue.push(v["id"].clone());
                }
            }
            ensure!(queue.len() <= 10_000, "Graph folder queue exceeds limit");
            let following = page["@odata.nextLink"].as_str();
            if following.is_none() {
                queue.remove(0);
            }
            let mut cursor = json!({"queue":queue});
            if let Some(next) = following {
                allowed(next, "graph.microsoft.com")?;
                cursor["next_link"] = json!(next);
            }
            Ok(MailPage {
                records,
                cursor,
                complete: queue.is_empty(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    struct Fake {
        responses: VecDeque<(u16, Value)>,
        requests: Arc<Mutex<Vec<Request>>>,
    }
    impl MailHttp for Fake {
        fn get<'a>(&'a mut self, r: Request) -> MailFuture<'a, Response> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(r);
                let (status, value) = self
                    .responses
                    .pop_front()
                    .context("unexpected HTTP request")?;
                Ok(Response {
                    status,
                    bytes: serde_json::to_vec(&value)?,
                })
            })
        }
    }
    fn fake(responses: Vec<(u16, Value)>) -> (Box<dyn MailHttp>, Arc<Mutex<Vec<Request>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Box::new(Fake {
                responses: responses.into(),
                requests: requests.clone(),
            }),
            requests,
        )
    }
    #[tokio::test]
    async fn gmail_history_is_captured_before_initial_listing() {
        let (http, requests) = fake(vec![
            (200, json!({"historyId":"90"})),
            (
                200,
                json!({"messages":[{"id":"one"}],"nextPageToken":"next"}),
            ),
            (
                200,
                json!({"id":"one","threadId":"thread","internalDate":"1000","payload":{"headers":[{"name":"Subject","value":"hello"}]}}),
            ),
        ]);
        let page = Gmail::new(http).scan("", &json!({})).await.unwrap();
        assert!(!page.complete);
        assert_eq!(
            page.cursor,
            json!({"initial_history_id":"90","page_token":"next"})
        );
        let requests = requests.lock().unwrap();
        assert!(requests[0].url.ends_with("profile"));
        assert!(requests[1].url.ends_with("messages"));
        assert!(
            requests[2]
                .query
                .contains(&("format".into(), "metadata".into()))
        );
    }
    #[tokio::test]
    async fn expired_history_is_not_an_empty_success() {
        let (http, _) = fake(vec![(404, json!({}))]);
        let error = Gmail::new(http)
            .scan("", &json!({"history_id":"stale"}))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<ExpiredCursor>().is_some());
    }
    #[tokio::test]
    async fn graph_move_resolves_immutable_id_and_rejects_foreign_origin() {
        let (http, requests) = fake(vec![
            (
                200,
                json!({"value":[{"id":"m/+","@removed":{}}],"@odata.deltaLink":"https://graph.microsoft.com/v1.0/me/delta?opaque=1"}),
            ),
            (
                200,
                json!({"id":"m/+","parentFolderId":"new-folder","subject":"moved"}),
            ),
        ]);
        let mut outlook = Outlook::new(http);
        let page = outlook.scan("inbox", &json!({})).await.unwrap();
        assert_eq!(page.records[0]["parent_folder_id"], "new-folder");
        assert_ne!(page.records[0]["deleted"], true);
        assert!(requests.lock().unwrap()[1].url.ends_with("m%2F%2B"));
        assert!(
            outlook
                .scan(
                    "inbox",
                    &json!({"next_link":"https://attacker.invalid/steal"})
                )
                .await
                .is_err()
        );
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
    #[tokio::test]
    async fn graph_folders_keep_breadth_first_queue() {
        let (http, _) = fake(vec![(
            200,
            json!({"value":[{"id":"nested","childFolderCount":1},{"id":"search","@odata.type":"#microsoft.graph.mailSearchFolder"}],"@odata.nextLink":"https://graph.microsoft.com/v1.0/me/mailFolders?$skiptoken=next"}),
        )]);
        let page = Outlook::new(http).folders(&json!({})).await.unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.cursor["queue"], json!(["root", "nested"]));
        assert!(!page.complete);
    }
}

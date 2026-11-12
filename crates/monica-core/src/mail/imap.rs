//! TLS-only IMAP reader. EXAMINE and BODY.PEEK preserve remote read state.
use super::*;
use anyhow::bail;
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};
pub struct Imap<S = TlsStream<TcpStream>> {
    stream: BufReader<S>,
    sequence: u64,
    folder: String,
    validity: u64,
    pub idle_supported: bool,
}
#[derive(Debug)]
struct Part {
    line: String,
    literal: Option<Vec<u8>>,
}
fn quote(value: &str) -> Result<String> {
    ensure!(!value.contains(['\r', '\n', '\0']), "invalid IMAP argument");
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
/// RFC3501 modified UTF-7 mailbox names, including the literal ampersand.
pub fn mailbox(value: &str) -> String {
    let mut result = String::new();
    let mut wide = Vec::new();
    let flush = |wide: &mut Vec<u8>, out: &mut String| {
        if !wide.is_empty() {
            out.push('&');
            out.push_str(&STANDARD_NO_PAD.encode(&*wide).replace('/', ","));
            out.push('-');
            wide.clear();
        }
    };
    for c in value.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut wide, &mut result);
            if c == '&' {
                result.push_str("&-")
            } else {
                result.push(c)
            }
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                wide.extend_from_slice(&unit.to_be_bytes());
            }
        }
    }
    flush(&mut wide, &mut result);
    result
}
impl Imap {
    pub async fn connect(host: &str, port: u16, username: &str, password: &str) -> Result<Self> {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .context("invalid IMAP TLS hostname")?;
        let stream = tokio::time::timeout(Duration::from_secs(30), async {
            let tcp = TcpStream::connect((host, port)).await?;
            TlsConnector::from(Arc::new(config))
                .connect(name, tcp)
                .await
        })
        .await??;
        let mut connection = Self {
            stream: BufReader::new(stream),
            sequence: 0,
            folder: String::new(),
            validity: 0,
            idle_supported: false,
        };
        let greeting = tokio::time::timeout(Duration::from_secs(30), connection.line())
            .await
            .context("IMAP greeting deadline exceeded")??;
        ensure!(
            greeting.starts_with("* OK") || greeting.starts_with("* PREAUTH"),
            "IMAP greeting rejected"
        );
        if !greeting.starts_with("* PREAUTH") {
            connection
                .command(&format!("LOGIN {} {}", quote(username)?, quote(password)?))
                .await?;
        }
        let capabilities = connection.command("CAPABILITY").await?;
        connection.idle_supported = capabilities
            .iter()
            .any(|p| p.line.split_ascii_whitespace().any(|w| w == "IDLE"));
        Ok(connection)
    }
}
impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Imap<S> {
    async fn line(&mut self) -> Result<String> {
        let mut bytes = Vec::new();
        let n = (&mut self.stream)
            .take(64 * 1024 + 1)
            .read_until(b'\n', &mut bytes)
            .await?;
        ensure!(n > 0, "IMAP connection closed");
        ensure!(
            bytes.len() <= 64 * 1024 && bytes.ends_with(b"\n"),
            "IMAP line exceeds limit"
        );
        Ok(String::from_utf8(bytes)?
            .trim_end_matches(['\r', '\n'])
            .into())
    }
    async fn command(&mut self, command: &str) -> Result<Vec<Part>> {
        tokio::time::timeout(Duration::from_secs(30), self.command_inner(command))
            .await
            .context("IMAP command deadline exceeded")?
    }
    async fn command_inner(&mut self, command: &str) -> Result<Vec<Part>> {
        ensure!(!command.contains(['\r', '\n']), "invalid IMAP command");
        self.sequence += 1;
        let tag = format!("M{}", self.sequence);
        self.stream
            .get_mut()
            .write_all(format!("{tag} {command}\r\n").as_bytes())
            .await?;
        self.stream.get_mut().flush().await?;
        let mut result = Vec::new();
        let mut total = 0;
        loop {
            let line = self.line().await?;
            total += line.len();
            ensure!(
                total <= MAX_BODY + 1024 * 1024,
                "IMAP response exceeds limit"
            );
            if line.starts_with(&format!("{tag} ")) {
                ensure!(
                    line.starts_with(&format!("{tag} OK")),
                    "IMAP command rejected"
                );
                return Ok(result);
            }
            if line.starts_with("* BYE") {
                bail!("IMAP server closed connection")
            }
            let literal = if line.ends_with('}') {
                line.rsplit_once('{')
                    .and_then(|(_, n)| n.trim_end_matches('}').parse::<usize>().ok())
            } else {
                None
            };
            let literal = if let Some(n) = literal {
                ensure!(
                    n <= MAX_BODY && total + n <= MAX_BODY + 1024 * 1024,
                    "IMAP literal exceeds limit"
                );
                let mut bytes = vec![0; n];
                self.stream.read_exact(&mut bytes).await?;
                total += n;
                Some(bytes)
            } else {
                None
            };
            result.push(Part { line, literal });
            ensure!(result.len() <= 20_000, "IMAP response count exceeds limit");
        }
    }
    pub async fn examine(&mut self, folder: &str) -> Result<(u64, u64)> {
        let parts = self
            .command(&format!("EXAMINE {}", quote(&mailbox(folder))?))
            .await?;
        let number = |label: &str| -> Result<u64> {
            parts
                .iter()
                .find_map(|p| {
                    p.line
                        .split_once(&format!("[{label} "))
                        .and_then(|(_, v)| v.split(']').next())
                        .and_then(|v| v.parse().ok())
                })
                .with_context(|| format!("IMAP {label} missing"))
        };
        self.validity = number("UIDVALIDITY")?;
        self.folder = folder.into();
        Ok((self.validity, number("UIDNEXT")?.saturating_sub(1)))
    }
    async fn search(&mut self, criteria: &str) -> Result<Vec<u64>> {
        let parts = self.command(&format!("UID SEARCH {criteria}")).await?;
        let mut result = Vec::new();
        for p in parts {
            if let Some(values) = p.line.strip_prefix("* SEARCH") {
                for value in values.split_ascii_whitespace() {
                    result.push(value.parse()?);
                }
            }
        }
        Ok(result)
    }
    async fn fetch(&mut self, uid: u64, header: bool) -> Result<Vec<u8>> {
        let parts = self
            .command(&format!(
                "UID FETCH {uid} (BODY.PEEK[{}])",
                if header { "HEADER" } else { "" }
            ))
            .await?;
        let mut bytes = Vec::new();
        for part in parts {
            if let Some(literal) = part.literal {
                bytes.extend(literal);
            }
        }
        ensure!(!bytes.is_empty(), "IMAP message no longer available");
        Ok(bytes)
    }
    pub async fn reconcile(&mut self, folder: &str, known: &[Value]) -> Result<Vec<Value>> {
        ensure!(known.len() <= 25, "IMAP reconciliation limit exceeded");
        let (validity, _) = self.examine(folder).await?;
        let current: Vec<_> = known
            .iter()
            .filter(|r| {
                r["external_id"]
                    .as_str()
                    .and_then(|s| s.rsplit(':').nth(1))
                    .and_then(|n| n.parse::<u64>().ok())
                    == Some(validity)
            })
            .filter_map(|r| r["uid"].as_u64())
            .collect();
        let present: BTreeSet<_> = if current.is_empty() {
            BTreeSet::new()
        } else {
            self.search(&format!(
                "UID {}",
                current
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ))
            .await?
            .into_iter()
            .collect()
        };
        Ok(known
            .iter()
            .filter(|r| {
                !current.contains(&r["uid"].as_u64().unwrap_or(0))
                    || !present.contains(&r["uid"].as_u64().unwrap_or(0))
            })
            .map(|r| {
                let mut r = r.clone();
                r["deleted"] = json!(true);
                r
            })
            .collect())
    }
    /// Caller owns cancellation: dropping this future drops its socket. A false
    /// result means periodic polling remains necessary, not an empty mailbox.
    pub async fn idle(&mut self, folder: &str) -> Result<bool> {
        self.examine(folder).await?;
        if !self.idle_supported {
            return Ok(false);
        }
        self.sequence += 1;
        let tag = format!("M{}", self.sequence);
        self.stream
            .get_mut()
            .write_all(format!("{tag} IDLE\r\n").as_bytes())
            .await?;
        self.stream.get_mut().flush().await?;
        let continuation = tokio::time::timeout(Duration::from_secs(30), self.line()).await??;
        ensure!(continuation.starts_with('+'), "IMAP IDLE rejected");
        let changed = tokio::time::timeout(Duration::from_secs(25 * 60), async {
            loop {
                let line = self.line().await?;
                if line.starts_with("* BYE") {
                    bail!("IMAP IDLE closed")
                }
                if line.starts_with('*')
                    && ["EXISTS", "EXPUNGE", "FETCH", "VANISHED"]
                        .iter()
                        .any(|w| line.split_ascii_whitespace().any(|v| v == *w))
                {
                    return Ok::<_, anyhow::Error>(true);
                }
            }
        })
        .await
        .unwrap_or(Ok(false))?;
        self.stream.get_mut().write_all(b"DONE\r\n").await?;
        self.stream.get_mut().flush().await?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let line = self.line().await?;
                if line.starts_with(&format!("{tag} ")) {
                    ensure!(
                        line.starts_with(&format!("{tag} OK")),
                        "IMAP IDLE termination failed"
                    );
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await??;
        Ok(changed)
    }
}
impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> MailTransport for Imap<S> {
    fn reconcile<'a>(
        &'a mut self,
        folder: &'a str,
        known: &'a [Value],
    ) -> MailFuture<'a, Vec<Value>> {
        Box::pin(async move { Imap::reconcile(self, folder, known).await })
    }
    fn scan<'a>(&'a mut self, folder: &'a str, cursor: &'a Value) -> MailFuture<'a, MailPage> {
        Box::pin(async move {
            let (validity, highest) = self.examine(folder).await?;
            let last = if cursor["uidvalidity"].as_u64() == Some(validity) {
                cursor["uid"].as_u64().unwrap_or(0)
            } else {
                0
            };
            let descending = cursor["descending"] == true;
            let (low, high) = if descending {
                let high = if last > 0 {
                    highest.min(last - 1)
                } else {
                    highest
                };
                (high.saturating_sub(4095).max(1), high)
            } else if cursor["tail"] == true && last == 0 {
                (highest.saturating_sub(4095).max(1), highest)
            } else {
                (last + 1, highest.min(last + 4096))
            };
            let mut next = json!({"uidvalidity":validity,"uid":last});
            for key in ["since", "before", "descending"] {
                if let Some(v) = cursor.get(key) {
                    next[key] = v.clone();
                }
            }
            if high < low {
                return Ok(MailPage {
                    records: vec![],
                    cursor: next,
                    complete: true,
                });
            }
            let mut criteria = format!("UID {low}:{high}");
            for (key, command) in [("since", "SINCE"), ("before", "BEFORE")] {
                if let Some(value) = cursor[key].as_str() {
                    let date = chrono::NaiveDate::parse_from_str(value, "%d-%b-%Y")?;
                    criteria.push_str(&format!(" {command} {}", date.format("%d-%b-%Y")));
                }
            }
            let mut ids = self.search(&criteria).await?;
            ids.retain(|uid| *uid >= low && *uid <= high);
            ids.sort_unstable();
            ids.dedup();
            if descending {
                ids.reverse()
            }
            if cursor["tail"] == true && last == 0 && ids.len() > 25 {
                ids = ids.split_off(ids.len() - 25);
            }
            let exhausted = ids.len() <= 25;
            let mut records = Vec::new();
            for uid in ids.iter().take(25) {
                let mut record = self.metadata(&uid.to_string()).await?;
                record["folder"] = json!(folder);
                records.push(record);
            }
            let boundary = if exhausted {
                if descending { low } else { high }
            } else {
                *ids.get(24).context("IMAP boundary missing")?
            };
            next["uid"] = json!(boundary);
            Ok(MailPage {
                records,
                cursor: next,
                complete: exhausted
                    && if descending {
                        low == 1
                    } else {
                        high == highest
                    },
            })
        })
    }
    fn metadata<'a>(&'a mut self, id: &'a str) -> MailFuture<'a, Value> {
        Box::pin(async move {
            let uid = id
                .rsplit(':')
                .next()
                .context("IMAP UID missing")?
                .parse::<u64>()?;
            if let Some((folder, validity)) = stored_namespace(id) {
                let (current, _) = self.examine(&folder).await?;
                if current != validity || !self.search(&format!("UID {uid}")).await?.contains(&uid)
                {
                    return Ok(json!({"external_id":id,"deleted":true}));
                }
            }
            let raw = self.fetch(uid, true).await?;
            let parsed = MessageParser::default()
                .parse(&raw)
                .context("invalid IMAP headers")?;
            let mut headers = serde_json::Map::new();
            for (k, v) in parsed.headers_raw() {
                headers.insert(k.to_lowercase(), json!(v.trim()));
            }
            if let Some(subject) = parsed.subject() {
                headers.insert("subject".into(), json!(subject));
            }
            Ok(
                json!({"external_id":format!("{}:{}:{uid}",self.folder,self.validity),"uid":uid,"headers":headers,"folder":self.folder}),
            )
        })
    }
    fn body<'a>(&'a mut self, id: &'a str) -> MailFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if let Some((folder, validity)) = stored_namespace(id) {
                let (current, _) = self.examine(&folder).await?;
                ensure!(
                    current == validity,
                    "IMAP UIDVALIDITY changed before body fetch"
                );
            }
            self.fetch(
                id.rsplit(':').next().context("IMAP UID missing")?.parse()?,
                false,
            )
            .await
        })
    }
}
fn stored_namespace(id: &str) -> Option<(String, u64)> {
    let mut parts = id.rsplitn(3, ':');
    parts.next()?;
    let validity = parts.next()?.parse().ok()?;
    Some((parts.next()?.to_owned(), validity))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodes_folder_and_rejects_injection() {
        assert_eq!(mailbox("INBOX & 中文"), "INBOX &- &Ti1lhw-");
        assert!(quote("INBOX\r\nLOGOUT").is_err());
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    async fn fake(
        script: Vec<(&'static str, String)>,
    ) -> (Imap<tokio::io::DuplexStream>, tokio::task::JoinHandle<()>) {
        let (client, server) = tokio::io::duplex(256 * 1024);
        let task = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            for (expected, response) in script {
                let mut line = String::new();
                server.read_line(&mut line).await.unwrap();
                assert!(
                    line.trim_end().ends_with(expected),
                    "expected {expected}, got {line}"
                );
                server
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        });
        (
            Imap {
                stream: BufReader::new(client),
                sequence: 0,
                folder: String::new(),
                validity: 0,
                idle_supported: true,
            },
            task,
        )
    }
    #[tokio::test]
    async fn uid_reset_sparse_window_and_header_only_fetch() {
        let header = "From: a@example.invalid\r\nSubject: Invoice\r\n\r\n";
        let mut script = vec![
            (
                "EXAMINE \"INBOX\"",
                "* OK [UIDVALIDITY 11] valid\r\n* OK [UIDNEXT 8] next\r\nM1 OK readonly\r\n".into(),
            ),
            (
                "UID SEARCH UID 1:7",
                "* SEARCH 2 4 999\r\nM2 OK search\r\n".into(),
            ),
        ];
        for (uid, tag) in [(2, 3), (4, 4)] {
            script.push((
                if uid == 2 {
                    "UID FETCH 2 (BODY.PEEK[HEADER])"
                } else {
                    "UID FETCH 4 (BODY.PEEK[HEADER])"
                },
                format!(
                    "* 1 FETCH (BODY[HEADER] {{{}}}\r\n{header})\r\nM{tag} OK fetched\r\n",
                    header.len()
                ),
            ));
        }
        let (mut imap, server) = fake(script).await;
        let page = imap
            .scan("INBOX", &json!({"uidvalidity":10,"uid":999}))
            .await
            .unwrap();
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.records[0]["external_id"], "INBOX:11:2");
        assert_eq!(page.cursor, json!({"uidvalidity":11,"uid":7}));
        assert!(page.complete);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn failed_search_never_becomes_a_tombstone() {
        let (mut imap, server) = fake(vec![
            (
                "EXAMINE \"INBOX\"",
                "* OK [UIDVALIDITY 11] valid\r\n* OK [UIDNEXT 8] next\r\nM1 OK readonly\r\n".into(),
            ),
            ("UID SEARCH UID 2", "M2 NO temporary failure\r\n".into()),
        ])
        .await;
        assert!(
            imap.reconcile("INBOX", &[json!({"external_id":"INBOX:11:2","uid":2})])
                .await
                .is_err()
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn idle_wakes_and_sends_done() {
        let (mut imap, server) = fake(vec![
            (
                "EXAMINE \"INBOX\"",
                "* OK [UIDVALIDITY 11] valid\r\n* OK [UIDNEXT 8] next\r\nM1 OK readonly\r\n".into(),
            ),
            ("IDLE", "+ waiting\r\n* 8 EXISTS\r\n".into()),
            ("DONE", "M2 OK ended\r\n".into()),
        ])
        .await;
        assert!(imap.idle("INBOX").await.unwrap());
        server.await.unwrap();
    }
}

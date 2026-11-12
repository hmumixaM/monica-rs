//! Offline phone exports. Names are scoped to a conversation; ZIPs are never extracted.
#[path = "whatsapp_counter.rs"]
mod counter;
use anyhow::{Context, Result, ensure};
use chrono::{NaiveDate, NaiveTime, Offset, TimeZone};
use chrono_tz::Tz;
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, Read},
    path::Path,
    sync::LazyLock,
};

static STAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[?(\d{1,4}[/.-]\d{1,2}[/.-]\d{1,4}),?\s+(\d{1,2}:\d{2}(?::\d{2})?(?:\s*[APap][Mm])?)\]?\s*(?:-\s*)?(.*)$").unwrap()
});
static ATTACHMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)(?:<attached:\s*(.+?)>|^(.+?)\s+\(file attached\)$)").unwrap()
});
const MAX_RECORD: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub conversation: String,
    pub owner: String,
    pub timezone: String,
    pub date_order: String,
    pub conversation_kind: String,
    pub participants: Vec<String>,
}
pub struct PhoneExport<R> {
    reader: R,
    options: ExportOptions,
    zone: Tz,
    current: Option<(String, String, String)>,
    duplicates: counter::Counter,
    ended: bool,
    first: bool,
}
impl<R: BufRead> PhoneExport<R> {
    pub fn new(reader: R, options: ExportOptions) -> Result<Self> {
        ensure!(
            !options.conversation.trim().is_empty() && !options.owner.trim().is_empty(),
            "conversation and owner required"
        );
        ensure!(
            ["mdy", "dmy"].contains(&options.date_order.as_str()),
            "date_order must be mdy or dmy"
        );
        ensure!(
            ["group", "direct"].contains(&options.conversation_kind.as_str()),
            "conversation_kind must be group or direct"
        );
        if options.conversation_kind == "direct" && !options.participants.is_empty() {
            ensure!(
                options.participants.len() == 2
                    && options.participants[0] != options.participants[1]
                    && options.participants.contains(&options.owner),
                "direct export requires two distinct participants including owner"
            );
        }
        let zone = options.timezone.parse().context("unknown IANA timezone")?;
        Ok(Self {
            reader,
            options,
            zone,
            current: None,
            duplicates: counter::Counter::default(),
            ended: false,
            first: true,
        })
    }
    fn record(&mut self, (date, clock, content): (String, String, String)) -> Result<Value> {
        let parts: Vec<_> = date.split(['/', '.', '-']).collect();
        let n: Vec<u32> = parts
            .iter()
            .map(|v| v.parse())
            .collect::<std::result::Result<_, _>>()?;
        let (mut y, m, d) = if parts[0].len() == 4 {
            (n[0], n[1], n[2])
        } else if self.options.date_order == "mdy" {
            (n[2], n[0], n[1])
        } else {
            (n[2], n[1], n[0])
        };
        if y < 100 {
            y += 2000;
        }
        let clock = clock.to_uppercase();
        let clock = clock
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>();
        let format = match (clock.contains('M'), clock.matches(':').count() == 2) {
            (true, true) => "%I:%M:%S%p",
            (true, false) => "%I:%M%p",
            (false, true) => "%H:%M:%S",
            _ => "%H:%M",
        };
        let naive = NaiveDate::from_ymd_opt(y as i32, m, d)
            .context("invalid export date")?
            .and_time(NaiveTime::parse_from_str(&clock, format)?);
        // Python ZoneInfo's fold=0 chooses the earlier offset for ambiguous
        // times and the pre-transition offset for a spring-forward gap.
        let offset = self
            .zone
            .from_local_datetime(&naive)
            .earliest()
            .map(|v| v.offset().fix())
            .or_else(|| {
                self.zone
                    .from_local_datetime(&(naive - chrono::Duration::hours(3)))
                    .earliest()
                    .map(|v| v.offset().fix())
            })
            .context("invalid export timezone offset")?;
        let at = offset
            .from_local_datetime(&naive)
            .single()
            .context("invalid export timestamp")?
            .format("%Y-%m-%dT%H:%M:%S%:z")
            .to_string();
        let (author, body) = content.split_once(": ").unwrap_or(("", &content));
        let digest = Sha256::digest(format!(
            "{}\0{at}\0{author}\0{body}",
            self.options.conversation
        ));
        let occurrence = self.duplicates.next(digest.into())?;
        let external_id = format!("export:{digest:x}:{occurrence}");
        let participant = |name: &str, role: &str| {
            let phone = name.starts_with('+')
                && (8..=21).contains(&name.len())
                && name[1..]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ' ' || c == '-');
            json!({"id":format!("export:{:x}",Sha256::digest(format!("{}\0{name}",self.options.conversation))),"name":name,"role":role,"is_self":name==self.options.owner,"phones":if phone {vec![name.replace([' ','-'],"")]} else {vec![]}})
        };
        let mut people = if author.is_empty() {
            vec![]
        } else {
            vec![participant(author, "sender")]
        };
        if self.options.conversation_kind == "direct" && self.options.participants.len() == 2 {
            people.extend(
                self.options
                    .participants
                    .iter()
                    .filter(|n| n.as_str() != author)
                    .map(|n| participant(n, "recipient")),
            );
        }
        let attachments:Vec<_>=ATTACHMENT.captures_iter(body).map(|c|json!({"filename":c.get(1).or(c.get(2)).unwrap().as_str(),"availability":"not_in_export"})).collect();
        Ok(
            json!({"external_id":external_id,"thread_id":format!("export:{}",self.options.conversation),"kind":if author.is_empty(){"event"}else{"message"},"title":self.options.conversation,"body":body,"occurred_at":at,"participants":people,"direction":if author==self.options.owner{"sent"}else{"received"},"is_group":self.options.conversation_kind!="direct","included":!author.is_empty(),"attachments":attachments,"coverage":"手机导出文件实际包含的消息；姓名只在此会话内识别"}),
        )
    }
    fn advance(&mut self) -> Result<Option<Value>> {
        loop {
            let mut bytes = Vec::new();
            let count = (&mut self.reader)
                .take(MAX_RECORD as u64 + 1)
                .read_until(b'\n', &mut bytes)?;
            ensure!(bytes.len() <= MAX_RECORD, "export line exceeds 8 MiB");
            if count == 0 {
                self.ended = true;
                return self.current.take().map(|r| self.record(r)).transpose();
            }
            let raw = std::str::from_utf8(&bytes).context("export must be UTF-8")?;
            let raw = if self.first {
                self.first = false;
                raw.trim_start_matches('\u{feff}')
            } else {
                raw
            };
            let line = raw
                .trim_end_matches(['\r', '\n'])
                .replace('\u{200e}', "")
                .replace('\u{202f}', " ");
            if let Some(c) = STAMP.captures(&line) {
                let next = (c[1].into(), c[2].into(), c[3].into());
                if let Some(previous) = self.current.replace(next) {
                    return self.record(previous).map(Some);
                }
            } else if let Some(current) = &mut self.current {
                ensure!(
                    current.2.len() + line.len() < MAX_RECORD,
                    "export message exceeds 8 MiB"
                );
                current.2.push('\n');
                current.2.push_str(&line);
            } else {
                ensure!(line.trim().is_empty(), "unrecognized WhatsApp date format");
            }
        }
    }
}
impl<R: BufRead> Iterator for PhoneExport<R> {
    type Item = Result<Value>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        match self.advance() {
            Ok(Some(v)) => Some(Ok(v)),
            Ok(None) => None,
            Err(e) => {
                self.ended = true;
                Some(Err(e))
            }
        }
    }
}

pub struct ZipExport {
    archive: zip::ZipArchive<File>,
    text_index: usize,
}
impl ZipExport {
    pub fn open(path: &Path) -> Result<Self> {
        let mut archive = zip::ZipArchive::new(File::open(path)?)?;
        ensure!(archive.len() <= 100_000, "too many ZIP members");
        let mut texts = Vec::new();
        for i in 0..archive.len() {
            let f = archive.by_index(i)?;
            if !f.is_dir() && f.name().to_ascii_lowercase().ends_with(".txt") {
                texts.push(i);
            }
        }
        ensure!(
            texts.len() == 1,
            "ZIP must contain exactly one TXT conversation"
        );
        Ok(Self {
            archive,
            text_index: texts[0],
        })
    }
    pub fn text(&mut self) -> Result<impl Read + '_> {
        Ok(self.archive.by_index(self.text_index)?)
    }
    /// An ambiguous basename is deliberately not attached to any message.
    pub fn attachment(&mut self, name: &str) -> Result<Option<impl Read + '_>> {
        ensure!(
            !name.contains(['/', '\\']) && !name.is_empty(),
            "attachment name must be a basename"
        );
        let mut matches = Vec::new();
        for i in 0..self.archive.len() {
            let f = self.archive.by_index(i)?;
            if !f.is_dir() && f.name().rsplit('/').next() == Some(name) {
                matches.push(i)
            }
        }
        if matches.len() != 1 {
            return Ok(None);
        }
        let file = self.archive.by_index(matches[0])?;
        ensure!(
            file.size() <= crate::media::MAX_FILE,
            "attachment exceeds storage limit"
        );
        Ok(Some(file))
    }
}
/// Replays a durable device publication. A failed or cancelled commit leaves the
/// input intact; delayed history cannot undo an edit/revoke/media completion.
pub async fn consume_device_page(
    store: &crate::media::MediaStore,
    fence: &crate::connections::Fence,
    root: &Path,
) -> Result<bool> {
    let pending = root.join("pending");
    let mut entries = match tokio::fs::read_dir(&pending).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let mut next = None;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_file()
            && entry.path().extension().is_some_and(|e| e == "json")
            && next
                .as_ref()
                .is_none_or(|p: &std::path::PathBuf| entry.path() < *p)
        {
            next = Some(entry.path());
        }
    }
    let Some(path) = next else { return Ok(false) };
    ensure!(
        tokio::fs::metadata(&path).await?.len() <= 16 * 1024 * 1024,
        "device page exceeds 16 MiB"
    );
    let value: Value = serde_json::from_slice(&tokio::fs::read(&path).await?)?;
    let incoming = value["records"]
        .as_array()
        .context("device records array required")?;
    ensure!(incoming.len() <= 1000, "device page exceeds 1000 records");
    let mut records = Vec::new();
    for incoming in incoming {
        let mut record = incoming.clone();
        if let Some(attachments) = record.get_mut("attachments").and_then(Value::as_array_mut) {
            for attachment in attachments {
                if let Some(hash) = attachment
                    .as_object_mut()
                    .and_then(|a| a.remove("local_blob"))
                {
                    let hash = hash.as_str().context("attachment digest must be text")?;
                    crate::media::object_path(hash)?;
                    let input = root.join("attachments").join(hash);
                    ensure!(
                        !tokio::fs::symlink_metadata(&input)
                            .await?
                            .file_type()
                            .is_symlink(),
                        "device attachment cannot be symlink"
                    );
                    let blob = crate::source_blobs::archive_path(
                        store,
                        "whatsapp",
                        fence.connection_id,
                        &input,
                    )
                    .await?;
                    ensure!(blob.blob == hash, "device attachment digest mismatch");
                    let a = attachment
                        .as_object_mut()
                        .context("attachment object required")?;
                    a.insert("blob".into(), json!(blob.blob));
                    a.insert("size".into(), json!(blob.size));
                }
            }
        }
        records.push(record);
    }
    let mut tx = store.pool.begin().await?;
    crate::connections::verify_fence(&mut tx, fence).await?;
    // Serialize merges with other producers before reading the previous head.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("connection:{}", fence.connection_id))
        .execute(&mut *tx)
        .await?;
    let mut merged = Vec::new();
    for record in records {
        let external = record["external_id"]
            .as_str()
            .context("device external_id required")?;
        let previous:Option<Value>=sqlx::query_scalar("SELECT payload FROM whatsapp.connection_records WHERE connection_id=$1 AND external_id=$2").bind(fence.connection_id).bind(external).fetch_optional(&mut *tx).await?;
        if let Some(record) = merge_device(record, previous.as_ref())? {
            merged.push(record)
        }
    }
    crate::connections::commit_page_fenced(&mut tx,crate::Provider::Whatsapp,fence,"device-events",&merged,&json!({"file":path.file_name().context("device filename missing")?.to_str().context("device filename invalid")?})).await?;
    tx.commit().await?;
    tokio::fs::remove_file(&path).await?;
    Ok(true)
}
pub fn merge_device(mut record: Value, previous: Option<&Value>) -> Result<Option<Value>> {
    let update = record
        .as_object_mut()
        .context("device record must be object")?
        .remove("media_update")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if let Some(previous) = previous {
        if previous["deleted"] == true {
            return Ok(None);
        }
        if update {
            if previous["raw_message"] != record["raw_message"] {
                return Ok(None);
            }
            let attachments = record["attachments"].clone();
            record = previous.clone();
            record["attachments"] = attachments;
        } else if previous["raw_message"] == record["raw_message"]
            && previous["attachments"].as_array().is_some_and(|a| {
                a.iter()
                    .any(|v| v.get("blob").is_some() || v.get("availability").is_some())
            })
        {
            record["attachments"] = previous["attachments"].clone();
        }
        if previous["edited"] == true && record["edited"] != true && record["deleted"] != true {
            return Ok(None);
        }
        if record["deleted"] == true {
            let revoke = record["raw_message"].clone();
            record = previous.clone();
            record["deleted"] = json!(true);
            record["raw_revoke"] = revoke;
        } else if record["edited"] == true {
            record["occurred_at"] = previous["occurred_at"].clone();
        }
    }
    Ok(Some(record))
}

/// Atomic control shared by the embedded reader and optional external container.
pub async fn write_control(root: &Path, enabled: bool, paused: bool) -> Result<()> {
    write_state(
        root,
        "control.json",
        &json!({"enabled":enabled,"paused":paused}),
    )
    .await
}
async fn write_state(root: &Path, name: &str, value: &Value) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    tokio::fs::create_dir_all(root).await?;
    let file = tempfile::NamedTempFile::new_in(root)?;
    let mut output = tokio::fs::File::create(file.path()).await?;
    output.write_all(&serde_json::to_vec(value)?).await?;
    output.sync_all().await?;
    drop(output);
    file.persist(root.join(name)).map_err(|error| error.error)?;
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}
/// Explicit disconnect clears only local linked-device keys. Source facts,
/// pending events, media and account provenance remain intact.
pub async fn clear_local_session(root: &Path) -> Result<()> {
    for name in ["device.sqlite", "device.sqlite-wal", "device.sqlite-shm"] {
        match tokio::fs::remove_file(root.join(name)).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    write_state(
        root,
        "status.json",
        &json!({"state":"disconnected","updated_at":chrono::Utc::now().to_rfc3339()}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> ExportOptions {
        ExportOptions {
            conversation: "synthetic chat".into(),
            owner: "Alice".into(),
            timezone: "America/Chicago".into(),
            date_order: "mdy".into(),
            conversation_kind: "group".into(),
            participants: vec![],
        }
    }
    #[test]
    fn parses_multiline_duplicates_and_scoped_identity() {
        let input = "\u{feff}[1/2/24, 9:30:00 AM] Alice: hello\ncontinued\n[1/2/24, 9:31 AM] Bob: <attached: photo.jpg>\n[1/2/24, 9:31 AM] Bob: <attached: photo.jpg>\n";
        let records = PhoneExport::new(std::io::Cursor::new(input), options())
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["body"], "hello\ncontinued");
        assert_eq!(records[0]["occurred_at"], "2024-01-02T09:30:00-06:00");
        assert_eq!(records[1]["attachments"][0]["filename"], "photo.jpg");
        assert_ne!(records[1]["external_id"], records[2]["external_id"]);
        let mut other = options();
        other.conversation = "another chat".into();
        let record = PhoneExport::new(
            std::io::Cursor::new("1/2/24, 9:30 AM - Alice: hello"),
            other,
        )
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
        assert_ne!(
            records[0]["participants"][0]["id"],
            record["participants"][0]["id"]
        );
    }
    #[test]
    fn malformed_and_direct_validation() {
        assert!(
            PhoneExport::new(std::io::Cursor::new("not an export"), options())
                .unwrap()
                .next()
                .unwrap()
                .is_err()
        );
        let mut o = options();
        o.conversation_kind = "direct".into();
        o.participants = vec!["Alice".into(), "Bob".into()];
        let r = PhoneExport::new(std::io::Cursor::new("2/3/24, 14:20 - Bob: hello"), o)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(r["participants"].as_array().unwrap().len(), 2);
        assert_eq!(r["is_group"], false);
    }
    #[test]
    fn zip_never_extracts_and_ambiguous_names_are_not_guessed() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.zip");
        let file = File::create(&path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("chat.txt", "1/2/24, 9:00 - Alice: hello"),
            ("../a/photo.jpg", "one"),
            ("b/photo.jpg", "two"),
            ("../../unique.jpg", "unique"),
        ] {
            archive.start_file(name, options).unwrap();
            archive.write_all(body.as_bytes()).unwrap();
        }
        archive.finish().unwrap();
        let mut archive = ZipExport::open(&path).unwrap();
        assert!(archive.attachment("photo.jpg").unwrap().is_none());
        assert!(archive.attachment("../unique.jpg").is_err());
        let mut bytes = String::new();
        archive
            .attachment("unique.jpg")
            .unwrap()
            .unwrap()
            .read_to_string(&mut bytes)
            .unwrap();
        assert_eq!(bytes, "unique");
        assert!(!dir.path().join("a").exists());
    }
    #[test]
    fn delayed_history_cannot_undo_edit_revoke_or_completed_media() {
        let previous = json!({"external_id":"m","raw_message":{"text":"edited"},"edited":true,"occurred_at":"2024-01-01T00:00:00Z","attachments":[]});
        assert!(
            merge_device(
                json!({"external_id":"m","raw_message":{"text":"old"}}),
                Some(&previous)
            )
            .unwrap()
            .is_none()
        );
        let updated=merge_device(json!({"external_id":"m","raw_message":{"text":"edited"},"media_update":true,"attachments":[{"blob":"synthetic"}]}),Some(&previous)).unwrap().unwrap();
        assert_eq!(updated["attachments"][0]["blob"], "synthetic");
        assert_eq!(updated["edited"], true);
        let revoked = merge_device(
            json!({"external_id":"m","deleted":true,"raw_message":{"revoke":true}}),
            Some(&updated),
        )
        .unwrap()
        .unwrap();
        assert_eq!(revoked["occurred_at"], previous["occurred_at"]);
        assert_eq!(revoked["raw_revoke"], json!({"revoke":true}));
        assert!(merge_device(previous, Some(&revoked)).unwrap().is_none());
    }
    #[tokio::test]
    async fn disconnect_clears_keys_only_and_pause_preserves_them() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "device.sqlite",
            "device.sqlite-wal",
            "device.sqlite-shm",
            "account.json",
            "history.json",
        ] {
            tokio::fs::write(root.path().join(name), b"synthetic")
                .await
                .unwrap();
        }
        write_control(root.path(), true, true).await.unwrap();
        assert!(root.path().join("device.sqlite").exists());
        clear_local_session(root.path()).await.unwrap();
        assert!(!root.path().join("device.sqlite").exists());
        assert!(root.path().join("history.json").exists());
        assert!(root.path().join("account.json").exists());
        let status: Value = serde_json::from_slice(
            &tokio::fs::read(root.path().join("status.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status["state"], "disconnected");
        assert!(status.get("qr_data").is_none());
    }
}

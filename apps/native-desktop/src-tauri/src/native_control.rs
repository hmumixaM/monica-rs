//! Explicitly enabled native readers; ACK disk cursors only after durable HTTP commit.
use crate::native_sources;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};
type Result<T> = std::result::Result<T, String>;
pub const PROVIDERS: [&str; 2] = ["apple_contacts", "apple_messages"];
#[derive(Clone)]
pub struct Transport {
    pub url: String,
    pub token: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub enabled: bool,
    pub busy: bool,
    pub error: Option<String>,
    pub delivered: u64,
}
pub struct NativeControl {
    root: PathBuf,
    states: Mutex<BTreeMap<String, Status>>,
    locks: [Mutex<()>; 2],
    messages_watch: Mutex<(native_sources::MessagesWatch, u64)>,
}
fn index(provider: &str) -> Result<usize> {
    PROVIDERS
        .iter()
        .position(|p| *p == provider)
        .ok_or("未知本机来源".into())
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(temporary, path).map_err(|e| e.to_string())?;
    fs::File::open(path.parent().ok_or("缺少目录")?)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
impl NativeControl {
    pub fn new(root: PathBuf) -> Result<Self> {
        let saved: BTreeMap<String, bool> = match fs::read(root.join("native-sources.json")) {
            Ok(raw) => serde_json::from_slice(&raw).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self {
            root,
            states: Mutex::new(
                PROVIDERS
                    .into_iter()
                    .map(|p| {
                        (
                            p.into(),
                            Status {
                                enabled: saved.get(p).copied().unwrap_or(false),
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
            ),
            locks: [Mutex::new(()), Mutex::new(())],
            messages_watch: Mutex::new((native_sources::MessagesWatch::new(), 0)),
        })
    }
    pub fn status(&self) -> Result<BTreeMap<String, Status>> {
        self.states
            .lock()
            .map(|s| s.clone())
            .map_err(|_| "来源状态不可用".into())
    }
    fn update(&self, provider: &str, f: impl FnOnce(&mut Status)) -> Result<()> {
        f(self
            .states
            .lock()
            .map_err(|_| "来源状态不可用")?
            .get_mut(provider)
            .ok_or("未知来源")?);
        Ok(())
    }
    pub fn enabled(&self, provider: &str) -> bool {
        self.states
            .lock()
            .is_ok_and(|s| s.get(provider).is_some_and(|s| s.enabled))
    }
    pub fn action(&self, transport: &Transport, provider: &str, action: &str) -> Result<()> {
        index(provider)?;
        if !["enable", "disable", "sync"].contains(&action) {
            return Err("未知来源操作".into());
        }
        if action != "sync" {
            let enabled = action == "enable";
            // Pause locally before contacting the server, so a network failure cannot keep reading.
            if !enabled {
                self.save_enabled(provider, false)?;
            }
            post(
                transport,
                "source-state",
                &json!({"provider":provider,"external_account":"local","display_name":name(provider),"enabled":enabled}),
            )?;
            if enabled {
                self.save_enabled(provider, true)?;
            }
        }
        if action != "disable" {
            self.sync(transport, provider, action == "enable")?;
        }
        Ok(())
    }
    fn save_enabled(&self, provider: &str, enabled: bool) -> Result<()> {
        let mut states = self.states.lock().map_err(|_| "来源状态不可用")?;
        let values: BTreeMap<_, _> = states
            .iter()
            .map(|(p, s)| (p.clone(), if p == provider { enabled } else { s.enabled }))
            .collect();
        atomic(
            &self.root.join("native-sources.json"),
            &serde_json::to_vec(&values).map_err(|e| e.to_string())?,
        )?;
        states.get_mut(provider).ok_or("未知来源")?.enabled = enabled;
        Ok(())
    }
    pub fn sync(&self, transport: &Transport, provider: &str, prompt: bool) -> Result<()> {
        let i = index(provider)?;
        let Ok(_guard) = self.locks[i].try_lock() else {
            return Ok(());
        };
        if !self.enabled(provider) {
            return Err("此来源尚未启用".into());
        }
        self.update(provider, |s| {
            s.busy = true;
            s.error = None
        })?;
        let run_id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{provider}:{:?}", std::time::SystemTime::now()).as_bytes(),
        );
        let report = |state: &str| {
            if post(
                transport,
                "source-run",
                &json!({"provider":provider,"run_id":run_id.to_string(),"state":state}),
            )
            .is_err()
            {
                eprintln!("Native source telemetry could not be saved");
            }
        };
        let mut reported = false;
        let result = (|| {
            if !remote_enabled(transport, provider)? {
                self.save_enabled(provider, false)?;
                return Ok(0);
            }
            report("collecting");
            reported = true;
            if cfg!(debug_assertions) && std::env::var_os("MONICA_RUST_TEST_DATA_DIR").is_some() {
                return Err("测试模式不读取本机联系人或 Messages；请使用合成读取器测试".into());
            }
            if provider == "apple_contacts" {
                native_sources::collect_contacts(&self.root, prompt)?;
            } else {
                let mut watch = self
                    .messages_watch
                    .lock()
                    .map_err(|_| "Messages监视状态不可用")?;
                watch.1 += 1;
                let reconcile = prompt || watch.1 % 60 == 0;
                watch.0.poll(&self.root, reconcile)?;
            }
            let folder = self.root.join("raw").join(provider);
            if folder.join("pending.json").exists() {
                deliver_pending(transport, provider, &folder)
            } else {
                Ok(0)
            }
        })();
        if reported {
            report(if result.is_ok() { "ready" } else { "failed" });
        }
        self.update(provider, |s| {
            s.busy = false;
            match &result {
                Ok(n) => s.delivered += n,
                Err(error) => s.error = Some(error.clone()),
            }
        })?;
        result.map(|_| ())
    }
}
fn remote_enabled(transport: &Transport, provider: &str) -> Result<bool> {
    let response = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?
        .get(format!("{}/internal/source-state", transport.url))
        .bearer_auth(&transport.token)
        .send()
        .map_err(|_| "无法确认来源状态，本轮未读取本机资料")?;
    if !response.status().is_success() {
        return Err("来源状态不可用，本轮未读取本机资料".into());
    }
    let values: Vec<Value> = response
        .json()
        .map_err(|_| "来源状态格式无效，本轮未读取本机资料")?;
    Ok(values.iter().any(|value| {
        value["provider"] == provider
            && value["enabled"] == true
            && value["state"] != "paused"
            && value["state"] != "disconnected"
    }))
}
fn name(provider: &str) -> &str {
    if provider == "apple_contacts" {
        "本机 Contacts"
    } else {
        "本机 Messages"
    }
}
fn post(transport: &Transport, path: &str, body: &Value) -> Result<Value> {
    let response = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?
        .post(format!("{}/internal/{path}", transport.url))
        .bearer_auth(&transport.token)
        .json(body)
        .send()
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "本机同步未确认（HTTP {}），检查点已保留",
            response.status().as_u16()
        ));
    }
    response.json().map_err(|e| e.to_string())
}
pub fn deliver_pending(transport: &Transport, provider: &str, folder: &Path) -> Result<u64> {
    index(provider)?;
    let pending = folder.join("pending.json");
    let value: Value = serde_json::from_slice(&fs::read(&pending).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let records = value["records"].as_array().ok_or("来源页缺少 records")?;
    let contacts_cursor = json!({"token":value["token"]});
    let cursor = if provider == "apple_contacts" {
        &contacts_cursor
    } else {
        value.get("cursor").ok_or("来源页缺少 cursor")?
    };
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(serde_json::to_vec(&value).map_err(|e| e.to_string())?);
    let snapshot_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, &digest).to_string();
    let reset = provider == "apple_contacts" && value["reset"] == true;
    let batches = record_batches(records)?;
    for (i, batch) in batches.iter().enumerate() {
        let complete = i + 1 == batches.len();
        post(
            transport,
            "source-page",
            &json!({"provider":provider,"external_account":"local","display_name":name(provider),
            "stream":if provider=="apple_contacts"{"contacts"}else{"messages"},"records":batch,"cursor":cursor,
            "page_complete":complete,"snapshot_id":if reset{Some(&snapshot_id)}else{None},"reset":reset && i==0,
            "snapshot_complete":reset && complete,"coverage":value["coverage"]}),
        )?;
    }
    atomic(
        &folder.join("cursor.json"),
        &serde_json::to_vec(cursor).map_err(|e| e.to_string())?,
    )?;
    fs::remove_file(pending).map_err(|e| e.to_string())?;
    fs::File::open(folder)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(records.len() as u64)
}

fn record_batches(records: &[Value]) -> Result<Vec<&[Value]>> {
    const MAX_BYTES: usize = 3 * 1024 * 1024;
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (i, record) in records.iter().enumerate() {
        let length = serde_json::to_vec(record).map_err(|e| e.to_string())?.len() + 1;
        if length > MAX_BYTES {
            return Err("单条来源记录超过同步页上限，检查点已保留".into());
        }
        if i - start >= 1000 || bytes + length > MAX_BYTES {
            batches.push(&records[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += length;
    }
    if start < records.len() || records.is_empty() {
        batches.push(&records[start..]);
    }
    Ok(batches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, net::TcpListener, thread};
    #[test]
    fn failed_delivery_preserves_page_and_success_advances_cursor_atomically() {
        let folder = tempfile::tempdir().unwrap();
        let pending = json!({"records":[{"external_id":"synthetic","body":"fixture"}],"cursor":{"recent_max":42}});
        fs::write(
            folder.path().join("pending.json"),
            serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        fs::write(folder.path().join("cursor.json"), b"{\"recent_max\":1}").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let transport = Transport {
            url: format!("http://{}", listener.local_addr().unwrap()),
            token: "synthetic-control-token".into(),
        };
        let server = thread::spawn(move || {
            for code in [503, 200] {
                let (mut connection, _) = listener.accept().unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    connection.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let header = String::from_utf8(request).unwrap().to_lowercase();
                assert!(header.contains("authorization: bearer synthetic-control-token"));
                assert!(!header.contains("origin:"));
                let length = header
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                let mut body = vec![0; length];
                connection.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["cursor"]["recent_max"], 42);
                assert_eq!(body["records"][0]["external_id"], "synthetic");
                write!(connection,"HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
            }
        });
        assert!(deliver_pending(&transport, "apple_messages", folder.path()).is_err());
        assert!(folder.path().join("pending.json").exists());
        assert_eq!(
            fs::read(folder.path().join("cursor.json")).unwrap(),
            b"{\"recent_max\":1}"
        );
        assert_eq!(
            deliver_pending(&transport, "apple_messages", folder.path()).unwrap(),
            1
        );
        assert!(!folder.path().join("pending.json").exists());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(folder.path().join("cursor.json")).unwrap())
                .unwrap()["recent_max"],
            42
        );
        server.join().unwrap();
    }
    #[test]
    fn remote_pause_and_failed_state_check_prevent_native_reads() {
        for (code, body) in [
            (200, "[]"),
            (503, "{}"),
            (
                200,
                r#"[{"provider":"apple_contacts","enabled":false,"state":"paused"}]"#,
            ),
        ] {
            let folder = tempfile::tempdir().unwrap();
            let control = NativeControl::new(folder.path().into()).unwrap();
            control.save_enabled("apple_contacts", true).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let transport = Transport {
                url: format!("http://{}", listener.local_addr().unwrap()),
                token: "synthetic".into(),
            };
            let server = thread::spawn(move || {
                let (mut connection, _) = listener.accept().unwrap();
                let mut bytes = [0; 4096];
                let count = connection.read(&mut bytes).unwrap();
                let header = String::from_utf8_lossy(&bytes[..count]);
                assert!(header.starts_with("GET /internal/source-state "));
                write!(connection,"HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            let result = control.sync(&transport, "apple_contacts", false);
            if code == 200 {
                assert!(result.is_ok());
                assert!(!control.enabled("apple_contacts"));
            } else {
                assert!(result.is_err());
                assert!(control.enabled("apple_contacts"));
            }
            assert!(!folder.path().join("raw").exists());
            server.join().unwrap();
        }
    }
    #[test]
    fn new_native_sources_are_disabled_and_unknown_provider_rejected() {
        let folder = tempfile::tempdir().unwrap();
        let control = NativeControl::new(folder.path().into()).unwrap();
        assert!(control.status().unwrap().values().all(|s| !s.enabled));
        assert!(index("../Messages").is_err());
        control.save_enabled("apple_contacts", true).unwrap();
        assert!(
            NativeControl::new(folder.path().into())
                .unwrap()
                .enabled("apple_contacts")
        );
    }
}

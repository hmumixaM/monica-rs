//! Own one explicitly enabled Chatlog child; never discover or adopt an existing service.
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::os::unix::{
    fs::{OpenOptionsExt, PermissionsExt},
    process::CommandExt,
};
use std::{
    fs,
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub enabled: bool,
    pub configured: bool,
    pub running: bool,
    pub url: String,
    pub error: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
struct Preferences {
    enabled: bool,
    port: u16,
}
#[derive(Default, Deserialize, Serialize)]
pub struct ImageCredentials {
    pub image_key: Option<String>,
    pub image_xor: Option<u8>,
}
impl ImageCredentials {
    pub fn validate(&self) -> Result<()> {
        if self.image_key.as_ref().is_some_and(|key| {
            key.len() != 16 && !(key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit()))
        }) {
            return Err("图片密钥须为16字节或32位十六进制".into());
        }
        Ok(())
    }
}
pub struct Chatlog {
    root: PathBuf,
    binary: PathBuf,
    log: PathBuf,
    preferences: Preferences,
    child: Option<Child>,
    error: Option<String>,
    retry: Instant,
}
fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(value).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(temporary, path).map_err(|e| e.to_string())?;
    fs::File::open(path.parent().ok_or("缺少目录")?)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
fn source_anchor(source: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(b"monica-chatlog-source-v1\0");
    hash.update(source.as_os_str().as_encoded_bytes());
    format!("native-chatlog-{:x}", hash.finalize())
}
impl Chatlog {
    pub fn new(root: PathBuf, binary: PathBuf, log: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let mut preferences: Preferences = match fs::read(root.join("preferences.json")) {
            Ok(raw) => serde_json::from_slice(&raw).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
            Err(e) => return Err(e.to_string()),
        };
        // Reserve an unused loopback number before the Rust child receives its URL.
        if preferences.port < 1024 || TcpListener::bind(("127.0.0.1", preferences.port)).is_err() {
            preferences.port = TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| e.to_string())?
                .local_addr()
                .map_err(|e| e.to_string())?
                .port();
        }
        save(&root.join("preferences.json"), &preferences)?;
        if root.join("chatlog-server.json").is_file() {
            let mut config: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join("chatlog-server.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            config["http_addr"] = json!(format!("127.0.0.1:{}", preferences.port));
            save(&root.join("chatlog-server.json"), &config)?;
        }
        Ok(Self {
            root,
            binary,
            log,
            preferences,
            child: None,
            error: None,
            retry: Instant::now(),
        })
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.preferences.port)
    }
    /// Export only this app's published snapshot directory and an opaque source anchor.
    /// Never discover a WeChat account or expose the original path/key to collectors.
    fn source_paths(&self) -> Result<Option<(PathBuf, PathBuf)>> {
        let path = self.root.join("chatlog-server.json");
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let config: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "微信配置格式无效")?;
        let source = Path::new(
            config["data_dir"]
                .as_str()
                .ok_or("微信配置缺少显式来源目录")?,
        );
        let work = Path::new(
            config["work_dir"]
                .as_str()
                .ok_or("微信配置缺少独立工作目录")?,
        );
        if !source.is_absolute() || work != self.root.join("work") {
            return Err("微信快照必须位于应用独立工作目录".into());
        }
        Ok(Some((work.join("db_storage"), source.to_path_buf())))
    }
    pub fn snapshot_source(&self) -> Result<Option<(PathBuf, String)>> {
        Ok(self
            .source_paths()?
            .map(|(snapshots, source)| (snapshots, source_anchor(&source))))
    }
    pub fn media_cache(&self) -> Result<Option<PathBuf>> {
        self.source_paths()?
            .map(|(_, source)| {
                let account = if source.ends_with("db_storage") {
                    source.parent().ok_or("微信账户目录缺失")?
                } else {
                    &source
                };
                Ok(account.join("msg"))
            })
            .transpose()
    }
    fn archive_old_snapshots(&self) -> Result<()> {
        let paths = ["work", "wcdb-cache"];
        if !paths.iter().any(|name| self.root.join(name).exists()) {
            return Ok(());
        }
        let archive = self.root.join("archived-snapshots");
        fs::create_dir_all(&archive).map_err(|e| e.to_string())?;
        fs::set_permissions(&archive, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let destination = archive.join(format!("{:016x}", rand::random::<u64>()));
        fs::create_dir(&destination).map_err(|e| e.to_string())?;
        for name in paths {
            let original = self.root.join(name);
            if original.exists() {
                fs::rename(original, destination.join(name)).map_err(|e| e.to_string())?;
            }
        }
        for folder in [&destination, &archive, &self.root] {
            fs::File::open(folder)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn status(&self) -> Status {
        Status {
            enabled: self.preferences.enabled,
            configured: self.root.join("chatlog-server.json").is_file(),
            running: self.child.is_some(),
            url: self.url(),
            error: self.error.clone(),
        }
    }
    pub fn configure(
        &mut self,
        source: &Path,
        key: &str,
        images: Option<&ImageCredentials>,
    ) -> Result<()> {
        if let Some(images) = images {
            images.validate()?;
        }
        if !source.is_absolute() || key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("请输入绝对微信数据目录与64位十六进制数据库密钥".into());
        }
        let work = self.root.join("work");
        if source.starts_with(&self.root) || self.root.starts_with(source) {
            return Err("微信原始目录必须与Monica工作目录分开".into());
        }
        let previous = self.snapshot_source()?.map(|(_, account)| account);
        self.stop()?;
        self.preferences.enabled = false;
        save(&self.root.join("preferences.json"), &self.preferences)?;
        if previous.as_deref() != Some(source_anchor(source).as_str()) {
            self.archive_old_snapshots()?;
        }
        save(
            &self.root.join("chatlog-server.json"),
            &json!({
                "platform": "darwin",
                "version": 4,
                "data_dir": source,
                "data_key": key,
                "work_dir": work,
                "http_addr": format!("127.0.0.1:{}", self.preferences.port),
                "desktop_mode": true,
            }),
        )?;
        if let Some(images) = images {
            save(
                &self.root.join("chatlog-image-key.json"),
                &json!({
                    "account": source_anchor(source),
                    "image_key": images.image_key,
                    "image_xor": images.image_xor,
                }),
            )?;
        }
        self.error = None;
        Ok(())
    }
    pub fn enable(&mut self, enabled: bool, paused: bool) -> Result<()> {
        if enabled && !self.status().configured {
            return Err("请先配置微信目录和数据库密钥".into());
        }
        self.preferences.enabled = enabled;
        save(&self.root.join("preferences.json"), &self.preferences)?;
        if enabled && !paused {
            self.start()
        } else {
            self.stop()
        }
    }
    pub fn start(&mut self) -> Result<()> {
        if self.child.is_some() || !self.preferences.enabled {
            return Ok(());
        }
        if cfg!(debug_assertions) && std::env::var_os("MONICA_RUST_TEST_DATA_DIR").is_some() {
            return Err("测试模式不会读取真实微信目录；请运行合成生命周期测试".into());
        }
        let result = self.start_owned();
        if let Err(error) = &result {
            self.error = Some(error.clone());
            self.retry = Instant::now() + Duration::from_secs(30);
        }
        result
    }
    fn start_owned(&mut self) -> Result<()> {
        if !self.binary.is_file() {
            return Err("缺少打包的Chatlog组件".into());
        }
        let output = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.log)
            .map_err(|e| e.to_string())?;
        let child = Command::new(&self.binary)
            .args(["serve", "--config-dir"])
            .arg(&self.root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .stdin(Stdio::null())
            .stdout(output.try_clone().map_err(|e| e.to_string())?)
            .stderr(output)
            .process_group(0)
            .spawn()
            .map_err(|e| e.to_string())?;
        self.child = Some(child);
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(500))
            .build()
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if self
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_some()
            {
                self.child = None;
                return Err("Chatlog未能打开指定微信数据，请检查配置与chatlog.log".into());
            }
            if client
                .get(format!("{}/api/v1/ready", self.url()))
                .send()
                .is_ok_and(|r| r.status().is_success())
            {
                self.error = None;
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.stop()?;
                return Err("Chatlog启动超时".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    pub fn stop(&mut self) -> Result<()> {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().map_err(|e| e.to_string())?.is_none() {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGTERM);
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                while child.try_wait().map_err(|e| e.to_string())?.is_none() {
                    if Instant::now() >= deadline {
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        child.wait().map_err(|e| e.to_string())?;
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
            self.child = None;
        }
        Ok(())
    }
    pub fn tick(&mut self, paused: bool) {
        if paused || !self.preferences.enabled {
            let _ = self.stop();
            return;
        }
        if self
            .child
            .as_mut()
            .is_some_and(|c| c.try_wait().is_ok_and(|s| s.is_some()))
        {
            self.child = None;
            self.error = Some("Chatlog意外退出，稍后重试".into());
            self.retry = Instant::now() + Duration::from_secs(10);
        }
        if self.child.is_none() && Instant::now() >= self.retry {
            let _ = self.start();
        }
    }
}
impl Drop for Chatlog {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_source_owns_child_pause_recovery_and_private_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let source = temp.path().join("synthetic source");
        fs::create_dir_all(&source).unwrap();
        let binary = temp.path().join("fake-chatlog.py");
        fs::write(
            &binary,
            r#"#!/usr/bin/python3
import json,sys,signal
from pathlib import Path
from http.server import BaseHTTPRequestHandler,HTTPServer
config=json.loads((Path(sys.argv[-1])/'chatlog-server.json').read_text())
class Handler(BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200);self.end_headers();self.wfile.write(b'{"ready":true}')
 def log_message(self,*args):pass
signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))
HTTPServer(('127.0.0.1',int(config['http_addr'].split(':')[-1])),Handler).serve_forever()
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let mut service =
            Chatlog::new(root.clone(), binary, temp.path().join("chatlog.log")).unwrap();
        assert!(!service.status().enabled);
        assert!(!service.status().running);
        assert!(service.configure(&source, "invalid", None).is_err());
        service.configure(&source, &"0".repeat(64), None).unwrap();
        assert_eq!(
            fs::metadata(root.join("chatlog-server.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        service.enable(true, false).unwrap();
        let first = service.child.as_ref().unwrap().id();
        assert!(service.status().running);
        service.tick(true);
        assert!(!service.status().running);
        assert!(service.status().enabled);
        assert_eq!(unsafe { libc::kill(first as i32, 0) }, -1);
        service.tick(false);
        let second = service.child.as_ref().unwrap().id();
        assert_ne!(first, second);
        unsafe {
            libc::kill(second as i32, libc::SIGTERM);
        }
        thread::sleep(Duration::from_millis(100));
        service.tick(false);
        assert!(!service.status().running);
        service.retry = Instant::now();
        service.tick(false);
        assert!(service.status().running);
        let last = service.child.as_ref().unwrap().id();
        service.enable(false, true).unwrap();
        assert_eq!(unsafe { libc::kill(last as i32, 0) }, -1);
        assert!(!service.status().enabled);
    }
    #[test]
    fn source_anchor_is_stable_and_changed_sources_archive_owned_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("chatlog");
        let mut service = Chatlog::new(
            root.clone(),
            temp.path().join("unused"),
            temp.path().join("log"),
        )
        .unwrap();
        assert!(service.snapshot_source().unwrap().is_none());
        let source = temp.path().join("explicit source A");
        service.configure(&source, &"1".repeat(64), None).unwrap();
        let (snapshots, account) = service.snapshot_source().unwrap().unwrap();
        assert_eq!(snapshots, root.join("work/db_storage"));
        assert!(account.starts_with("native-chatlog-"));
        assert_eq!(account.len(), 79);
        assert!(!account.contains("source"));
        fs::create_dir_all(&snapshots).unwrap();
        fs::write(snapshots.join("synthetic.db"), b"only synthetic data").unwrap();
        service.configure(&source, &"2".repeat(64), None).unwrap();
        assert_eq!(service.snapshot_source().unwrap().unwrap().1, account);
        assert!(snapshots.join("synthetic.db").exists());
        drop(service);
        let mut service = Chatlog::new(
            root.clone(),
            temp.path().join("unused"),
            temp.path().join("log"),
        )
        .unwrap();
        assert_eq!(service.snapshot_source().unwrap().unwrap().1, account);
        service
            .configure(
                &temp.path().join("explicit source B"),
                &"3".repeat(64),
                None,
            )
            .unwrap();
        assert_ne!(service.snapshot_source().unwrap().unwrap().1, account);
        assert!(!snapshots.exists());
        let archive = fs::read_dir(root.join("archived-snapshots"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read(archive.join("work/db_storage/synthetic.db")).unwrap(),
            b"only synthetic data"
        );
        let mut config: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("chatlog-server.json")).unwrap()).unwrap();
        config["work_dir"] = json!(temp.path().join("foreign"));
        save(&root.join("chatlog-server.json"), &config).unwrap();
        assert!(service.snapshot_source().is_err());
    }
    #[test]
    fn foreign_port_is_not_adopted() {
        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let occupied = listener.local_addr().unwrap().port();
        save(
            &temp.path().join("preferences.json"),
            &Preferences {
                enabled: false,
                port: occupied,
            },
        )
        .unwrap();
        let service = Chatlog::new(
            temp.path().into(),
            temp.path().join("absent"),
            temp.path().join("log"),
        )
        .unwrap();
        assert_ne!(service.preferences.port, occupied);
        assert!(listener.local_addr().is_ok());
    }
}

//! User-initiated login in the app's isolated collector directory. No account discovery.
use crate::native_control::Transport;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
type Result<T> = std::result::Result<T, String>;
const PROVIDERS: [&str; 2] = ["instagram", "linkedin"];
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub authenticated: bool,
    pub login_running: bool,
    pub enabled: bool,
    pub state: String,
    pub error: Option<String>,
}
pub struct Collectors {
    root: PathBuf,
    executable: PathBuf,
    children: BTreeMap<String, Child>,
    errors: BTreeMap<String, String>,
    enabled: BTreeMap<String, bool>,
}
impl Collectors {
    pub fn new(root: PathBuf, executable: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let enabled = match fs::read(root.join("enabled.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self {
            root,
            executable,
            children: BTreeMap::new(),
            errors: BTreeMap::new(),
            enabled,
        })
    }
    fn validate(provider: &str) -> Result<()> {
        if PROVIDERS.contains(&provider) {
            Ok(())
        } else {
            Err("未知采集来源".into())
        }
    }
    fn read(path: &Path) -> Value {
        fs::metadata(path)
            .ok()
            .filter(|m| m.len() < 1024 * 1024)
            .and_then(|_| fs::read(path).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null)
    }
    pub fn statuses(&mut self) -> Result<BTreeMap<String, Status>> {
        for provider in PROVIDERS {
            if let Some(child) = self.children.get_mut(provider)
                && let Some(exit) = child.try_wait().map_err(|e| e.to_string())?
            {
                if !exit.success() {
                    self.errors.insert(
                        provider.into(),
                        "登录未完成。可重新登录，或查看本机私有日志。".into(),
                    );
                }
                self.children.remove(provider);
            }
        }
        Ok(PROVIDERS
            .into_iter()
            .map(|p| {
                let folder = self.root.join("collectors").join(p);
                let auth = Self::read(&folder.join("auth.json"));
                let state = Self::read(&folder.join("status.json"));
                let authenticated = auth
                    .get("authenticated_at")
                    .and_then(Value::as_f64)
                    .is_some()
                    && (p == "linkedin"
                        || auth
                            .get("username")
                            .and_then(Value::as_str)
                            .is_some_and(|u| {
                                valid_username(u)
                                    && folder
                                        .join("sessions")
                                        .join(format!("session-{u}"))
                                        .is_file()
                            }))
                    && !["needs_login", "failed"]
                        .contains(&state.get("state").and_then(Value::as_str).unwrap_or(""));
                (
                    p.into(),
                    Status {
                        authenticated,
                        login_running: self.children.contains_key(p),
                        enabled: self.enabled.get(p).copied().unwrap_or(false),
                        state: state
                            .get("state")
                            .and_then(Value::as_str)
                            .unwrap_or("needs_login")
                            .into(),
                        error: self.errors.get(p).cloned().or_else(|| {
                            state
                                .get("error")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        }),
                    },
                )
            })
            .collect())
    }
    pub fn refresh(&mut self, transport: &Transport) -> Result<()> {
        let response = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| e.to_string())?
            .get(format!("{}/internal/adapter-state", transport.url))
            .bearer_auth(&transport.token)
            .send()
            .map_err(|_| "无法读取后台采集状态")?;
        if !response.status().is_success() {
            return Err("后台采集状态暂不可用".into());
        }
        let values: Vec<Value> = response.json().map_err(|_| "后台采集状态格式无效")?;
        for value in values {
            if let Some(provider) = value
                .get("provider")
                .and_then(Value::as_str)
                .filter(|p| PROVIDERS.contains(p))
            {
                let enabled = value
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or("状态缺少 enabled")?;
                let paused = value
                    .get("paused")
                    .and_then(Value::as_bool)
                    .ok_or("状态缺少 paused")?;
                self.enabled.insert(provider.into(), enabled && !paused);
            }
        }
        Ok(())
    }
    fn set_enabled(&mut self, transport: &Transport, provider: &str, enabled: bool) -> Result<()> {
        let response = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?
            .post(format!("{}/internal/adapter-state", transport.url))
            .bearer_auth(&transport.token)
            .json(&json!({"provider":provider,"enabled":enabled,"paused":!enabled}))
            .send()
            .map_err(|_| "无法连接采集控制接口")?;
        if !response.status().is_success() {
            return Err(format!(
                "采集控制未完成（HTTP {}）",
                response.status().as_u16()
            ));
        }
        self.enabled.insert(provider.into(), enabled);
        let path = self.root.join("enabled.json");
        let temporary = path.with_extension("tmp");
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(&self.enabled).map_err(|e| e.to_string())?)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(temporary, path).map_err(|e| e.to_string())
    }
    pub fn action(
        &mut self,
        transport: Option<&Transport>,
        provider: &str,
        action: &str,
        username: Option<&str>,
    ) -> Result<()> {
        Self::validate(provider)?;
        if action == "cancel" {
            return self.cancel(provider);
        }
        if action == "status" {
            self.statuses()?;
            return Ok(());
        }
        let transport = transport.ok_or("请先启动后台服务")?;
        match action {
            "login" => {
                if cfg!(debug_assertions) && std::env::var_os("MONICA_RUST_TEST_DATA_DIR").is_some()
                {
                    return Err("测试模式不打开真实账号登录；登录生命周期使用模拟进程验证".into());
                }
                self.login(transport, provider, username)
            }
            "enable" => {
                let status = self.statuses()?.remove(provider).ok_or("来源状态不可用")?;
                if !status.authenticated || status.login_running {
                    return Err("请先完成此应用内的来源登录，再启用采集".into());
                }
                self.set_enabled(transport, provider, true)
            }
            "disable" => {
                self.cancel(provider)?;
                self.set_enabled(transport, provider, false)
            }
            _ => Err("未知采集操作".into()),
        }
    }
    fn login(
        &mut self,
        transport: &Transport,
        provider: &str,
        username: Option<&str>,
    ) -> Result<()> {
        if self.children.contains_key(provider) {
            return Err("登录窗口已打开".into());
        }
        if provider == "instagram" && !username.is_some_and(valid_username) {
            return Err("请填写 Instagram 用户名（字母、数字、点或下划线）".into());
        }
        self.set_enabled(transport, provider, false)?;
        let folder = self.root.join("collectors").join(provider);
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(folder.join("login.log"))
            .map_err(|e| e.to_string())?;
        let mut command = Command::new(&self.executable);
        command
            .args(["--provider", provider, "--data-root"])
            .arg(&self.root);
        if provider == "instagram" {
            command.arg("--username").arg(username.unwrap_or_default());
        }
        command
            .arg("login")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("MONICA_COLLECTOR_OWNED_GROUP", "1")
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .process_group(0);
        let child = command
            .spawn()
            .map_err(|e| format!("无法打开内置登录程序：{e}"))?;
        self.children.insert(provider.into(), child);
        self.errors.remove(provider);
        Ok(())
    }
    pub fn cancel(&mut self, provider: &str) -> Result<()> {
        if let Some(mut child) = self.children.remove(provider) {
            // Keep the leader unreaped until the final group signal, avoiding PID reuse.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(500));
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            child.wait().map_err(|e| e.to_string())?;
            self.errors.remove(provider);
        }
        Ok(())
    }
    pub fn stop(&mut self) -> Result<()> {
        for p in PROVIDERS {
            self.cancel(p)?;
        }
        Ok(())
    }
}
impl Drop for Collectors {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn valid_username(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 30
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    fn transport() -> Transport {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut connection in listener.incoming().take(3).flatten() {
                let mut bytes = [0; 4096];
                let _ = connection.read(&mut bytes);
                connection
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .unwrap();
            }
        });
        Transport {
            url,
            token: "synthetic-token".into(),
        }
    }
    #[test]
    fn login_is_owned_cancelled_and_never_enables_collection() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("fake-login");
        fs::write(
            &script,
            "#!/bin/sh\ntrap '' TERM\nsleep 60 &\necho $! > grandchild.pid\nwait\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let root = temp.path().join("private data");
        let mut controller = Collectors::new(root.clone(), script).unwrap();
        let transport = transport();
        assert!(!controller.statuses().unwrap()["linkedin"].enabled);
        assert!(
            controller
                .action(Some(&transport), "linkedin", "enable", None)
                .is_err()
        );
        controller.login(&transport, "linkedin", None).unwrap();
        let pid = controller.children["linkedin"].id();
        for _ in 0..50 {
            if root.join("grandchild.pid").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let grandchild: i32 = fs::read_to_string(root.join("grandchild.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(controller.statuses().unwrap()["linkedin"].login_running);
        assert!(!controller.statuses().unwrap()["linkedin"].enabled);
        controller.cancel("linkedin").unwrap();
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
        // Grandchild may briefly be a zombie until launchd reaps it; it must not run.
        let output = Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &grandchild.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        assert!(state.trim().is_empty() || state.trim().starts_with('Z'));
        let folder = root.join("collectors/linkedin");
        fs::write(folder.join("auth.json"), r#"{"authenticated_at":1234}"#).unwrap();
        fs::write(folder.join("status.json"), r#"{"state":"ready"}"#).unwrap();
        controller
            .action(Some(&transport), "linkedin", "enable", None)
            .unwrap();
        assert!(controller.statuses().unwrap()["linkedin"].enabled);
        controller
            .action(Some(&transport), "linkedin", "disable", None)
            .unwrap();
        assert!(!controller.statuses().unwrap()["linkedin"].enabled);
    }
}

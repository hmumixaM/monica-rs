//! Owns one independent, loopback-only PostgreSQL cluster and Rust server.
//! No existing Monica data/configuration is read and no external adapter is started.
use fs2::FileExt;
use rand::RngCore;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub logs: PathBuf,
    pub postgres_bin: PathBuf,
    pub server: PathBuf,
    pub web: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Secrets {
    schema: u32,
    postgres: String,
    session: String,
    control: String,
}
#[derive(Serialize, Deserialize)]
struct Preferences {
    web_port: u16,
    paused: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            web_port: 17494,
            paused: false,
        }
    }
}
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct RuntimeStatus {
    pub phase: String,
    pub url: Option<String>,
    pub paused: bool,
    pub draining: bool,
    pub active_jobs: u64,
    pub error: Option<String>,
    pub data_directory: String,
    pub chatlog: crate::chatlog::Status,
    pub adapters_enabled: bool,
    pub ai_enabled: bool,
    pub postgres_pid: Option<u32>,
    pub server_pid: Option<u32>,
}

pub struct Supervisor {
    chatlog: crate::chatlog::Chatlog,
    paths: Paths,
    secrets: Secrets,
    _lock: File,
    postgres: Option<Child>,
    server: Option<Child>,
    pg_port: u16,
    web_port: u16,
    client: Client,
    status: RuntimeStatus,
    desired_paused: bool,
    recovery_attempts: u8,
}
fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    Ok(())
}
fn private_file(path: &Path, append: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).map_err(|e| e.to_string())
}
fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn load_secrets(root: &Path) -> Result<Secrets> {
    let path = root.join("runtime-secrets.json");
    if path.exists() {
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > 4096 {
            return Err("本地凭据文件无效".into());
        }
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("本地凭据文件必须仅当前用户可读写（0600）".into());
        }
        let secrets: Secrets = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|_| "本地凭据格式无效")?;
        if secrets.schema != 1
            || [&secrets.postgres, &secrets.session, &secrets.control]
                .iter()
                .any(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("本地凭据版本或格式无效".into());
        }
        return Ok(secrets);
    }
    let secrets = Secrets {
        schema: 1,
        postgres: random_secret(),
        session: random_secret(),
        control: random_secret(),
    };
    let temporary = root.join("runtime-secrets.new");
    let mut file = private_file(&temporary, false)?;
    file.write_all(&serde_json::to_vec(&secrets).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    fs::rename(temporary, path).map_err(|e| e.to_string())?;
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(secrets)
}
fn available_port() -> Result<u16> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|socket| socket.local_addr())
        .map(|address| address.port())
        .map_err(|e| e.to_string())
}
fn bounded_command(mut command: Command, timeout: Duration) -> Result<()> {
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("初始化步骤未完成（{status}），请查看本地日志"))
            };
        }
        if start.elapsed() >= timeout {
            signal(&child, libc::SIGTERM)?;
            // Only an initialization child, never a database server, is forcibly reaped.
            thread::sleep(Duration::from_millis(500));
            if child.try_wait().map_err(|e| e.to_string())?.is_none() {
                child.kill().map_err(|e| e.to_string())?;
            }
            child.wait().map_err(|e| e.to_string())?;
            return Err("初始化步骤超时，请查看本地日志".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}
fn signal(child: &Child, signal: i32) -> Result<()> {
    // Only PIDs obtained from Child handles owned by this supervisor are signalled.
    let result = unsafe { libc::kill(child.id() as i32, signal) };
    if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}
fn stop_child(slot: &mut Option<Child>, sig: i32, timeout: Duration) -> Result<()> {
    let Some(child) = slot.as_mut() else {
        return Ok(());
    };
    if child.try_wait().map_err(|e| e.to_string())?.is_none() {
        signal(child, sig)?;
    }
    let start = Instant::now();
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            *slot = None;
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err("服务仍在安全退出，请稍候再试；没有强制终止数据库".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}
impl Supervisor {
    pub fn new(paths: Paths) -> Result<Self> {
        private_directory(&paths.data)?;
        private_directory(&paths.logs)?;
        let lock_path = paths.data.join("runtime.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|e| e.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|_| "这个 Monica Rust 数据目录已由另一个实例运行")?;
        let secrets = load_secrets(&paths.data)?;
        let preferences = match fs::read(paths.data.join("runtime-preferences.json")) {
            Ok(bytes) => {
                serde_json::from_slice::<Preferences>(&bytes).map_err(|_| "本地运行设置无效")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
            Err(error) => return Err(error.to_string()),
        };
        if preferences.web_port < 1024 {
            return Err("本地端口设置无效".into());
        }
        let status = RuntimeStatus {
            phase: "stopped".into(),
            data_directory: paths.data.display().to_string(),
            ..Default::default()
        };
        let client = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        let chatlog = crate::chatlog::Chatlog::new(
            paths.data.join("chatlog"),
            paths
                .server
                .parent()
                .ok_or("运行目录无效")?
                .join("tools/bin/chatlog-alpha"),
            paths.logs.join("chatlog.log"),
        )?;
        Ok(Self {
            chatlog,
            paths,
            secrets,
            _lock: lock,
            postgres: None,
            server: None,
            pg_port: 0,
            web_port: preferences.web_port,
            client,
            status,
            desired_paused: preferences.paused,
            recovery_attempts: 0,
        })
    }
    pub fn status(&self) -> RuntimeStatus {
        let mut status = self.status.clone();
        status.chatlog = self.chatlog.status();
        status
    }
    pub fn logs(&self) -> &Path {
        &self.paths.logs
    }
    fn logged(&self, binary: &Path, log: &str) -> Result<Command> {
        if !binary.is_file() {
            return Err(format!(
                "应用缺少运行组件：{}",
                binary.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        let output = private_file(&self.paths.logs.join(log), true)?;
        let error = output.try_clone().map_err(|e| e.to_string())?;
        let mut command = Command::new(binary);
        command.stdin(Stdio::null()).stdout(output).stderr(error);
        Ok(command)
    }
    fn db_url(&self) -> String {
        format!(
            "postgresql://monica:{}@127.0.0.1:{}/monica_rust",
            self.secrets.postgres, self.pg_port
        )
    }
    fn server_command(&self, log: &str) -> Result<Command> {
        let mut command = self.logged(&self.paths.server, log)?;
        let tools = self
            .paths
            .server
            .parent()
            .ok_or("运行目录无效")?
            .join("tools/bin");
        let mut paths = vec![tools];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command.env(
            "PATH",
            std::env::join_paths(paths).map_err(|e| e.to_string())?,
        );
        command
            .env("MONICA_DATABASE_URL", self.db_url())
            .env("MONICA_SESSION_SECRET", &self.secrets.session)
            .env("MONICA_CONTROL_TOKEN", &self.secrets.control)
            .env(
                "MONICA_ORIGIN",
                format!("http://127.0.0.1:{}", self.web_port),
            )
            .env("MONICA_MEDIA_ROOT", self.paths.data.join("media"))
            .env("MONICA_NATIVE_ROOT", self.paths.data.join("raw"))
            .env(
                "MONICA_COLLECTOR_DATA_ROOT",
                self.paths.data.join("collector-data"),
            )
            .env("MONICA_CHATLOG_ALPHA_URL", self.chatlog.url())
            .env("MONICA_WEB_ROOT", &self.paths.web)
            .env_remove("MONICA_AI_ENABLED")
            .env("MONICA_ADAPTERS_ENABLED", "false")
            .env(
                "MONICA_START_PAUSED",
                if self.desired_paused { "true" } else { "false" },
            )
            .env("AI_ENABLED", "false")
            .env("RUST_LOG", "info");
        // Do not inherit another installation's account or snapshot directory.
        command
            .env_remove("MONICA_CHATLOG_SNAPSHOT_ROOT")
            .env_remove("MONICA_CHATLOG_ACCOUNT");
        if let Some((snapshots, account)) = self.chatlog.snapshot_source()? {
            command
                .env("MONICA_CHATLOG_SNAPSHOT_ROOT", snapshots)
                .env("MONICA_CHATLOG_ACCOUNT", account);
        }
        let runtime = self.paths.server.parent().ok_or("运行目录无效")?;
        for (name, relative) in [
            (
                "MONICA_COLLECTOR_PYTHON",
                "collectors/python/bin/python3.12",
            ),
            ("MONICA_COLLECTOR_BRIDGE", "collectors/run-bridge"),
            ("MONICA_COLLECTOR_ENTRY", "collectors/run-collector"),
            ("MONICA_WHATSAPP_BINARY", "tools/bin/monica-whatsapp"),
        ] {
            let asset = runtime.join(relative);
            if asset.is_file() {
                command.env(name, asset);
            } else {
                command.env_remove(name);
            }
        }
        if runtime.join("collectors/run-bridge").is_file() {
            command.env("MONICA_COLLECTOR_RUNTIME", runtime.join("collectors"));
        } else {
            command.env_remove("MONICA_COLLECTOR_RUNTIME");
        }
        Ok(command)
    }
    pub fn start(&mut self) -> Result<RuntimeStatus> {
        if self.server.is_some() || self.postgres.is_some() {
            return Err("服务已运行或仍在退出，请先完成停止".into());
        }
        self.status.phase = "starting".into();
        self.status.error = None;
        match self.start_inner() {
            Ok(()) => {
                self.status.phase = "running".into();
                Ok(self.status())
            }
            Err(error) => {
                let stopped = self.stop();
                self.status.phase = "failed".into();
                self.status.error = Some(if let Err(stop_error) = stopped {
                    format!("{error}；{stop_error}")
                } else {
                    error.clone()
                });
                Err(error)
            }
        }
    }
    fn start_inner(&mut self) -> Result<()> {
        if !self.paths.web.join("index.html").is_file() {
            return Err("应用缺少 Web 页面，请重新构建完整应用".into());
        }
        private_directory(&self.paths.data.join("media"))?;
        let cluster = self.paths.data.join("postgres");
        if !cluster.join("PG_VERSION").exists() {
            // Never replace an unknown nonempty directory with a fresh cluster.
            if cluster.exists()
                && fs::read_dir(&cluster)
                    .map_err(|e| e.to_string())?
                    .next()
                    .is_some()
            {
                return Err("数据库目录未完成初始化，请保留日志并检查；不会覆盖已有文件".into());
            }
            let password_path = self.paths.data.join("initdb-password.tmp");
            let mut password = private_file(&password_path, false)?;
            writeln!(password, "{}", self.secrets.postgres).map_err(|e| e.to_string())?;
            password.sync_all().map_err(|e| e.to_string())?;
            let mut init = self.logged(&self.paths.postgres_bin.join("initdb"), "postgres.log")?;
            init.arg("-D")
                .arg(&cluster)
                .args([
                    "-U",
                    "monica",
                    "--encoding=UTF8",
                    "--locale=C",
                    "--auth-local=scram-sha-256",
                    "--auth-host=scram-sha-256",
                ])
                .arg("--pwfile")
                .arg(&password_path);
            let result = bounded_command(init, Duration::from_secs(90));
            let _ = fs::remove_file(password_path);
            result?;
        }
        let version = fs::read_to_string(cluster.join("PG_VERSION")).map_err(|e| e.to_string())?;
        if version.trim() != "16" {
            return Err("此应用需要 PostgreSQL 16 数据目录；不会自动升级已有数据库".into());
        }
        self.pg_port = available_port()?;
        if TcpListener::bind(("127.0.0.1", self.web_port)).is_err() {
            self.web_port = available_port()?;
        }
        let mut postgres =
            self.logged(&self.paths.postgres_bin.join("postgres"), "postgres.log")?;
        postgres.arg("-D").arg(&cluster).args([
            "-h",
            "127.0.0.1",
            "-p",
            &self.pg_port.to_string(),
            "-c",
            "unix_socket_directories=",
            "-c",
            "max_connections=48",
            "-c",
            "shared_buffers=128MB",
            "-c",
            "wal_level=logical",
        ]);
        self.postgres = Some(postgres.spawn().map_err(|e| e.to_string())?);
        self.status.postgres_pid = self.postgres.as_ref().map(Child::id);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self
                .postgres
                .as_mut()
                .unwrap()
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("本地数据库启动失败，请查看 postgres.log".into());
            }
            let ready = Command::new(self.paths.postgres_bin.join("pg_isready"))
                .args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &self.pg_port.to_string(),
                    "-U",
                    "monica",
                    "-t",
                    "1",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| e.to_string())?;
            if ready.success() {
                break;
            }
            if Instant::now() >= deadline {
                return Err("本地数据库启动超时".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        let marker = self.paths.data.join("database-created");
        if !marker.exists() {
            let mut create =
                self.logged(&self.paths.postgres_bin.join("createdb"), "postgres.log")?;
            create
                .args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &self.pg_port.to_string(),
                    "-U",
                    "monica",
                    "monica_rust",
                ])
                .env("PGPASSWORD", &self.secrets.postgres);
            // Verify existence separately: a crash after CREATE DATABASE must remain recoverable.
            let exists = Command::new(self.paths.postgres_bin.join("psql"))
                .args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &self.pg_port.to_string(),
                    "-U",
                    "monica",
                    "-d",
                    "postgres",
                    "-Atqc",
                    "SELECT 1 FROM pg_database WHERE datname='monica_rust'",
                ])
                .env("PGPASSWORD", &self.secrets.postgres)
                .output()
                .map_err(|e| e.to_string())?;
            if !exists.status.success() {
                return Err("无法验证本地数据库，请查看 postgres.log".into());
            }
            if exists.stdout != b"1\n" {
                bounded_command(create, Duration::from_secs(30))?;
            }
            private_file(&marker, false)?
                .sync_all()
                .map_err(|e| e.to_string())?;
        }
        let mut migrate = self.server_command("server.log")?;
        migrate.arg("migrate");
        bounded_command(migrate, Duration::from_secs(120))?;
        self.start_server()
    }
    fn start_server(&mut self) -> Result<()> {
        let mut server = self.server_command("server.log")?;
        server.args(["serve", "--listen", &format!("127.0.0.1:{}", self.web_port)]);
        self.server = Some(server.spawn().map_err(|e| e.to_string())?);
        self.status.server_pid = self.server.as_ref().map(Child::id);
        let url = format!("http://127.0.0.1:{}", self.web_port);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self
                .server
                .as_mut()
                .unwrap()
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("Rust 服务启动失败，请查看 server.log".into());
            }
            if self
                .client
                .get(format!("{url}/api/v1/ready"))
                .send()
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err("Rust 服务就绪超时".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        self.status.url = Some(url);
        self.save_preferences()?;
        if self.desired_paused {
            self.control("pause")?;
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    pub fn native_transport(&self) -> Result<crate::native_control::Transport> {
        if self.status.phase != "running" || self.status.paused || self.status.draining {
            return Err("请先启动或恢复后台任务".into());
        }
        Ok(crate::native_control::Transport {
            url: self.status.url.clone().ok_or("服务尚未就绪")?,
            token: self.secrets.control.clone(),
        })
    }
    pub fn collector_transport(&self) -> Result<crate::native_control::Transport> {
        Ok(crate::native_control::Transport {
            url: self.status.url.clone().ok_or("请先启动后台服务")?,
            token: self.secrets.control.clone(),
        })
    }
    fn control(&mut self, action: &str) -> Result<Value> {
        let url = self.status.url.as_ref().ok_or("本地服务尚未就绪")?;
        let request = self
            .client
            .post(format!("{url}/internal/runtime/{action}"))
            .bearer_auth(&self.secrets.control);
        let response = request.send().map_err(|_| "无法连接后台控制接口")?;
        if !response.status().is_success() {
            return Err(format!(
                "后台控制未完成（HTTP {}）；服务状态没有被假定为成功",
                response.status().as_u16()
            ));
        }
        let value: Value = response.json().map_err(|_| "后台控制返回格式无效")?;
        self.apply_control_status(&value)?;
        Ok(value)
    }
    fn apply_control_status(&mut self, value: &Value) -> Result<()> {
        self.status.paused = value
            .get("paused")
            .and_then(Value::as_bool)
            .ok_or("后台状态缺少 paused")?;
        self.status.draining = value
            .get("draining")
            .and_then(Value::as_bool)
            .ok_or("后台状态缺少 draining")?;
        self.status.active_jobs = value
            .get("active_jobs")
            .and_then(Value::as_u64)
            .ok_or("后台状态缺少 active_jobs")?;
        Ok(())
    }
    fn save_preferences(&self) -> Result<()> {
        let path = self.paths.data.join("runtime-preferences.new");
        let mut file = private_file(&path, false)?;
        file.write_all(
            &serde_json::to_vec(&Preferences {
                web_port: self.web_port,
                paused: self.desired_paused,
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(path, self.paths.data.join("runtime-preferences.json"))
            .map_err(|e| e.to_string())?;
        File::open(&self.paths.data)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn pause(&mut self) -> Result<RuntimeStatus> {
        self.control("pause")?;
        self.desired_paused = true;
        self.chatlog.stop()?;
        self.save_preferences()?;
        Ok(self.status())
    }
    pub fn resume(&mut self) -> Result<RuntimeStatus> {
        self.control("resume")?;
        self.desired_paused = false;
        self.save_preferences()?;
        self.tick();
        Ok(self.status())
    }
    pub fn chatlog_action(
        &mut self,
        action: &str,
        source: Option<&Path>,
        key: Option<&str>,
    ) -> Result<RuntimeStatus> {
        match action {
            "configure" => {
                let source = source.ok_or("缺少微信目录")?;
                let key = key.ok_or("缺少数据库密钥")?;
                if !source.is_absolute()
                    || key.len() != 64
                    || !key.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("请输入绝对微信数据目录与64位十六进制数据库密钥".into());
                }
                self.adapter_enabled("wechat", false)?;
                // Environment is immutable after spawn. Drain only our Rust child,
                // retain PostgreSQL and the browser origin, then install fresh env.
                self.desired_paused = self.status.paused;
                stop_child(&mut self.server, libc::SIGTERM, Duration::from_secs(120))?;
                self.status.server_pid = None;
                let configured = self.chatlog.configure(source, key);
                if let Err(error) = self.start_server() {
                    let cleanup = self.stop();
                    self.status.phase = "failed".into();
                    self.status.error = Some(if let Err(stop_error) = cleanup {
                        format!("{error}；{stop_error}")
                    } else {
                        error.clone()
                    });
                    return Err(error);
                }
                configured?;
            }
            "enable" => {
                if !self.chatlog.status().configured {
                    return Err("请先配置微信目录和数据库密钥".into());
                }
                // Store user intent in both control planes before starting. If startup
                // fails, the visible error and owned-child retry preserve that intent.
                self.adapter_enabled("wechat", true)?;
                self.chatlog.enable(true, self.status.paused)?;
            }
            "disable" => {
                self.chatlog.enable(false, true)?;
                self.adapter_enabled("wechat", false)?;
            }
            _ => return Err("未知微信来源操作".into()),
        };
        Ok(self.status())
    }
    fn adapter_enabled(&self, provider: &str, enabled: bool) -> Result<()> {
        let response = self
            .client
            .post(format!(
                "{}/internal/adapter-state",
                self.status.url.as_ref().ok_or("请先启动后台服务")?
            ))
            .bearer_auth(&self.secrets.control)
            .json(&json!({"provider":provider,"enabled":enabled,"paused":!enabled}))
            .send()
            .map_err(|_| "无法连接采集控制接口")?;
        if !response.status().is_success() {
            return Err(format!(
                "采集控制未完成（HTTP {}）",
                response.status().as_u16()
            ));
        }
        Ok(())
    }
    pub fn account(&self) -> Result<Value> {
        let url = self.status.url.as_ref().ok_or("本地服务尚未就绪")?;
        self.client
            .get(format!("{url}/internal/account"))
            .bearer_auth(&self.secrets.control)
            .send()
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .map_err(|e| e.to_string())
    }
    pub fn reset_account(&self, email: &str, password: &str, reset_totp: bool) -> Result<()> {
        let url = self.status.url.as_ref().ok_or("本地服务尚未就绪")?;
        let response = self
            .client
            .post(format!("{url}/internal/account/reset-password"))
            .bearer_auth(&self.secrets.control)
            .json(&json!({"email":email,"password":password,"reset_totp":reset_totp}))
            .send()
            .map_err(|e| e.to_string())?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response
                .json::<Value>()
                .ok()
                .and_then(|v| v["detail"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "密码重置未完成".into()))
        }
    }
    pub fn bootstrap(&self, email: &str, password: &str) -> Result<()> {
        let url = self.status.url.as_ref().ok_or("本地服务尚未就绪")?;
        let response = self
            .client
            .post(format!("{url}/api/v1/auth/bootstrap"))
            .json(&json!({"email":email,"password":password}))
            .send()
            .map_err(|_| "无法连接账号服务")?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status().as_u16();
        let error = response
            .json::<Value>()
            .ok()
            .and_then(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| format!("账号创建未完成（HTTP {status}）"));
        Err(error)
    }
    pub fn stop(&mut self) -> Result<RuntimeStatus> {
        self.status.phase = "stopping".into();
        stop_child(&mut self.server, libc::SIGTERM, Duration::from_secs(120))?;
        self.status.server_pid = None;
        self.chatlog.stop()?;
        stop_child(&mut self.postgres, libc::SIGINT, Duration::from_secs(60))?;
        self.status.postgres_pid = None;
        self.status.url = None;
        self.status.phase = "stopped".into();
        self.status.draining = false;
        self.status.active_jobs = 0;
        Ok(self.status())
    }
    /// Called after wake and periodically. Unexpected owned-child exit triggers bounded recovery.
    pub fn tick(&mut self) {
        if self.status.phase != "running" {
            return;
        }
        let exited = [&mut self.server, &mut self.postgres]
            .iter_mut()
            .any(|slot| {
                slot.as_mut()
                    .is_none_or(|child| child.try_wait().is_ok_and(|s| s.is_some()))
            });
        if exited {
            self.status.error = Some("本地服务意外停止，正在恢复".into());
            if let Err(error) = self.stop() {
                self.status.error = Some(error);
                self.status.phase = "failed".into();
                return;
            }
            self.recovery_attempts += 1;
            if self.recovery_attempts > 3 {
                self.status.phase = "failed".into();
                self.status.error = Some("多次恢复失败，请查看日志后手动重试".into());
                return;
            }
            thread::sleep(Duration::from_millis(
                500 * u64::from(self.recovery_attempts),
            ));
            let _ = self.start();
            return;
        }
        if let Some(url) = self.status.url.as_ref() {
            match self
                .client
                .get(format!("{url}/internal/runtime/status"))
                .bearer_auth(&self.secrets.control)
                .send()
            {
                Ok(response) if response.status().is_success() => match response
                    .json::<Value>()
                    .ok()
                    .and_then(|v| self.apply_control_status(&v).ok())
                {
                    Some(()) => self.status.error = None,
                    None => self.status.error = Some("后台状态格式无效".into()),
                },
                Ok(response) => {
                    self.status.error = Some(format!(
                        "后台状态暂不可用（HTTP {}）",
                        response.status().as_u16()
                    ))
                }
                Err(_) => {
                    self.status.error = Some("后台服务暂未响应；数据与进程保留，等待恢复".into())
                }
            }
        }
        if self.chatlog.status().enabled && self.status.error.is_none() {
            let allowed = self
                .status
                .url
                .as_ref()
                .and_then(|url| {
                    self.client
                        .get(format!("{url}/internal/adapter-state"))
                        .bearer_auth(&self.secrets.control)
                        .send()
                        .ok()
                })
                .filter(|r| r.status().is_success())
                .and_then(|r| r.json::<Vec<Value>>().ok())
                .map(|values| {
                    values.iter().any(|v| {
                        v["provider"] == "wechat" && v["enabled"] == true && v["paused"] == false
                    })
                });
            match allowed {
                Some(true) => self
                    .chatlog
                    .tick(self.status.paused || self.status.draining),
                Some(false) => {
                    let _ = self.chatlog.enable(false, true);
                }
                None => {
                    let _ = self.chatlog.stop();
                    self.status.error = Some("无法确认微信采集状态，已暂停本机读取".into());
                }
            }
        } else {
            let _ = self.chatlog.stop();
        }
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paths(root: &Path) -> Paths {
        Paths {
            data: root.join("data"),
            logs: root.join("logs"),
            postgres_bin: root.join("pg/bin"),
            server: root.join("monica-server"),
            web: root.join("web"),
        }
    }
    #[test]
    fn collector_environment_requires_an_explicit_chatlog_source() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Supervisor::new(paths(temp.path())).unwrap();
        fs::write(&supervisor.paths.server, b"synthetic executable marker").unwrap();
        let command = supervisor.server_command("fixture.log").unwrap();
        for name in ["MONICA_CHATLOG_SNAPSHOT_ROOT", "MONICA_CHATLOG_ACCOUNT"] {
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == name && value.is_none())
            );
        }
        let mut supervisor = supervisor;
        let source = temp.path().join("explicit source");
        supervisor
            .chatlog
            .configure(&source, &"1".repeat(64))
            .unwrap();
        let command = supervisor.server_command("fixture.log").unwrap();
        let (root, account) = supervisor.chatlog.snapshot_source().unwrap().unwrap();
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "MONICA_CHATLOG_SNAPSHOT_ROOT"
                    && value == Some(root.as_os_str()))
        );
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "MONICA_CHATLOG_ACCOUNT"
                    && value == Some(std::ffi::OsStr::new(&account)))
        );
    }
    #[test]
    fn credentials_private_stable_and_not_in_status() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Supervisor::new(paths(temp.path())).unwrap();
        let secret = supervisor.secrets.session.clone();
        let status = serde_json::to_string(&supervisor.status()).unwrap();
        assert!(!status.contains(&secret));
        drop(supervisor);
        let other = Supervisor::new(paths(temp.path())).unwrap();
        assert_eq!(secret, other.secrets.session);
        assert_eq!(
            fs::metadata(temp.path().join("data/runtime-secrets.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn second_owner_rejected_and_released_on_exit() {
        let temp = tempfile::tempdir().unwrap();
        let first = Supervisor::new(paths(temp.path())).unwrap();
        assert!(Supervisor::new(paths(temp.path())).is_err());
        drop(first);
        assert!(Supervisor::new(paths(temp.path())).is_ok());
    }
    #[test]
    fn absent_components_fail_without_fake_running_status() {
        let temp = tempfile::tempdir().unwrap();
        let mut supervisor = Supervisor::new(paths(temp.path())).unwrap();
        assert!(supervisor.start().is_err());
        assert_eq!(supervisor.status().phase, "failed");
        assert!(supervisor.status().server_pid.is_none());
        assert!(supervisor.pause().is_err());
        assert!(!supervisor.status().paused);
    }
    #[test]
    fn malformed_control_status_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let mut supervisor = Supervisor::new(paths(temp.path())).unwrap();
        assert!(
            supervisor
                .apply_control_status(&json!({"ok":true}))
                .is_err()
        );
        assert!(
            supervisor
                .apply_control_status(&json!({"paused":true,"draining":true,"active_jobs":2}))
                .is_ok()
        );
        assert!(supervisor.status().paused);
        assert_eq!(supervisor.status().active_jobs, 2);
    }
    #[test]
    fn occupied_port_does_not_prevent_selecting_another() {
        let socket = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        assert_ne!(
            socket.local_addr().unwrap().port(),
            available_port().unwrap()
        );
    }
}

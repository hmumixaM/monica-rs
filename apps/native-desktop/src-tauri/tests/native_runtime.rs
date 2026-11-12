use monica_rust_desktop::supervisor::{Paths, Supervisor};
use reqwest::blocking::Client;
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[test]
#[ignore = "requires explicit freshly built server and PG16; creates only a new temporary cluster"]
fn native_postgres_server_lifecycle() {
    let server = PathBuf::from(
        std::env::var_os("MONICA_NATIVE_SERVER")
            .expect("Set MONICA_NATIVE_SERVER to an explicit built Rust binary"),
    );
    let pg = PathBuf::from(
        std::env::var_os("MONICA_NATIVE_PG_BIN")
            .expect("Set MONICA_NATIVE_PG_BIN to a PG16 bin directory"),
    );
    let web = PathBuf::from(
        std::env::var_os("MONICA_NATIVE_WEB")
            .expect("Set MONICA_NATIVE_WEB to built static frontend"),
    );
    let temporary = tempfile::Builder::new()
        .prefix("monica-rust-native-")
        .tempdir()
        .unwrap();
    let root = temporary.path().join("a deliberately long independent data path with spaces to exercise TCP rather than PostgreSQL socket path limitations");
    struct FailureLogs(PathBuf);
    impl Drop for FailureLogs {
        fn drop(&mut self) {
            if std::thread::panicking() {
                for name in ["server.log", "postgres.log"] {
                    if let Ok(log) = std::fs::read_to_string(self.0.join(name)) {
                        eprintln!(
                            "native failure {name}: {}",
                            log.chars()
                                .rev()
                                .take(12000)
                                .collect::<String>()
                                .chars()
                                .rev()
                                .collect::<String>()
                        );
                    }
                }
            }
        }
    }
    let _failure_logs = FailureLogs(temporary.path().join("logs"));
    let occupied_default = std::net::TcpListener::bind(("127.0.0.1", 17494)).ok();
    let mut supervisor = Supervisor::new(Paths {
        data: root.clone(),
        logs: temporary.path().join("logs"),
        postgres_bin: pg,
        server,
        web,
    })
    .unwrap();
    let status = supervisor.start().unwrap_or_else(|error| {
        let log =
            std::fs::read_to_string(temporary.path().join("logs/server.log")).unwrap_or_default();
        panic!("native startup failed: {error}; server log: {log}");
    });
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let url = status.url.unwrap();
    if occupied_default.is_some() {
        assert!(!url.ends_with(":17494"));
    }
    let page = client.get(format!("{url}/login")).send().unwrap();
    assert!(page.status().is_success());
    assert!(page.text().unwrap().contains("Monica Rust"));
    assert_eq!(
        client
            .get(format!("{url}/api/unknown-native-smoke"))
            .send()
            .unwrap()
            .status(),
        404
    );
    supervisor
        .bootstrap(
            "native-fixture@example.invalid",
            "synthetic-native-password-42",
        )
        .unwrap();
    assert!(
        supervisor
            .bootstrap(
                "native-fixture@example.invalid",
                "synthetic-native-password-42"
            )
            .is_err()
    );
    let login = client.post(format!("{url}/api/v1/auth/login")).json(&json!({"email":"native-fixture@example.invalid","password":"synthetic-native-password-42"})).send().unwrap();
    assert!(login.status().is_success());
    let cookies: Vec<_> = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|h| h.to_str().unwrap().split(';').next().unwrap().to_string())
        .collect();
    assert!(
        client
            .get(format!("{url}/api/v1/me"))
            .header("Cookie", cookies.join("; "))
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        client
            .post(format!("{url}/internal/runtime/pause"))
            .send()
            .unwrap()
            .status(),
        401
    );
    // Synthetic native spool -> token-only HTTP -> immutable journal -> evidence.
    // No Contacts/Messages reader or personal directory is involved.
    let transport = supervisor.native_transport().unwrap();
    let state_url = format!("{url}/internal/source-state");
    let native_state = json!({"provider":"apple_messages","enabled":true});
    assert_eq!(
        client
            .post(&state_url)
            .header("Origin", &url)
            .bearer_auth(&transport.token)
            .json(&native_state)
            .send()
            .unwrap()
            .status(),
        401
    );
    assert!(
        client
            .post(&state_url)
            .bearer_auth(&transport.token)
            .json(&native_state)
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    let spool = root.join("raw/apple_messages");
    std::fs::create_dir_all(&spool).unwrap();
    let pending = json!({"records":[{"external_id":"synthetic-native-message","kind":"message","title":"Synthetic native evidence","body":"Synthetic WAL body","thread_id":"synthetic-thread","occurred_at":"2026-01-02T12:00:00Z","direction":"received","participants":[{"id":"synthetic-peer","name":"Synthetic native peer","role":"sender","phones":["+15555550123"]}]}],"cursor":{"recent_max":42}});
    std::fs::write(
        spool.join("pending.json"),
        serde_json::to_vec(&pending).unwrap(),
    )
    .unwrap();
    assert_eq!(
        monica_rust_desktop::native_control::deliver_pending(&transport, "apple_messages", &spool)
            .unwrap(),
        1
    );
    assert!(!spool.join("pending.json").exists());
    let evidence: serde_json::Value = client
        .get(format!("{url}/api/v1/evidence?provider=apple_messages"))
        .header("Cookie", cookies.join("; "))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(evidence[0]["title"], "Synthetic native evidence");
    assert_eq!(evidence[0]["revision"], 1);
    let id = evidence[0]["id"].as_str().unwrap();
    let original: serde_json::Value = client
        .get(format!("{url}/api/v1/evidence/{id}?revision=1"))
        .header("Cookie", cookies.join("; "))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(original["body"], "Synthetic WAL body");
    // Retry is idempotent. Pause rejects another page and preserves local spool.
    std::fs::write(
        spool.join("pending.json"),
        serde_json::to_vec(&pending).unwrap(),
    )
    .unwrap();
    monica_rust_desktop::native_control::deliver_pending(&transport, "apple_messages", &spool)
        .unwrap();
    assert!(
        client
            .post(&state_url)
            .bearer_auth(&transport.token)
            .json(&json!({"provider":"apple_messages","enabled":false}))
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    std::fs::write(
        spool.join("pending.json"),
        serde_json::to_vec(&pending).unwrap(),
    )
    .unwrap();
    assert!(
        monica_rust_desktop::native_control::deliver_pending(&transport, "apple_messages", &spool)
            .is_err()
    );
    assert!(spool.join("pending.json").exists());
    // A suspended process can remain alive across sleep/wake. A missed health
    // response must not spawn a competing server or discard its database.
    let asleep_pid = supervisor.status().server_pid.unwrap();
    struct ResumeOnDrop(u32);
    impl Drop for ResumeOnDrop {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0 as i32, libc::SIGCONT);
            }
        }
    }
    unsafe {
        libc::kill(asleep_pid as i32, libc::SIGSTOP);
    }
    let wake = ResumeOnDrop(asleep_pid);
    supervisor.tick();
    assert_eq!(supervisor.status().server_pid, Some(asleep_pid));
    assert_eq!(supervisor.status().phase, "running");
    assert!(supervisor.status().error.is_some());
    drop(wake);
    supervisor.tick();
    assert_eq!(supervisor.status().server_pid, Some(asleep_pid));
    assert!(supervisor.status().error.is_none());
    let paused = supervisor.pause().unwrap();
    assert!(paused.paused);
    // Configure and enable only while globally paused: no real Chatlog process or
    // source reader starts, but the actual backend adapter control must agree.
    let synthetic_wechat = temporary.path().join("empty synthetic WeChat source");
    std::fs::create_dir(&synthetic_wechat).unwrap();
    let old_server = supervisor.status().server_pid;
    let old_postgres = supervisor.status().postgres_pid;
    supervisor
        .chatlog_action("configure", Some(&synthetic_wechat), Some(&"1".repeat(64)))
        .unwrap();
    assert_ne!(supervisor.status().server_pid, old_server);
    assert_eq!(supervisor.status().postgres_pid, old_postgres);
    assert_eq!(supervisor.status().url.as_deref(), Some(url.as_str()));
    assert!(
        client
            .get(format!("{url}/api/v1/me"))
            .header("Cookie", cookies.join("; "))
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    let enabled = supervisor.chatlog_action("enable", None, None).unwrap();
    assert!(enabled.chatlog.enabled);
    assert!(!enabled.chatlog.running);
    let adapters: serde_json::Value = client
        .get(format!("{url}/internal/adapter-state"))
        .bearer_auth(&transport.token)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert!(
        adapters
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["provider"] == "wechat" && v["enabled"] == true && v["paused"] == false)
    );
    supervisor.chatlog_action("disable", None, None).unwrap();
    assert!(!supervisor.status().chatlog.enabled);
    assert!(
        std::fs::read_dir(&synthetic_wechat)
            .unwrap()
            .next()
            .is_none()
    );
    let server_pid = supervisor.status().server_pid.unwrap();
    unsafe {
        libc::kill(server_pid as i32, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_secs(1));
    supervisor.tick();
    let recovered = supervisor.status();
    assert_eq!(recovered.phase, "running");
    assert_ne!(recovered.server_pid, Some(server_pid));
    assert!(recovered.paused);
    assert!(!supervisor.resume().unwrap().paused);
    // Stop the owned isolated database unexpectedly. The monitor must rebuild the service tree.
    let postgres_pid = supervisor.status().postgres_pid.unwrap();
    unsafe {
        libc::kill(postgres_pid as i32, libc::SIGINT);
    }
    std::thread::sleep(Duration::from_secs(1));
    supervisor.tick();
    assert_eq!(supervisor.status().phase, "running");
    assert_ne!(supervisor.status().postgres_pid, Some(postgres_pid));
    assert!(
        supervisor
            .bootstrap("second@example.invalid", "synthetic-native-password-42")
            .is_err()
    );
    let server_pid = supervisor.status().server_pid.unwrap();
    let postgres_pid = supervisor.status().postgres_pid.unwrap();
    assert_eq!(supervisor.stop().unwrap().phase, "stopped");
    assert_eq!(unsafe { libc::kill(server_pid as i32, 0) }, -1);
    assert_eq!(unsafe { libc::kill(postgres_pid as i32, 0) }, -1);
    // A normal restart preserves the empty/synthetic database and independent credentials.
    let restarted = supervisor.start().unwrap();
    assert_eq!(restarted.url.as_deref(), Some(url.as_str()));
    supervisor.stop().unwrap();
    assert!(root.join("postgres/PG_VERSION").exists());
}

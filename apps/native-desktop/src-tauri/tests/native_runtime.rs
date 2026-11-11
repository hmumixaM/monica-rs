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
    let paused = supervisor.pause().unwrap();
    assert!(paused.paused);
    let server_pid = paused.server_pid.unwrap();
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

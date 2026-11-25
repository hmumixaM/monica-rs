//! Run each WebAssembly guest in a disposable subprocess. The parent can end
//! the process even when execution is inside a JavaScript built-in or waiting
//! on the read-only bridge.
use super::{
    SCRIPT_RESULT_BYTES,
    sandbox::{self, LOOKUP_INPUT_BYTES, LOOKUP_REPLY_BYTES, SOURCE_BYTES},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use monica_core::resources::ResourcePermit;
use std::{
    io::{self, BufRead, Read, Write},
    process::Stdio,
    sync::mpsc,
    thread,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
};

const WORKER_FLAG: &str = "--monica-ask-js-worker";
const READY: &str = "monica-ask-js-ready";
const CHILD_FRAME_BYTES: usize = SCRIPT_RESULT_BYTES + 1;
pub(super) const SCRIPT_TIMEOUT: Duration = Duration::from_secs(120);
// Let the server report the regular deadline and reap the worker first.
const WORKER_WATCHDOG_TIMEOUT: Duration = SCRIPT_TIMEOUT.saturating_add(Duration::from_secs(1));
const WORKER_TIMEOUT_EXIT: i32 = 124;
const TIMEOUT_MESSAGE: &str = "JavaScript 查询超时。请缩小范围或分批查询。";

pub(crate) fn entry() -> Result<bool> {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_none_or(|arg| arg != WORKER_FLAG) || args.next().is_some() {
        return Ok(false);
    }
    serve(
        io::stdin().lock(),
        io::stdout().lock(),
        WORKER_WATCHDOG_TIMEOUT,
    )?;
    Ok(true)
}

fn serve(mut input: impl BufRead, mut output: impl Write, timeout: Duration) -> Result<()> {
    // A compute-only guest cannot notice a closed pipe if the server dies.
    let (_finished, pending) = mpsc::channel::<()>();
    thread::Builder::new()
        .name("ask-js-watchdog".into())
        .spawn(move || {
            if pending.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
                std::process::exit(WORKER_TIMEOUT_EXIT);
            }
        })?;
    writeln!(output, "{READY}")?;
    output.flush()?;
    let source: String = serde_json::from_str(&read_input(&mut input, SOURCE_BYTES * 6 + 2)?)?;
    let result = sandbox::execute(&source, &mut |query| {
        writeln!(output, "?{query}")?;
        output.flush()?;
        read_input(&mut input, LOOKUP_REPLY_BYTES)
    });
    match result {
        Ok(json) => writeln!(output, "+{json}")?,
        Err(error) => writeln!(output, "-{}", serde_json::to_string(&error.to_string())?)?,
    }
    output.flush()?;
    Ok(())
}

fn read_input(input: &mut impl BufRead, max: usize) -> Result<String> {
    let mut line = String::new();
    input.take((max + 1) as u64).read_line(&mut line)?;
    finish_frame(line)
}

async fn read_output(
    input: &mut (impl tokio::io::AsyncBufRead + Unpin),
    max: usize,
) -> Result<String> {
    let mut line = String::new();
    input.take((max + 1) as u64).read_line(&mut line).await?;
    finish_frame(line)
}

fn finish_frame(mut line: String) -> Result<String> {
    ensure!(
        line.ends_with('\n'),
        "JavaScript 查询进程已中断，或返回的数据过长。"
    );
    line.pop();
    Ok(line)
}

fn command() -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if cfg!(test) {
        command
            .args([
                "--exact",
                "ask::script_worker::tests::child_entry",
                "--nocapture",
                "--quiet",
            ])
            .env("MONICA_ASK_TEST_CHILD", "1");
    } else {
        command.arg(WORKER_FLAG);
    }
    Ok(command)
}

// Keep the resource reservation until the child is reaped, including when a
// cancelled Ask run drops the future during a query or while the guest runs.
struct Process(Option<(Child, ResourcePermit)>);
impl Drop for Process {
    fn drop(&mut self) {
        if let Some((mut child, permit)) = self.0.take() {
            let _ = child.start_kill();
            tokio::spawn(async move {
                let _ = child.wait().await;
                drop(permit);
            });
        }
    }
}

pub(super) trait Bridge {
    fn reply(&mut self, request: String) -> impl Future<Output = String> + Send;
}

pub(super) async fn run(
    source: &str,
    deadline: Duration,
    permit: ResourcePermit,
    lookup: &mut (impl Bridge + Send),
) -> Result<String> {
    ensure!(source.len() <= SOURCE_BYTES, "JavaScript 查询脚本过长。");
    let mut process = Process(Some((
        command()?
            .spawn()
            .context("无法启动 JavaScript 查询进程。")?,
        permit,
    )));
    let (child, _) = process.0.as_mut().unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let result = tokio::time::timeout(deadline, async {
        let mut ready = read_output(&mut output, 128).await?;
        // The test harness prints a short preamble before calling serve().
        if cfg!(test) {
            for _ in 0..4 {
                if ready == READY {
                    break;
                }
                ready = read_output(&mut output, 128).await?;
            }
        }
        ensure!(ready == READY, "JavaScript 查询进程未能初始化。");
        input
            .write_all(serde_json::to_string(source)?.as_bytes())
            .await?;
        input.write_all(b"\n").await?;
        let reply = loop {
            let frame = read_output(&mut output, CHILD_FRAME_BYTES).await?;
            if let Some(request) = frame.strip_prefix('?') {
                ensure!(
                    request.len() <= LOOKUP_INPUT_BYTES,
                    "JavaScript 查询参数过长。"
                );
                let reply = lookup.reply(request.to_owned()).await;
                ensure!(reply.len() <= LOOKUP_REPLY_BYTES, "查询结果过长。");
                input.write_all(reply.as_bytes()).await?;
                input.write_all(b"\n").await?;
            } else if let Some(result) = frame.strip_prefix('+') {
                break Ok(result.to_owned());
            } else if let Some(message) = frame.strip_prefix('-') {
                let message: String = serde_json::from_str(message)?;
                break Err(anyhow!(message));
            } else {
                bail!("JavaScript 查询进程返回了无效的数据。");
            }
        };
        drop(input);
        ensure!(
            child.wait().await?.success(),
            "JavaScript 查询进程异常退出。"
        );
        reply
    })
    .await;
    match result {
        Ok(result) => {
            if child.try_wait()?.is_some() {
                process.0.take();
            }
            result
        }
        Err(_) => {
            child
                .kill()
                .await
                .context("无法结束超时的 JavaScript 查询进程。")?;
            process.0.take();
            bail!(TIMEOUT_MESSAGE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monica_core::resources::{ResourceBudget, TaskResources};

    const TEST_WATCHDOG_MS: &str = "MONICA_ASK_TEST_WATCHDOG_MS";

    #[test]
    fn child_entry() {
        if std::env::var_os("MONICA_ASK_TEST_CHILD").is_some() {
            let timeout = std::env::var(TEST_WATCHDOG_MS)
                .map(|ms| Duration::from_millis(ms.parse().unwrap()))
                .unwrap_or(WORKER_WATCHDOG_TIMEOUT);
            serve(io::stdin().lock(), io::stdout().lock(), timeout).unwrap();
        }
    }

    #[tokio::test]
    async fn worker_stops_without_server_to_enforce_deadline() {
        let mut child = command()
            .unwrap()
            .env(TEST_WATCHDOG_MS, "500")
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(b"\"while (true) {}\"\n").await.unwrap();
        drop(input);

        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("the worker enforces its own deadline")
            .unwrap();
        assert_eq!(status.code(), Some(WORKER_TIMEOUT_EXIT));
    }

    struct NoLookup;
    impl Bridge for NoLookup {
        async fn reply(&mut self, _: String) -> String {
            unreachable!()
        }
    }

    async fn worker_permit(budget: &ResourceBudget) -> ResourcePermit {
        budget
            .acquire(TaskResources {
                cpu: 1,
                ..Default::default()
            })
            .await
            .unwrap()
    }

    #[derive(Default)]
    struct Fixture {
        calls: usize,
        response: String,
    }

    impl Bridge for Fixture {
        async fn reply(&mut self, _: String) -> String {
            self.calls += 1;
            self.response.clone()
        }
    }

    #[tokio::test]
    async fn wall_clock_stops_infinite_loop_and_expensive_regex() {
        let budget = ResourceBudget::new(Default::default()).unwrap();
        let slots = budget.status().cpu_available;
        for script in [
            "cli('ready'); while (true) {}",
            r"cli('ready'); const regex = /(?:finance|analytics|medicine|product)\b/i; while (regex.test('product engineering')) {}",
        ] {
            let mut fixture = Fixture {
                response: "{}".into(),
                ..Default::default()
            };
            let error = run(
                script,
                Duration::from_millis(1500),
                worker_permit(&budget).await,
                &mut fixture,
            )
            .await
            .unwrap_err();
            assert_eq!(fixture.calls, 1, "guest reached the loop");
            assert_eq!(error.to_string(), TIMEOUT_MESSAGE);
            assert_eq!(
                budget.status().cpu_available,
                slots,
                "the timed-out process was reaped"
            );
        }
    }

    #[tokio::test]
    async fn one_script_can_process_several_large_pages() {
        let budget = ResourceBudget::new(Default::default()).unwrap();
        let row = serde_json::json!({
            "name":"Synthetic contact","headline":"VP product at synthetic company",
            "summary":"x".repeat(180),"link":"https://example.invalid/profiles/synthetic",
            "tags":["UNC","Classmates","Synthetic"]
        });
        let mut fixture = Fixture {
            response: serde_json::json!({"rows":vec![row; 500]}).to_string(),
            ..Default::default()
        };
        let script = r#"
            let count = 0;
            for (let offset = 0; offset < 2000; offset += 500) {
                const data = cli("directory", {offset, limit:500});
                for (const row of data.rows) {
                    if (/product|finance|medicine|analytics/.test(row.headline)) count++;
                }
            }
            return count;
        "#;
        let result = run(
            script,
            Duration::from_secs(20),
            worker_permit(&budget).await,
            &mut fixture,
        )
        .await
        .unwrap();
        assert_eq!(result, "2000");
        assert_eq!(fixture.calls, 4);
    }

    #[tokio::test]
    async fn result_above_the_old_limit_reaches_the_parent() {
        let budget = ResourceBudget::new(Default::default()).unwrap();
        let result = run(
            "return 'x'.repeat(95_998);",
            Duration::from_secs(8),
            worker_permit(&budget).await,
            &mut NoLookup,
        )
        .await
        .unwrap();
        assert_eq!(result.len(), SCRIPT_RESULT_BYTES);
        let error = run(
            "return 'x'.repeat(95_999);",
            Duration::from_secs(8),
            worker_permit(&budget).await,
            &mut NoLookup,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("脚本输出过长"));
    }
}

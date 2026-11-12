# Monica Rust native shell

An independent Tauri menu-bar application for Apple Silicon / macOS 26+. The settings window manages a native PostgreSQL 16 cluster and the same `monica-server` used by containers. The main CRM opens in the default browser. This shell does not start the previous Python API, a Next server or Docker.

The application identifier is `org.monica.rust.desktop`. Normal data lives under `~/Library/Application Support/Monica Rust`, with logs under `~/Library/Logs/Monica Rust`. Existing Monica folders are never opened. A private owner lock prevents two supervisors using one data directory. Database, session and control credentials are generated locally and retained in a `0600` file inside the `0700` data directory; settings IPC never returns those credentials.

The supervisor initializes only a new empty PG16 cluster, runs Rust migrations, then waits for actual API readiness. PostgreSQL uses authenticated loopback TCP, so long data paths do not hit Unix-socket length limits. The preferred Web port is 17494; an occupied port causes allocation of another port. The selected port is persisted to preserve browser sessions and IndexedDB across restarts. Paused state is also persisted and passed as `MONICA_START_PAUSED` before workers start. The backend's durable paused state remains authoritative.

Closing the settings window hides it while the menu bar remains active. Pause/resume changes the actual scheduler through bearer-protected internal HTTP. The UI reports failures without claiming the operation succeeded. Quitting cooperatively stops the Rust server (120 seconds maximum drain wait), then performs PostgreSQL fast shutdown (60 seconds). No database SIGKILL is used. Unexpected owned-process exits trigger bounded dependency-tree recovery. SIGTERM/SIGINT of the shell also request this graceful exit. Login auto-start is opt-in through the native LaunchAgent plugin.

## Runtime integration

The shell starts `monica-server migrate`, then `monica-server serve --listen 127.0.0.1:<port>`, passing these environment values:

- `MONICA_DATABASE_URL`, `MONICA_SESSION_SECRET`, `MONICA_CONTROL_TOKEN`.
- `MONICA_ORIGIN`, `MONICA_MEDIA_ROOT`, `MONICA_WEB_ROOT`, `MONICA_START_PAUSED`.
- `MONICA_NATIVE_ROOT` points to the independent raw spool; `MONICA_WHATSAPP_BINARY` points to the bundled Go bridge.
- 模型设置初始关闭，可在网页「设置 → 模型设置」显式启用；重启保留选择。`MONICA_ADAPTERS_ENABLED=false`、`AI_ENABLED=false` 仍用于隔离旧适配器。

It uses `GET /api/v1/ready`, `GET /internal/runtime/status`, and `POST /internal/runtime/pause` or `/resume`. Internal calls carry only a native-side bearer token, with no browser Origin header. Status must include `paused: boolean`, `draining: boolean`, `active_jobs: number`. First account creation uses the real `/api/v1/auth/bootstrap` contract.

The settings window exposes opt-in Contacts and Messages controls. The browser Sources page manages separate mail and WhatsApp connections. The package includes the Go WhatsApp bridge and ffprobe; neither reads an account until explicitly connected. The package also includes locked Python 3.12.11 collector bridges, Playwright Chromium, and a pinned Chatlog build. Instagram and LinkedIn each expose explicit login, local login status, enable, pause and cancel controls; collection remains off after login until enabled. Optional face model assets are not included and report needs_models until explicitly installed. No personal credentials or previously installed collector configuration are bundled.

## Build an isolated local test artifact

```sh
npm ci --prefix apps/web
npm run build --prefix apps/web
cargo build --release -p monica-server
npm ci --prefix apps/native-desktop
(cd services/whatsapp && CGO_ENABLED=1 go build -trimpath -o ../../target/release/monica-whatsapp .)
python3 scripts/whatsapp-licenses.py
python3 scripts/macos-collectors.py --output .local-validation/collector-runtime
python3 scripts/macos-chatlog.py --output .local-validation/chatlog-runtime
python3 scripts/macos-collectors-smoke.py .local-validation/collector-runtime
python3 scripts/macos-refresh-collectors.py .local-validation/collector-runtime
python3 scripts/macos-bundle.py
npm --prefix apps/native-desktop run desktop -- build
python3 scripts/macos-finalize.py 'apps/native-desktop/src-tauri/target/release/bundle/macos/Monica Rust.app'
python3 scripts/macos-verify.py 'apps/native-desktop/src-tauri/target/release/bundle/macos/Monica Rust.app'
```

`macos-bundle.py` requires a fresh output directory (default `apps/native-desktop/runtime`). It copies only explicit server/static assets, PostgreSQL executables/libraries, ffprobe, the WhatsApp Go bridge, Chatlog and the isolated Python/Chromium distribution, relocates dynamic dependencies, signs changed native files and rejects remaining Homebrew links. For a repeat build, preserve the previous runtime outside that location and create a new output. `--server` and `--postgres-prefix` can select explicit test inputs. No existing database, browser session, collector configuration or `.env` is read.

The local artifact is ad-hoc signed, not notarized or ready for public distribution. The recorded initial 161.68 MiB app included a debug Rust backend and no external collectors/models; its size is not a like-for-like comparison with the previous full desktop bundle.

## Tests

```sh
npm run build --prefix apps/native-desktop
npm run test --prefix apps/native-desktop
cargo test --manifest-path apps/native-desktop/src-tauri/Cargo.toml --no-default-features --lib
cargo clippy --manifest-path apps/native-desktop/src-tauri/Cargo.toml --all-targets -- -D warnings
MONICA_NATIVE_SERVER="$PWD/target/debug/monica-server" \
MONICA_NATIVE_PG_BIN="$PWD/apps/native-desktop/runtime/postgres/bin" \
MONICA_NATIVE_WEB="$PWD/apps/web/dist" \
cargo test --manifest-path apps/native-desktop/src-tauri/Cargo.toml \
  --no-default-features --test native_runtime -- --ignored --nocapture
```

The native lifecycle test creates a fresh temporary cluster and synthetic account. It verifies real startup, static Web, authentication, unauthorized-control rejection, pause/resume, port collision, long paths, server and database failure recovery, persistence, repeated startup and shutdown without surviving child PIDs. It can point at binaries inside the `.app` to verify relocation. Settings Playwright tests use a fake native bridge and are separately labelled UI tests, not lifecycle proof.

For a debug GUI test, build with `--features custom-protocol`, then set `MONICA_RUST_RUNTIME` to an explicit generated runtime and `MONICA_RUST_TEST_DATA_DIR` to a fresh temporary directory. Debug logs remain inside that temporary directory. Release builds ignore those debug overrides. No old personal data is needed.

Application icons are copied from the pinned public Desktop source `e35b49e4ca33406f0fdd7ecf8767931b44fdac87`; the Rust shell and settings code are new. No private or uncommitted source prototype is copied.

### Native window acceptance (2026-11-11)

The actual Tauri WebKit settings window was exercised through native accessibility
controls against a fresh temporary PG16 cluster, using the current debug backend.
Observed startup to `运行中`, pause to `后台任务已暂停`, resume to `运行中`, synthetic
first-account creation, and the `打开 Monica` button opening the real default-browser
login page. The UI displayed its temporary data directory. `退出 Monica Rust`
closed the app, and a process check found no remaining owned server/PG processes.
This is separate from the three fake-bridge UI regressions and five supervisor
unit tests, which also pass. A final release bundle still requires the integrated
backend and artifact lifecycle check; these observations do not certify a stale
previous `.app`.

To repeat native window acceptance without risking the regular data directory:

```sh
cargo build --manifest-path apps/native-desktop/src-tauri/Cargo.toml --features custom-protocol
python3 scripts/macos-gui-test.py
```

Launch the printed temporary `.app` through the native UI. Its explicit wrapper
preserves isolated data/runtime overrides even when LaunchServices restarts it.
Use only synthetic credentials. Do not enable the login item during this test.
The debug override no longer eagerly resolves a missing bundled resource path.

`macos-bundle.py` now includes `ffprobe` and relocates its dynamic dependencies
alongside PostgreSQL. The supervisor prepends the runtime tools directory to the
backend PATH, and packaging rejects unresolved Homebrew links. GeoNames credit
and exact embedded-data hashes are shipped under `runtime/licenses`; the map UI
also displays GeoNames / CC BY 4.0 attribution. These latest packaging additions
must be tested in the final release artifact before delivery.

The lifecycle regression also suspends only its owned server with SIGSTOP, checks
that a timed-out health probe preserves the same PID, resumes with SIGCONT, and
verifies recovery without a second server. This tests the process/health boundary
used after waking; it is not a claim that the user's Mac was put to sleep. Actual
system sleep and login-item persistence are not changed by automated acceptance.
The login-start checkbox remains opt-in and was observed unchecked in native UI.

After the final release build, validate the exact delivered bundle, including
its embedded server hash, signature, relocation, ffprobe, and fresh-cluster
lifecycle (the regular application data directory is never used):

```sh
python3 scripts/macos-verify.py \
  'apps/native-desktop/src-tauri/target/release/bundle/macos/Monica Rust.app' \
  --expected-commit "$(git rev-parse HEAD)" --require-clean
```

The runtime manifest now explicitly records whether the source tree was dirty,
so an intermediate test package cannot masquerade as a commit-exact release.
Third-party native dependency copyright/license files from installed formula
roots are copied under `runtime/licenses` alongside GeoNames attribution.

## Native source controls

The multisource baseline is `monica-multisource@1a6e40e20c6ac12aca434b61244059d067847473`. The settings window exposes separate opt-in controls for Contacts and Messages. Both start disabled, use only this app's independent `raw/apple_contacts` and `raw/apple_messages` spool, and stop beginning work while background tasks are paused. Contacts permission is requested only by its explicit enable action. Messages reads the system database only after explicit enablement and the OS permission check. Debug test-data mode rejects personal Contacts/Messages access; synthetic reader tests call the path-based SQLite entry point instead.

A pending page remains on disk until the token-protected Rust endpoint confirms durable journal storage. Only then is `cursor.json` fsynced and renamed and the pending page removed. Failed requests can safely replay. Contacts history resets are split into at most 1,000 records and 3 MiB per request, with a stable snapshot ID and final-page confirmation. The server finalizes missing-contact tombstones in bounded batches. Evidence already stored is retained when a source is paused.

Synthetic validation commands:

```sh
python3 apps/native-desktop/tests/test_native_messages.py
cargo test --manifest-path apps/native-desktop/src-tauri/Cargo.toml --lib --offline
cd apps/native-desktop && npm run test
```

The SQLite fixture exercises the real Objective-C reader, including live WAL, pagination, edits, retractions, and group sender identities. It does not request Contacts permission or read a user's Messages database.

After Tauri creates the final test `.app`, run `python3 scripts/macos-finalize.py /absolute/path/Monica\ Rust.app` before `scripts/macos-verify.py`. Ad-hoc signing changes embedded Mach-O bytes; finalization records the delivered server digest, preserves the pre-bundle build digest, and reseals the outer bundle. This artifact is a local test build, not a notarized distribution.

## Isolated social login and Chatlog lifecycle

The supervisor passes `MONICA_COLLECTOR_DATA_ROOT=<app-data>/collector-data`, `MONICA_COLLECTOR_BRIDGE=<runtime>/collectors/run-bridge`, and `MONICA_COLLECTOR_ENTRY=<runtime>/collectors/run-collector`. Wrappers locate their own Python, Chromium and publisher assets after relocation. Login uses the same independent collector root as scheduled jobs. Instagram usernames are explicitly entered and saved only by successful login; LinkedIn exports its own messaging auth snapshot. No existing browser profile is discovered. Settings read the bearer-protected adapter state and local auth status before enabling collection.

Login subprocesses have an owned process group. Cancel, global pause and quit terminate that group, including provider descendants. The Python wrapper preserves the parent's group in this mode. Synthetic unit tests exercise a stubborn fake child/grandchild and verify default-off, no auto-enable, authentication gating and successful enable/pause acknowledgements. Debug test-data mode rejects real login launches.

Chatlog uses an independently allocated loopback port and private config/work directory. The user explicitly supplies a WeChat 4 data path and existing key; neither is discovered from another app. It starts only after enablement, stops on pause/quit, and recovers only its own crashed child. The Rust backend receives `MONICA_CHATLOG_ALPHA_URL`. Synthetic lifecycle tests use a fake HTTP child, not a real WeChat database.

The collector distribution was moved to a path containing spaces and tested there: bundled Chromium rendered an offline synthetic document; both provider status commands reported needs_login in a fresh root; seven publication bridge fixtures passed. This proves wrapper/interpreter/browser relocation without accessing a real account. The final signed app must repeat this smoke test via `macos-verify.py`.

The latest additional source-state guard asks the backend before every native Contacts/Messages read. A missing or paused source disables local polling; a failed status request skips the read. The guard never enables a locally disabled source. Twelve native unit tests include these fail-closed cases. Chatlog enable/disable is coupled to the `wechat` adapter control; global pause retains user intent, and a Web-side adapter pause stops the owned daemon on the next status tick.

The actual pinned Chatlog binary also passes `scripts/macos-chatlog-smoke.py`: an empty synthetic source is rejected, no loopback listener survives, and the source directory remains untouched. `macos-verify.py` repeats this check against the delivered binary.

Newest native GUI acceptance (social/Chatlog controls) was attempted but blocked before interaction by the Mac being locked. The earlier actual base-shell/Contacts/Messages acceptance remains recorded above; the newest controls currently have synthetic Playwright and native process tests only. Final delivery must keep this distinction if the screen remains locked.

Source refresh does not reinstall dependencies: `macos-refresh-collectors.py` first checks the lock hash, verifies every vendored checksum, then copies only the 77 explicitly listed source/metadata files plus the Rust publication bridge. Packaging rejects stale staged source or bridge hashes. This prevents an earlier prepared Python distribution from silently carrying older collector code in the final app.

The read-only WeChat bridge receives `MONICA_CHATLOG_SNAPSHOT_ROOT=<owned-chatlog-root>/work/db_storage` and `MONICA_CHATLOG_ACCOUNT=native-chatlog-<SHA256>`. The domain-separated hash derives only from the explicitly configured source path; it is an opaque connection anchor, never a guessed WeChat user ID. Original encrypted paths and keys are not passed to the collector bridge. Without explicit configuration both environment variables are removed, including inherited values.

Saving Chatlog configuration drains and restarts only the owned Rust backend to refresh its environment. PostgreSQL, Web port and browser session remain intact; the real temporary-cluster test verifies all three. Changing the source path moves old owned snapshots/cache into a private recoverable archive before accepting new snapshots. Pending records/checkpoints are separately scoped to the opaque account by the bridge. The Python runtime includes both `collector-bridge.py` and its explicit `collector-chatlog.py` module, each checked for stale hashes before packaging.

# Monica Rust native shell

An independent Tauri menu-bar application for Apple Silicon / macOS 26+. The settings window manages a native PostgreSQL 16 cluster and the same `monica-server` used by containers. The main CRM opens in the default browser. This shell does not start the previous Python API, a Next server or Docker.

The application identifier is `org.monica.rust.desktop`. Normal data lives under `~/Library/Application Support/Monica Rust`, with logs under `~/Library/Logs/Monica Rust`. Existing Monica folders are never opened. A private owner lock prevents two supervisors using one data directory. Database, session and control credentials are generated locally and retained in a `0600` file inside the `0700` data directory; settings IPC never returns those credentials.

The supervisor initializes only a new empty PG16 cluster, runs Rust migrations, then waits for actual API readiness. PostgreSQL uses authenticated loopback TCP, so long data paths do not hit Unix-socket length limits. The preferred Web port is 17494; an occupied port causes allocation of another port. The selected port is persisted to preserve browser sessions and IndexedDB across restarts. Paused state is also persisted and passed as `MONICA_START_PAUSED` before workers start. The backend's durable paused state remains authoritative.

Closing the settings window hides it while the menu bar remains active. Pause/resume changes the actual scheduler through bearer-protected internal HTTP. The UI reports failures without claiming the operation succeeded. Quitting cooperatively stops the Rust server (120 seconds maximum drain wait), then performs PostgreSQL fast shutdown (60 seconds). No database SIGKILL is used. Unexpected owned-process exits trigger bounded dependency-tree recovery. SIGTERM/SIGINT of the shell also request this graceful exit. Login auto-start is opt-in through the native LaunchAgent plugin.

## Runtime integration

The shell starts `monica-server migrate`, then `monica-server serve --listen 127.0.0.1:<port>`, passing these environment values:

- `MONICA_DATABASE_URL`, `MONICA_SESSION_SECRET`, `MONICA_CONTROL_TOKEN`.
- `MONICA_ORIGIN`, `MONICA_MEDIA_ROOT`, `MONICA_WEB_ROOT`, `MONICA_START_PAUSED`.
- `MONICA_AI_ENABLED=false`, `MONICA_ADAPTERS_ENABLED=false`, `AI_ENABLED=false`.

It uses `GET /api/v1/ready`, `GET /internal/runtime/status`, and `POST /internal/runtime/pause` or `/resume`. Internal calls carry only a native-side bearer token, with no browser Origin header. Status must include `paused: boolean`, `draining: boolean`, `active_jobs: number`. First account creation uses the real `/api/v1/auth/bootstrap` contract.

The current settings window deliberately exposes no real-account adapter enrollment. Python collectors, Chatlog, Chromium and face models are not included by the core-runtime packaging script. Their independent runtime and credential boundary still need to be connected before this app can claim full collection capability. The app's current message states that limitation explicitly.

## Build an isolated local test artifact

```sh
npm ci --prefix apps/web
npm run build --prefix apps/web
cargo build --release -p monica-server
npm ci --prefix apps/native-desktop
python3 scripts/macos-bundle.py
npm --prefix apps/native-desktop run desktop -- build
codesign --verify --deep --strict 'apps/native-desktop/src-tauri/target/release/bundle/macos/Monica Rust.app'
```

`macos-bundle.py` requires a fresh output directory (default `apps/native-desktop/runtime`). It copies only explicit server/static assets and PostgreSQL executables/libraries, relocates dynamic dependencies, signs changed native files and rejects remaining Homebrew links. For a repeat build, preserve the previous runtime outside that location and create a new output. `--server` and `--postgres-prefix` can select explicit test inputs. No existing database, browser session, collector configuration or `.env` is read.

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

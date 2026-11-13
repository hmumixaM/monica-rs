# Compose readiness review — 2026-11-13

Reviewed parent `5dfa96a7928a2b78532091c3f161c586f527aac2`, actual base `301241bf41e8ba8bb18d981f4e9138fe220e9c18`. This is a deployment configuration follow-up; application source, binaries, test matrices and benchmark freeze remain at 5dfa96a.

## R1 — Probe final PostgreSQL TCP readiness

`infra/compose.yaml` checks PostgreSQL with `pg_isready` without a host. It probes the local Unix socket. The official container's temporary initialization server also accepts Unix-socket connections while its TCP listener is disabled. Compose can therefore release the dependent migration service before the final TCP server is ready. The database client uses `postgres:5432`, so an early healthy result can turn a valid cold start into a failed migration connection.

Actual isolated NAS initialization logs show a temporary server, a long shutdown checkpoint and a later final TCP listener. The interval can exceed connection acquisition deadlines. A read-only probe of this task's own socket-only PostgreSQL16 confirms Unix readiness returns0; the reference harness explicitly starts that server with listen_addresses empty. A separate reserved, non-listening TCP port returns2. No unrelated local database was contacted and no data was modified. This demonstrates transport-specific readiness and does not present the reserved port as the server's default port.

Add `-h 127.0.0.1` to the existing PostgreSQL healthcheck. The temporary initialization server cannot satisfy that TCP probe; the normal final server can. User, database, intervals, retries, secrets, dependencies and connection budgets remain unchanged. Existing production NAS configuration and services are outside this change.

Code-review traced the PostgreSQL startup stages, Compose dependency and migration's TCP URL. Excessive-tests assessment: a read-only real-protocol observation plus Compose configuration validation is proportionate to this one configuration flag. No additional delay-based or source-string test is needed. Current real NAS runtime acceptance independently waits for an authenticated final connection. Personal-data deployment remains outside this test.

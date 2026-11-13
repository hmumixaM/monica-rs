# Linux acceptance follow-up — 2026-11-13

The first ARM64 isolated test-image execution failed before tests: `initdb: file /usr/share/postgresql/16/postgresql.conf.sample does not exist`. Inspection showed this is a symlink to `../postgresql.conf.sample`. The benchmark image copied only the version-specific directory from official `postgres:16-bookworm`, dropping the shared target. This also prevents the planned NAS benchmark image from initializing fresh databases.

The repair in `infra/benchmark.Dockerfile` copies the complete PostgreSQL share directory, preserving both the versioned assets and the common symlink target. It changes only the isolated benchmark image, with no production Compose database or application behavior change. The generated, untracked Linux test image uses the same correction. Old failure logs are retained in `.local-validation/acceptance-2026-11-12/linux-arm64-initialization-before-fix.log`.

Code-review traced `initdb` asset resolution and both stage-copy paths; no API, dependency, credential or production volume change is introduced. Excessive-tests assessment: use actual fresh-database initialization/full matrix rather than a source-string test for a one-line Docker asset-copy correction. The correction must be verified by actual Linux PostgreSQL initialization and the full acceptance runs. No standalone implementation-mirroring test was added.

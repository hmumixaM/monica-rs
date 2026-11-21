# Working on Monica Rust

## Verify changes

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
python3 scripts/test-cargo.py --workspace --no-fail-fast   # starts its own PostgreSQL 16
sh scripts/test.sh                                         # full suite plus CLI and HTTP smoke
```

`scripts/test-cargo.py` accepts normal `cargo test` arguments, for example
`-p monica-core --test ai`. Python import tests need
`uv run --no-project --python 3.12 --with 'psycopg[binary]==3.2.12' --with cryptography==46.0.3 python scripts/test-nas-live-imports.py`.

## NAS deployment

The live instance runs on the Synology NAS at `https://nas.tailfb022c.ts.net:9444`
from `/volumeUSB1/usbshare/monica-rs` (`config/`, `data/`, `state/`). See
`docs/NAS_ISOLATED_DEPLOYMENT.md`.

- `infra/nas.compose.yaml` is the source of `config/compose.yaml`; keep them identical.
- Run compose as `docker-compose -p monica-rs -f config/compose.yaml --env-file config/.env.live`.
- Apply migrations with `--profile migrate run --rm migrate` before starting a new server image.
- `infra/nas-boot.sh` is installed as `/usr/local/etc/rc.d/monica-rs.sh`.
- GitHub Actions cannot build images while its budget is exhausted. The server is
  cross-compiled on the Mac (cargo-zigbuild, target `x86_64-unknown-linux-gnu.2.36`)
  and loaded on the NAS with `docker load`.
- Take a `pg_dump` before any change that restarts or migrates PostgreSQL.
- The image is assembled without Docker by `scripts/nas-image.py COMMIT`: the Debian
  layers of the previous image, the cross-compiled binary, and `apps/web/dist` built
  from that commit.

## Web interface

Design tokens, navigation groups and page structure are described in
`docs/ui-clarity.md`. `npm run build --prefix apps/web` type-checks and builds;
`npm run test --prefix apps/web` runs the Playwright suite against the build.

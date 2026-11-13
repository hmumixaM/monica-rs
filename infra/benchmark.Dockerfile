# Build context is an allowlisted directory made by prepare-benchmark-context.py.
ARG MONICA_IMAGE
FROM ${MONICA_IMAGE} AS rust
FROM postgres:16-bookworm AS postgres
FROM ghcr.io/astral-sh/uv:0.9.17 AS uv
FROM python:3.12-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
      libicu72 libldap-2.5-0 liblz4-1 libpam0g libreadline8 libssl3 \
      libxml2 libxslt1.1 libzstd1 zlib1g libkrb5-3 libgssapi-krb5-2 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 monica
COPY --from=postgres /usr/lib/postgresql/16 /usr/lib/postgresql/16
COPY --from=postgres /usr/share/postgresql /usr/share/postgresql
COPY --from=uv /uv /usr/local/bin/uv
COPY --from=rust /usr/local/bin/monica-server /benchmark/target/release/monica-server
WORKDIR /benchmark
COPY --chown=monica:monica references /benchmark/references
COPY --chown=monica:monica scripts /benchmark/scripts
COPY --chown=monica:monica source.json /benchmark/source.json
RUN pip install --no-cache-dir psutil==7.1.3 && chown -R monica:monica /benchmark
ENV MONICA_REFERENCE_ROOT=/benchmark/references MONICA_PG_BIN=/usr/lib/postgresql/16/bin UV_PYTHON_PREFERENCE=only-system
USER monica
RUN python -c 'import sys; sys.path.insert(0,"scripts"); from reference_backend import BASELINES,tracked; from pathlib import Path; import subprocess; [(Path("/tmp/deps-"+n).mkdir(),[(Path("/tmp/deps-"+n)/f).write_bytes(tracked(r,c,"services/api/"+f)) for f in ("pyproject.toml","uv.lock")],subprocess.run(["uv","sync","--project","/tmp/deps-"+n,"--frozen","--no-dev","--python","3.12"],check=True)) for n,(r,c) in BASELINES.items()]'
ENTRYPOINT ["python", "scripts/benchmark-backends.py"]

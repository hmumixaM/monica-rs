FROM ghcr.io/astral-sh/uv:0.9.17 AS uv
FROM python:3.12-bookworm
COPY --from=uv /uv /usr/local/bin/uv
WORKDIR /opt/monica/collectors
COPY apps/desktop/collectors ./
RUN uv sync --frozen --no-dev --python 3.12
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/monica/browsers PYTHONNOUSERSITE=1 PYTHONDONTWRITEBYTECODE=1
RUN .venv/bin/python -m playwright install --with-deps chromium \
    && useradd --create-home --uid 10001 monica \
    && mkdir -p /data/collectors && chown -R monica:monica /data
COPY scripts/collector-bridge.py scripts/collector-chatlog.py scripts/chatlog_moments.py /opt/monica/
COPY infra/collector-supervisor.py /opt/monica/collector-supervisor.py
USER monica
# Explicit one-shot execution only; no accounts or collection start by default.
ENTRYPOINT ["/opt/monica/collectors/.venv/bin/python", "/opt/monica/collector-bridge.py", "--collector", "/opt/monica/collectors/collector.py"]
CMD ["--help"]

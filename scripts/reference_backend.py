#!/usr/bin/env python3
"""Run a pinned Python baseline on four newly-created, disposable databases.

No checkout environment, runtime directories or existing database is loaded.
Used by differential and performance checks; source repositories are read-only.
"""
import contextlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

BASELINES = {
    "multisource": ("/Users/maxhu/play/monica-multisource", "1a6e40e20c6ac12aca434b61244059d067847473"),
    "crm": ("/Users/maxhu/play/monica", "6d59cfef4b1a12f678d809a00e37d8332be70b06"),
    "desktop": ("/Users/maxhu/play/monica-desktop-public", "e35b49e4ca33406f0fdd7ecf8767931b44fdac87"),
}
PG = Path(os.environ.get("MONICA_PG_BIN", "/opt/homebrew/opt/postgresql@16/bin"))


def staged_manifest(repository, commit):
    root = os.environ.get("MONICA_REFERENCE_ROOT")
    if not root:
        return None
    root = Path(root).resolve()
    manifest = json.loads((root / "manifest.json").read_text())
    for name, value in manifest["baselines"].items():
        if value["repository"] == repository and value["commit"] == commit:
            return root / name, value["files"]
    raise RuntimeError("Pinned reference is absent from staged manifest")


def tracked(repository, commit, path):
    staged = staged_manifest(repository, commit)
    if staged:
        import hashlib
        root, files = staged
        target = (root / path).resolve()
        if not target.is_relative_to(root) or path not in files:
            raise RuntimeError("Reference path is outside pinned manifest")
        content = target.read_bytes()
        if hashlib.sha256(content).hexdigest() != files[path]:
            raise RuntimeError("Reference source checksum mismatch: " + path)
        return content
    return subprocess.check_output(["git", "-C", repository, "show", f"{commit}:{path}"])


class ReferenceBackend:
    def __init__(self, baseline="crm"):
        self.baseline = baseline
        self.repository, self.commit = BASELINES[baseline]
        self.process = None
        self.pg_started = False
        self.temp = None
        self.log = None

    def __enter__(self):
        self.temp = tempfile.TemporaryDirectory(prefix=f"monica-reference-{self.baseline}-", dir="/tmp")
        self.root = Path(self.temp.name)
        try:
            self._start()
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def _start(self):
        staged = staged_manifest(self.repository, self.commit)
        paths = list(staged[1]) if staged else subprocess.check_output(["git", "-C", self.repository, "ls-tree", "-r", "--name-only", self.commit,
                                         "services/api/app", "migrations"]).decode().splitlines()
        for relative in paths:
            if not relative.endswith((".py", ".sql")):
                continue
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(tracked(self.repository, self.commit, relative))
        dependency_root = self.root / "dependencies"
        dependency_root.mkdir()
        for filename in ("pyproject.toml", "uv.lock"):
            (dependency_root / filename).write_bytes(tracked(self.repository, self.commit, f"services/api/{filename}"))
        # Exact dependency lock; no editable install from an original worktree.
        subprocess.run(["uv", "sync", "--project", str(dependency_root), "--frozen", "--no-dev", "--python", "3.12"],
                       check=True, stdout=subprocess.DEVNULL)
        self.python = dependency_root / ".venv/bin/python"
        subprocess.run([str(PG / "initdb"), "-D", str(self.root / "pg"), "--auth=trust", "--no-locale", "--encoding=UTF8"],
                       check=True, stdout=subprocess.DEVNULL)
        subprocess.run([str(PG / "pg_ctl"), "-D", str(self.root / "pg"), "-l", str(self.root / "pg.log"), "-o",
                        f"-c listen_addresses='' -k {self.root} -c max_connections=100 -c shared_preload_libraries=pg_stat_statements -c track_io_timing=on -c track_wal_io_timing=on", "-w", "start"],
                       check=True, stdout=subprocess.DEVNULL)
        self.pg_started = True
        for database in ("crm", "wechat", "instagram", "linkedin"):
            subprocess.run([str(PG / "createdb"), "-h", str(self.root), database], check=True)
        env = {"PATH": os.defpath, "PYTHONNOUSERSITE": "1", "PYTHONDONTWRITEBYTECODE": "1",
               "DATABASE_URL": f"postgresql+psycopg:///crm?host={self.root}",
               "RAW_ROOT": str(self.root / "raw"), "MEDIA_ROOT": str(self.root / "media"),
               "STAGING_ROOT": str(self.root / "staging"), "AI_ENABLED": "false", "APP_ENV": "development",
               "SESSION_SECRET": "synthetic-reference-session-secret-32bytes",
               "IMPORT_HMAC_SECRET": "synthetic-reference-import-secret-32bytes",
               "CHATLOG_ALPHA_URL": "http://127.0.0.1:1", "LITELLM_BASE_URL": "http://127.0.0.1:1/v1"}
        for provider in ("wechat", "instagram", "linkedin"):
            env[f"{provider.upper()}_DATABASE_URL"] = f"postgresql+psycopg:///{provider}?host={self.root}"
        self.environment = env
        migrate = self.root / "migrate.py"
        migrate.write_text('''import os,sys
from pathlib import Path
import psycopg
root=Path(sys.argv[1])
with psycopg.connect("dbname=crm host="+str(root),autocommit=True) as connection:
    for path in sorted((root/"migrations").glob("*.sql")):
        connection.execute(path.read_text())
    connection.execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
for provider in ("wechat","instagram","linkedin"):
    with psycopg.connect("dbname="+provider+" host="+str(root),autocommit=True) as connection:
        connection.execute((root/"services/api/app/platform_schema.sql").read_text())
''')
        subprocess.run([str(self.python), str(migrate), str(self.root)], env=env, check=True)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        self.url = f"http://127.0.0.1:{port}"
        self.database_url = f"postgresql:///crm?host={self.root}"
        self.log = (self.root / "http.log").open("wb")
        self.process = subprocess.Popen([str(self.python), "-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", str(port)],
                                        cwd=self.root / "services/api", env=env, stdout=self.log, stderr=self.log)
        for _ in range(1200):
            if self.process.poll() is not None:
                raise RuntimeError((self.root / "http.log").read_text())
            try:
                with urllib.request.urlopen(self.url + "/api/v1/ready", timeout=1) as response:
                    if response.status == 200:
                        return
            except OSError:
                time.sleep(.05)
        raise RuntimeError("isolated reference backend did not become ready: " + (self.root / "http.log").read_text()[-6000:])

    def __exit__(self, *_):
        if self.process is not None and self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if self.log:
            self.log.close()
        if self.pg_started:
            subprocess.run([str(PG / "pg_ctl"), "-D", str(self.root / "pg"), "-m", "immediate", "-w", "stop"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        if self.temp:
            self.temp.cleanup()


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", choices=tuple(BASELINES), default="crm")
    args = parser.parse_args()
    with ReferenceBackend(args.baseline) as backend:
        for path in ("/health", "/api/v1/health", "/api/v1/ready"):
            with urllib.request.urlopen(backend.url + path) as response:
                print(path, json.load(response))
        print("Isolated reference startup/cleanup smoke passed", backend.commit)

#!/usr/bin/env python3
"""Copy one online legacy database to a private Mac archive, without source writes.

NAS_PASSWORD is accepted only through the environment and sent to sudo's stdin.
Only the dedicated export's processes may be cancelled. No deployment operation
or production configuration update is provided by this tool.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

DATABASES = ("crm", "crm_wechat", "crm_instagram", "crm_linkedin")
MIN_FREE = 20 * 1024**3


def ssh_args(host):
    # A shared ControlMaster can retain a channel after its remote process has
    # exited. Each bulk export owns a connection with a bounded liveness check.
    return ["ssh","-o","ControlMaster=no","-o","ControlPath=none",
            "-o","ServerAliveInterval=10","-o","ServerAliveCountMax=3",
            "-o","BatchMode=yes","-o","ConnectTimeout=15",host]


def health(url):
    # Do not follow the legacy login redirect or create a browser session.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    start = time.monotonic()
    try:
        with urllib.request.build_opener(NoRedirect).open(url, timeout=25) as r:
            code = r.status
    except urllib.error.HTTPError as e:
        code = e.code
    if code not in (200, 301, 302, 303, 307, 308):
        raise RuntimeError("legacy health request failed")
    return time.monotonic() - start


def remote_script(database, tag):
    # Only constants and validated database/tag values enter this program.
    return '''import json,os,pathlib,signal,subprocess,sys,time
D="/var/packages/ContainerManager/target/usr/bin/docker"
database=DATABASE
tag=TAG
stop=False
def request_stop(*_):
 global stop
 stop=True
signal.signal(signal.SIGTERM,request_stop)
signal.signal(signal.SIGHUP,request_stop)
def cancel():
 for entry in pathlib.Path("/proc").iterdir():
  if not entry.name.isdigit():continue
  try:
   argv=(entry/"cmdline").read_bytes().split(b"\\0")
   if not argv or argv[0].split(b"/")[-1]!=b"pg_dump":continue
   env=(entry/"environ").read_bytes().split(b"\\0")
   if ("PGAPPNAME="+tag).encode() in env:os.kill(int(entry.name),signal.SIGTERM)
  except (OSError,ProcessLookupError):pass
cmd=[D,"exec","-e","PGAPPNAME="+tag,"-e","PGOPTIONS=-c default_transaction_read_only=on -c lock_timeout=1000","infra-postgres-1","pg_dump","-U","crm","--format=custom","--compress=1","--no-owner","--no-privileges","--lock-wait-timeout=1s","--dbname="+database]
start=time.monotonic()
p=subprocess.Popen(cmd,stdin=subprocess.DEVNULL,stdout=sys.stdout.buffer,stderr=sys.stderr.buffer)
reason=None
while p.poll() is None:
 memory={line.split(":")[0]:int(line.split()[1]) for line in pathlib.Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemAvailable:")}
 if memory["MemAvailable"]<768*1024:reason="low_memory"
 if time.monotonic()-start>1800:reason="export_deadline"
 if stop:reason="caller_cancelled"
 if reason:
  cancel()
  try:p.wait(timeout=30)
  except subprocess.TimeoutExpired:p.terminate()
  print(json.dumps({"export_cancelled":reason}),file=sys.stderr,flush=True)
  sys.exit(75)
 time.sleep(2)
sys.exit(p.returncode)
'''.replace("DATABASE", repr(database)).replace("TAG", repr(tag))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("database", choices=DATABASES)
    p.add_argument("--directory", required=True, type=Path)
    p.add_argument("--host", default="nas")
    p.add_argument("--legacy-url", default="https://nas.tailfb022c.ts.net:9443/api/v1/ready")
    args = p.parse_args()
    password = os.environ.get("NAS_PASSWORD")
    if not password:
        p.error("NAS_PASSWORD must be set in the environment")
    directory = args.directory.resolve()
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    directory.chmod(0o700)
    target = directory / (args.database + ".dump")
    receipt = directory / (args.database + ".json")
    if target.exists() or receipt.exists():
        p.error("completed destination exists; use a new snapshot directory")
    if shutil.disk_usage(directory).free < MIN_FREE:
        p.error("Mac must retain at least 20 GiB free")
    latencies = [health(args.legacy_url) for _ in range(3)]
    baseline = max(latencies)
    print(json.dumps({"phase": "export", "database": args.database,
                      "baseline_seconds": latencies}), flush=True)
    tag = "monica-rust-export-" + uuid.uuid4().hex
    remote = "sudo -S -p '' /usr/bin/python3 -u -c " + shlex.quote(remote_script(args.database, tag))
    partial = directory / (args.database + ".partial")
    error_path = directory / (args.database + ".stderr")
    started = time.time()
    hasher = hashlib.sha256()
    size = 0
    next_health = time.monotonic() + 30
    last_output = time.monotonic()
    health_failures = 0
    with partial.open("wb") as output, error_path.open("wb") as errors:
        partial.chmod(0o600)
        error_path.chmod(0o600)
        process = subprocess.Popen([*ssh_args(args.host),remote], stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=errors)
        process.stdin.write((password + "\n").encode())
        process.stdin.close()
        try:
            while True:
                if shutil.disk_usage(directory).free < MIN_FREE:
                    raise RuntimeError("Mac free-space floor reached")
                if time.monotonic()-last_output>300:
                    raise RuntimeError("export made no transport progress for five minutes")
                if time.monotonic() >= next_health:
                    try:
                        elapsed = health(args.legacy_url)
                        health_failures = health_failures + 1 if elapsed > max(baseline*1.5, baseline+.5) else 0
                    except (OSError, RuntimeError):
                        health_failures += 1
                    if health_failures >= 3:
                        raise RuntimeError("legacy health degraded during export")
                    next_health = time.monotonic() + 30
                    print(json.dumps({"phase": "export", "database": args.database,
                                      "bytes": size}), flush=True)
                ready, _, _ = select.select([process.stdout], [], [], 2)
                if not ready:
                    if process.poll() is not None:
                        break
                    continue
                data = os.read(process.stdout.fileno(), 1024*1024)
                if not data:
                    break
                output.write(data)
                hasher.update(data)
                size += len(data)
                last_output=time.monotonic()
            code = process.wait(timeout=40)
            if code:
                raise RuntimeError("export failed; inspect the private stderr file")
            output.flush()
            os.fsync(output.fileno())
        except BaseException:
            # Docker exec can outlive its disconnected client. Explicitly
            # cancel the uniquely tagged export, never an application worker.
            cancel = remote_script(args.database, tag).split('cmd=[D,')[0] + '\ncancel()\n'
            try:
                subprocess.run([*ssh_args(args.host),
                                "sudo -S -p '' /usr/bin/python3 -c " + shlex.quote(cancel)],
                               input=(password+"\n").encode(), stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=45)
            except (OSError, subprocess.SubprocessError):
                pass  # The independent remote deadline remains the last fence.
            process.terminate()
            try:
                process.wait(timeout=40)
            except subprocess.TimeoutExpired:
                process.kill()
            raise
    if size < 5 or partial.open("rb").read(5) != b"PGDMP":
        raise RuntimeError("archive signature missing")
    subprocess.run([os.environ.get("MONICA_PG_RESTORE", "/opt/homebrew/opt/postgresql@16/bin/pg_restore"),
                    "--list", str(partial)], check=True, stdout=subprocess.DEVNULL)
    partial.replace(target)
    receipt.write_text(json.dumps({"database": args.database, "application_name": tag,
                                  "started_at": started, "finished_at": time.time(),
                                  "bytes": size, "sha256": hasher.hexdigest(),
                                  "legacy_baseline_seconds": latencies}, indent=2)+"\n")
    receipt.chmod(0o600)
    print(json.dumps({"phase": "snapshot_saved", "database": args.database,
                      "bytes": size, "sha256": hasher.hexdigest()}), flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        print(str(exc), file=sys.stderr)
        sys.exit(1)

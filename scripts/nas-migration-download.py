#!/usr/bin/env python3
"""Download only the new, completed Chatlog snapshots, then verify their bytes.

Uses the copy worker's durable per-file checksums. No original Chatlog source or
key file is read. The subsequent offline inspector must run SQLite quick_check.
"""
import argparse
import hashlib
import importlib.util
import json
import os
import select
from pathlib import Path
import shlex
import shutil
import subprocess
import time

spec=importlib.util.spec_from_file_location("export_guard",Path(__file__).with_name("nas-migration-export.py"))
guard=importlib.util.module_from_spec(spec);spec.loader.exec_module(guard)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("--directory",required=True,type=Path)
    p.add_argument("--host",default="nas")
    p.add_argument("--legacy-url",default="https://nas.tailfb022c.ts.net:9443/api/v1/ready")
    a=p.parse_args();root=a.directory.resolve()
    root.mkdir(mode=0o700,parents=True,exist_ok=True)
    marker=root/"copy-policy.json"
    policy={"kind":"isolated-chatlog-snapshot","host":a.host,"source":"/volume1/docker/monica-rs/snapshots/chatlog/db_storage"}
    if marker.exists():
        if json.loads(marker.read_text())!=policy:raise ValueError("destination belongs to another copy")
    elif any(root.iterdir()):raise ValueError("initial download destination must be empty")
    else:marker.write_text(json.dumps(policy));marker.chmod(0o600)
    if shutil.disk_usage(root).free<guard.MIN_FREE:raise ValueError("Mac must retain at least 20 GiB free")
    baseline=max(guard.health(a.legacy_url) for _ in range(3))
    password=os.environ.get('NAS_PASSWORD')
    if not password:p.error('NAS_PASSWORD is required for restricted reading of the new archive')
    remote='''import json,pathlib,sqlite3
root=pathlib.Path('/volume1/docker/monica-rs/migration')
status=root/'copy-status-chatlog-db.json'
if not status.exists():status=root/'copy-status.json'  # Original copy worker compatibility; source identity is still checked.
state=json.loads(status.read_text())
if state.get('source')!='chatlog-db' or state.get('state')!='complete':raise RuntimeError('Chatlog copy must complete before download')
db=sqlite3.connect((root/'chatlog-db.sqlite').as_uri()+'?mode=ro',uri=True)
files=[dict(zip(['path','sha256','bytes','mode'],r)) for r in db.execute('SELECT path,sha256,bytes,mode FROM files ORDER BY path')]
print(json.dumps({'status':state,'files':files}))
'''
    receipt=subprocess.run([*guard.ssh_args(a.host),"sudo -S -p '' /usr/bin/python3 -c "+shlex.quote(remote)],input=password+"\n",capture_output=True,text=True,timeout=45,check=True)
    manifest=json.loads(receipt.stdout)
    for f in manifest["files"]:
        path=Path(f["path"])
        if path.is_absolute() or any(part in {"..","."} for part in path.parts) or path.suffix!=".db":raise ValueError("unexpected snapshot manifest path")
    saved_receipt=root/"source-receipt.json"
    if saved_receipt.exists() and json.loads(saved_receipt.read_text())["files"]!=manifest["files"]:
        raise ValueError("source snapshot manifest changed; use a new private download directory")
    saved_receipt.write_text(json.dumps(manifest,indent=2));saved_receipt.chmod(0o600)
    partials=root/"partials";partials.mkdir(exist_ok=True)
    log=root/"download.log"
    next_health=time.monotonic()+30;failures=0
    for f in manifest["files"]:
        target=root/"db_storage"/f["path"]
        if target.is_symlink() or not target.resolve().is_relative_to(root/"db_storage"):
            raise ValueError("download destination escapes its private root")
        target.parent.mkdir(parents=True,exist_ok=True)
        def checksum(path):
            h=hashlib.sha256()
            with path.open("rb") as stream:
                for data in iter(lambda:stream.read(1024*1024),b""):h.update(data)
            return h.hexdigest()
        if target.is_file() and target.stat().st_size==f["bytes"] and checksum(target)==f["sha256"]:continue
        partial=partials/(f["sha256"]+".partial")
        if partial.is_symlink():raise ValueError("download partial is a symlink")
        offset=partial.stat().st_size if partial.exists() else 0
        if offset>f["bytes"]:raise ValueError("download partial exceeds its source snapshot")
        stream_program="import os,pathlib,sys\nroot=pathlib.Path('/volume1/docker/monica-rs/snapshots/chatlog/db_storage')\np=root/PATH\nif p.is_symlink() or root not in p.resolve().parents:raise RuntimeError('snapshot path escape')\nwith os.fdopen(os.open(p,os.O_RDONLY|os.O_NOFOLLOW),'rb') as source:\n source.seek(OFFSET)\n for data in iter(lambda:source.read(256*1024),b''):sys.stdout.buffer.write(data)\n".replace("PATH",repr(f["path"])).replace("OFFSET",str(offset))
        # DSM can restrict rsync independently of SSH command access. This
        # transport reads only completed files from the new isolated archive.
        with log.open("ab") as errors,partial.open("ab") as output:
            process=subprocess.Popen([*guard.ssh_args(a.host),"sudo -S -p '' /usr/bin/python3 -u -c "+shlex.quote(stream_program)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=errors)
            process.stdin.write((password+"\n").encode());process.stdin.close()
            started=time.monotonic();last_output=started;received=0
            try:
                while True:
                    if shutil.disk_usage(root).free<guard.MIN_FREE+128*1024**2:raise RuntimeError("Mac free-space floor reached")
                    if time.monotonic()-last_output>300:raise RuntimeError("snapshot download stopped making progress")
                    if time.monotonic()>=next_health:
                        try:elapsed=guard.health(a.legacy_url)
                        except (OSError,RuntimeError):failures+=1
                        else:failures=failures+1 if elapsed>max(baseline*1.5,baseline+.5) else 0
                        if failures>=3:raise RuntimeError("legacy health degraded; pausing only the new download")
                        next_health=time.monotonic()+30
                    ready,_,_=select.select([process.stdout],[],[],2)
                    if not ready:
                        if process.poll() is not None:break
                        continue
                    data=os.read(process.stdout.fileno(),256*1024)
                    if not data:break
                    output.write(data);received+=len(data);last_output=time.monotonic()
                    delay=received/(2*1024**2)-(time.monotonic()-started)
                    if delay>0:time.sleep(min(delay,.5))
                if process.wait(timeout=35):raise RuntimeError("new snapshot download failed; resume its private directory")
                output.flush();os.fsync(output.fileno())
            except BaseException:
                process.terminate();process.wait(timeout=35);raise
        if partial.stat().st_size!=f["bytes"] or checksum(partial)!=f["sha256"]:
            raise ValueError("downloaded snapshot checksum mismatch; retain this partial as evidence")
        partial.replace(target)
        print(json.dumps({"phase":"chatlog_file_verified","bytes":f["bytes"]}),flush=True)
    (root/"byte-verification.json").write_text(json.dumps({"state":"byte_copy_verified","files":len(manifest["files"]),"bytes":sum(f["bytes"] for f in manifest["files"]),"sqlite_integrity":"offline_pending"}))
    print(json.dumps({"phase":"chatlog_download_verified","files":len(manifest["files"])}),flush=True)


if __name__=="__main__":main()

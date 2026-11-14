#!/usr/bin/env python3
"""Run one resource-bounded copy into the isolated Rust archive on the NAS.

Sources are an explicit allowlist. A one-shot Python worker receives only a
read-only source mount, the new data directory and no network or credentials.
Cancellation stops only the uniquely labelled copy container.
"""
import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import time
import uuid

spec = importlib.util.spec_from_file_location("export_guard", Path(__file__).with_name("nas-migration-export.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

DATA_ROOT = "/volume1/docker/monica-rs"
CONFIG_ROOT = "/volume1/homes/maximumh/docker/monica-rs"
SOURCES = {
    "chatlog-db": ("/volume1/homes/maximumh/docker/chatlog/work/db_storage", "chatlog/db_storage", True),
    "media": ("/volume1/docker/monica-next/data/media", "monica-next/media", False),
    "raw": ("/volume1/docker/monica-next/data/raw", "monica-next/raw", False),
    "staging": ("/volume1/docker/monica-next/data/staging", "monica-next/staging", False),
    "exports": ("/volume1/docker/monica-next/data/exports", "monica-next/exports", False),
    "backups": ("/volume1/docker/monica-next/data/backups", "monica-next/backups", False),
    "sns-cache": ("/volume1/homes/maximumh/docker/chatlog/work/.chatlog_sns_cache", "monica-next/sns-cache", False),
}


def remote_program(source, image, name, worker, resume=False):
    payload = {"source": source, "image": image, "name": name,
               "data_root": DATA_ROOT, "config_root": CONFIG_ROOT,
               "worker": base64.b64encode(worker).decode(), "mapping": SOURCES[source],"resume":resume}
    return '''import base64,fcntl,hashlib,json,os,pathlib,signal,subprocess,sys,time
p=PAYLOAD
D="/var/packages/ContainerManager/target/usr/bin/docker"
root=pathlib.Path(p["data_root"])
config=pathlib.Path(p["config_root"])
source,relative,sqlite_only=p["mapping"]
source=pathlib.Path(source).resolve(strict=True)
for path in (root,config):
 if str(path.resolve())!=str(path):raise RuntimeError("new directory has a symlink or alias")
 if path==source or path in source.parents or source in path.parents:raise RuntimeError("source and new data overlap")
destination=root/"snapshots"/relative
journal=root/"migration"/(p["source"]+".sqlite")
for path in [root/"migration",root/"snapshots",destination,config/"migration",journal,root/"migration/partials"]:
 if path.resolve()!=path:raise RuntimeError("new migration path contains a symlink or alias")
root.mkdir(mode=0o700,parents=True,exist_ok=True)
(root/"migration").mkdir(mode=0o700,exist_ok=True)
lock=(root/"migration/copy.lock").open("a")
fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
if destination.exists() and any(destination.iterdir()) and not journal.exists():
 raise RuntimeError("nonempty destination does not belong to this copy")
destination.mkdir(mode=0o700,parents=True,exist_ok=True)
(config/"migration").mkdir(mode=0o700,parents=True,exist_ok=True)
worker=base64.b64decode(p["worker"])
script=config/"migration"/("copy-"+hashlib.sha256(worker).hexdigest()+".py")
if script.is_symlink():raise RuntimeError("worker script is a symlink")
if script.exists() and script.read_bytes()!=worker:raise RuntimeError("worker checksum conflict")
script.write_bytes(worker);script.chmod(0o444)
os.chown(root,0,0);root.chmod(0o700)
# Only the container service group can traverse mounted evidence/status roots.
# setgid preserves that group for newly copied files and nested directories.
for path in [root/"snapshots",root/"snapshots"/relative.split("/")[0],destination,root/"migration"]:
 os.chown(path,0,10001);path.chmod(0o2750)
for folder,dirs,_ in os.walk(destination,followlinks=False):
 path=pathlib.Path(folder)
 if any((path/name).is_symlink() for name in dirs):raise RuntimeError("archive directory symlink")
 info=path.stat()
 if (info.st_uid,info.st_gid,info.st_mode&0o7777)!=(0,10001,0o2750):
  os.chown(path,0,10001);path.chmod(0o2750)
for path in [config,config/"migration"]:os.chown(path,1026,100)
for path in [journal,journal.with_name(journal.name+"-wal"),journal.with_name(journal.name+"-shm")]:
 if path.exists():os.chown(path,0,0)
partials=root/"migration/partials"
if partials.exists():
 os.chown(partials,0,10001);partials.chmod(0o2750)
pause=root/"migration/pause"
memory=lambda:next(int(x.split()[1]) for x in pathlib.Path("/proc/meminfo").read_text().splitlines() if x.startswith("MemAvailable:"))
if memory()<768*1024:raise RuntimeError("NAS copy memory floor not met")
if pause.exists():
 if not p["resume"]:raise RuntimeError("copy is paused; recheck health and explicitly resume")
 pause.rename(root/"migration"/("pause-resolved-"+p["name"]))
# Synology's kernel exposes cpusets and shares, but no CFS quotas or pids
# controller. All new work is pinned to one of the four cores, with low shares.
# Some old CAS objects are mode0600 under the old service UID. DAC_READ_SEARCH
# grants reading only. The source mount remains kernel-enforced read-only.
command=[D,"create","--name",p["name"],"--label","monica.migration.copy="+p["name"],"--label","com.docker.compose.project=monica-rs","--network=none","--read-only","--cap-drop=ALL","--cap-add=DAC_READ_SEARCH","--security-opt=no-new-privileges","--memory=128m","--cpuset-cpus=3","--cpu-shares=128","--user=0:0","--tmpfs","/tmp:rw,noexec,nosuid,size=16m","--mount","type=bind,src="+str(source)+",dst=/source,readonly","--mount","type=bind,src="+str(root)+",dst=/target","--mount","type=bind,src="+str(script)+",dst=/worker.py,readonly","--entrypoint=python",p["image"],"-u","/worker.py","--target","/target/snapshots/"+relative,"--journal","/target/migration/"+p["source"]+".sqlite"]
if sqlite_only:command.extend(["--sqlite-only","--defer-sqlite-check"])
created=subprocess.check_output(command,text=True).strip()
details=json.loads(subprocess.check_output([D,"inspect",created]))[0]
mounts={m["Destination"]:m for m in details["Mounts"]}
if mounts["/source"]["RW"] or mounts["/source"]["Source"]!=str(source) or mounts["/target"]["Source"]!=str(root):
 raise RuntimeError("copy mount verification failed")
print(json.dumps({"phase":"copy_start","source":p["source"],"container":created,"image":details["Image"],"source_read_only":True}),flush=True)
stop=False
def cancelled(*_):
 global stop
 stop=True
signal.signal(signal.SIGHUP,cancelled);signal.signal(signal.SIGTERM,cancelled)
process=subprocess.Popen([D,"start","--attach",created],stdin=subprocess.DEVNULL)
started=time.monotonic()
try:
 while process.poll() is None:
  # An independent deadline also fences an orphaned SSH client. No old
  # container, service or transaction is a cancellation target.
  if stop or memory()<768*1024 or time.monotonic()-started>24*3600:
   pause.write_text("copy guard stopped this new worker\\n")
   subprocess.run([D,"stop","--time=10",created],check=True,stdout=subprocess.DEVNULL)
   break
  time.sleep(2)
 code=process.wait(timeout=30)
 state=json.loads(subprocess.check_output([D,"inspect",created]))[0]["State"]
 print(json.dumps({"phase":"copy_exit","source":p["source"],"exit_code":state["ExitCode"],"oom":state["OOMKilled"]}),flush=True)
 if code or state["ExitCode"]:sys.exit(75)
finally:
 # Retain failed-container evidence. Successful, stopped helpers are the
 # only objects removed; source mounts and old containers are untouched.
 if process.poll() is not None and process.returncode==0:
  subprocess.run([D,"rm",created],check=True,stdout=subprocess.DEVNULL)
'''.replace("PAYLOAD", repr(payload))


def stop_program(name):
    return '''import json,pathlib,subprocess
D="/var/packages/ContainerManager/target/usr/bin/docker"
name=NAME
result=subprocess.run([D,"inspect",name],capture_output=True,text=True)
if result.returncode==0:
 item=json.loads(result.stdout)[0]
 if item["Config"]["Labels"].get("monica.migration.copy")!=name:raise RuntimeError("refusing to stop an unrelated container")
 pathlib.Path(ROOT+"/migration/pause").write_text("legacy health guard paused the new copy\\n")
 subprocess.run([D,"stop","--time=10",item["Id"]],check=True,stdout=subprocess.DEVNULL)
'''.replace("NAME", repr(name)).replace("ROOT", repr(DATA_ROOT))


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source",choices=SOURCES)
    parser.add_argument("--image",required=True,help="already cached immutable sha256 image ID with Python")
    parser.add_argument("--directory",required=True,type=Path,help="private local receipt directory")
    parser.add_argument("--host",default="nas")
    parser.add_argument("--resume",action="store_true",help="resume after fresh legacy health and memory checks, preserving the pause receipt")
    parser.add_argument("--legacy-url",default="https://nas.tailfb022c.ts.net:9443/api/v1/ready")
    args=parser.parse_args()
    import re
    if not re.fullmatch(r"sha256:[0-9a-f]{64}",args.image):parser.error("a pinned local image ID is required")
    password=os.environ.get("NAS_PASSWORD")
    if not password:parser.error("NAS_PASSWORD must be set in the environment")
    args.directory.mkdir(mode=0o700,parents=True,exist_ok=True)
    args.directory.chmod(0o700)
    baseline=max(guard.health(args.legacy_url) for _ in range(3))
    name="monica-rs-copy-"+uuid.uuid4().hex
    code=remote_program(args.source,args.image,name,Path(__file__).with_name("migration-file-copy.py").read_bytes(),args.resume)
    remote="sudo -S -p '' /usr/bin/python3 -u -c "+shlex.quote(code)
    log=args.directory/(name+".log")
    with log.open("wb") as output:
        log.chmod(0o600)
        process=subprocess.Popen([*guard.ssh_args(args.host),remote],stdin=subprocess.PIPE,stdout=output,stderr=output)
        process.stdin.write((password+"\n").encode());process.stdin.close()
        failures=0
        try:
            while process.poll() is None:
                # Poll locally so Ctrl-C and health cancellation stay responsive.
                try:elapsed=guard.health(args.legacy_url)
                except (OSError,RuntimeError):failures+=1
                else:failures=failures+1 if elapsed>max(baseline*1.5,baseline+.5) else 0
                if failures>=3:raise RuntimeError("legacy health degraded; only the new copy is being paused")
                for _ in range(30):
                    if process.poll() is not None:break
                    time.sleep(1)
            if process.returncode:raise RuntimeError("copy stopped; inspect its private receipt before resuming")
        except BaseException:
            try:
                if process.poll() is None:
                    subprocess.run([*guard.ssh_args(args.host),"sudo -S -p '' /usr/bin/python3 -c "+shlex.quote(stop_program(name))],input=(password+"\n").encode(),stdout=subprocess.DEVNULL,stderr=output,timeout=45,check=True)
            finally:
                process.terminate()
            raise
    print(json.dumps({"source":args.source,"state":"complete","receipt":log.name}),flush=True)


if __name__=="__main__":main()

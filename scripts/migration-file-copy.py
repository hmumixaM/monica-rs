#!/usr/bin/env python3
"""One-shot bounded copy worker. Production mounts /source read-only and /target independently.

No connection, credential import, collector or deletion of source files is provided.
The local SQLite journal and status document belong solely to the new instance.
"""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import time

EXCLUDED={"chatlog.json",".env","cookies.json","storage_state.json","browser-state.json"}


def fingerprint(s):
    return [s.st_dev,s.st_ino,s.st_size,s.st_mtime_ns,s.st_ctime_ns]


def sha256(path,pace=None):
    h=hashlib.sha256()
    with path.open("rb") as stream:
        for part in iter(lambda:stream.read(1024*1024),b""):
            h.update(part)
            if pace:pace(len(part))
    return h.hexdigest()


class Copier:
    def __init__(self,source,target,journal,rate=2*1024**2,guard=None,verify_sqlite=True):
        self.source=source.resolve(strict=True)
        self.target=target.resolve()
        if self.source==self.target or self.source in self.target.parents or self.target in self.source.parents:
            raise ValueError("source and destination must be disjoint")
        journal=Path(journal)
        self.partial_root=journal.resolve().parent/"partials"
        for path in [journal,journal.with_name(journal.name+"-wal"),journal.with_name(journal.name+"-shm"),self.partial_root]:
            if path.is_symlink() or self.source==path.resolve() or self.source in path.resolve().parents:
                raise ValueError("copy state must be outside the read-only source")
        self.target.mkdir(parents=True,exist_ok=True)
        self.partial_root.mkdir(parents=True,exist_ok=True)
        self.journal=sqlite3.connect(journal)
        self.journal.execute("PRAGMA journal_mode=WAL")
        self.journal.executescript("""CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY,source_stamp TEXT NOT NULL,destination_stamp TEXT NOT NULL,sha256 TEXT NOT NULL,bytes INTEGER NOT NULL,mode TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS policy(name TEXT PRIMARY KEY,value TEXT NOT NULL);""")
        policy={"source":str(self.source),"target":str(self.target)}
        old=dict(self.journal.execute("SELECT name,value FROM policy"))
        if old and old!=policy:raise ValueError("copy journal belongs to another source/destination")
        self.journal.executemany("INSERT OR IGNORE INTO policy VALUES(?,?)",policy.items());self.journal.commit()
        self.rate=rate;self.guard=guard or (lambda:None);self.started=time.monotonic();self.bytes=0
        self.copied=0;self.skipped=0;self.excluded=0
        self.current_file=None;self.backup_status=None
        self.verify_sqlite=verify_sqlite
        self.total_files,self.total_bytes=self.journal.execute("SELECT count(*),coalesce(sum(bytes),0) FROM files").fetchone()

    def pace(self,count):
        self.bytes+=count
        delay=self.bytes/self.rate-(time.monotonic()-self.started)
        while delay>0:
            time.sleep(min(delay,.5));self.guard()
            delay=self.bytes/self.rate-(time.monotonic()-self.started)
        self.guard()

    def copy_file(self,path,sqlite_backup=False):
        relative=path.relative_to(self.source)
        self.current_file=hashlib.sha256(str(relative).encode()).hexdigest()
        target=self.target/relative
        if path.name in EXCLUDED:
            self.excluded+=1;return
        def source_fingerprint():
            main=fingerprint(path.lstat())
            wal=path.with_name(path.name+"-wal")
            return [main,fingerprint(wal.lstat()) if wal.exists() else None] if sqlite_backup else main
        source_stamp=source_fingerprint()
        if not stat.S_ISREG(path.lstat().st_mode):
            raise ValueError("non-regular source requires an explicit migration mapping")
        if target.is_symlink() or any(p.is_symlink() for p in target.parents if p!=self.target.parent):
            raise ValueError("destination path contains a symlink")
        saved=self.journal.execute("SELECT source_stamp,destination_stamp,sha256,mode,bytes FROM files WHERE path=?",(str(relative),)).fetchone()
        mode="sqlite" if sqlite_backup else "file"
        # A journal commit can outlive a not-yet-flushed destination on power
        # loss. Always verify resumed bytes, even when inode metadata matches.
        if saved and saved[0]==json.dumps(source_stamp) and saved[3]==mode and target.is_file() and saved[1]==json.dumps(fingerprint(target.stat())) and target.stat().st_gid==target.parent.stat().st_gid and sha256(target,self.pace)==saved[2]:
            self.skipped+=1;return
        target.parent.mkdir(parents=True,exist_ok=True)
        temporary=self.partial_root/(hashlib.sha256((str(self.source)+":"+str(relative)).encode()).hexdigest()+".partial")
        for attempt in range(3):
            self.guard()
            before=source_fingerprint()
            # Recreate our incomplete file so resume inherits the current setgid
            # directory, rather than publishing an old root-group inode.
            if temporary.is_symlink():raise ValueError("copy partial is a symlink")
            temporary.unlink(missing_ok=True)
            if sqlite_backup:
                with closing(sqlite3.connect(path.as_uri()+"?mode=ro",uri=True,timeout=.25)) as source:
                    source.execute("PRAGMA query_only=on")
                    # Pin an existing WAL read snapshot. Incremental backup can
                    # otherwise restart on every live writer's commit. WAL
                    # writers remain free to append while we read this snapshot.
                    if source.execute("PRAGMA journal_mode").fetchone()[0]=="wal":
                        source.execute("BEGIN")
                        source.execute("SELECT rootpage FROM sqlite_master LIMIT 1").fetchone()
                    with closing(sqlite3.connect(temporary)) as destination:
                        size=source.execute("PRAGMA page_size").fetchone()[0]
                        last=[0]
                        last_progress=[time.monotonic()]
                        started=time.monotonic()
                        def progress(status,remaining,total):
                            self.backup_status=status
                            copied=total-remaining
                            if copied!=last[0]:last_progress[0]=time.monotonic()
                            if time.monotonic()-started>900:raise RuntimeError("SQLite snapshot deadline reached")
                            if time.monotonic()-last_progress[0]>60:raise RuntimeError("SQLite backup stopped making progress (status="+str(status)+")")
                            self.pace((copied-last[0] if copied>=last[0] else copied)*size);last[0]=copied
                        source.backup(destination,pages=256,progress=progress,sleep=.1)
                        source.rollback()  # Release the live WAL snapshot before local validation.
                        def check_progress():
                            self.guard();return 0
                        if self.verify_sqlite:
                            destination.set_progress_handler(check_progress,10000)
                            result=destination.execute("PRAGMA quick_check").fetchall()
                            if result!=[("ok",)]:raise ValueError("copied SQLite snapshot failed quick_check")
                # The backup API supplies consistency across changing DB/WAL;
                # its hash describes this independent snapshot, not the live file.
                expected=sha256(temporary)
            else:
                h=hashlib.sha256()
                source_fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
                try:
                    with os.fdopen(source_fd,"rb") as source,temporary.open("wb") as destination:
                        for data in iter(lambda:source.read(256*1024),b""):
                            destination.write(data);h.update(data);self.pace(len(data))
                        destination.flush()
                except BaseException:
                    raise
                if before!=source_fingerprint():
                    if attempt==2:raise ValueError("source changed during all three copy attempts")
                    continue
                expected=h.hexdigest()
            if not sqlite_backup and sha256(temporary)!=expected:raise ValueError("destination checksum mismatch")
            temporary.replace(target)
            if not sqlite_backup:
                original=path.stat();os.utime(target,ns=(original.st_atime_ns,original.st_mtime_ns))
            self.journal.execute("INSERT INTO files VALUES(?,?,?,?,?,?) ON CONFLICT(path) DO UPDATE SET source_stamp=excluded.source_stamp,destination_stamp=excluded.destination_stamp,sha256=excluded.sha256,bytes=excluded.bytes,mode=excluded.mode",(str(relative),json.dumps(before),json.dumps(fingerprint(target.stat())),expected,target.stat().st_size,mode))
            self.total_files+=int(saved is None)
            self.total_bytes+=target.stat().st_size-(saved[4] if saved else 0)
            self.copied+=1
            if sqlite_backup or self.copied%50==0:self.journal.commit()
            return

    def run(self,sqlite_only=False):
        for root,directories,files in os.walk(self.source,followlinks=False):
            for directory in directories:
                if (Path(root)/directory).is_symlink():raise ValueError("source directory symlink requires an explicit mapping")
            for name in sorted(files):
                if sqlite_only and not name.endswith(".db"):continue
                self.copy_file(Path(root)/name,sqlite_backup=sqlite_only)
                if self.copied+self.skipped and (self.copied+self.skipped)%50==0:
                    print(json.dumps(self.status()),flush=True)
        self.journal.commit()
        return self.status()

    def status(self):
        return {"files_verified":self.total_files,"bytes_verified":self.total_bytes,"copied_this_pass":self.copied,"unchanged_this_pass":self.skipped,"excluded_credentials":self.excluded,"current_file_token":self.current_file,"sqlite_backup_status":self.backup_status}


def main():
    os.umask(0o027)
    os.nice(19)
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source",type=Path,default=Path("/source"))
    parser.add_argument("--target",type=Path,required=True)
    parser.add_argument("--journal",type=Path,required=True)
    parser.add_argument("--sqlite-only",action="store_true")
    parser.add_argument("--defer-sqlite-check",action="store_true",help="record a consistent backup and checksum; run quick_check on the offline Mac copy before import")
    args=parser.parse_args()
    if args.source.resolve()!=Path("/source") or Path("/target") not in args.target.resolve().parents or Path("/target") not in args.journal.resolve().parents:
        parser.error("production copy requires independent /source:ro and /target mounts")
    if not os.statvfs("/source").f_flag & os.ST_RDONLY:
        parser.error("source mount must be read-only")
    if (os.stat("/source").st_dev,os.stat("/source").st_ino)==(os.stat("/target").st_dev,os.stat("/target").st_ino):
        parser.error("source and target mounts refer to the same directory")
    args.journal.parent.mkdir(parents=True,exist_ok=True)
    last_status=[0.0]
    status_path=Path("/target/migration/copy-status.json")
    def publish(state,error=None):
        document={"state":state,"source":args.journal.stem,"phase":"sqlite_snapshot" if args.sqlite_only else "file_copy","sqlite_integrity":"offline_pending" if args.defer_sqlite_check else "checked","heartbeat_at":time.time(),"bytes_processed_this_pass":worker.bytes,**worker.status()}
        if error:document["error"]=error
        for path in [status_path,status_path.with_name("copy-status-"+args.journal.stem+".json")]:
            temporary=path.with_suffix(".partial");temporary.write_text(json.dumps(document));temporary.replace(path)
    def guard():
        memory=next(int(line.split()[1]) for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemAvailable:"))
        if memory<768*1024:raise RuntimeError("NAS memory floor reached; resume after pressure clears")
        if Path("/target/migration/pause").exists():raise RuntimeError("copy paused by the legacy health guard")
        if time.monotonic()-last_status[0]>5:
            publish("running");last_status[0]=time.monotonic()
    worker=Copier(args.source,args.target,args.journal,guard=guard,verify_sqlite=not args.defer_sqlite_check)
    try:
        print(json.dumps({"state":"complete",**worker.run(args.sqlite_only)}),flush=True)
        publish("complete")
    except BaseException as exc:
        publish("paused",type(exc).__name__)
        raise
    finally:worker.journal.close()


if __name__=="__main__":
    main()

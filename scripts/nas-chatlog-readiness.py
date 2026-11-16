#!/usr/bin/env python3
"""Initialize SQLite reader coordination for existing plaintext NAS exports.

Connections always use mode=ro and query_only. SQLite may update its SHM reader
coordination file; no database contents, WAL transactions or source settings are
changed. The import worker keeps its entire source mount read-only.
"""
import argparse
from contextlib import closing
import fcntl
import json
import os
from pathlib import Path
import signal
import sqlite3
import tempfile
import time

ROOT=Path('/volumeUSB1/usbshare/monica-rs')
SOURCE=Path('/volume1/homes/maximumh/docker/chatlog/work/db_storage')
STOP=False

def signature(path):
    result=[]
    for candidate in(path,Path(str(path)+'-wal'),Path(str(path)+'-shm'),Path(str(path)+'-journal')):
        if candidate.is_symlink():raise ValueError('source sidecar alias')
        s=candidate.stat()if candidate.exists()else None
        result.append([s.st_dev,s.st_ino,s.st_size,s.st_mtime_ns]if s else None)
    return result

def probe(path,timeout=2):
    before=signature(path)
    started=time.monotonic()
    with closing(sqlite3.connect(path.as_uri()+'?mode=ro',uri=True,timeout=timeout))as connection:
        connection.execute('PRAGMA query_only=ON')
        version=connection.execute('PRAGMA schema_version').fetchone()[0]
    after=signature(path)
    return dict(source_signature=after,schema_version=version,seconds=round(time.monotonic()-started,3),main_file_unchanged=before[0]==after[0],source_stable=all(before[i]==after[i]for i in(0,1,3)))

def atomic(path,value):
    fd,name=tempfile.mkstemp(prefix='.readiness-',dir=path.parent)
    try:
        with os.fdopen(fd,'wb')as output:
            output.write(json.dumps(value,sort_keys=True).encode());output.flush();os.fsync(output.fileno())
        os.replace(name,path)
        directory=os.open(path.parent,os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(directory)
        finally:os.close(directory)
    finally:
        try:os.unlink(name)
        except FileNotFoundError:pass

def main():
    global STOP
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--watch',action='store_true');args=parser.parse_args()
    os.umask(0o077)
    assert os.geteuid()==0 and ROOT.resolve()==ROOT and ROOT.stat().st_dev==2081
    assert(ROOT/'data/.monica-rs-ssd-target').read_bytes()==b'monica-rs-ssd-target-v1\n'
    assert SOURCE.resolve()==SOURCE and SOURCE.is_dir()
    state=ROOT/'state/chatlog-sqlite-readiness';state.mkdir(mode=0o700,exist_ok=True);assert state.resolve()==state
    def stop(*_):
        global STOP
        STOP=True
    signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
    with(state/'control.lock').open('a')as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        known={};results={}
        while not STOP:
            paths=sorted(set([*SOURCE.glob('contact/contact.db'),*SOURCE.glob('sns/sns.db'),*SOURCE.glob('message/message_[0-9]*.db'),*SOURCE.glob('message/biz_message_[0-9]*.db')]))
            assert paths and len(paths)<=1024
            for path in paths:
                if STOP:break
                relative=path.relative_to(SOURCE).as_posix()
                try:
                    assert path.resolve()==path and path.is_file()
                    current=signature(path)
                    if known.get(relative)!=current:
                        result=probe(path);verified=result.pop('source_signature');results[relative]=dict(result,state='readable'if result['source_stable']else'changed_during_probe',at=time.time())
                        if result['source_stable']:known[relative]=verified
                        else:known.pop(relative,None)
                except Exception as error:
                    known.pop(relative,None);results[relative]=dict(state='retry_pending',error_type=type(error).__name__,at=time.time())
                atomic(state/'status.json',dict(at=time.time(),pid=os.getpid(),files=results,source_database_writes=False))
            if not args.watch:break
            for _ in range(60):
                if STOP:break
                time.sleep(1)

if __name__=='__main__':main()

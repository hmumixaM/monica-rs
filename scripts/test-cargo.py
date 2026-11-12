#!/usr/bin/env python3
"""Run selected Cargo tests against a fresh socket-only synthetic PostgreSQL16."""
import os,subprocess,sys,tempfile
from pathlib import Path
from test_diagnostics import preserve_failure_logs
root=Path(__file__).resolve().parents[1]
pg=Path(os.environ.get('MONICA_PG_BIN','/opt/homebrew/opt/postgresql@16/bin'))
code=1
with tempfile.TemporaryDirectory(prefix='monica-rs-cargo-',dir='/tmp') as temporary:
    path=Path(temporary)
    try:
        with (path/'postgres.log').open('w') as log:
            subprocess.run([str(pg/'initdb'),'-D',str(path/'db'),'--auth=trust','--no-locale','--encoding=UTF8'],check=True,stdout=log,stderr=log)
            subprocess.run([str(pg/'pg_ctl'),'-D',str(path/'db'),'-l',str(path/'postgres.log'),'-o',f"-c listen_addresses='' -k {path} -c wal_level=logical -c max_replication_slots=30 -c max_connections=100 -c shared_preload_libraries=pg_stat_statements",'-w','start'],check=True,stdout=log,stderr=log)
        environment=dict(os.environ,DATABASE_URL=f'postgresql:///postgres?host={path}')
        code=subprocess.run(['cargo','test',*sys.argv[1:]],cwd=root,env=environment).returncode
        if code:preserve_failure_logs(path,'cargo')
    except BaseException:
        preserve_failure_logs(path,'cargo')
        raise
    finally:
        failing=sys.exc_info()[0] is not None
        if (path/'db').exists():
            try:
                cleanup=subprocess.run([str(pg/'pg_ctl'),'-D',str(path/'db'),'-m','immediate','stop'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                if cleanup.returncode:
                    preserve_failure_logs(path,'cargo-cleanup')
                    if not failing and code==0:code=cleanup.returncode
            except OSError:
                if not failing and code==0:raise
raise SystemExit(code)

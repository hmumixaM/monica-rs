#!/usr/bin/env python3
"""Real-browser smoke against a brand-new disposable PG cluster; no old config/data."""
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.request
import sys
from test_diagnostics import preserve_failure_logs
ROOT=Path(__file__).resolve().parents[1]
PG=Path(os.environ.get('MONICA_PG_BIN','/opt/homebrew/opt/postgresql@16/bin'))
def run(*args,**kwargs):return subprocess.run([str(a) for a in args],check=True,**kwargs)
with tempfile.TemporaryDirectory(prefix='monica-rust-browser-') as temporary:
    root=Path(temporary);db=root/'postgres';server=None
    try:
        with (root/'postgres.log').open('w') as log:
            run(PG/'initdb','-D',db,'--auth=trust','--no-locale','--encoding=UTF8',stdout=log,stderr=log)
            run(PG/'pg_ctl','-D',db,'-l',root/'postgres.log','-o',f"-c listen_addresses='' -k {root}",'-w','start',stdout=log,stderr=log)
        with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
        origin=f'http://127.0.0.1:{port}'
        env={**os.environ,'MONICA_DATABASE_URL':f'postgresql:///postgres?host={root}','MONICA_SESSION_SECRET':secrets.token_hex(32),'MONICA_CONTROL_TOKEN':secrets.token_hex(32),'MONICA_ORIGIN':origin,'MONICA_MEDIA_ROOT':str(root/'media'),'MONICA_WEB_ROOT':str(ROOT/'apps/web/dist'),'MONICA_AI_ENABLED':'false','MONICA_ADAPTERS_ENABLED':'false'}
        # Configuration acceptance must never dispatch background model/source work.
        env['MONICA_START_PAUSED']='true'
        binary=ROOT/'target/debug/monica-server'
        run(binary,'migrate',env=env)
        with (root/'server.log').open('w') as log:
            server=subprocess.Popen([str(binary),'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
            for _ in range(100):
                if server.poll() is not None:raise RuntimeError('Isolated Rust server exited before ready')
                try:
                    with urllib.request.urlopen(origin+'/api/v1/ready',timeout=1) as response:
                        if response.status==200:break
                except OSError:time.sleep(.1)
            else:raise RuntimeError('Isolated Rust server was not ready')
            result=subprocess.run(['npm','run','test','--','tests/real-api.spec.ts'],cwd=ROOT/'apps/web',env={**os.environ,'MONICA_WEB_TEST_URL':origin,'MONICA_REAL_API':'1'})
            if result.returncode:
                print('Isolated browser smoke failed; artifacts: .local-validation/frontend-results')
                raise SystemExit(result.returncode)
    except BaseException:
        preserve_failure_logs(root,'frontend')
        raise
    finally:
        failing=sys.exc_info()[0] is not None
        if server and server.poll() is None:
            server.terminate()
            try:server.wait(timeout=120)
            except subprocess.TimeoutExpired:server.kill();server.wait(timeout=5)
        if db.exists():
            try:
                cleanup=subprocess.run([str(PG/'pg_ctl'),'-D',str(db),'-m','fast','-w','stop'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                if cleanup.returncode:
                    preserve_failure_logs(root,'frontend-cleanup')
                    if not failing:raise RuntimeError('Synthetic PostgreSQL cleanup failed')
            except OSError:
                if not failing:raise

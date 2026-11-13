#!/usr/bin/env python3
"""Compare pinned Python CRM/Desktop and release Rust using only fresh synthetic stores.

All contacts are imported through signed HTTP, profiles are created through HTTP,
then source accounts are explicitly linked through the real link API. No business
rows or precomputed results are inserted directly. The only setup SQL enables
source-first mode on the Python baselines and installs pg_stat_statements.
"""
import argparse
import concurrent.futures as futures
import contextlib
import datetime as dt
import hashlib
import hmac
import http.cookiejar
import json
import math
import os
from pathlib import Path
import platform
import re
import signal
import socket
import statistics
import subprocess
import tempfile
import threading
import traceback
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import psutil
from reference_backend import PG, ReferenceBackend, BASELINES

ROOT = Path(__file__).resolve().parents[1]
SECRET = 'synthetic-reference-import-secret-32bytes'
MARK = '/* monica_benchmark_probe */ '


def sql(backend, query):
    result = subprocess.run([str(PG/'psql'), backend.database_url, '-X', '-v', 'ON_ERROR_STOP=1', '-Atc', MARK+query], check=True, capture_output=True, text=True)
    return result.stdout.strip()


def sql_json(backend, query):
    return json.loads(sql(backend, query))


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def ready(process, url, log):
    for _ in range(400):
        if process.poll() is not None:
            raise RuntimeError(Path(log).read_text())
        try:
            urllib.request.urlopen(url+'/api/v1/ready', timeout=1).close()
            return
        except OSError:
            time.sleep(.05)
    raise TimeoutError('isolated backend startup')


class RustBackend:
    def __init__(self, migration_timeout_seconds=1800):
        self.migration_timeout_seconds = migration_timeout_seconds

    def __enter__(self):
        self.temp = tempfile.TemporaryDirectory(prefix='monica-benchmark-rust-', dir='/tmp')
        self.root = Path(self.temp.name)
        self.pg_started = False
        self.process = None
        try:
            subprocess.run([str(PG/'initdb'), '-D', str(self.root/'pg'), '--auth=trust', '--no-locale', '--encoding=UTF8'], check=True, stdout=subprocess.DEVNULL)
            subprocess.run([str(PG/'pg_ctl'), '-D', str(self.root/'pg'), '-l', str(self.root/'pg.log'), '-o', f"-c listen_addresses='' -k {self.root} -c max_connections=100 -c shared_preload_libraries=pg_stat_statements -c track_io_timing=on -c track_wal_io_timing=on", '-w', 'start'], check=True, stdout=subprocess.DEVNULL)
            self.pg_started = True
            self.database_url = f'postgresql:///postgres?host={self.root}'
            self.environment = {'PATH': os.defpath, 'MONICA_DATABASE_URL': self.database_url,
                'MONICA_MIGRATION_STATEMENT_TIMEOUT_SECONDS': str(self.migration_timeout_seconds),
                'MONICA_SESSION_SECRET': 'synthetic-reference-session-secret-32bytes',
                'MONICA_IMPORT_HMAC_SECRET': SECRET, 'MONICA_MEDIA_ROOT': str(self.root/'media'),
                'RUST_LOG':'info,sqlx::pool::acquire=debug'}
            self.binary = str(ROOT/'target/release/monica-server')
            subprocess.run([self.binary, 'migrate'], env=self.environment, check=True)
            sql(self, 'CREATE EXTENSION pg_stat_statements')
            self.url = f'http://127.0.0.1:{port()}'
            self.log = (self.root/'http.log').open('wb')
            self.process = subprocess.Popen([self.binary, 'serve', '--listen', urllib.parse.urlsplit(self.url).netloc], env=self.environment, stdout=self.log, stderr=self.log)
            ready(self.process, self.url, self.root/'http.log')
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def __exit__(self, *_):
        if self.process is not None and self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            try: self.process.wait(timeout=100)
            except subprocess.TimeoutExpired: self.process.kill(); self.process.wait(timeout=5)
        if hasattr(self, 'log'): self.log.close()
        if self.pg_started:
            subprocess.run([str(PG/'pg_ctl'), '-D', str(self.root/'pg'), '-m', 'immediate', '-w', 'stop'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.temp.cleanup()


class RequestFailure(RuntimeError):
    def __init__(self, path, status, body):
        self.status = status
        super().__init__(f'{path} returned {status}: {body[:800]!r}')


class Client:
    def __init__(self, base):
        self.base = base
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.csrf = ''
        self.cookie = None

    def request(self, path, body=None, method=None, signed=False):
        headers = {'Origin': self.base, 'X-CSRF-Token': self.csrf}
        if self.cookie is not None: headers['Cookie'] = self.cookie
        raw = None if body is None else json.dumps(body, separators=(',', ':'), ensure_ascii=False).encode()
        if raw is not None: headers['Content-Type'] = 'application/json'
        if signed:
            stamp, nonce = str(int(time.time())), str(uuid.uuid4())
            signature = hmac.new(SECRET.encode(), stamp.encode()+b'.'+nonce.encode()+b'.'+raw, hashlib.sha256).hexdigest()
            headers.update({'X-Import-Timestamp':stamp,'X-Import-Nonce':nonce,'X-Import-Signature':signature})
        request = urllib.request.Request(self.base+path, data=raw, method=method, headers=headers)
        # Each worker has its own connection/opener; immutable cookies are shared.
        opener = self.opener if self.cookie is None else urllib.request.build_opener()
        try:
            response = opener.open(request, timeout=120)
        except urllib.error.HTTPError as error:
            with error:
                body = error.read(801)
            raise RequestFailure(path, error.code, body) from error
        with response:
            data = response.read()
            return json.loads(data) if data else None

    def authenticate(self):
        credentials = {'email':'benchmark@example.invalid','password':'synthetic-benchmark-password'}
        self.request('/api/v1/auth/bootstrap', credentials)
        self.csrf = self.request('/api/v1/auth/csrf')['csrf_token']
        self.request('/api/v1/auth/login', credentials)
        self.cookie = '; '.join(cookie.name+'='+cookie.value for cookie in self.jar)


def batch(stream, records, key):
    return {'source':'wechat','stream':stream,'schema_version':1,'batch_id':key,'idempotency_key':key,
            'observed_at':'2026-11-01T00:00:00Z','records':records}


def settle(backend, rust, expected_messages=None, timeout=240):
    deadline = time.monotonic()+timeout
    while True:
        if rust:
            state = sql_json(backend, "SELECT json_build_object('messages',(SELECT count(*) FROM crm.message_search),'pending',(SELECT count(*) FROM crm.import_batches WHERE status<>'complete')+(SELECT count(*) FROM crm.jobs)+(SELECT count(*) FROM crm.conversation_work),'failures',(SELECT count(*) FROM crm.scheduler_failures WHERE quarantined))")
        else:
            state = sql_json(backend, "SELECT json_build_object('messages',(SELECT count(*) FROM message_events),'pending',(SELECT count(*) FROM import_batches WHERE status NOT IN ('complete','completed')),'failures',0)")
        if state['failures']: raise RuntimeError(f'import quarantined: {state}')
        if not state['pending'] and (expected_messages is None or state['messages']==expected_messages): return state
        if time.monotonic()>deadline: raise TimeoutError(f'import did not settle: {state}')
        time.sleep(.05)


def setup(backend, client, contacts, rust):
    if not rust:
        sql(backend, "INSERT INTO platform_migration_state(provider,enabled) VALUES('wechat',true) ON CONFLICT(provider) DO UPDATE SET enabled=true")
    records = [{'external_id':f'bench-contact-{i:06}', 'entity_type':'profile',
                'source_account_external_id':f'bench-contact-{i:06}',
                'content':{'fields':{'wechat.nickname':f'Synthetic Contact {i:06}'}}} for i in range(contacts)]
    for start in range(0, contacts, 100):
        client.request('/api/v1/imports/batches', batch('contacts',records[start:start+100],f'contacts-{start}'),signed=True)
    settle(backend,rust)
    accounts = {}
    cursor = ''
    while True:
        rows = client.request('/api/v1/source-accounts?provider=wechat'+('&cursor='+cursor if cursor else ''))
        accounts.update((row['external_id'], row) for row in rows)
        if len(rows)<50: break
        cursor = rows[-1]['id']
    assert len(accounts)==contacts, (len(accounts),contacts)
    def link(i):
        external = f'bench-contact-{i:06}'
        profile = client.request('/api/v1/profiles',{'display_name':f'Synthetic Contact {i:06}','profile_type':'person'})
        client.request('/api/v1/source-accounts/'+accounts[external]['id']+'/link',{'profile_id':profile['id'],'expected_profile_id':None},method='PUT')
        return external,profile['id']
    with futures.ThreadPoolExecutor(max_workers=8) as executor:
        profiles = dict(executor.map(link,range(contacts)))
    prefix = 'crm.' if rust else ''
    actual = sql_json(backend, f"SELECT json_build_object('profiles',(SELECT count(*) FROM {prefix}profiles WHERE archived_at IS NULL),'accounts',(SELECT count(*) FROM {prefix}source_accounts),'links',(SELECT count(*) FROM {prefix}source_account_links))")
    assert actual=={'profiles':contacts,'accounts':contacts,'links':contacts},actual
    return profiles


def messages(profiles, offset, size):
    externals = sorted(profiles)
    stamp = dt.datetime(2026,11,1,tzinfo=dt.timezone.utc)
    rows=[]
    for n in range(offset,offset+size):
        who=externals[n%len(externals)]
        occurred=stamp+dt.timedelta(seconds=n)
        rows.append({'external_id':f'message-{n:09}','entity_type':'message','source_account_external_id':who,
            'profile_id':profiles[who],'sender_profile_id':profiles[who],'conversation_external_id':who,
            'conversation_type':'direct','message_type':'text','occurred_at':occurred.isoformat(),
            'content':{'account':'benchmark-owner','conversation':who,'conversation_external_id':who,'sender':who,
                'sender_external_id':who,'sent_at_ms':int(occurred.timestamp()*1000),'text':f'Synthetic message {n:09} topic-{n%17}'}})
    return rows


def database_metrics(backend):
    prefix='crm.' if getattr(backend,'benchmark_rust',False) else ''
    queue_sql=f"SELECT json_build_object('pending_imports',(SELECT count(*) FROM {prefix}import_batches WHERE status NOT IN ('complete','completed')))"
    result=sql_json(backend,"""SELECT json_build_object(
      'wal',pg_current_wal_insert_lsn()::text,
      'calls',(SELECT coalesce(sum(calls),0) FROM pg_stat_statements WHERE query NOT LIKE '%monica_benchmark_probe%'),
      'sql_exec_ms',(SELECT coalesce(sum(total_exec_time),0) FROM pg_stat_statements WHERE query NOT LIKE '%monica_benchmark_probe%'),
      'transactions',(SELECT sum(xact_commit+xact_rollback) FROM pg_stat_database WHERE datname NOT IN ('template0','template1')),
      'db_bytes',(SELECT sum(pg_database_size(oid)) FROM pg_database WHERE datname NOT IN ('template0','template1')),
      'database_io',(SELECT json_build_object('read_bytes',sum(coalesce(reads,0)*op_bytes),'write_bytes',sum(coalesce(writes,0)*op_bytes),'read_ms',sum(coalesce(read_time,0)),'write_ms',sum(coalesce(write_time,0)),'fsync_ms',sum(coalesce(fsync_time,0))) FROM pg_stat_io),
      'wal_io',(SELECT json_build_object('write_ms',wal_write_time,'sync_ms',wal_sync_time,'writes',wal_write,'syncs',wal_sync) FROM pg_stat_wal),
      'wait_lock',(SELECT count(*) FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND wait_event_type='Lock'),
      'lock_wait_query_age_ms',(SELECT coalesce(json_agg(extract(epoch FROM(clock_timestamp()-query_start))*1000),'[]'::json) FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND wait_event_type='Lock'),
      'wait_io',(SELECT count(*) FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND wait_event_type='IO'),
      'longest_xact_ms',(SELECT coalesce(max(extract(epoch FROM(clock_timestamp()-xact_start))*1000),0) FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND state<>'idle'))""")
    result.update(sql_json(backend,queue_sql))
    return result



class Monitor:
    def __init__(self,backend):
        self.backend=backend;self.stop=threading.Event();self.samples=[];self.errors=[]
    def __enter__(self):
        self.process=psutil.Process(self.backend.process.pid)
        self.cpu_start=sum(self.process.cpu_times()[:2])
        self.host_cpu_start=psutil.cpu_times()
        try:self.io_start=self.process.io_counters()._asdict()
        except (AttributeError,psutil.Error):self.io_start=None
        def sample():
            while not self.stop.is_set():
                try:
                    db=database_metrics(self.backend)
                    db['rss_bytes']=self.process.memory_info().rss
                    db['host_available_memory_bytes']=psutil.virtual_memory().available
                    db['monotonic']=time.monotonic()
                    self.samples.append(db)
                except (OSError,RuntimeError,psutil.Error,subprocess.CalledProcessError) as error:self.errors.append(type(error).__name__)
                self.stop.wait(.2)
        self.thread=threading.Thread(target=sample,daemon=True);self.thread.start();return self
    def __exit__(self,*_):
        self.stop.set();self.thread.join(timeout=10)
        self.cpu_seconds=sum(self.process.cpu_times()[:2])-self.cpu_start
        self.host_cpu_end=psutil.cpu_times()
        try:self.io_end=self.process.io_counters()._asdict()
        except (AttributeError,psutil.Error):self.io_end=None
    def result(self):
        return {'samples':len(self.samples),'sample_errors':self.errors,'server_cpu_seconds':self.cpu_seconds,
            'server_peak_rss_bytes':max((s['rss_bytes'] for s in self.samples),default=None),
            'host_cpu_busy_fraction':1-(self.host_cpu_end.idle-self.host_cpu_start.idle)/max(.001,sum(self.host_cpu_end)-sum(self.host_cpu_start)),
            'host_min_available_memory_bytes':min((s['host_available_memory_bytes'] for s in self.samples),default=None),
            'max_observed_pending_imports':max((s['pending_imports'] for s in self.samples),default=None),
            'lock_wait_samples':sum(s['wait_lock']>0 for s in self.samples),'max_lock_waiters':max((s['wait_lock'] for s in self.samples),default=None),
            'io_wait_samples':sum(s['wait_io']>0 for s in self.samples),
            'sampled_lock_blocked_query_age':quantiles([age for s in self.samples for age in s.get('lock_wait_query_age_ms',[])]) if any(s.get('lock_wait_query_age_ms') for s in self.samples) else None,
            'longest_observed_transaction_ms':max((s['longest_xact_ms'] for s in self.samples),default=None),
            'process_io_delta':{k:self.io_end[k]-v for k,v in self.io_start.items()} if self.io_start and self.io_end else None,
            'raw_samples':self.samples}


def quantiles(samples):
    values=sorted(samples)
    if not values:
        return {'samples':0,'p50_ms':None,'p95_ms':None,'p99_ms':None}
    return {'samples':len(values),'p50_ms':statistics.median(values),'p95_ms':values[max(0,math.ceil(len(values)*.95)-1)],'p99_ms':values[max(0,math.ceil(len(values)*.99)-1)]}


def measured_read(client, name, path):
    start=time.perf_counter();error=None
    try:
        if client.request(path) is None:
            raise ValueError('empty JSON response')
    except RequestFailure as failure:
        error={'status':str(failure.status),'detail':str(failure)}
    except (OSError, ValueError) as failure:
        error={'status':type(failure).__name__,'detail':str(failure)}
    return name,(time.perf_counter()-start)*1000,error


def read_summary(rows, paths):
    by_route={}
    for name in paths:
        selected=[row for row in rows if row[0]==name]
        failed=[row for row in selected if row[2] is not None]
        counts={}
        for _,_,error in failed:
            counts[error['status']]=counts.get(error['status'],0)+1
        by_route[name]={**quantiles([ms for _,ms,error in selected if error is None]),
            'requests':len(selected),'failures':len(failed),
            'error_rate':len(failed)/len(selected) if selected else None,
            'errors_by_status':counts,
            'failure_latency':quantiles([ms for _,ms,_ in failed]),
            'first_error':failed[0][2] if failed else None}
    failures=sum(row[2] is not None for row in rows)
    return {'requests':len(rows),'failures':failures,
        'error_rate':failures/len(rows) if rows else None,'by_route':by_route}


def page_load(client,paths,samples,concurrency):
    tasks=[item for _ in range(samples) for item in paths.items()]
    begin=time.perf_counter()
    with futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
        rows=list(executor.map(lambda item:measured_read(client,*item),tasks))
    return {'elapsed_seconds':time.perf_counter()-begin,**read_summary(rows,paths)}


def mixed_pages(client,paths,samples,concurrency,import_done):
    """Keep readers active through import; measure failures without retrying them."""
    routes=list(paths.items());minimum=math.ceil(samples*len(routes)/concurrency)
    begin=time.perf_counter()
    def reader(worker):
        rows=[];index=0
        while index<minimum or not import_done.is_set():
            if time.perf_counter()-begin>900:raise TimeoutError('mixed import/read phase exceeded 15 minutes')
            name,path=routes[(worker+index)%len(routes)]
            rows.append(measured_read(client,name,path));index+=1
        return rows
    with futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
        rows=[row for group in executor.map(reader,range(concurrency)) for row in group]
    elapsed=time.perf_counter()-begin
    return {'elapsed_seconds':elapsed,'requests_per_second':len(rows)/elapsed,
            **read_summary(rows,paths)}


def ingest(backend,client,profiles,count,rust,offset=0,batch_size=1000):
    before=database_metrics(backend);started=time.perf_counter();accept=[]
    for start in range(offset,offset+count,batch_size):
        begin=time.perf_counter()
        client.request('/api/v1/imports/batches',batch('messages',messages(profiles,start,min(batch_size,offset+count-start)),f'messages-{start}'),signed=True)
        accept.append((time.perf_counter()-begin)*1000)
    accepted=time.perf_counter()-started
    settle(backend,rust,offset+count)
    elapsed=time.perf_counter()-started;after=database_metrics(backend)
    wal=int(sql(backend,f"SELECT pg_wal_lsn_diff('{after['wal']}'::pg_lsn,'{before['wal']}'::pg_lsn)::bigint"))
    return {'messages':count,'accept_seconds':accepted,'settled_seconds':elapsed,'settled_messages_per_second':count/elapsed,
        'batch_http':quantiles(accept),'sql_calls_per_1000':(after['calls']-before['calls'])*1000/count,
        'wal_bytes_per_1000':wal*1000/count,'sql_exec_ms':after['sql_exec_ms']-before['sql_exec_ms'],
        'transaction_count_delta':after['transactions']-before['transactions'],
        'database_io_delta':{key:after['database_io'][key]-value for key,value in before['database_io'].items()},
        'wal_io_delta':{key:after['wal_io'][key]-value for key,value in before['wal_io'].items()},'before':before,'after':after}


def case(backend,kind,args,round_number,scale):
    rust=kind=='rust';backend.benchmark_rust=rust;client=Client(backend.url);client.authenticate()
    contacts=args.contacts*(scale if args.scale_contacts else 1);count=args.messages*scale
    started=time.perf_counter();profiles=setup(backend,client,contacts,rust)
    setup_seconds=time.perf_counter()-started
    paths={'directory':'/api/v1/directory','search':'/api/v1/search?q=Synthetic&limit=30',
           'overview':'/api/v1/profiles/'+profiles[sorted(profiles)[0]]+'/overview',
           'profiles':'/api/v1/profiles?limit=30','conversations':'/api/v1/conversations?limit=40',
           'messages':'/api/v1/profiles/'+profiles[sorted(profiles)[0]]+'/messages?limit=50'}
    log_offset=(backend.root/'http.log').stat().st_size
    with Monitor(backend) as monitor:
        imported=ingest(backend,client,profiles,count,rust,batch_size=args.batch_size)
        # First API use after import, without privileged OS/PG cache eviction.
        cold=page_load(client,paths,1,1)
        warm=page_load(client,paths,args.samples,args.concurrency)
        mixed_started=time.perf_counter()
        with futures.ThreadPoolExecutor(max_workers=1) as executor:
            incoming=executor.submit(ingest,backend,client,profiles,max(args.batch_size,count//10),rust,count,args.batch_size)
            import_done=threading.Event()
            incoming.add_done_callback(lambda _future:import_done.set())
            mixed=mixed_pages(client,paths,args.samples,args.concurrency,import_done)
            mixed_ingest=incoming.result()
        mixed['phase_seconds']=time.perf_counter()-mixed_started
        mixed['import']=mixed_ingest
    acquire_samples=[]
    if rust:
        log=(backend.root/'http.log').read_bytes()[log_offset:].decode(errors='replace')
        log=re.sub(r'\x1b\[[0-9;]*m','',log)
        acquire_samples=[float(value)*1000 for value in re.findall(r'aquired_after_secs=([0-9.eE+-]+)',log)]
    prefix='crm.' if rust else ''
    linked=int(sql(backend,f'SELECT count(*) FROM {prefix}conversations WHERE profile_id IS NOT NULL'))
    assert linked==contacts,(linked,contacts)
    return {'backend':kind,'round':round_number,'scale':scale,'contacts':contacts,'initial_messages':count,
        'setup_seconds':setup_seconds,'ingest':imported,'cold_first_request':cold,'warm':warm,'mixed':mixed,
        'monitor':monitor.result(),'pool_acquire':quantiles(acquire_samples) if acquire_samples else None,
        'pool_acquire_note':'SQLx exact acquisition latency, all pools combined' if rust else 'Python pool acquisition not equivalently instrumented',
        'verified_linked_conversations':linked}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rounds',type=int,default=3);parser.add_argument('--scales',type=int,nargs='+',default=[1,10])
    parser.add_argument('--scale-contacts',action='store_true',help='Also scale directory size; default keeps contacts fixed to isolate history growth')
    parser.add_argument('--contacts',type=int,default=100);parser.add_argument('--messages',type=int,default=10000)
    parser.add_argument('--batch-size',type=int,default=1000);parser.add_argument('--samples',type=int,default=64);parser.add_argument('--concurrency',type=int,default=8)
    parser.add_argument('--backends',nargs='+',choices=['crm','desktop','multisource','rust'],default=['crm','desktop','rust'])
    parser.add_argument('--output',type=Path,default=ROOT/'.autonomous/complete-monica/benchmark-backends.jsonl')
    parser.add_argument('--skip-build',action='store_true')
    parser.add_argument('--migration-timeout-seconds',type=int,default=1800,help='Administrative Rust schema deadline; outside measured API phases')
    args=parser.parse_args()
    assert min(args.rounds,args.contacts,args.messages,args.samples,args.concurrency,args.batch_size,*args.scales)>0
    assert args.batch_size<=10000
    assert 1<=args.migration_timeout_seconds<=86400
    if not args.skip_build:subprocess.run(['cargo','build','--release','--locked','-p','monica-server'],cwd=ROOT,check=True)
    args.output.parent.mkdir(parents=True,exist_ok=True)
    provenance=json.loads((ROOT/'source.json').read_text()) if (ROOT/'source.json').exists() else {'rust_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'rust_dirty':bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT))}
    metadata={'type':'metadata','time':dt.datetime.now(dt.timezone.utc).isoformat(),'host':platform.platform(),'machine':platform.machine(),
        'cpu_count':os.cpu_count(),'memory_bytes':psutil.virtual_memory().total,'baselines':BASELINES,
        'rust_commit':provenance['rust_commit'],
        'rust_dirty':provenance['rust_dirty'],
        'binary_sha256':hashlib.sha256((ROOT/'target/release/monica-server').read_bytes()).hexdigest() if (ROOT/'target/release/monica-server').exists() else None,
        'arguments':{k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        'limitations':['Ordinary coexisting host services are not stopped. Host CPU/memory pressure is recorded; this is not a dedicated machine.',
        'Cold means first endpoint call after imports; OS/PostgreSQL caches are not evicted.',
        'Read failures are counted without retry; latency quantiles cover successful requests only, with failure latency and error rate reported separately.',
        'History grows 10x with a fixed contact set unless --scale-contacts is explicitly supplied.',
        'HTTP client uses fresh connections equally for all backends; latency includes local connection and JSON decode.',
        'Rust accepts durable receipts asynchronously; both acceptance and settled timings are reported.',
        'Python signed API projects per record; Rust uses bounded bulk transactions. Differences include schema/projection algorithms, not just language.',
        'SQL/WAL cover all isolated backend databases; mixed-load SQL/WAL also include concurrent reads.',
        'Transaction count includes monitoring connections and can lag pg_stat_database flush; SQL execution sum is not transaction wall time.',
        'Lock/transaction/RSS samples run every 200 ms plus SQL probe duration, so brief spikes may be missed. Blocked-query age is sampled query age, not exact duration of the lock wait; do not compare it to a true lock-wait p95.',
        'SQLx acquisition logs provide exact Rust pool latency; no corresponding Python measurement is inferred. Unavailable process I/O is null.',
        'Mixed load uses eight readers (or configured concurrency) plus one serial importer; readers run until durable import settlement, with a minimum sample count.']}
    failures=0
    with args.output.open('w') as output:
        output.write(json.dumps(metadata)+'\n');output.flush()
        for round_number in range(1,args.rounds+1):
            for scale in args.scales:
                # Rotate order between rounds to reduce fixed ordering bias.
                order=args.backends[(round_number-1)%len(args.backends):]+args.backends[:(round_number-1)%len(args.backends)]
                for kind in order:
                    print(f'Benchmark {kind} round={round_number} scale={scale}',flush=True)
                    try:
                        with (RustBackend(args.migration_timeout_seconds) if kind=='rust' else ReferenceBackend(kind)) as backend:
                            result=case(backend,kind,args,round_number,scale)
                        result['status']='complete'
                    except Exception as error:
                        failures+=1
                        log=args.output.with_name(f'{args.output.stem}-{kind}-{round_number}-{scale}-failure.log')
                        log.write_text(traceback.format_exc())
                        result={'status':'failed','backend':kind,'round':round_number,'scale':scale,
                            'initial_messages':args.messages*scale,'error_type':type(error).__name__,
                            'error':str(error),'failure_log':str(log)}
                    output.write(json.dumps(result)+'\n');output.flush()
                    print(json.dumps({k:result.get(k) for k in ('status','backend','round','scale','initial_messages','verified_linked_conversations','error')}),flush=True)
    if failures:raise SystemExit(1)

if __name__=='__main__':main()

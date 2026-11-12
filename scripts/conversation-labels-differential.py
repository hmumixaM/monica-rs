#!/usr/bin/env python3
"""Actual c0007f5 label projection and search differential, disposable stores only."""
import importlib.util,json,os,signal,socket,subprocess,time,urllib.request
from pathlib import Path
import reference_backend
ROOT=Path(__file__).resolve().parents[1]
def module(name,path):
 spec=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
journal=module('journal',ROOT/'scripts/connections-differential.py');auth=module('auth',ROOT/'scripts/auth-differential.py')
reference_backend.BASELINES['labels']=('/Users/maxhu/play/monica-multisource','c0007f50f39e4c38f6dd7b0caef9cf64e1f5c75a')
def scenario():
 def op(provider,account,records):return dict(provider=provider,account=account,records=records,cursor={},relink=False,labels=True)
 def record(external,person,at='2026-11-11T12:00:00Z',**kw):return dict(external_id=external,thread_id='thread',body='Hello',occurred_at=at,participants=[{'id':'me','name':'My Name','is_self':True},person],**kw)
 return [op('apple_messages','phone',[record('message',{'id':'+15551234567','phones':['+15551234567']})]),op('linkedin','named',[record('new',{'id':'urn:li:member:123','name':'Alice Example'})]),op('linkedin','opaque',[record('message',{'id':'urn:li:member:456'})]),op('linkedin','named',[record('old',{'id':'urn:li:member:123','name':'Old Name'},'2025-01-01T12:00:00Z')]),op('whatsapp','group',[record('new',{'id':'alice','name':'Alice'},title=' Trip plans ',is_group=True)]),op('linkedin','named',[record('new',{'id':'urn:li:member:123','name':'Current Name'})])]
def http(url):
 c=auth.Client(url);v={'email':'labels@example.test','password':'synthetic-labels-password'};c.request('/api/v1/auth/bootstrap',v,record=False);c.csrf=c.request('/api/v1/auth/csrf',record=False)['csrf_token'];c.request('/api/v1/auth/login',v,record=False)
 out=[]
 for q in ['','555123','Current','Old','Trip','urn%3Ali']:
  rows=c.request('/api/v1/conversations?q='+q,record=False);out.append(sorted([{k:r[k] for k in ['source','display_name','conversation_type','message_count']} for r in rows],key=lambda r:json.dumps(r,sort_keys=True)))
 return out
if __name__=='__main__':
 with reference_backend.ReferenceBackend('labels') as ref:
  (ref.root/'scenario.json').write_text(json.dumps(scenario()))
  code=journal.REFERENCE.replace('outputs.append(dict(changed=changed,cursor=cursor,records=records,evidence=evidence,people=people,candidates=candidates,messages=messages))','''with crm.connect() as db:labels=db.execute(text("SELECT jsonb_build_array(c.source_provider,n.external_account,c.source_external_id,c.conversation_type,c.profile_id,c.source_display_name,to_char(c.source_display_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI:SS')) FROM conversations c JOIN source_connections n ON n.id=c.connection_id ORDER BY c.source_provider,n.external_account,c.source_external_id")).scalars().all()
 outputs.append(dict(changed=changed,cursor=cursor,records=records,evidence=evidence,people=people,candidates=candidates,messages=messages,labels=labels))''')
  code+='\nfrom app.contact_stats import refresh\nwith Session(crm) as db:refresh(db)\n'
  (ref.root/'probe.py').write_text(code);subprocess.run([str(ref.python),str(ref.root/'probe.py'),str(ref.root)],env=dict(ref.environment,PYTHONPATH=str(ref.root/'services/api')),check=True)
  subprocess.run([str(reference_backend.PG/'createdb'),'-h',str(ref.root),'rust_labels'],check=True)
  rust=json.loads(subprocess.check_output([str(ROOT/'target/debug/examples/connection_probe'),str(ref.root/'scenario.json')],env=dict(os.environ,DATABASE_URL=f'postgresql:///rust_labels?host={ref.root}')))
  expected=json.loads((ref.root/'reference-results.json').read_text());assert rust==expected,json.dumps({'expected':expected,'actual':rust},ensure_ascii=False,indent=2)
  for database,schema in [('crm',''),('rust_labels','crm.')]:subprocess.run([str(reference_backend.PG/'psql'),'-h',str(ref.root),'-d',database,'-c',f'UPDATE {schema}source_connections SET enabled=false'],stdout=subprocess.DEVNULL,check=True)
  expected_http=http(ref.url)
  with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
  env={'PATH':os.defpath,'MONICA_DATABASE_URL':f'postgresql:///rust_labels?host={ref.root}','MONICA_SESSION_SECRET':'synthetic-labels-secret-32bytes-key','MONICA_MEDIA_ROOT':str(ref.root/'rust-media')}
  with (ref.root/'rust-labels.log').open('wb') as log:
   process=subprocess.Popen([str(ROOT/'target/debug/monica-server'),'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
   try:
    for _ in range(200):
     if process.poll() is not None:raise RuntimeError((ref.root/'rust-labels.log').read_text())
     try:urllib.request.urlopen(f'http://127.0.0.1:{port}/health',timeout=1).close();break
     except OSError:time.sleep(.05)
    actual_http=http(f'http://127.0.0.1:{port}');assert actual_http==expected_http,json.dumps({'expected':expected_http,'actual':actual_http},ensure_ascii=False,indent=2)
    print('Real c0007f5 labels differential passed: 6 steps / 48 journal+projection observations / 6 HTTP search observations')
   finally:
    process.send_signal(signal.SIGTERM)
    try:process.wait(timeout=20)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

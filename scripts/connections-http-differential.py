#!/usr/bin/env python3
"""HTTP contract differential on the same synthetic multi-source source journals."""
import importlib.util,json,os,signal,socket,subprocess,time,urllib.request,uuid
from pathlib import Path
from reference_backend import PG,ReferenceBackend
ROOT=Path(__file__).resolve().parents[1]
def module(name,path):
 spec=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
auth=module('auth',ROOT/'scripts/auth-differential.py');journal=module('journal',ROOT/'scripts/connections-differential.py')
NS=uuid.UUID('a874aa7b-0650-4950-ac70-f979c6b020eb')
def cid(provider,account):return uuid.uuid5(NS,provider+':'+account)
def scenario(url):
 c=auth.Client(url);credentials={'email':'connection-http@example.test','password':'synthetic-connections-password'}
 c.request('/api/v1/auth/bootstrap',credentials,record=False);c.csrf=c.request('/api/v1/auth/csrf',record=False)['csrf_token'];c.request('/api/v1/auth/login',credentials,record=False)
 observations=[]
 def observe(path,body=None,expected=200):
  result=c.request(path,body,expected=expected,record=False)
  # Runtime availability is an additive Rust deployment capability. Its NAS
  # enforcement is tested by connections_tests; compare the frozen catalogue
  # contract here without requiring Python to implement the new metadata.
  if path == '/api/v1/sources':
   result=[{k:v for k,v in row.items() if k not in ('collection_available','unavailable_reason')} for row in result]
  def normal(v,key=''):
   if isinstance(v,dict):return {k:normal(x,k) for k,x in v.items()}
   if isinstance(v,list):return [normal(x,key) for x in v]
   if isinstance(v,str):
    if key in ('earliest','latest') or key.endswith('_at'):
     from datetime import datetime,timezone
     return datetime.fromisoformat(v.replace('Z','+00:00')).astimezone(timezone.utc).isoformat() if key in ('earliest','latest','earliest_at','latest_at','occurred_at') else '$timestamp'
    return "$"+key if key in ("message_id","conversation_id") else v
   return v
  observations.append(dict(path=path,body=normal(result)));return result
 observe('/api/v1/sources');observe('/api/v1/source-connections')
 for provider,account in [('apple_contacts','contacts'),('mail','me@example.test'),('mail','second@example.test'),('apple_messages','device')]:observe(f'/api/v1/source-connections/{cid(provider,account)}/people')
 records=observe('/api/v1/evidence')
 observe('/api/v1/evidence?q=hello');observe('/api/v1/evidence?provider=mail');observe('/api/v1/evidence?include_filtered=true');observe('/api/v1/evidence?limit=1')
 profile=uuid.uuid5(cid('apple_contacts','contacts'),'contact:a')
 observe(f'/api/v1/profiles/{profile}/source-coverage');observe(f'/api/v1/evidence?profile_id={profile}')
 for item in records:
  observe('/api/v1/evidence/'+item['id']);observe('/api/v1/evidence/'+item['id']+'?revision=1');observe('/api/v1/evidence/'+item['id']+'?body_offset=1&body_limit=2')
 observe(f'/api/v1/profiles/{profile}/identifiers',{'kind':'email','value':' ME+Alias@EXAMPLE.test ','is_self':True});observe(f'/api/v1/profiles/{profile}/source-coverage')
 return observations
if __name__=='__main__':
 with ReferenceBackend('multisource') as ref:
  (ref.root/'scenario.json').write_text(json.dumps(journal.scenario()));(ref.root/'probe.py').write_text(journal.REFERENCE)
  subprocess.run([str(ref.python),str(ref.root/'probe.py'),str(ref.root)],env=dict(ref.environment,PYTHONPATH=str(ref.root/'services/api')),check=True)
  subprocess.run([str(PG/'createdb'),'-h',str(ref.root),'rust_connections'],check=True)
  subprocess.run([str(ROOT/'target/debug/examples/connection_probe'),str(ref.root/'scenario.json')],env=dict(os.environ,DATABASE_URL=f'postgresql:///rust_connections?host={ref.root}'),stdout=subprocess.DEVNULL,check=True)
  for database,schema in [('crm',''),('rust_connections','crm.')]:subprocess.run([str(PG/'psql'),'-h',str(ref.root),'-d',database,'-c',f"UPDATE {schema}source_connections SET enabled=false"],stdout=subprocess.DEVNULL,check=True)
  expected=scenario(ref.url)
  with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
  env={'PATH':os.defpath,'MONICA_DATABASE_URL':f'postgresql:///rust_connections?host={ref.root}','MONICA_SESSION_SECRET':'synthetic-reference-session-secret-32bytes','MONICA_MEDIA_ROOT':str(ref.root/'rust-media')}
  with (ref.root/'rust-http.log').open('wb') as log:
   process=subprocess.Popen([str(ROOT/'target/debug/monica-server'),'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
   try:
    for _ in range(200):
     if process.poll() is not None:raise RuntimeError((ref.root/'rust-http.log').read_text())
     try:urllib.request.urlopen(f'http://127.0.0.1:{port}/health',timeout=1).close();break
     except OSError:time.sleep(.05)
    actual=scenario(f'http://127.0.0.1:{port}')
    differences=[dict(index=i,expected=a,actual=b) for i,(a,b) in enumerate(zip(expected,actual)) if a!=b]
    (ROOT/'.autonomous/complete-monica/connections-http-differential.json').write_text(json.dumps(dict(observations=len(expected),differences=differences),ensure_ascii=False,indent=2))
    if differences:raise AssertionError(json.dumps(differences,ensure_ascii=False,indent=2))
    print(f'Real pinned multi-source HTTP differential passed: {len(expected)} observations')
   finally:
    process.send_signal(signal.SIGTERM)
    try:process.wait(timeout=20)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

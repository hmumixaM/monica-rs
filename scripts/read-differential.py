#!/usr/bin/env python3
"""Read actual default HTTP contracts on fresh synthetic reference and Rust stores."""
import argparse,importlib.util,json,os,signal,socket,subprocess,time,urllib.request
from pathlib import Path
from reference_backend import ReferenceBackend,PG
spec=importlib.util.spec_from_file_location('profiles',Path(__file__).with_name('profile-differential.py'))
profiles=importlib.util.module_from_spec(spec);spec.loader.exec_module(profiles)

# Meaningful empty-state reads used by the preserved frontend; no external services.
PATHS=['/api/v1/profiles','/api/v1/tags','/api/v1/activities','/api/v1/relationships',
'/api/v1/directory','/api/v1/directory/posts','/api/v1/home','/api/v1/search?q=Synthetic','/api/v1/overview','/api/v1/insights',
'/api/v1/insights/calendar','/api/v1/insights/orbit','/api/v1/metrics/locations','/api/v1/metrics/summary','/api/v1/metrics/snapshots','/api/v1/metrics/people-overview','/api/v1/metrics/social','/api/v1/link-review','/api/v1/link-review/linked','/api/v1/link-review/contacts','/api/v1/identity-candidates','/api/v1/source-accounts','/api/v1/linking/queue',
'/api/v1/conversations','/api/v1/timeline','/api/v1/timeline/summary','/api/v1/imports/batches','/api/v1/imports/cursors','/api/v1/imports/jobs','/api/v1/imports/events','/api/v1/imports/reconciliation',
'/api/v1/reconnect','/api/v1/digests','/api/v1/ai/suggestions','/api/v1/ai/weekly-people','/api/v1/ai/weekly-highlights','/api/v1/ai/status','/api/v1/jobs/status','/api/v1/faces/summary','/api/v1/faces/groups','/api/v1/archived-profiles']

def scenario(url):
 c=profiles.Client(url); credentials={'email':'reads@example.invalid','password':'synthetic-reads-password'}
 c.request('/api/v1/auth/bootstrap',credentials,record=False);c.csrf=c.request('/api/v1/auth/csrf',record=False)['csrf_token'];c.request('/api/v1/auth/login',credentials,record=False)
 p=c.request('/api/v1/profiles',{'display_name':'Synthetic Reader'},expected=201,record=False);c.ids[p['id']]='$profile'
 spec=json.load(open('docs/contracts/crm-openapi.json'))['paths']
 paths=[path for path in PATHS if path.split('?')[0] in spec]
 for tail in ('summary','metrics','overview','ribbon','locations','photos','messages','chat-months','contact-events','contact-events/summary','moments-circle','linkedin-targets'):
  paths.append('/api/v1/profiles/'+p['id']+'/'+tail)
 paths.append('/api/v1/ai/profiles/'+p['id'])
 for path in paths:
  if path=="/api/v1/timeline/summary":path+="?profile_id="+p["id"]
  c.request(path,expected=None)
 for row in c.results:
  row['operation']=row['operation'].replace(p['id'],'$profile')
  if row['operation']=='GET /api/v1/jobs/status':
   # Python's separate worker is intentionally absent in this harness. Rust
   # owns an integrated coordinator; verify that boundary independently of
   # empty business queues rather than comparing volatile process identities.
   for worker in row['body']['workers']:
    assert worker['resource_class']=='scheduler' and worker['stale'] is False
    assert worker['heartbeat_at']=='$timestamp' and isinstance(worker['active_jobs'],int)
   row['body']['workers']=[]
 return c.results

def main():
 parser=argparse.ArgumentParser();parser.add_argument('--baseline',choices=['crm','desktop'],default='crm');args=parser.parse_args()
 with ReferenceBackend(args.baseline) as ref:
  expected=scenario(ref.url)
  subprocess.run([str(PG/'createdb'),'-h',str(ref.root),'rust'],check=True)
  with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
  env={'PATH':os.defpath,'MONICA_DATABASE_URL':f'postgresql:///rust?host={ref.root}','MONICA_SESSION_SECRET':'synthetic-reference-session-secret-32bytes','MONICA_MEDIA_ROOT':str(ref.root/'rust-media')}
  binary=str(Path('target/debug/monica-server').resolve());subprocess.run([binary,'migrate'],env=env,check=True)
  with (ref.root/'rust.log').open('wb') as log:
   process=subprocess.Popen([binary,'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
   try:
    url=f'http://127.0.0.1:{port}'
    for _ in range(1200):
     if process.poll() is not None:raise RuntimeError((ref.root/'rust.log').read_text())
     try:urllib.request.urlopen(url+'/health',timeout=1).close();break
     except OSError:time.sleep(.05)
    actual=scenario(url)
    diffs=[{'operation':e['operation'],'expected_status':e['status'],'actual_status':a['status'],'expected':e['body'],'actual':a['body']} for e,a in zip(expected,actual) if e!=a]
    output=Path('.autonomous/complete-monica/read-diff-'+args.baseline+'.json');output.write_text(json.dumps(diffs,ensure_ascii=False,indent=2))
    assert not diffs,f'{len(diffs)} differences: '+str(output)
    print(f'Real {args.baseline}/Rust empty reads passed: {len(actual)} observations')
   finally:
    if process.poll() is None:
     process.send_signal(signal.SIGTERM)
     try:process.wait(timeout=100)
     except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
if __name__=='__main__':main()

#!/usr/bin/env python3
"""Real pinned Python/Rust timeline, participant and pagination differential.
All stores/processes are newly created by ReferenceBackend and removed on exit.
"""
import argparse
import base64
from datetime import datetime, timezone
import importlib.util
import json
import hashlib
import uuid
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.request
from reference_backend import PG, ReferenceBackend

spec = importlib.util.spec_from_file_location('auth_diff', Path(__file__).with_name('auth-differential.py'))
auth = importlib.util.module_from_spec(spec)
spec.loader.exec_module(auth)


def seed_social(root, database, rust, people):
    def quote(value): return "'"+str(value).replace("'", "''")+"'"
    statements=[]
    schema='crm.' if rust else ''
    source=uuid.UUID('00000000-0000-0000-0000-000000001001')
    if rust:
        statements.append(f"INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES('{source}','wechat','synthetic-source','person','Synthetic')")
    posts=[]
    for n,owner in enumerate(people[:2]):
        event=uuid.UUID(int=5001+n); posts.append(str(event))
        content={'caption':f' Social post {n} ','like_count':2+n,'comment_count':1,'media':[], 'location':{'name':'Synthetic park','latitude':30.5,'longitude':-80.25}}
        payload=json.dumps(content,ensure_ascii=False,separators=(',',':'),sort_keys=True)
        digest=hashlib.sha256(payload.encode()).hexdigest()
        if rust:
            statements.append(f"INSERT INTO crm.timeline_events(id,object_id,source_account_id,provider,external_id,profile_id,event_type,occurred_at,content,content_hash,version) VALUES('{event}','{event}','{source}','wechat','post{n}','{owner}','post','2024-03-01T12:00:00Z',{quote(payload)},'{digest}',1)")
        else:
            content_id=uuid.UUID(int=6001+n)
            statements.append(f"INSERT INTO content_objects(id,sha256,content_kind,payload_json) VALUES('{content_id}','{digest}','social_post',{quote(payload)})")
            statements.append(f"INSERT INTO social_posts(id,provider,external_id,profile_id,occurred_at,content_object_id,current_content_hash) VALUES('{event}','wechat','post{n}','{owner}','2024-03-01T12:00:00Z','{content_id}','{digest}')")
    for i,pid in enumerate(people):
        statements.append(f"INSERT INTO {schema}identities(id,profile_id,provider,external_id,status) VALUES('{uuid.UUID(int=7001+i)}','{pid}','wechat','person{i}','active')")
    facts=[(0,1,'like',None,None),(0,2,'like',None,None),(1,0,'comment',None,None),(1,2,'like',None,None),(1,0,'mention','person2',None),(0,1,'reply',None,'person2')]
    for i,(post,author,kind,target,reference) in enumerate(facts):
        fields={'target_external_id':target,'ref_author_external_id':reference}
        if rust:
            statements.append(f"INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_profile_id,occurred_at,content,metadata) VALUES('{uuid.UUID(int=8001+i)}','{posts[post]}','wechat','interaction{i}','{kind}','{people[author]}','2024-03-01T12:00:00Z','null',{quote(json.dumps(fields))})")
        else:
            nullable=lambda value:'NULL' if value is None else quote(value)
            statements.append(f"INSERT INTO social_interactions(id,post_id,provider,external_id,interaction_type,author_profile_id,occurred_at,target_external_id,ref_author_external_id,current_content_hash,metadata) VALUES('{uuid.UUID(int=8001+i)}','{posts[post]}','wechat','interaction{i}','{kind}','{people[author]}','2024-03-01T12:00:00Z',{nullable(target)},{nullable(reference)},repeat('0',64),{quote(json.dumps(fields))})")
    subprocess.run([str(PG/'psql'),'-X','-h',str(root),'-d',database,'-v','ON_ERROR_STOP=1'],input=';\n'.join(statements)+';',text=True,check=True,stdout=subprocess.DEVNULL)
    return posts


def scenario(url, root, database, rust):
    c = auth.Client(url)
    credentials = {'email': 'domain@example.invalid', 'password': 'synthetic-domain-password'}
    c.request('/api/v1/auth/bootstrap', credentials, record=False)
    c.csrf = c.request('/api/v1/auth/csrf', record=False)['csrf_token']
    c.request('/api/v1/auth/login', credentials, record=False)
    ids = []
    for name in ['Alpha', 'Beta', 'Outside']:
        p = c.request('/api/v1/profiles', {'profile_type': 'person', 'display_name': name}, expected=201, record=False)
        ids.append(p['id'])
        c.ids[p['id']] = '$' + name
    events = []
    for n, participants in [(1, ids[:2]), (2, ids[:1]), (3, ids[1:2])]:
        e = c.request('/api/v1/activities', {'activity_type':'note', 'occurred_at': f'2024-02-0{n}T12:00:00Z',
                    'title':f'Note {n}', 'body':f' Body {n} ', 'participant_profile_ids':participants}, expected=201, record=False)
        events.append(e['id'])
        c.ids[e['id']] = f'$event{n}'
    def observe(path):
        value = c.request(path, record=False)
        def normalize(v, key=''):
            if isinstance(v, dict): return {k:normalize(x,k) for k,x in v.items()}
            if isinstance(v,list): return [normalize(x,key) for x in v]
            if key=='cursor':
                stamp, event_id = json.loads(base64.urlsafe_b64decode(v))
                return [datetime.fromisoformat(stamp).astimezone(timezone.utc).isoformat() if stamp else None, c.ids[event_id]]
            if key=='as_of':
                datetime.fromisoformat(v)
                return '$timestamp'
            if key=='url' and isinstance(v,str) and v.startswith('/profiles/'):
                return '/profiles/'+c.ids[v.split('/')[-1]]
            if key in ('occurred_at', 'latest', 'first', 'last') and v is not None:
                return datetime.fromisoformat(v).astimezone(timezone.utc).isoformat()
            return c.normalize(v,key)
        c.results.append(normalize(value))
        return value
    for profile in ids:
        observe('/api/v1/timeline?profile_id='+profile)
        observe('/api/v1/timeline/summary?profile_id='+profile)
    all_events = observe('/api/v1/timeline?provider=manual')
    assert len(all_events)==3
    page = observe('/api/v1/timeline?limit=1')
    seen=[]
    while page:
        seen.append(page[0]['id'])
        page=observe('/api/v1/timeline?limit=1&cursor='+page[0]['cursor'])
    assert seen == [row['id'] for row in all_events]
    observe('/api/v1/timeline?after=2024-02-02T00:00:00Z&before=2024-02-03T00:00:00Z')
    observe('/api/v1/timeline?media=ready')
    for event in events: observe('/api/v1/timeline/'+event)
    social=seed_social(root,database,rust,ids)
    for i,event in enumerate(social):c.ids[event]=f'$post{i}'
    for event in social: observe('/api/v1/timeline/'+event+'?provider=wechat')
    observe('/api/v1/timeline?provider=wechat')
    observe('/api/v1/timeline/summary?profile_id='+ids[0])
    relation = c.request('/api/v1/profiles/'+ids[0]+'/relationships', {'to_profile_id':ids[1],'relationship_type':'friend','note':'Synthetic relationship'}, expected=201, record=False)
    c.ids[relation['id']]='$relationship'
    observe('/api/v1/relationships/graph')
    observe('/api/v1/relationships/graph?source_type=missing')
    observe('/api/v1/relationships/graph?min_confidence=1')
    observe('/api/v1/profiles/'+ids[0]+'/moments-circle')
    group=c.request('/api/v1/profiles', {'profile_type':'group','display_name':'Synthetic Group'}, expected=201, record=False)
    c.ids[group['id']]='$group'
    for peer in [ids[0],ids[2]]:
        c.request('/api/v1/groups/'+group['id']+'/members', {'person_profile_id':peer,'role':'member'}, expected=201, record=False)
    observe('/api/v1/profiles/'+ids[0]+'/moments-circle')
    graph=c.request('/api/v1/relationships/graph?format=monica&focus='+ids[0],record=False)
    assert graph['version']==2 and len(graph['nodes'])==3 and len(graph['edges'])==2,graph
    # Node index order is UUID-dependent. Compare the complete graph facts by
    # resolved names while independently verifying finite seeded coordinates.
    import math
    for node in graph['nodes']: assert math.isfinite(node['x']) and math.isfinite(node['y'])
    named_nodes=sorted((n['name'],n['facts'],n['deg'],n['partners']) for n in graph['nodes'])
    named_edges=sorted((sorted([graph['nodes'][e['a']]['name'],graph['nodes'][e['b']]['name']]),e['direct'],e['group'],e['co'],e.get('relationship_types',[])) for e in graph['edges'])
    c.results.append({'graph_nodes':named_nodes,'graph_edges':named_edges,'scope_limit':graph['scope']['limit']})
    observe('/api/v1/metrics/social')
    observe('/api/v1/metrics/social?profile_id='+ids[0])
    return c.results


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--baseline',choices=['crm','desktop'],default='crm')
    args=parser.parse_args()
    with ReferenceBackend(args.baseline) as reference:
        expected=scenario(reference.url,reference.root,'crm',False)
        subprocess.run([str(PG/'createdb'),'-h',str(reference.root),'rust'],check=True)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
        env={'PATH':os.defpath,'MONICA_DATABASE_URL':f'postgresql:///rust?host={reference.root}',
             'MONICA_SESSION_SECRET':'synthetic-reference-session-secret-32bytes','MONICA_MEDIA_ROOT':str(reference.root/'rust-media')}
        binary=str(Path('target/debug/monica-server').resolve())
        subprocess.run([binary,'migrate'],env=env,check=True)
        with (reference.root/'rust.log').open('wb') as log:
            process=subprocess.Popen([binary,'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
            try:
                url=f'http://127.0.0.1:{port}'
                for _ in range(200):
                    if process.poll() is not None: raise RuntimeError((reference.root/'rust.log').read_text())
                    try: urllib.request.urlopen(url+'/health',timeout=1).close(); break
                    except OSError: time.sleep(.05)
                actual=scenario(url,reference.root,'rust',True)
                assert actual==expected,json.dumps({'expected':expected,'actual':actual},ensure_ascii=False,indent=2)
                print(f'Real {args.baseline}/Rust domain differential passed: {len(actual)} observations')
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                    try: process.wait(timeout=20)
                    except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=5)

if __name__=='__main__': main()

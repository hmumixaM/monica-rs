#!/usr/bin/env python3
"""Actual pinned native-notebook import contract, on disposable synthetic databases."""
import argparse,importlib.util,json,os,signal,socket,subprocess,time,urllib.request
from pathlib import Path
from reference_backend import PG,ReferenceBackend
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('signed_import',Path(__file__).with_name('connection-import-differential.py'));helper=importlib.util.module_from_spec(spec);spec.loader.exec_module(helper)
spec=importlib.util.spec_from_file_location('profile_http',Path(__file__).with_name('profile-differential.py'));http=importlib.util.module_from_spec(spec);spec.loader.exec_module(http)
sql=helper.sql
PROFILE='00000000-0000-0000-0000-000000005010'
def observe(root,database,rust):
 prefix='crm.' if rust else ''
 def query(q):return json.loads(sql(root,database,q))
 profiles=query(f"SELECT coalesce(jsonb_agg(jsonb_build_array(profile_type,display_name,summary) ORDER BY display_name),'[]') FROM {prefix}profiles")
 identities=query(f"SELECT coalesce(jsonb_agg(jsonb_build_array(i.provider,i.external_id,i.status,p.display_name) ORDER BY i.external_id),'[]') FROM {prefix}identities i JOIN {prefix}profiles p ON p.id=i.profile_id")
 if rust:
  fields=query("SELECT coalesce(jsonb_agg(jsonb_build_array(p.display_name,c.field_key,r.value,r.content_hash,r.source_type) ORDER BY p.display_name,c.field_key),'[]') FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id JOIN crm.profiles p ON p.id=c.profile_id")
  facts=query("SELECT coalesce(jsonb_agg(jsonb_build_array(p.display_name,c.fact_type,c.fact_key,c.value,c.current_content_hash,c.current_source_type) ORDER BY p.display_name,c.fact_type,c.fact_key),'[]') FROM crm.profile_structured_current c JOIN crm.profiles p ON p.id=c.profile_id")
  revisions=query("SELECT coalesce(jsonb_agg(jsonb_build_array(p.display_name,r.field_key,r.value,r.is_conflict) ORDER BY r.revision_seq),'[]') FROM crm.field_revisions r JOIN crm.profiles p ON p.id=r.profile_id WHERE r.source_type='monica'")
  posts=query("SELECT coalesce(jsonb_agg(jsonb_build_array(e.event_type,e.provider,e.external_id,p.display_name,e.content,e.content_hash,e.status,(SELECT count(*) FROM crm.notebook_social_revisions r WHERE r.event_id=e.id)) ORDER BY e.external_id),'[]') FROM crm.timeline_events e LEFT JOIN crm.profiles p ON p.id=e.profile_id")
  interactions=query("SELECT coalesce(jsonb_agg(jsonb_build_array(i.external_id,i.interaction_type,p.display_name,i.author_name,i.content,i.metadata) ORDER BY i.external_id),'[]') FROM crm.social_interactions i LEFT JOIN crm.profiles p ON p.id=i.author_profile_id")
  messages=query("SELECT coalesce(jsonb_agg(jsonb_build_array(c.provider,c.source_external_id,c.conversation_type,p.display_name,m.body,m.message_type,m.direction) ORDER BY c.source_external_id,m.occurred_at),'[]') FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id LEFT JOIN crm.profiles p ON p.id=c.profile_id")
 else:
  fields=query("SELECT coalesce(jsonb_agg(jsonb_build_array(p.display_name,c.field_key,o.payload_json,c.current_content_hash,c.current_source_type) ORDER BY p.display_name,c.field_key),'[]') FROM profile_field_current c JOIN profile_field_revisions r ON r.id=c.current_revision_id JOIN content_objects o ON o.id=r.content_object_id JOIN profiles p ON p.id=c.profile_id")
  facts=query("WITH revisions AS (SELECT id,content_object_id FROM profile_address_revisions UNION ALL SELECT id,content_object_id FROM profile_employment_revisions UNION ALL SELECT id,content_object_id FROM profile_education_revisions) SELECT coalesce(jsonb_agg(jsonb_build_array(p.display_name,c.fact_type,c.fact_key,o.payload_json,c.current_content_hash,c.current_source_type) ORDER BY p.display_name,c.fact_type,c.fact_key),'[]') FROM profile_structured_current c JOIN revisions r ON r.id=c.current_revision_id JOIN content_objects o ON o.id=r.content_object_id JOIN profiles p ON p.id=c.profile_id")
  revisions=query("SELECT coalesce(jsonb_agg(jsonb_build_array(p.display_name,r.field_key,o.payload_json,r.is_conflict) ORDER BY r.revision_seq),'[]') FROM profile_field_revisions r JOIN content_objects o ON o.id=r.content_object_id JOIN profiles p ON p.id=r.profile_id WHERE r.source_type='monica'")
  posts=query("WITH events AS (SELECT id,'post' kind,provider,external_id,profile_id,content_object_id,current_content_hash,status,(SELECT count(*) FROM social_post_revisions r WHERE r.post_id=p.id) revisions FROM social_posts p UNION ALL SELECT id,'story',provider,external_id,profile_id,content_object_id,current_content_hash,coverage_status,(SELECT count(*) FROM social_story_revisions r WHERE r.story_id=p.id) FROM social_stories p) SELECT coalesce(jsonb_agg(jsonb_build_array(e.kind,e.provider,e.external_id,p.display_name,o.payload_json,e.current_content_hash,e.status,e.revisions) ORDER BY e.external_id),'[]') FROM events e JOIN content_objects o ON o.id=e.content_object_id LEFT JOIN profiles p ON p.id=e.profile_id")
  interactions=query("SELECT coalesce(jsonb_agg(jsonb_build_array(i.external_id,i.interaction_type,p.display_name,i.author_name,o.payload_json,i.metadata) ORDER BY i.external_id),'[]') FROM social_interactions i LEFT JOIN content_objects o ON o.id=i.content_object_id LEFT JOIN profiles p ON p.id=i.author_profile_id")
  messages=query("SELECT coalesce(jsonb_agg(jsonb_build_array(coalesce(to_jsonb(c)->>'source_provider','monica'),coalesce(to_jsonb(c)->>'source_external_id',c.external_id),c.conversation_type,p.display_name,coalesce(nullif(o.payload_json->>'text',''),o.payload_json->>'body',''),e.message_type,CASE WHEN e.sender_profile_id=c.profile_id THEN 'received' ELSE 'sent' END) ORDER BY c.external_id,e.occurred_at),'[]') FROM message_events e JOIN content_objects o ON o.id=e.content_object_id JOIN conversations c ON c.id=e.conversation_id LEFT JOIN profiles p ON p.id=c.profile_id")
 return dict(profiles=profiles,identities=identities,fields=fields,facts=facts,revisions=revisions,posts=posts,interactions=interactions,messages=messages)
def scenario(url,root,database,rust,baseline):
 prefix='crm.' if rust else '';sql(root,database,f"INSERT INTO {prefix}profiles(id,profile_type,display_name) VALUES('{PROFILE}','person','Explicit')")
 client=http.Client(url);credentials={'email':'notebook@example.invalid','password':'synthetic-notebook-password'}
 client.request('/api/v1/auth/bootstrap',credentials,record=False);client.csrf=client.request('/api/v1/auth/csrf',record=False)['csrf_token'];client.request('/api/v1/auth/login',credentials,record=False)
 results=[]
 cases=[('contacts',[{'external_id':'person-a','content':{'full_name':'Notebook Person','headline':'A notebook','fields':{'bio':'Original','linkedin.experience':[{'company':'Must stay generic'}]},'addresses':[{'city':'Chicago'}],'experience':[{'company':'Example','title':'Engineer'}],'schools':[{'school':'Example College'}]}}]),('groups',[{'external_id':'group-a','content':{'display_name':'Notebook Group','experience':[{'company':'Must stay ignored'}]}}]),('custom',[{'external_id':'single','profile_id':PROFILE,'content':{'field_key':'bio','value':'Before manual'}}]),('posts',[{'external_id':'post-a','profile_id':PROFILE,'content':{'caption':'Hello','interactions':[{'external_id':'comment-a','author_external_id':'person-a','text':'Nice','nickname':'Alias'}]}}]),('stories',[{'external_id':'story-a','profile_id':PROFILE,'content':{'text':'Story','coverage_status':'complete'}}]),('messages',[{'external_id':'message-a','profile_id':PROFILE,'sender_profile_id':PROFILE,'conversation_external_id':'thread-a','occurred_at':'2026-01-01T00:00:00Z','content':{'text':'Hello message'}}])]
 cases += [cases[0],('contacts',[{'external_id':'person-a','content':{'full_name':'Name stays original','fields':{'bio':'Changed'},'experience':[{'company':'Example','title':'Senior'}]}}]),cases[0],('posts',[{'external_id':'post-a','content':{'caption':'Edited','interactions':[{'external_id':'comment-a','text':'Edited comment'}]}}]),('messages',[{'external_id':'message-a','conversation_external_id':'thread-a','occurred_at':'2026-01-01T00:00:00Z','content':{'text':'Edited message'}}])]
 # The older two baselines deliberately ignored message corrections (DO NOTHING);
 # the newer accepted multisource contract updates them. Compare that new behavior only there.
 if baseline != 'multisource':cases.pop(10)
 cases += [('custom',[{'external_id':'single','profile_id':PROFILE,'content':{'field_key':'bio','value':'Imported after manual'}}])]*2
 for i,(stream,records) in enumerate(cases):
  if i==len(cases)-2:client.request('/api/v1/profiles/'+PROFILE+'/fields',{'field_key':'bio','value':'Manual wins'},method='PATCH',record=False)
  value={'source':'monica','stream':stream,'batch_id':str(i),'idempotency_key':'notebook-'+str(i),'observed_at':f'2026-01-01T12:00:{i:02d}Z','records':records}
  status,receipt=helper.signed(url,value,'notebook-'+str(i));assert status==201,(status,receipt)
  if rust:
   for _ in range(600):
    status=sql(root,database,f"SELECT status||':'||coalesce(error_summary,'') FROM crm.import_batches WHERE id='{receipt['id']}'")
    if status.startswith('complete:'):break
    if 'failed' in status or status.split(':',1)[1]:raise AssertionError(status)
    time.sleep(.05)
   else:raise AssertionError(status)
  results.append(observe(root,database,rust))
 events=client.request('/api/v1/timeline?profile_id='+PROFILE,record=False)
 details=[]
 for event in events:
  detail=client.request('/api/v1/timeline/'+event['id'],record=False)
  details.append({k:detail.get(k) for k in ['provider','external_id','event_type','content','content_hash','status','coverage_status','revisions']})
 for detail in details:
  for revision in detail['revisions']:
   revision.pop('observed_at',None)
 token=client.request('/api/v1/imports/tokens',{'source':'monica','label':'Notebook token'},expected=201,record=False)
 results.append({'http_timeline':sorted(details,key=lambda v:v['external_id']),'http_token':{k:token[k] for k in ['source','label']}})
 return results
if __name__=='__main__':
 parser=argparse.ArgumentParser();parser.add_argument('--baseline',default='multisource',choices=['crm','desktop','multisource']);args=parser.parse_args()
 with ReferenceBackend(args.baseline) as ref:
  expected=scenario(ref.url,ref.root,'crm',False,args.baseline)
  subprocess.run([str(PG/'createdb'),'-h',str(ref.root),'rust_notebook'],check=True)
  with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
  env={'PATH':os.defpath,'MONICA_DATABASE_URL':f'postgresql:///rust_notebook?host={ref.root}','MONICA_SESSION_SECRET':'synthetic-reference-session-secret-32bytes','MONICA_IMPORT_HMAC_SECRET':helper.SECRET,'MONICA_MEDIA_ROOT':str(ref.root/'rust-media')}
  binary=str(ROOT/'target/debug/monica-server');subprocess.run([binary,'migrate'],env=env,check=True)
  with (ref.root/'rust.log').open('wb') as log:
   process=subprocess.Popen([binary,'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
   try:
    for _ in range(200):
     if process.poll() is not None:raise RuntimeError((ref.root/'rust.log').read_text())
     try:urllib.request.urlopen(f'http://127.0.0.1:{port}/health',timeout=1).close();break
     except OSError:time.sleep(.05)
    actual=scenario(f'http://127.0.0.1:{port}',ref.root,'rust_notebook',True,args.baseline)
    for step,(e,a) in enumerate(zip(expected,actual)):
     assert a==e,json.dumps({'step':step,'expected':e,'actual':a},ensure_ascii=False,indent=2)
    print(f'Real pinned {args.baseline} native notebook differential passed: {(len(actual)-1)*8} database observations across {len(actual)-1} signed batches plus {len(actual[-1]["http_timeline"])} HTTP timeline details; typed facts, fields, groups, social interactions, messages, replay, A-B-A and manual conflict precedence')
   finally:
    process.send_signal(signal.SIGTERM)
    try:process.wait(timeout=20)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

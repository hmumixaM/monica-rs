#!/usr/bin/env python3
"""Actual signed pinned HTTP imports: two source accounts sharing external IDs."""
import hashlib,hmac,json,os,signal,socket,subprocess,time,urllib.request,urllib.error,uuid
from pathlib import Path
from reference_backend import PG,ReferenceBackend
ROOT=Path(__file__).resolve().parents[1];SECRET='synthetic-reference-import-secret-32bytes'
CID=[str(uuid.uuid5(uuid.UUID('a874aa7b-0650-4950-ac70-f979c6b020eb'),'linkedin:signed-'+str(i))) for i in range(2)]
PROFILES=['00000000-0000-0000-0000-000000005001','00000000-0000-0000-0000-000000005002']
def sql(root,database,query):return subprocess.check_output([str(PG/'psql'),'-h',str(root),'-d',database,'-At','-c',query]).decode().strip()
def seed(root,database,schema):
 for i,cid in enumerate(CID):sql(root,database,f"INSERT INTO {schema}profiles(id,profile_type,display_name) VALUES('{PROFILES[i]}','person','Person {i}'); INSERT INTO {schema}source_connections(id,provider,external_account,display_name,connection_method,state,enabled) VALUES('{cid}','linkedin','signed-{i}','Signed {i}','browser','connected',false)")
def signed(url,value,nonce):
 body=json.dumps(value).encode();stamp=str(int(time.time()));signature=hmac.new(SECRET.encode(),stamp.encode()+b'.'+nonce.encode()+b'.'+body,hashlib.sha256).hexdigest();request=urllib.request.Request(url+'/api/v1/imports/batches',data=body,headers={'Content-Type':'application/json','X-Import-Timestamp':stamp,'X-Import-Nonce':nonce,'X-Import-Signature':signature})
 try:response=urllib.request.urlopen(request,timeout=30)
 except urllib.error.HTTPError as e:response=e
 with response:return response.status,json.load(response)
def observe(root,database,rust):
 if rust:query="SELECT coalesce(jsonb_agg(jsonb_build_array(n.external_account,c.source_external_id,c.conversation_type,c.profile_id,m.sender_profile_id,m.body) ORDER BY n.external_account),'[]') FROM crm.conversations c JOIN crm.source_connections n ON n.id=c.connection_id JOIN crm.message_search m ON m.conversation_id=c.id WHERE n.external_account LIKE 'signed-%'"
 else:query="SELECT coalesce(jsonb_agg(jsonb_build_array(n.external_account,c.source_external_id,c.conversation_type,c.profile_id,e.sender_profile_id,m.body) ORDER BY n.external_account),'[]') FROM conversations c JOIN source_connections n ON n.id=c.connection_id JOIN message_events e ON e.conversation_id=c.id JOIN message_search m ON m.message_id=e.id AND m.occurred_at=e.occurred_at WHERE n.external_account LIKE 'signed-%'"
 return json.loads(sql(root,database,query))
def scenario(url,root,database,rust):
 results=[]
 for i,cid in enumerate(CID):
  value={'source':'linkedin','connection_id':cid,'stream':'messages','batch_id':'batch-'+str(i),'idempotency_key':'key-'+str(i),'observed_at':'2026-01-01T12:00:00Z','cursor_after':'1','records':[{'external_id':'same-message','conversation_external_id':'same-thread','profile_id':PROFILES[i],'sender_profile_id':PROFILES[i],'occurred_at':'2026-01-01T00:00:00Z','content':{'text':'hello-'+str(i)}}]}
  status,receipt=signed(url,value,'nonce-'+str(i));assert status==201,(status,receipt)
  if rust:
   for _ in range(600):
    actual=observe(root,database,True)
    if len(actual)==i+1:break
    time.sleep(.05)
   else:raise AssertionError(sql(root,database,'SELECT status,error_summary FROM crm.import_batches'))
  results.append(observe(root,database,rust));status,replay=signed(url,value,'repeat-'+str(i));assert status==201,(status,replay);results.append({'replay_same_receipt':replay['id']==receipt['id']});status,error=signed(url,value,'repeat-'+str(i));results.append({'nonce_status':status,'nonce_error':error})
 # Explicit None ownership clears the conversation and sender on a mutable revision.
 value['connection_id']=CID[0];value['batch_id']='edit';value['idempotency_key']='edit';value['cursor_before']='1';value['cursor_after']='2';value['records'][0].pop('profile_id');value['records'][0].pop('sender_profile_id');value['records'][0]['content']={'text':'edited'}
 status,result=signed(url,value,'edited');assert status==201,(status,result)
 if rust:
  for _ in range(600):
   actual=observe(root,database,True)
   if actual[0][-1]=='edited':break
   time.sleep(.05)
  else:raise AssertionError(sql(root,database,'SELECT status,error_summary FROM crm.import_batches'))
 results.append(observe(root,database,rust))
 value['source']='mail';value['idempotency_key']='mismatch';value['batch_id']='mismatch';status,result=signed(url,value,'provider-mismatch');results.append({'mismatch_status':status,'mismatch_error':result})
 return results
if __name__=='__main__':
 with ReferenceBackend('multisource') as ref:
  seed(ref.root,'crm','');expected=scenario(ref.url,ref.root,'crm',False)
  subprocess.run([str(PG/'createdb'),'-h',str(ref.root),'rust_imports'],check=True)
  with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
  env={'PATH':os.defpath,'MONICA_DATABASE_URL':f'postgresql:///rust_imports?host={ref.root}','MONICA_SESSION_SECRET':'synthetic-reference-session-secret-32bytes','MONICA_IMPORT_HMAC_SECRET':SECRET,'MONICA_MEDIA_ROOT':str(ref.root/'rust-media')}
  binary=str(ROOT/'target/debug/monica-server');subprocess.run([binary,'migrate'],env=env,check=True);seed(ref.root,'rust_imports','crm.')
  with (ref.root/'rust.log').open('wb') as log:
   process=subprocess.Popen([binary,'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
   try:
    for _ in range(200):
     if process.poll() is not None:raise RuntimeError((ref.root/'rust.log').read_text())
     try:urllib.request.urlopen(f'http://127.0.0.1:{port}/health',timeout=1).close();break
     except OSError:time.sleep(.05)
    actual=scenario(f'http://127.0.0.1:{port}',ref.root,'rust_imports',True)
    assert actual==expected,json.dumps({'expected':expected,'actual':actual},indent=2)
    print('Real pinned signed connection import HTTP differential passed: 8 observations, replay/nonces/mismatch, two accounts with identical external IDs, mutable explicit ownership')
   finally:
    process.send_signal(signal.SIGTERM)
    try:process.wait(timeout=20)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

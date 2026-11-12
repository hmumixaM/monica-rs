#!/usr/bin/env python3
"""Run actual pinned multi-source journal/projection against isolated Rust stores."""
import json
import os
from pathlib import Path
import subprocess
from reference_backend import PG, ReferenceBackend

ROOT=Path(__file__).resolve().parents[1]

def scenario():
    contact=lambda who:dict(external_id=who,kind='contact',emails=['shared@example.test'],participants=[dict(id=who,name=who,emails=['shared@example.test'])])
    message=dict(external_id='mail-1',kind='message',thread_id='thread',occurred_at='2026-01-01T00:00:00Z',body='hello',raw_body='private transport envelope',participants=[dict(id='shared@example.test',role='sender',emails=['SHARED@example.test']),dict(id='me@example.test',role='recipient',emails=['me@example.test'])])
    def op(provider,account,records,relink=False):return dict(provider=provider,account=account,records=records,cursor={'committed':len(records)},relink=relink)
    return [op('apple_contacts','contacts',[contact('a')]),op('mail','me@example.test',[message]),op('mail','me@example.test',[message]),op('apple_contacts','contacts',[contact('b')],True),op('mail','me@example.test',[dict(message,body='corrected',occurred_at='2026-02-01T00:00:00Z',thread_id='corrected-thread')]),op('mail','second@example.test',[message]),op('apple_contacts','contacts',[dict(contact('b'),deleted=True)],True),op('mail','me@example.test',[dict(message,included=0)]),op('mail','me@example.test',[message]),op('apple_messages','device',[dict(message,participants=[dict(id='me',role='sender',is_self=1),dict(id='friend',role='recipient')])])]

REFERENCE=r'''
import json,sys
from pathlib import Path
from sqlalchemy import create_engine,text
from sqlalchemy.orm import Session
from app import connection_store as store
from app.connection_projection import project_revision
from app.source_registry import connection_id
root=Path(sys.argv[1]);operations=json.loads((root/'scenario.json').read_text())
crm=create_engine('postgresql+psycopg:///crm?host='+str(root))
from app.platform_store import platform_engine
for provider in {op['provider'] for op in operations}:store.prepare(provider)
outputs=[]
for op in operations:
 cid=connection_id(op['provider'],op['account']);source=platform_engine(op['provider'])
 with crm.begin() as db:db.execute(text("INSERT INTO source_connections(id,provider,external_account,display_name,connection_method,state,enabled) VALUES(:id,:provider,:account,:account,'local','connected',true) ON CONFLICT DO NOTHING"),dict(id=cid,provider=op['provider'],account=op['account']))
 with source.begin() as db:changed=store.commit_page(db,cid,'records',op['records'],op['cursor'])
 with source.begin() as db:pending=store.pending(db,cid,1000)
 with Session(crm) as db:
  for row in pending:project_revision(db,dict(id=cid,provider=op['provider']),row)
  db.commit()
 with source.begin() as db:store.acknowledge(db,[r['sequence'] for r in pending])
 if op['relink']:
  with crm.connect() as db:requests=db.execute(text('SELECT e.*,c.provider FROM connection_relink r JOIN connection_evidence e ON e.id=r.evidence_id JOIN source_connections c ON c.id=e.connection_id ORDER BY e.id')).mappings().all()
  for request in requests:
   with platform_engine(request['provider']).connect() as db:row=db.execute(text('SELECT r.*,updated_at observed_at FROM connection_records r WHERE connection_id=:cid AND external_id=:external'),dict(cid=request['connection_id'],external=request['external_id'])).mappings().one()
   with Session(crm) as db:
    project_revision(db,dict(id=request['connection_id'],provider=request['provider']),row,force=True)
    db.execute(text('DELETE FROM connection_relink WHERE evidence_id=:id'),dict(id=request['id']));db.commit()
 with source.connect() as db:
  cursor=store.cursor(db,cid,'records')
  records=db.execute(text('SELECT jsonb_build_array(external_id,revision,content_hash,payload) FROM connection_records WHERE connection_id=:cid ORDER BY external_id'),dict(cid=cid)).scalars().all()
 with crm.connect() as db:
  evidence=db.execute(text("SELECT jsonb_build_array(c.external_account,e.external_id,e.revision,e.content_hash,e.kind,to_char(e.occurred_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI:SS'),e.body,e.included,e.deleted,e.metadata) FROM connection_evidence e JOIN source_connections c ON c.id=e.connection_id ORDER BY c.external_account,e.external_id")).scalars().all()
  people=db.execute(text("SELECT jsonb_build_array(c.external_account,p.external_id,p.display_name,p.is_self,p.resolved_profile_id) FROM connection_people p JOIN source_connections c ON c.id=p.connection_id ORDER BY c.external_account,p.external_id")).scalars().all()
  candidates=db.execute(text("SELECT jsonb_build_array(c.external_account,p.external_id,p.profile_id,p.reason) FROM connection_identity_candidates p JOIN source_connections c ON c.id=p.connection_id ORDER BY c.external_account,p.external_id,p.profile_id")).scalars().all()
  messages=db.execute(text("SELECT jsonb_build_array(cn.external_account,c.source_external_id,c.conversation_type,c.profile_id,m.body,to_char(m.occurred_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI:SS'),e.sender_profile_id) FROM message_search m JOIN message_events e ON e.id=m.message_id AND e.occurred_at=m.occurred_at JOIN conversations c ON c.id=m.conversation_id JOIN source_connections cn ON cn.id=c.connection_id ORDER BY cn.external_account,c.source_external_id,m.occurred_at,m.body")).scalars().all()
 outputs.append(dict(changed=changed,cursor=cursor,records=records,evidence=evidence,people=people,candidates=candidates,messages=messages))
(root/'reference-results.json').write_text(json.dumps(outputs,ensure_ascii=False))
'''

if __name__=='__main__':
    with ReferenceBackend('multisource') as ref:
        (ref.root/'scenario.json').write_text(json.dumps(scenario()))
        (ref.root/'probe.py').write_text(REFERENCE)
        env=dict(ref.environment,PYTHONPATH=str(ref.root/'services/api'))
        subprocess.run([str(ref.python),str(ref.root/'probe.py'),str(ref.root)],env=env,check=True)
        subprocess.run([str(PG/'createdb'),'-h',str(ref.root),'rust_connections'],check=True)
        rust=json.loads(subprocess.check_output([str(ROOT/'target/debug/examples/connection_probe'),str(ref.root/'scenario.json')],env=dict(os.environ,DATABASE_URL=f'postgresql:///rust_connections?host={ref.root}')))
        reference=json.loads((ref.root/'reference-results.json').read_text())
        differences=[]
        for i,(expected,actual) in enumerate(zip(reference,rust)):
            for key in expected:
                if expected[key]!=actual[key]:differences.append(dict(step=i,field=key,expected=expected[key],actual=actual[key]))
        result=dict(steps=len(reference),observations=sum(len(v) for v in reference),differences=differences)
        (ROOT/'.autonomous/complete-monica/connections-differential.json').write_text(json.dumps(result,ensure_ascii=False,indent=2))
        if differences:raise AssertionError(json.dumps(differences,ensure_ascii=False,indent=2))
        print(f'Real pinned multi-source differential passed: {result["steps"]} steps / {result["observations"]} observations')

#!/usr/bin/env python3
"""Real pinned Python/Rust HTTP contracts, default AI disabled; no model/accounts.

Reuses the fresh four-database reference harness and synthetic auth client. Both
backend lifecycles and all data belong to this invocation's temporary directory.
"""
import importlib.util
from pathlib import Path

spec=importlib.util.spec_from_file_location('profile_diff',Path(__file__).with_name('profile-differential.py'))
module=importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

def scenario(base):
    c=module.Client(base)
    credentials={'email':'ai-adapter@example.invalid','password':'synthetic-ai-adapter-password'}
    c.request('/api/v1/auth/bootstrap',credentials,record=False)
    c.csrf=c.request('/api/v1/auth/csrf',record=False)['csrf_token']
    c.request('/api/v1/auth/login',credentials,record=False)
    profile=c.request('/api/v1/profiles',{'display_name':'Synthetic AI Adapter Person'},expected=201,record=False)
    peer=c.request('/api/v1/profiles',{'display_name':'Synthetic AI Adapter Peer'},expected=201,record=False)
    c.ids[profile['id']]='$profile'
    c.ids[peer['id']]='$peer'
    pid=profile['id'];root='/api/v1/profiles/'+pid
    for path in ['/api/v1/ai/status','/api/v1/ai/profiles/'+pid,'/api/v1/ai/suggestions','/api/v1/ai/weekly-people','/api/v1/ai/weekly-highlights','/api/v1/digests','/api/v1/reconnect']:
        c.request(path)
    c.request('/api/v1/ai/profiles/'+pid+'/refresh',{},expected=404)
    c.request('/api/v1/ai/jobs/ai.profiles',{},expected=404)
    c.request('/api/v1/ai/image-ocr',{'enabled':True},expected=404,method='PATCH')
    c.request('/api/v1/digests/2026-01-05',expected=404)
    c.request('/api/v1/ai/suggestions/'+pid+'/missing',{'action':'ignore'},expected=404)
    c.request('/api/v1/ai/suggestions/'+pid+'/missing',expected=404,method='DELETE')
    c.request(root+'/linkedin-targets')
    saved=c.request(root+'/linkedin-targets',{'account':'https://www.linkedin.com/in/Synthetic-Fixture/?trk=synthetic'},method='PUT')
    c.request(root+'/linkedin-targets')
    c.request('/api/v1/profiles/'+peer['id']+'/linkedin-targets',{'account':'synthetic-fixture'},method='PUT',expected=409)
    c.request(root+'/linkedin-targets',{'account':'https://linkedin.com.evil/in/example'},method='PUT',expected=422)
    c.request('/api/v1/linkedin/collection-targets',expected=401)
    c.request(root+'/linkedin-targets/'+saved['account_id'],method='DELETE',expected=204)
    c.request(root+'/linkedin-targets')
    for state in ['done','dismissed']:
        c.request('/api/v1/reconnect/'+pid,{'state':state})
        c.request('/api/v1/reconnect/'+pid,method='DELETE')
    c.request('/api/v1/reconnect/'+pid,{'state':'snoozed'},expected=422)
    for row in c.results:
        for original,mapped in c.ids.items():row['operation']=row['operation'].replace(original,mapped)
    return c.results

module.scenario=scenario
module.DIFFERENTIAL_NAME="ai-adapter"
if __name__=='__main__':module.main()

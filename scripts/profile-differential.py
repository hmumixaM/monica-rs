#!/usr/bin/env python3
"""Compare real pinned Python and Rust profile/field/date/tag/relationship behavior.
Runs only fresh databases and synthetic data. UUIDs and generated times normalized;
response fields, user values, conflict responses and no-op identities are preserved.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.request
import urllib.error
from reference_backend import PG, ReferenceBackend

spec=importlib.util.spec_from_file_location('auth_diff',Path(__file__).with_name('auth-differential.py'))
auth=importlib.util.module_from_spec(spec)
spec.loader.exec_module(auth)

class Client(auth.Client):
    def request(self,path,body=None,expected=200,record=True,method=None):
        headers={'Content-Type':'application/json','X-CSRF-Token':self.csrf}
        req=urllib.request.Request(self.base+path,data=None if body is None else json.dumps(body,ensure_ascii=False).encode(),headers=headers,method=method)
        try: response=self.opener.open(req,timeout=30)
        except urllib.error.HTTPError as error: response=error
        with response:
            raw=response.read()
            try: payload=json.loads(raw) if raw else None
            except ValueError: payload=raw.decode()
            if expected is not None: assert response.status==expected,(req.get_method(),path,response.status,expected,payload)
            if record:
                self.results.append({'operation':req.get_method()+' '+path.split('?')[0].replace(self.base,''),'status':response.status,'body':self.normalize(payload)})
            return payload


def scenario(base):
    c=Client(base)
    credentials={'email':'profiles@example.invalid','password':'synthetic-profile-password'}
    c.request('/api/v1/auth/bootstrap',credentials,record=False)
    c.csrf=c.request('/api/v1/auth/csrf',record=False)['csrf_token']
    c.request('/api/v1/auth/login',credentials,record=False)
    p=c.request('/api/v1/profiles',{'display_name':'Synthetic Alpha','summary':'Fixture only'},expected=201)
    pid=p['id']; root='/api/v1/profiles/'+pid
    c.request(root)
    c.request(root+'/summary')
    for val in ('A','B','A','A'):
        c.request(root+'/fields',{'field_key':'display_name','value':val},method='PATCH')
    for val in ('0',0,0,'0'):
        c.request(root+'/fields',{'field_key':'synthetic.scalar','value':val},method='PATCH')
    for val in (42,'42'):
        c.request(root+'/fields',{'field_key':'synthetic.reverse','value':val},method='PATCH')
    revisions=c.request(root+'/history?field_key=display_name')
    c.request(root+'/history/'+revisions[-1]['id']+'/restore',{})
    c.request(root+'/details',{'field_key':' City ','value':' Chicago '},method='PUT')
    c.request(root+'/details',{'field_key':'Town','value':'Boston','replaces':[{'field_key':'City'}]},method='PUT')
    c.request(root+'/fields',{'field_key':'linkedin.experience','value':[{'company_name':'SyntheticCompany','title':'Engineer','start_date':'2020-01-01'}]},method='PATCH')
    c.request(root+'/fields',{'field_key':'linkedin.education','value':[{'school_name':'SyntheticSchool','degree':'Masters'}]},method='PATCH')
    c.request('/api/v1/profiles?company=SyntheticCompany')
    c.request('/api/v1/profiles?school=SyntheticSchool')
    c.request(root+'/structured-history')
    c.request(root)
    birthday={'kind':'birthday','precision':'month_day','month':2,'day':29,'label':'ignored'}
    date=c.request(root+'/dates',birthday,expected=201)
    c.request(root+'/dates',birthday,expected=201)
    c.request(root+'/dates',dict(birthday,precision='exact',year=2000),expected=409)
    c.request(root+'/dates/'+date['id'],dict(birthday,precision='exact',year=2000),method='PUT')
    peer=c.request('/api/v1/profiles',{'display_name':'Synthetic Beta'},expected=201)
    group=c.request('/api/v1/profiles',{'display_name':'Synthetic Group','profile_type':'group'},expected=201)
    edge={'to_profile_id':peer['id'],'relationship_type':'friend','note':'Fixture'}
    c.request(root+'/relationships',edge,expected=201)
    c.request(root+'/relationships',edge,expected=201)
    c.request(root+'/relationships')
    c.request('/api/v1/profiles/'+peer['id']+'/relationships')
    c.request('/api/v1/groups/'+group['id']+'/members',{'person_profile_id':pid,'role':'owner'},expected=201)
    c.request('/api/v1/groups/'+group['id']+'/members')
    tag=c.request('/api/v1/tags',{'name':'Fixture'},expected=201)
    c.request('/api/v1/tags',{'name':'Fixture'},expected=201)
    c.request(root+'/tags/'+tag['id'],{})
    c.request('/api/v1/profiles?tag='+tag['id'])
    c.request(root)
    c.request(root+'/summary')
    c.request(root+'/tags/'+tag['id'],method='DELETE',expected=204)
    c.request(root+'/dates/'+date['id'],method='DELETE',expected=204)
    c.request(root+'/details',{'refs':[{'field_key':'Town'}]},method='DELETE',expected=204)
    c.request(root)
    # Operation paths contain server-generated identifiers; compare their mapped identities.
    for row in c.results:
        for original,mapped in c.ids.items(): row['operation']=row['operation'].replace(original,mapped)
    return c.results


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline',choices=['crm','desktop'],default='crm')
    args=parser.parse_args()
    with ReferenceBackend(args.baseline) as reference:
        expected=scenario(reference.url)
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
                actual=scenario(url)
                differences=[{'observation':i,'expected':e,'actual':a} for i,(e,a) in enumerate(zip(expected,actual)) if e!=a]
                if differences:
                    output=Path('.autonomous/complete-monica/'+globals().get('DIFFERENTIAL_NAME','profile')+'-diff-'+args.baseline+'.json')
                    output.write_text(json.dumps(differences,ensure_ascii=False,indent=2))
                    raise AssertionError(f'{len(differences)} differences; first: {json.dumps(differences[0],ensure_ascii=False)}; full {output}')
                assert len(expected)==len(actual)
                print(f'Real {args.baseline}/Rust profile differential passed: {len(actual)} observations')
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                    try: process.wait(timeout=100)
                    except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=5)

if __name__=='__main__': main()

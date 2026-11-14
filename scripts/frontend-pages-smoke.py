#!/usr/bin/env python3
"""Isolated synthetic HTTP/projection regression for Rust page contracts."""
import datetime, http.cookiejar, json, os, secrets, shutil, socket, subprocess, tempfile, time, urllib.error, urllib.request, uuid
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
GOLDEN=json.loads((ROOT/'docs/contracts/page-presentation.json').read_text())
PG=Path(os.environ.get('MONICA_PG_BIN','/opt/homebrew/opt/postgresql@16/bin'))

def run(*args,**kwargs):return subprocess.run([str(a) for a in args],check=True,**kwargs)

def check_pages(origin,dsn,control_token,native_root):
    def sql(query):return run(PG/'psql',dsn,'-v','ON_ERROR_STOP=1','-Atc',query,capture_output=True,text=True).stdout.strip()
    jar=http.cookiejar.CookieJar();opener=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar));csrf=None
    def request(path,payload=None,method=None,status=200,headers=None,raw=False):
        h={'Origin':origin,'Content-Type':'application/json',**(headers or {})}
        if csrf:h['X-CSRF-Token']=csrf
        req=urllib.request.Request(origin+path,data=None if payload is None else json.dumps(payload).encode(),headers=h,method=method)
        try:response=opener.open(req,timeout=30)
        except urllib.error.HTTPError as e:response=e
        with response:
            data=response.read();assert response.status==status,(path,response.status,status,data[:500])
            return (data,response.headers) if raw else (json.loads(data) if data else None)
    credentials={'email':'pages@example.invalid','password':'synthetic-pages-password'}
    request('/api/v1/home',status=401)
    request('/api/v1/auth/bootstrap',credentials)
    csrf=request('/api/v1/auth/csrf')['csrf_token'];request('/api/v1/auth/login',credentials)
    p=request('/api/v1/profiles',{'display_name':'Synthetic Alice','summary':'careful search %_ literal'},status=201)['id']
    peer=request('/api/v1/profiles',{'display_name':'Synthetic Bob'},status=201)['id']
    group=request('/api/v1/profiles',{'display_name':'Synthetic Group','profile_type':'group'},status=201)['id']
    field=f'/api/v1/profiles/{p}/fields'
    for key,value in [('wechat.nickname','合成Alice'),('wechat.alias','Straße'),('monica.nickname','STRASSE'),('wechat.group_nicknames','one｜Group Alice\ntwo|group alice\nthree|合成Alice'),('linkedin.headline','Synthetic Engineer'),('linkedin.location','Chicago, Illinois, United States'),('linkedin.experience',[{'company_name':'Synthetic Company','title':'Engineer','is_current':True}]),('linkedin.education',[{'school_name':'Synthetic School','degree':'BSc'}])]:request(field,{'field_key':key,'value':value},method='PATCH')
    request(f'/api/v1/profiles/{peer}/fields',{'field_key':'linkedin.location','value':'Springfield'},method='PATCH')
    tag=request('/api/v1/tags',{'name':'SyntheticTag'},status=201)
    request(f'/api/v1/profiles/{p}/tags/{tag["id"]}',{})
    request(f'/api/v1/groups/{group}/members',{'person_profile_id':p,'role':'member'},status=201)
    request(f'/api/v1/groups/{group}/members',{'person_profile_id':peer,'role':'member'},status=201)
    account,post,story,conversation,otherconv,call,interaction=[str(uuid.uuid4()) for _ in range(7)]
    now=datetime.datetime.now(datetime.timezone.utc)
    today=sql("SELECT (now() AT TIME ZONE 'America/Chicago')::date")
    month=today[:7];year=int(today[:4])
    sql(f"""INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES('{account}','wechat','synthetic-pages','person','Synthetic Alice');
    INSERT INTO crm.source_account_links(account_id,profile_id) VALUES('{account}','{p}');
    INSERT INTO crm.conversations(id,provider,account,external_id,profile_id,conversation_type,first_message_at,last_message_at,message_count) VALUES('{conversation}','wechat','self','alice','{p}','direct',now()-interval '1 day',now(),3),('{otherconv}','wechat','self','bob','{peer}','direct',now(),now(),1);
    INSERT INTO crm.message_months(conversation_id,month,sent,received,first_message_at,last_message_at) VALUES('{conversation}','{month}',2,1,now()-interval '1 day',now()),('{otherconv}','{month}',0,1,now(),now());
    INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body) VALUES
    ('{uuid.uuid4()}','{conversation}','wechat','self','1','self',now(),'Synthetic outgoing'),('{uuid.uuid4()}','{conversation}','wechat','self','2','alice',now(),'Synthetic incoming'),('{uuid.uuid4()}','{conversation}','wechat','self','3','self',now()-interval '1 day','Earlier'),('{uuid.uuid4()}','{otherconv}','wechat','self','4','bob',now(),'Hello');
    INSERT INTO crm.contact_events(id,provider,profile_id,kind,external_id,occurred_at,content) VALUES('{call}','wechat','{p}','call','call',now(),'{{"media_type":"voice","direction":"in","status":"completed","duration_seconds":60}}');
    INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) VALUES
    ('{post}','wechat','{uuid.uuid4()}','{account}','{p}','post','post',now(),'{{"text":"Synthetic Chicago post","location":{{"latitude":"41.88","longitude":"-87.63","name":"Synthetic Chicago Place"}}}}','synthetic-post',1),
    ('{story}','wechat','{uuid.uuid4()}','{account}','{peer}','story','story',now(),'{{"text":"Synthetic story","coordinate_format":"chatlog_sns_xml","location":{{"latitude":"2.35","longitude":"48.85","name":"Synthetic Paris"}}}}','synthetic-story',1);
    INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_profile_id,occurred_at,content) VALUES('{interaction}','{post}','wechat','like','like','{peer}',now(),'{{}}');""")
    sql('INSERT INTO crm.message_months_utc SELECT * FROM crm.message_months')
    # Dedicated source-owned structured facts; arbitrary manual JSON is not a typed fact.
    import hashlib
    linkedin=str(uuid.uuid4())
    sql(f"INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES('{linkedin}','linkedin','synthetic-linkedin','person','Synthetic Alice');INSERT INTO crm.source_account_links(account_id,profile_id) VALUES('{linkedin}','{p}');")
    for kind,value in [('employment',{'company_name':'Synthetic Company','title':'Engineer','is_current':True}),('education',{'school_name':'Synthetic School','degree':'BSc'})]:
        payload=json.dumps(value,ensure_ascii=False,sort_keys=True,separators=(',',':'));digest=hashlib.sha256(payload.encode()).hexdigest();content,revision,obj=[str(uuid.uuid4()) for _ in range(3)]
        organization_kind='company' if kind=='employment' else 'school';name=value['company_name' if kind=='employment' else 'school_name'];normalized=' '.join(name.lower().split());entity=str(uuid.uuid5(uuid.NAMESPACE_URL,f'monica:{organization_kind}:{normalized}'))
        sql(f"INSERT INTO crm.organizations(id,kind,normalized_name,name) VALUES('{entity}','{organization_kind}','{normalized}','{name}');")
        sql(f"INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json) VALUES('{content}','structured_fact','{digest}',{len(payload.encode())},'{payload}');INSERT INTO crm.source_structured_revisions(id,source_account_id,fact_type,fact_key,entity_id,content_object_id,content_hash,value,source_type,source_record_id,object_id,source_version,observed_at) VALUES('{revision}','{linkedin}','{kind}','synthetic-{kind}','{entity}','{content}','{digest}','{payload}','linkedin','synthetic','{obj}',1,now());INSERT INTO crm.source_structured_current(source_account_id,fact_type,fact_key,revision_id) VALUES('{linkedin}','{kind}','synthetic-{kind}','{revision}');")
    # Count the real directory projection SQL, not just a cache wrapper callback.
    from concurrent.futures import ThreadPoolExecutor
    sql('SELECT pg_stat_statements_reset()')
    with ThreadPoolExecutor(max_workers=32) as executor:
        parallel=list(executor.map(lambda _:request('/api/v1/directory'),range(32)))
    assert all(value==parallel[0] for value in parallel)
    projection_calls="SELECT coalesce(sum(calls),0) FROM pg_stat_statements WHERE query LIKE '%FROM crm.profiles p LEFT JOIN crm.page_contact_stats%'"
    assert sql(projection_calls)=='1',sql(projection_calls)
    directory,headers=request('/api/v1/directory',raw=True);directory=json.loads(directory)
    assert sql(projection_calls)=='1'
    assert sql('SELECT count(*) FROM crm.cache_invalidations')=='0'
    assert sql('SELECT count(*) FROM crm.cache_flush_requests')=='0'
    def reverse_tokens(order):
        statements="BEGIN;SET LOCAL lock_timeout='5s';"+''.join(f"INSERT INTO crm.cache_invalidations VALUES(pg_current_xact_id(),'test-reverse','{subject}');SELECT pg_sleep(0.01);" for subject in order)+"COMMIT;"
        return sql(statements)
    with ThreadPoolExecutor(max_workers=2) as executor:
        list(executor.map(reverse_tokens,[(p,peer),(peer,p)]))
    assert sql("SELECT count(*) FROM crm.cache_versions WHERE domain='test-reverse'")=='2'
    assert sql('SELECT count(*) FROM crm.cache_invalidations')=='0'
    assert sql('SELECT count(*) FROM crm.cache_flush_requests')=='0'
    before=sql(f"SELECT version FROM crm.cache_versions WHERE domain='profile' AND subject='{p}'")
    sql(f"BEGIN;UPDATE crm.profiles SET display_name='Rolled back' WHERE id='{p}';ROLLBACK;")
    assert sql(f"SELECT version FROM crm.cache_versions WHERE domain='profile' AND subject='{p}'")==before
    peer_version=sql(f"SELECT version FROM crm.cache_versions WHERE domain='profile' AND subject='{peer}'")

    row=dict(zip(directory['columns'],next(r for r in directory['profiles'] if r[0]==p)))
    assert row['name']=='Synthetic Alice' and '合成Alice' in row['aliases'] and row['direct_message_count']==3,row
    assert directory['tags'][row['tag_ids'][0]][1]=='SyntheticTag'
    assert directory['companies'][row['company_ids'][0]][1]=='Synthetic Company'
    assert directory['schools'][row['school_ids'][0]][1]=='Synthetic School'
    request('/api/v1/directory',status=304,headers={'If-None-Match':'W/'+headers['ETag']})
    request(field,{'field_key':'display_name','value':'Synthetic Alice Updated'},method='PATCH')
    changed,changed_headers=request('/api/v1/directory',headers={'If-None-Match':headers['ETag']},raw=True)
    assert changed_headers['ETag']!=headers['ETag'] and b'Synthetic Alice Updated' in changed
    assert sql(f"SELECT version FROM crm.cache_versions WHERE domain='profile' AND subject='{peer}'")==peer_version
    assert sql(projection_calls)=='2',sql(projection_calls)

    presentation=GOLDEN['presentation']
    expected_aliases=presentation['aliases']
    expected_headline=presentation['headline']
    expected_city=presentation['city']
    changed_directory=json.loads(changed);changed_row=dict(zip(changed_directory['columns'],next(row for row in changed_directory['profiles'] if row[0]==p)))
    assert changed_row['aliases']==expected_aliases,(changed_row['aliases'],expected_aliases)
    assert changed_row['group_names']==presentation['group_names']
    assert changed_row['city']==expected_city
    assert changed_row['headline']==expected_headline
    posts=request('/api/v1/directory/posts');assert [r[0] for r in posts['posts']]==[post]
    home=request('/api/v1/home')['key_numbers'];assert home=={'contacts':2,'groups':1,'messages_30d':4,'active_contacts_30d':2},home
    summary=request('/api/v1/metrics/summary');assert summary['people']==2 and summary['posts']==1 and summary['stories']==1 and summary['revisions']>=6,summary
    people=request('/api/v1/metrics/people-overview');assert people['companies'][0]['name']=='Synthetic Company'
    assert request('/api/v1/metrics/snapshots')==[]
    assert request(f'/api/v1/profiles/{p}/metrics')==[]
    assert sql('SELECT crm.refresh_page_metrics()')=='t'
    first_metrics=request('/api/v1/metrics/snapshots')
    assert sql('SELECT crm.refresh_page_metrics()')=='f'
    assert request('/api/v1/metrics/snapshots')==first_metrics
    metrics=request(f'/api/v1/profiles/{p}/metrics');assert next(x for x in metrics if x['metric_key']=='timeline.count')['value']==1
    assert any(m['metric_key']=='global.profile_count' and m['value']==3 for m in request('/api/v1/metrics/snapshots'))
    overview=request(f'/api/v1/profiles/{p}/overview');assert overview['header']['name']=='Synthetic Alice Updated' and overview['messages']['total']==3 and overview['contact_events']['calls']['total']==1,overview
    assert overview['header']['headline']==expected_headline and overview['header']['city']==expected_city
    assert overview['groups'][0]['profile_id']==group and overview['recent_posts'][0]['id']==post
    company_id=directory['companies'][row['company_ids'][0]][0]
    assert overview['linkedin_experience'][0]['company_id']==company_id
    sql(f"UPDATE crm.organizations SET name='Renamed Synthetic Company' WHERE id='{company_id}'")
    renamed=request('/api/v1/directory');assert renamed['companies']==[[company_id,'Renamed Synthetic Company']]
    assert request(f'/api/v1/profiles/{p}/overview')['header']['company']=='Renamed Synthetic Company'
    assert request('/api/v1/insights')['top_companies'][0]['id']==company_id
    sql(f"UPDATE crm.organizations SET name='Synthetic Company' WHERE id='{company_id}'")
    insights=request('/api/v1/insights');assert len(insights['recent_contacts'])==2 and len(insights['recent_posters'])==2
    request('/api/v1/insights?dormant_days=17',status=422)
    calendar=request(f'/api/v1/insights/calendar?year={year}');assert next(d for d in calendar['days'] if d['date']==today)['people']==2,calendar
    day=request('/api/v1/insights/calendar/day?day='+today);a=next(x for x in day['people'] if x['profile_id']==p);assert a['messages']==2 and a['calls']==1,day
    ribbon=request(f'/api/v1/profiles/{peer}/ribbon');assert ribbon['months'][-1]['interactions']==1,ribbon
    orbit=request('/api/v1/insights/orbit');assert len(orbit['people'])==2 and orbit['communities'][0]['size']==2,orbit
    community=orbit['communities'][0]['id'];from urllib.parse import quote
    request('/api/v1/communities/'+quote(community,safe='')+'/name',{'label':'Synthetic Friends'},method='PUT')
    assert request('/api/v1/insights/orbit')['communities'][0]['label']=='Synthetic Friends'
    request('/api/v1/communities/'+quote(community,safe='')+'/name',method='DELETE')
    atlas=request('/api/v1/metrics/locations');assert atlas['post_records']==2 and atlas['profile_records']==1 and atlas['unresolved'][0]['label']=='Springfield',atlas
    assert any(abs(point['latitude']-48.85)<.001 and abs(point['longitude']-2.35)<.001 for point in atlas['points']),atlas
    assert request('/api/v1/metrics/locations?kind=post')['profile_records']==0
    assert request('/api/v1/metrics/locations?kind=profile')['post_records']==0
    map_tokens="SELECT md5(coalesce(string_agg(domain||subject::text||version::text,',' ORDER BY domain,subject),'')) FROM crm.cache_versions WHERE domain IN('profile','posts')"
    map_version=sql(map_tokens)
    sql(f"BEGIN;UPDATE crm.conversation_days SET sent=sent+1 WHERE conversation_id='{conversation}';UPDATE crm.conversation_days SET sent=sent-1 WHERE conversation_id='{conversation}';COMMIT;")
    assert sql(map_tokens)==map_version  # message refresh never invalidates maps
    assert request(f'/api/v1/profiles/{p}/locations')['place_count']>=1
    request('/api/v1/metrics/locations?provider=invalid',status=422)
    hits=request('/api/v1/search?q=Synthetic');assert {'profile','tag','timeline'}<=set(h['entity_type'] for h in hits)
    exact=request('/api/v1/search?q=%25_');assert len(exact)==1 and exact[0]['id']==p,exact
    # Statement transition tables apply corrections, tombstones and owner changes.
    sql(f"UPDATE crm.message_search SET tombstone=true WHERE conversation_id='{conversation}' AND external_id='2';UPDATE crm.contact_events SET content=content||'{{\"is_deleted\":true}}' WHERE id='{call}';UPDATE crm.social_interactions SET is_deleted=true WHERE id='{interaction}';UPDATE crm.timeline_events SET status='deleted',version=2 WHERE id='{post}';")
    day=request('/api/v1/insights/calendar/day?day='+today);a=next(x for x in day['people'] if x['profile_id']==p);assert a['messages']==1 and a['calls']==0,day
    assert request('/api/v1/directory/posts')['posts']==[]
    assert request(f'/api/v1/profiles/{peer}/ribbon')['months'][-1]['interactions']==0
    sql(f"UPDATE crm.message_search SET occurred_at=occurred_at-interval '1 day',sender='alice',tombstone=false WHERE conversation_id='{conversation}' AND external_id='2';DELETE FROM crm.message_search WHERE conversation_id='{otherconv}';")
    assert sql(f"SELECT sum(sent+received) FROM crm.conversation_days WHERE conversation_id='{conversation}'")=='3'
    assert sql(f"SELECT sum(sent+received) FROM crm.conversation_days WHERE conversation_id='{otherconv}'")=='0'
    assert sql("SELECT count(*) FROM crm.conversation_days WHERE sent<0 OR received<0")=='0'
    # Global city resolution must not apply one provider's country to another.
    request(field,{'field_key':'wechat.country','value':'中国'},method='PATCH')
    assert request('/api/v1/metrics/locations?kind=profile')['profile_records']==1
    assert request(f'/api/v1/profiles/{p}/locations')['points']==[]  # no fictional city visits
    assert request(f'/api/v1/profiles/{group}/locations')=={'profile_id':group,'total_posts_with_location':0,'place_count':0,'points':[],'field_sources':[]}
    visits=[{'name':'Archived place','lat':41.88,'lon':-87.63,'visits':4,'first':'2024-01-01T00:00:00Z','last':'2024-02-01T00:00:00Z','moments':[{'at':'2024-01-02T00:00:00Z','text':'Synthetic visit'}]},{'poiName':'Another place','latitude':41.880001,'longitude':-87.630001,'visits':2}]
    request(field,{'field_key':'wechat.location_history','value':{'places':visits}},method='PATCH')
    # Exact pinned Python reducer output; source hashes are recorded in GOLDEN.
    expected=GOLDEN['location_points']
    actual=request(f'/api/v1/profiles/{p}/locations')
    assert actual['points']==expected,(actual['points'],expected)
    assert actual['total_posts_with_location']==6 and actual['place_count']==2
    def native(path,body):
        req=urllib.request.Request(origin+'/internal/'+path,data=json.dumps(body).encode(),headers={'Content-Type':'application/json','Authorization':'Bearer '+control_token})
        with urllib.request.urlopen(req,timeout=30) as response:return json.load(response)
    native('source-state',{'provider':'apple_messages','enabled':True})
    request(f'/api/v1/profiles/{p}/identifiers',{'kind':'phone','value':'+15555550123','is_self':False})
    attachment=b'synthetic evidence attachment\n';digest=hashlib.sha256(attachment).hexdigest();folder=native_root/'apple_messages/attachments';folder.mkdir(parents=True);(folder/digest).write_bytes(attachment)
    primary={'external_id':'synthetic-browser-evidence','kind':'message','title':'Synthetic immutable evidence','body':'Synthetic evidence body with a saved promise.','raw_body':'Synthetic original evidence body.','occurred_at':'2026-01-02T12:00:00Z','thread_id':'synthetic-browser-thread','direction':'received','participants':[{'id':'synthetic-peer','name':'Synthetic native peer','role':'sender','phones':['+15555550123']}],'attachments':[{'local_blob':digest,'filename':'synthetic.txt','size':len(attachment)}]}
    records=[primary]+[{**primary,'external_id':f'synthetic-browser-page-{i}','title':f'Synthetic page {i}','body':f'Paged synthetic evidence {i}','attachments':[]} for i in range(44)]
    def send_records(records,position):return native('source-page',{'provider':'apple_messages','external_account':'local','display_name':'Synthetic native source','stream':'messages','records':records,'cursor':{'position':position},'page_complete':True})
    send_records(records,45)
    evidence=request('/api/v1/evidence?provider=apple_messages&limit=40')
    assert len(evidence)==40,evidence
    last=evidence[-1]
    from urllib.parse import urlencode
    older=request('/api/v1/evidence?'+urlencode({'provider':'apple_messages','limit':'40','before':last['occurred_at'],'before_id':last['id']}))
    assert len(older)==5 and len({row['id'] for row in evidence+older})==45
    primary_row=next(row for row in evidence+older if row['external_id']==primary['external_id']);evidence_id=primary_row['id']
    send_records([{**primary,'body':'Synthetic revised promise body.'}],46)
    assert request(f'/api/v1/evidence/{evidence_id}')['revision']==2
    assert request(f'/api/v1/evidence/{evidence_id}?revision=1')['body']==primary['body']
    matches=request('/api/v1/evidence?provider=apple_messages&q=promise');assert len(matches)==1 and matches[0]['id']==evidence_id
    assert request(f'/api/v1/evidence/{evidence_id}?revision=1&body_limit=10&body_offset=10')['body']==primary['body'][10:20]
    data,_=request(f'/api/v1/evidence/{evidence_id}/files/{digest}?revision=1',raw=True);assert data==attachment
    coverage=request(f'/api/v1/profiles/{p}/source-coverage');assert coverage['sources'][0]['records']==45,coverage
    # A prior Rust build stored incomplete digest facts. The real API must
    # supply the complete browser contract without dispatching a model call.
    sql("UPDATE crm.ai_settings SET enabled=true; INSERT INTO crm.weekly_digests(week_start,output,input_hash) VALUES('2026-01-05','{\"summary\":\"Synthetic old digest\",\"facts\":{\"sources\":[]}}','synthetic-old-digest')")
    recap=request('/api/v1/digests/2026-01-05');assert isinstance(recap['facts']['totals']['messages'],int) and recap['facts']['top_contacts']==[]
    print('PASS: 18 page routes, real data, directory dictionaries/ETag, exact rolling counts, calendar/corrections, ribbon, orbit/manual names, city resolution/maps, metrics/search; native evidence journal')
    return evidence_id

with tempfile.TemporaryDirectory(prefix='monica-rust-pages-',dir=os.environ.get('MONICA_TEST_TMP_ROOT')) as temp:
    root=Path(temp);db=root/'postgres';server=None
    with (root/'postgres.log').open('w') as log:
        run(PG/'initdb','-D',db,'--auth=trust','--no-locale','--encoding=UTF8',stdout=log,stderr=log)
        run(PG/'pg_ctl','-D',db,'-l',root/'postgres.log','-o',f"-c listen_addresses='' -c shared_preload_libraries=pg_stat_statements -k {root}",'-w','start',stdout=log,stderr=log)
    try:
        with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
        origin=f'http://127.0.0.1:{port}';dsn=f'postgresql:///postgres?host={root}'
        env={**os.environ,'MONICA_DATABASE_URL':dsn,'MONICA_SESSION_SECRET':secrets.token_hex(32),'MONICA_CONTROL_TOKEN':secrets.token_hex(32),'MONICA_ORIGIN':origin,'MONICA_MEDIA_ROOT':str(root/'media'),'MONICA_WEB_ROOT':str(ROOT/'apps/web/dist'),'MONICA_NATIVE_ROOT':str(root/'raw'),'MONICA_AI_ENABLED':'false','MONICA_ADAPTERS_ENABLED':'false','MONICA_START_PAUSED':'true'}
        binary=root/'monica-server';shutil.copy2(Path(os.environ.get('MONICA_TEST_SERVER',str(ROOT/'target/debug/monica-server'))),binary);run(PG/'psql',dsn,'-v','ON_ERROR_STOP=1','-c','CREATE EXTENSION pg_stat_statements',stdout=subprocess.DEVNULL);run(binary,'migrate',env=env)
        with (root/'server.log').open('w') as log:
            server=subprocess.Popen([str(binary),'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
            for _ in range(150):
                if server.poll() is not None:raise RuntimeError('isolated server exited')
                try:
                    with urllib.request.urlopen(origin+'/api/v1/ready',timeout=1):break
                except OSError:time.sleep(.1)
            else:raise RuntimeError('isolated server not ready')
            evidence_id=check_pages(origin,dsn,env['MONICA_CONTROL_TOKEN'],root/'raw')
            run('npm','run','test','--','tests/pages-real-api.spec.ts',cwd=ROOT/'apps/web',env={**os.environ,'MONICA_WEB_TEST_URL':origin,'MONICA_REAL_PAGES':'1','MONICA_TEST_EVIDENCE':evidence_id})
    except BaseException:
        print((root/'server.log').read_text()[-7000:] if (root/'server.log').exists() else 'migration failed')
        raise
    finally:
        if server and server.poll() is None:
            server.terminate()
            try:server.wait(timeout=120)
            except subprocess.TimeoutExpired:server.kill();server.wait(timeout=5)
        subprocess.run([str(PG/'pg_ctl'),'-D',str(db),'-m','fast','-w','stop'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)

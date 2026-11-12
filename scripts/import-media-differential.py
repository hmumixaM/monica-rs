#!/usr/bin/env python3
"""Real pinned signed inline-media imports; all databases/files are synthetic."""
import argparse, hashlib, importlib.util, json, os, signal, socket, subprocess, time, urllib.request
from pathlib import Path
from reference_backend import PG, ReferenceBackend
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('signed',Path(__file__).with_name('connection-import-differential.py'))
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
def scenario(url, root, db, rust):
    p='crm.' if rust else ''
    def query(q): return h.sql(root,db,q)
    def observe():
        assets=json.loads(query(f"SELECT coalesce(jsonb_agg(jsonb_build_array(sha256,media_type,byte_length) ORDER BY sha256),'[]') FROM {p}media_assets"))
        events='crm.timeline_events' if rust else "(SELECT id,provider,external_id FROM social_posts UNION ALL SELECT id,provider,external_id FROM social_stories)"
        links=json.loads(query(f"SELECT coalesce(jsonb_agg(jsonb_build_array(e.provider,e.external_id,l.entity_type,l.role,a.sha256) ORDER BY e.provider,e.external_id,l.role,a.sha256),'[]') FROM {p}media_links l JOIN {p}media_assets a ON a.id=l.media_id JOIN {events} e ON e.id=l.entity_id"))
        paths=json.loads(query(f"SELECT coalesce(jsonb_agg(jsonb_build_array(sha256,object_path) ORDER BY sha256),'[]') FROM {p}media_assets"))
        folder=root/('rust-media' if rust else 'media')
        for sha,path in paths: assert hashlib.sha256((folder/path).read_bytes()).hexdigest()==sha
        return dict(assets=assets,links=links)
    digest=hashlib.sha256(b'ABC').hexdigest()
    records=[{'external_id':'post','content':{'caption':'Synthetic'},'media':[{'bytes':'41 42\n43','media_type':'application/octet-stream'},{'sha256':digest},{'sha256':'f'*64}]}, {'external_id':'story','entity_type':'story','content':{'text':'Synthetic'},'media':[{'bytes':'414243','role':'cover'}]}]
    results=[]
    for step in range(3):
        if step==2:
            mid=query(f"SELECT id FROM {p}media_assets WHERE sha256='{digest}'")
            records=[{'external_id':'post','content':{'caption':'Synthetic'},'media':[{'media_id':mid,'sha256':'e'*64,'role':'by-id'},{'bytes':'44','role':'media-1'}]}]
        batch={'source':'monica','stream':'posts','batch_id':'inline' if step<2 else 'update','idempotency_key':'inline' if step<2 else 'update','observed_at':'2026-01-01T00:00:00Z','records':records}
        status,receipt=h.signed(url,batch,'inline-'+str(step));assert status==201,(status,receipt)
        if rust:
            for _ in range(400):
                done=query("SELECT count(*) FROM crm.import_record_media_progress WHERE complete")
                linked=query("SELECT count(*) FROM crm.media_links")
                if int(done)==(1 if step<2 else 2) and int(linked)==(3 if step<2 else 5):break
                time.sleep(.05)
            else:raise AssertionError(query('SELECT status,error_summary FROM crm.import_batches'))
        results.append(observe())
    batch['batch_id']='invalid';batch['idempotency_key']='invalid';batch['records'][0]['media']=[{'bytes':'4 1'}]
    status,_=h.signed(url,batch,'invalid');assert status==422,status
    results.append({'invalid_hex_status':status})
    return results
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--scheduler-lanes',action='store_true',help='Mac budget: verify independent fake profile/message processes');args=parser.parse_args()
    with ReferenceBackend('multisource') as ref:
        expected=scenario(ref.url,ref.root,'crm',False)
        subprocess.run([str(PG/'createdb'),'-h',str(ref.root),'rust_inline'],check=True)
        with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
        env={'PATH':os.defpath,'MONICA_DATABASE_URL':f'postgresql:///rust_inline?host={ref.root}','MONICA_SESSION_SECRET':'synthetic-reference-session-secret-32bytes','MONICA_IMPORT_HMAC_SECRET':h.SECRET,'MONICA_MEDIA_ROOT':str(ref.root/'rust-media'),'MONICA_CDC_ENABLED':'true'}
        if args.scheduler_lanes:
            for provider, command in [('LINKEDIN','sleep 60'),('LINKEDIN_MESSAGES','exit 0')]:
                env[f'MONICA_ADAPTER_{provider}_EXECUTABLE']='/bin/sh'
                env[f'MONICA_ADAPTER_{provider}_WORK_DIR']=str(ref.root)
                env[f'MONICA_ADAPTER_{provider}_ARGS_JSON']=json.dumps(['-c',command])
        binary=str(ROOT/'target/debug/monica-server');subprocess.run([binary,'migrate'],env=env,check=True)
        h.sql(ref.root,'rust_inline',"INSERT INTO crm.cdc_consumers(provider,slot_name,state) VALUES('database','synthetic_uninitialized','pending')")
        if args.scheduler_lanes:
            h.sql(ref.root,'rust_inline',"UPDATE crm.adapter_controls SET enabled=true,minimum_interval_seconds=3600 WHERE provider IN ('linkedin','linkedin_messages')")
        with (ref.root/'rust.log').open('wb') as log:
            process=subprocess.Popen([binary,'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
            try:
                for _ in range(200):
                    if process.poll() is not None:raise RuntimeError((ref.root/'rust.log').read_text())
                    try:urllib.request.urlopen(f'http://127.0.0.1:{port}/health',timeout=1).close();break
                    except OSError:time.sleep(.05)
                if args.scheduler_lanes:
                    for _ in range(200):
                        lanes=h.sql(ref.root,'rust_inline',"SELECT string_agg(provider||':'||status,',' ORDER BY provider) FROM crm.adapter_runs")
                        if lanes=='linkedin:running,linkedin_messages:complete':break
                        time.sleep(.05)
                    else:raise AssertionError(('independent lanes',lanes))
                    print('PASS independent message lane: fake message collector completed while profile collector still running')
                actual=scenario(f'http://127.0.0.1:{port}',ref.root,'rust_inline',True)
                assert h.sql(ref.root,'rust_inline',"SELECT state||':'||error_code FROM crm.cdc_consumers")== 'error:capture_failed'
                assert h.sql(ref.root,'rust_inline',"SELECT count(*) FROM pg_replication_slots WHERE database=current_database()")== '0'
                print('PASS optional CDC error isolation: uninitialized consumer records capture_failed, no slot created, signed imports remain operational')
                assert actual==expected,json.dumps({'expected':expected,'actual':actual},indent=2)
                print('PASS actual pinned signed media: inline hex, real file digest, SHA dedup, media_id priority, additive post/story links, receipt replay, metadata-only no fake asset, invalid hex422; four observations')
            finally:
                process.send_signal(signal.SIGTERM)
                try:process.wait(timeout=20)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

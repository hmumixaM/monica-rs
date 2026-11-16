"""Download and link SNS photos when a source page is imported.

Completed image identities are never polled or periodically checked. Failed
source downloads remain visible and recorded; only a new image identity or an
explicit repair starts another attempt. Decoded, uncommitted batches resume
without another network request when a source page is replayed.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
from types import SimpleNamespace


def module(name, path):
    spec=importlib.util.spec_from_file_location(name,path)
    result=importlib.util.module_from_spec(spec);spec.loader.exec_module(result)
    return result


def atomic(path, value):
    temporary=path.with_name(path.name+'.partial')
    with temporary.open('w')as output:
        json.dump(value,output);output.flush();os.fsync(output.fileno())
    os.replace(temporary,path)


def import_event(db, source, media, state, runtime, cache, event_id):
    scripts=Path(__file__).resolve().parent
    fetch=module('sns_source_frame',scripts.parent/'tools/sns-decrypt/fetch_frame.py')
    publisher=module('sns_verified_publisher',scripts/'publish-sns-verified-images.py')
    bindings=module('sns_url_bindings',scripts/'sns-media-bindings.py')
    state=Path(state);state.mkdir(parents=True,exist_ok=True)
    assert state.resolve()==state and media.resolve()==media and source.resolve()==source
    row=db.execute("SELECT external_id,content FROM crm.timeline_events WHERE id=%s AND provider='wechat'",(event_id,)).fetchone()
    if not row:raise ValueError('sns_import_event_missing')
    external,content=row
    identities=[]
    for photo in content.get('media',[]):
        if photo.get('media_type')!='image':continue
        value=[str(event_id),photo.get('role'),photo.get('source_media_id'),photo.get('source_md5'),photo.get('source_url'),photo.get('thumbnail_url')]
        key=hashlib.sha256(json.dumps(value,sort_keys=True).encode()).hexdigest()
        identities.append((key,photo))
    if not identities:return dict(ready=0,failed=0)
    event_state=state/str(event_id);event_state.mkdir(exist_ok=True)
    ledger=event_state/'photos.json'
    saved=json.loads(ledger.read_text())if ledger.exists()else{}
    # Only a changed/new source image enters the resolver. Comment revisions
    # do not trigger filesystem checks or downloads of finished photos.
    pending=[(key,photo)for key,photo in identities if saved.get(key,{}).get('status')not in('ready','failed')]
    if not pending:return dict(ready=0,failed=0)
    bindings.repair_event(db,media,event_id)
    staged=[];failed=0;ready=0
    stage=event_state/'stage';objects=stage/'objects';receipt=event_state/'receipt'
    objects.mkdir(parents=True,exist_ok=True);receipt.mkdir(exist_ok=True)
    for key,photo in pending:
        if saved.get(key,{}).get('status')=='downloaded':
            staged.append((key,saved[key]['row']));continue
        found=db.execute("SELECT 1 FROM crm.media_links l JOIN crm.media_provenance p ON p.media_id=l.media_id WHERE l.entity_type='social_post' AND l.entity_id=%s AND l.role=%s AND p.source_url=%s LIMIT 1",(event_id,photo['role'],photo.get('source_url'))).fetchone()
        if found:
            saved[key]=dict(status='ready');ready+=1;atomic(ledger,saved);continue
        if bindings.link_existing(db,media,event_id,photo['role'],photo.get('source_url')):
            saved[key]=dict(status='ready');ready+=1;atomic(ledger,saved);continue
        role=photo['role'];slot=int(role.removeprefix('media-'))
        try:
            header,data=fetch.frame(source,external,slot,cache_root=cache,source_tid=content.get('source_tid'))
            encoded=json.dumps(header).encode()
            private_frame=struct.pack('>I',len(encoded))+encoded+data
            decoded=subprocess.run([str(runtime/'node-run'),str(scripts.parent/'tools/sns-decrypt/decrypt.cjs'),str(runtime/'wasm')],input=private_frame,capture_output=True,timeout=45)
            if decoded.returncode:raise ValueError('sns_decode_failed')
            checked=subprocess.run([str(runtime/'validate')],input=decoded.stdout,capture_output=True,timeout=20)
            if checked.returncode:raise ValueError('sns_pixels_failed')
            asset=json.loads(checked.stdout)
            target=objects/asset['sha256']
            if not target.exists():
                with target.open('xb')as output:output.write(decoded.stdout);output.flush();os.fsync(output.fileno())
            header.pop('seed',None)
            header.update(event_id=str(event_id),media=asset,full_pixels=True)
            saved[key]=dict(status='downloaded',row=header);staged.append((key,header))
        except Exception as error:
            # Exceptions from HTTP/XML may include image credentials. Persist
            # only the exception class, never its text, request URL or stderr.
            saved[key]=dict(status='failed',code=type(error).__name__);failed+=1
        atomic(ledger,saved)
    if staged:
        manifest=stage/'manifest.json';atomic(manifest,dict(rows=[row for _,row in staged]))
        schema=db.execute('SELECT max(version)FROM _sqlx_migrations WHERE success').fetchone()[0]
        if schema not in(86,87,88):raise ValueError('sns_schema_changed')
        args=SimpleNamespace(stage=stage,media=media,state=receipt,validator=runtime/'validate',manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest(),expected_records=len(staged),expected_system_id='7707886811981668386',expected_schema=schema)
        # Lock contention or uncertain publication leaves decoded rows pending.
        # The caller does not ACK this source page until publication succeeds.
        publisher.publish(db,args)
        for key,_ in staged:saved[key]=dict(status='ready');ready+=1
        atomic(ledger,saved)
    return dict(ready=ready,failed=failed)

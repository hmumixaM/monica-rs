"""Download and link SNS photos and native videos when a source page is imported.

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
from monica_imports.sns_publication import DatabaseTarget, Publication, publish
from monica_imports.source_media import link_existing


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
    state=Path(state);state.mkdir(parents=True,exist_ok=True)
    assert state.resolve()==state and media.resolve()==media and source.resolve()==source
    row=db.execute("SELECT external_id,content FROM crm.timeline_events WHERE id=%s AND provider='wechat'",(event_id,)).fetchone()
    if not row:raise ValueError('sns_import_event_missing')
    external,content=row
    identities=[]
    for photo in content.get('media',[]):
        if photo.get('media_type')not in('image','video'):continue
        value=[str(event_id),photo.get('role'),photo.get('source_media_id'),photo.get('source_md5'),photo.get('source_url'),photo.get('thumbnail_url')]
        key=hashlib.sha256(json.dumps(value,sort_keys=True).encode()).hexdigest()
        identities.append((key,photo))
        if photo.get('media_type')=='video'and photo.get('thumbnail_url'):
            poster={**photo,'role':photo['role']+'-poster','media_type':'image','source_url':photo['thumbnail_url'],'thumbnail_url':None,'_slot':int(photo['role'].removeprefix('media-')),'_thumbnail':True}
            value=[str(event_id),poster['role'],poster.get('source_media_id'),poster.get('source_md5'),poster['source_url'],None]
            identities.append((hashlib.sha256(json.dumps(value,sort_keys=True).encode()).hexdigest(),poster))
    if not identities:return dict(ready=0,failed=0)
    event_state=state/str(event_id);event_state.mkdir(exist_ok=True)
    ledger=event_state/'photos.json'
    saved=json.loads(ledger.read_text())if ledger.exists()else{}
    # Only a changed/new source image enters the resolver. Comment revisions
    # do not trigger filesystem checks or downloads of finished photos.
    pending=[(key,photo)for key,photo in identities if saved.get(key,{}).get('status')not in('ready','failed','source_only')]
    if not pending:return dict(ready=0,failed=sum(saved.get(key,{}).get('status')=='failed'for key,_ in identities))
    staged=[];failed=0;ready=0
    stage=event_state/'stage';objects=stage/'objects';receipt=event_state/'receipt'
    objects.mkdir(parents=True,exist_ok=True);receipt.mkdir(exist_ok=True)
    for key,photo in pending:
        if photo['media_type']=='video' and not photo.get('source_url'):
            saved[key]=dict(status='source_only',code='source_has_cover_only');atomic(ledger,saved);continue
        if saved.get(key,{}).get('status')=='downloaded':
            staged.append((key,saved[key]['row']));continue
        if link_existing(db,media,event_id,photo['role'],photo.get('source_url'),photo['media_type']):
            saved[key]=dict(status='ready');ready+=1;atomic(ledger,saved);continue
        role=photo['role'];slot=photo.get('_slot')or int(role.removeprefix('media-'))
        try:
            if photo['media_type']=='video':
                video=module('sns_video_stream',scripts/'sns-video.py')
                header=video.obtain(fetch,source,external,slot,cache,content.get('source_tid'),stage,runtime,source_media_id=photo.get('source_media_id'))
            else:
                header,data=fetch.frame(source,external,slot,cache_root=cache,source_tid=content.get('source_tid'),thumbnail=photo.get('_thumbnail',False),source_media_id=photo.get('source_media_id'),source_kind=photo.get('source_kind'))
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
                header.update(media=asset,full_pixels=True)
            header.update(event_id=str(event_id),asset_role=role)
            saved[key]=dict(status='downloaded',row=header);staged.append((key,header))
        except Exception as error:
            # Exceptions from HTTP/XML may include image credentials. Persist
            # only the exception class, never its text, request URL or stderr.
            saved[key]=dict(status='failed',code=type(error).__name__);failed+=1
        atomic(ledger,saved)
    if staged:
        manifest=stage/'manifest.json';atomic(manifest,dict(rows=[row for _,row in staged]))
        request = Publication(
            stage=stage, media=media, state=receipt, validator=runtime/'validate',
            manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest(),
            expected_records=len(staged), target=DatabaseTarget.read(db), runtime=runtime,
        )
        # Lock contention or uncertain publication leaves decoded rows pending.
        # The caller does not ACK this source page until publication succeeds.
        publish(db, request)
        for key,_ in staged:saved[key]=dict(status='ready');ready+=1
        atomic(ledger,saved)
    return dict(ready=ready,failed=sum(saved.get(key,{}).get('status')=='failed'for key,_ in identities))

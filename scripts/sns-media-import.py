"""Download and link SNS photos and native videos when a source page is imported.

Completed media identities are reused. The live importer schedules another
attempt for failed downloads. Decoded, uncommitted batches resume without
another network request when publication is retried.
"""
from contextlib import closing
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
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
    directory=os.open(path.parent,os.O_RDONLY|os.O_DIRECTORY)
    try:os.fsync(directory)
    finally:os.close(directory)


def source_motion(fetch, source, external, content):
    expected_hash = content.get('raw_xml_sha256')
    if not expected_hash:
        return []
    with fetch.read_post(source, external, content.get('source_tid')) as xml:
        if not ('<LivePhoto' in xml if isinstance(xml, str) else b'<LivePhoto' in xml):
            return []
        if fetch.xml_hash(xml) != expected_hash:
            return []  # The source reader will enqueue the new revision.
        _, obj = fetch.post_xml(xml)
    if obj is None or obj.findtext('id') != external:
        raise ValueError('live photo post identity mismatch')
    parents = {photo.get('role'): photo for photo in content.get('media', [])}
    return [motion for motion in fetch.live_assets(obj)
            if (parent := parents.get(f"media-{motion['_slot']}"))
            and parent.get('media_type') == 'image'
            and parent.get('source_media_id') == motion['parent_media_id']]


def historical_motion_events(db, source, stopped):
    scripts = Path(__file__).resolve().parent
    fetch = module('sns_source_frame', scripts.parent/'tools/sns-decrypt/fetch_frame.py')
    with closing(sqlite3.connect(source.as_uri() + '?mode=ro', uri=True)) as snapshot:
        # JSON text and CDN credentials stay in the local source snapshot.
        tables = snapshot.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='SnsTimeLine'").fetchone()
        if not tables:
            return None
        candidates = snapshot.execute("SELECT content FROM SnsTimeLine WHERE instr(content, '<LivePhoto') > 0")
        pending = []
        while rows := candidates.fetchmany(100):
            if stopped():
                return None
            posts = {}
            for (xml,) in rows:
                try:
                    _, obj = fetch.post_xml(xml)
                except fetch.ET.ParseError:
                    continue
                if obj is not None and (external := obj.findtext('id')):
                    posts[external] = (fetch.xml_hash(xml), {motion['role'] for motion in fetch.live_assets(obj)})
            if not posts:
                continue
            events = db.execute("""
                SELECT e.id, e.external_id, e.content->>'raw_xml_sha256',
                       ARRAY(SELECT l.role FROM crm.media_links l
                             JOIN crm.media_assets a ON a.id=l.media_id
                             WHERE l.entity_type='social_post' AND l.entity_id=e.id
                             AND l.role LIKE 'live-media-%%' AND a.media_type='video/mp4')
                FROM crm.timeline_events e
                WHERE e.provider='wechat' AND e.event_type='post' AND e.status='active' AND e.external_id=ANY(%s)
            """, (list(posts),))
            pending.extend((str(event_id),) for event_id, external, digest, roles in events
                           if posts[external][0] == digest and posts[external][1].difference(roles))
    return pending


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
    for motion in source_motion(fetch, source, external, content):
        value = [str(event_id), motion['role'], motion['source_media_id'], motion['source_md5'], motion['source_url']]
        identities.append((hashlib.sha256(json.dumps(value).encode()).hexdigest(), motion))
    if not identities:return dict(ready=0,failed=0)
    event_state=state/str(event_id);event_state.mkdir(exist_ok=True)
    ledger=event_state/'photos.json'
    saved=json.loads(ledger.read_text())if ledger.exists()else{}
    # Comment revisions reuse finished photos. A failed slot is eligible again
    # once the caller's retry backoff has elapsed.
    pending=[(key,photo)for key,photo in identities if saved.get(key,{}).get('status')not in('ready','source_only')]
    if not pending:return dict(ready=0,failed=0)
    staged=[];ready=0
    stage=event_state/'stage';objects=stage/'objects';receipt=event_state/'receipt'
    objects.mkdir(parents=True,exist_ok=True);receipt.mkdir(exist_ok=True)
    for key,photo in pending:
        if photo['media_type']=='video' and not photo.get('source_url'):
            saved[key]=dict(status='source_only',code='source_has_cover_only');atomic(ledger,saved);continue
        if saved.get(key,{}).get('status')=='downloaded':
            staged.append((key,saved[key]['row']));continue
        live = photo['role'].startswith('live-media-')
        if live:
            archived = db.execute('''
                SELECT a.sha256,a.byte_length FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id
                WHERE l.entity_type='social_post' AND l.entity_id=%s AND l.role=%s AND a.media_type='video/mp4'
            ''', (event_id, photo['role'])).fetchall()
            if len(archived) == 1:
                digest, length = archived[0]
                path = media/'sha256'/digest[:2]/digest[2:4]/digest
                if path.is_file() and path.stat().st_size == length:
                    saved[key]=dict(status='ready');ready+=1;atomic(ledger,saved);continue
        if not live and link_existing(db,media,event_id,photo['role'],photo.get('source_url'),photo['media_type']):
            saved[key]=dict(status='ready');ready+=1;atomic(ledger,saved);continue
        role=photo['role'];slot=photo.get('_slot')or int(role.removeprefix('media-'))
        try:
            if photo['media_type']=='video':
                video=module('sns_video_stream',scripts/'sns-video.py')
                header=video.obtain(fetch,source,external,slot,cache,content.get('source_tid'),stage,runtime,source_media_id=photo.get('source_media_id'),live_motion=live)
                if live:
                    if (header['raw_xml_sha256'] != content['raw_xml_sha256']
                            or header['parent_media_id'] != photo['parent_media_id']
                            or header['source_md5'] != photo['source_md5']
                            or header['source_url'] != photo['source_url'].replace('http://', 'https://', 1)):
                        raise ValueError('live photo source revision changed')
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
            saved[key]=dict(status='failed',code=type(error).__name__)
        atomic(ledger,saved)
    if staged:
        manifest=stage/'manifest.json';atomic(manifest,dict(rows=[row for _,row in staged]))
        request = Publication(
            stage=stage, media=media, state=receipt, validator=runtime/'validate',
            manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest(),
            expected_records=len(staged), target=DatabaseTarget.read(db), runtime=runtime,
        )
        # Lock contention or uncertain publication leaves decoded rows pending.
        # The durable queue retains the event until publication succeeds.
        publish(db, request)
        for key,_ in staged:saved[key]=dict(status='ready');ready+=1
        atomic(ledger,saved)
    return dict(ready=ready,failed=sum(saved.get(key,{}).get('status')=='failed'for key,_ in identities))

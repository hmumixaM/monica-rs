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
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import Request, build_opener
import xml.etree.ElementTree as ET
from chatlog_moments import comment_images, parse_moments_xml
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
    try:
        with fetch.read_post(source, external, content.get('source_tid'), missing_ok=True) as xml:
            if not xml or '<LivePhoto' not in xml or fetch.xml_hash(xml) != expected_hash:
                return []
            _, obj = fetch.post_xml(xml)
    except (fetch.ET.ParseError, UnicodeError):
        return []  # Ready/static assets can still be reused.
    if obj is None or obj.findtext('id') != external:
        return []
    parents = {photo.get('role'): photo for photo in content.get('media', [])}
    return [motion for motion in fetch.live_assets(obj)
            if (parent := parents.get(f"media-{motion['_slot']}"))
            and parent.get('media_type') == 'image'
            and parent.get('source_media_id') == motion['parent_media_id']]


def archived_file(media, digest, length):
    if not digest or len(digest) != 64:
        return False
    path = media/'sha256'/digest[:2]/digest[2:4]/digest
    try:
        return path.is_file() and path.stat().st_size == length
    except FileNotFoundError:
        return False


def historical_motion_events(db, source, media, stopped):
    scripts = Path(__file__).resolve().parent
    fetch = module('sns_source_frame', scripts.parent/'tools/sns-decrypt/fetch_frame.py')
    with closing(sqlite3.connect(source.as_uri() + '?mode=ro', uri=True)) as snapshot:
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
                    xml = fetch.source_xml(xml)
                    _, obj = fetch.post_xml(xml)
                except (fetch.ET.ParseError, UnicodeError):
                    continue
                if obj is not None and (external := obj.findtext('id')):
                    posts[external] = (fetch.xml_hash(xml), list(fetch.live_assets(obj)))
            if not posts:
                continue
            events = db.execute("""
                SELECT e.id, e.external_id, e.content->>'raw_xml_sha256', e.occurred_at,
                       ARRAY(SELECT jsonb_build_object('role',l.role,'type',a.media_type,
                                   'digest',a.sha256,'bytes',a.byte_length,
                                   'urls',ARRAY(SELECT p.source_url FROM crm.media_provenance p
                                                WHERE p.media_id=a.id AND p.source_url IS NOT NULL))
                             FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id
                             WHERE l.entity_type='social_post' AND l.entity_id=e.id)
                FROM crm.timeline_events e
                WHERE e.provider='wechat' AND e.event_type='post' AND e.status='active' AND e.external_id=ANY(%s)
            """, (list(posts),))
            for event_id, external, digest, occurred, assets in events:
                source_hash, motions = posts[external]
                if source_hash != digest or not motions:
                    continue
                ready = {}
                for asset in assets:
                    if archived_file(media, asset['digest'], asset['bytes']):
                        ready.setdefault(asset['role'], []).append(asset)
                for motion in motions:
                    source_url = motion['source_url'].replace('http://', 'https://', 1)
                    motion_ready = any(asset['type'] == 'video/mp4' and
                                       any(url.replace('http://', 'https://', 1) == source_url for url in asset['urls'])
                                       for asset in ready.get(motion['role'], []))
                    still_ready = any(asset['type'].startswith('image/')
                                      for asset in ready.get(f"media-{motion['_slot']}", []))
                    if not (motion_ready and still_ready):
                        pending.append((str(event_id), -occurred.timestamp() if occurred else 0))
                        break
    return pending
def archived_xml(db, event):
    row = db.execute("""
        SELECT raw FROM (
            SELECT v.original->>'raw_content' AS raw, v.observed_at AS seen
            FROM crm.legacy_social_versions v WHERE v.event_id=%s
            UNION ALL
            SELECT o.payload_json->>'raw_content', v.created_at
            FROM crm.legacy_chatlog_versions v JOIN crm.content_objects o
              ON o.id=(v.saved_row->>'raw_object_id')::uuid
            WHERE v.entity_type='post' AND v.entity_id=%s AND v.saved_row ? 'raw_object_id'
        ) source WHERE raw IS NOT NULL ORDER BY seen DESC LIMIT 1
    """, (event, event)).fetchone()
    return row[0] if row else None


def comment_xml(raw, external, comment, author):
    if raw is None:
        return None
    root, _ = parse_moments_xml(raw)
    if root.findtext('TimelineObject/id') != external:
        return None
    for index, item in enumerate(root.findall('LocalExtraInfo/comment_user_list/user_comment')):
        username = item.findtext('username') or ''
        created = item.findtext('create_time')
        key = next((value for value in (item.findtext('comment_64id'), item.findtext('comment_id'))
                    if value and value != '0'), f'{username}:{created or index}')
        if f'{external}:comment:{key}' != comment or username != author or item.findtext('b_deleted') == '1':
            continue
        post = ET.Element('TimelineObject')
        ET.SubElement(post, 'id').text = external
        ET.SubElement(ET.SubElement(post, 'ContentObject'), 'mediaList').extend(comment_images(item))
        return ET.tostring(post, encoding='unicode')
    return None


def normalized_url(value):
    return value.replace('http://', 'https://', 1) if value else None


def public_image(fetch, photo, external, slot):
    """Fetch the exact stored CDN reference when a raw snapshot no longer contains it."""
    original = photo.get('source_url') or photo.get('thumbnail_url')
    options = [original]
    if not photo.get('source_md5') or photo.get('source_kind') == 'wechat_channels':
        options.append(photo.get('thumbnail_url'))
    for url in dict.fromkeys(options):
        if not url:
            continue
        url = normalized_url(url)
        parsed = urlsplit(url)
        host = parsed.hostname or ''
        if (parsed.scheme != 'https' or parsed.username or parsed.password or parsed.port not in (None, 443)
                or not (host in ('mmsns.c2c.wechat.com', 'mmsns.hk.wechat.com', 'wxapp.tc.qq.com', 'vweixinthumb.tc.qq.com')
                        or host.endswith(('.qpic.cn', '.qlogo.cn', '.video.qq.com')))):
            continue
        try:
            request = Request(url, headers={'User-Agent': 'MicroMessenger Client', 'Accept': 'image/*'})
            with build_opener(fetch.NoRedirect()).open(request, timeout=10) as response:
                data = response.read(32 * 1024 * 1024 + 1)
        except HTTPError:
            continue
        if len(data) <= 32 * 1024 * 1024 and data.startswith((b'\xff\xd8\xff', b'\x89PNG\r\n\x1a\n')):
            return dict(source_url=normalized_url(original), external_id=external, slot=slot,
                        source_media_id=photo.get('source_media_id'), source_md5=photo.get('source_md5'),
                        source_reference_sha256=hashlib.sha256(json.dumps(photo, sort_keys=True).encode()).hexdigest()), data
    raise ValueError('sns_public_image_unavailable')


def import_event(db, source, media, state, runtime, cache, event_id):
    scripts=Path(__file__).resolve().parent
    fetch=module('sns_source_frame',scripts.parent/'tools/sns-decrypt/fetch_frame.py')
    state=Path(state);state.mkdir(parents=True,exist_ok=True)
    assert state.resolve()==state and media.resolve()==media and source.resolve()==source
    row=db.execute("SELECT external_id,content FROM crm.timeline_events WHERE id=%s AND provider='wechat' AND status='active'",(event_id,)).fetchone()
    if not row:return dict(ready=0,failed=0)
    external,content=row
    identities=[]
    comments = db.execute("""
        SELECT id::text,external_id,author_external_id,metadata->'media' FROM crm.social_interactions
        WHERE post_id=%s AND provider='wechat' AND interaction_type IN ('comment','reply') AND NOT is_deleted
          AND jsonb_typeof(metadata->'media')='array' AND metadata->'media'<>'[]'
        ORDER BY id
    """, (event_id,)).fetchall()
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
        value = [str(event_id), motion['role'], motion['source_media_id'], motion['source_md5'],
                 motion['source_url'], content['raw_xml_sha256']]
        identities.append((hashlib.sha256(json.dumps(value).encode()).hexdigest(), motion))
    for interaction, comment, author, media_slots in comments:
        for photo in media_slots:
            if photo.get('media_type') != 'image':
                continue
            value = [str(event_id), interaction, photo.get('role'), photo.get('source_media_id'),
                     photo.get('source_md5'), photo.get('source_url'), photo.get('thumbnail_url')]
            key = hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()
            identities.append((key, {**photo, '_interaction': interaction, '_comment': comment, '_author': author}))
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
    raw_sources = None
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
                AND EXISTS (SELECT 1 FROM crm.media_provenance p WHERE p.media_id=a.id AND p.source_url=ANY(%s))
            ''', (event_id, photo['role'], [photo['source_url'], photo['source_url'].replace('http://','https://',1)])).fetchall()
            if len(archived) == 1:
                digest, length = archived[0]
                if archived_file(media, digest, length):
                    saved[key]=dict(status='ready');ready+=1;atomic(ledger,saved);continue
        if not live and link_existing(db, media, event_id, photo['role'], photo.get('source_url'),
                                      photo['media_type'], interaction_id=photo.get('_interaction')):
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
                if raw_sources is None:
                    try:
                        with fetch.read_post(source, external, content.get('source_tid'), missing_ok=True) as xml:
                            live_xml = xml
                    except (FileNotFoundError, ValueError, sqlite3.Error):
                        live_xml = None
                    raw_sources = tuple(dict.fromkeys(filter(None, (live_xml, archived_xml(db, event_id)))))
                header, data = None, None
                for raw in raw_sources:
                    try:
                        selected_xml = comment_xml(raw, external, photo['_comment'], photo['_author']) if photo.get('_interaction') else raw
                        if selected_xml is None:
                            continue
                        proof, payload = fetch.frame(source, external, slot, cache_root=cache,
                            thumbnail=photo.get('_thumbnail',False), source_media_id=photo.get('source_media_id'),
                            source_kind=photo.get('source_kind'), raw_xml=selected_xml)
                    except (HTTPError, ET.ParseError, ValueError, IndexError):
                        continue
                    expected = normalized_url(photo.get('source_url') or photo.get('thumbnail_url'))
                    if (proof['source_url'] == expected and proof['source_media_id'] == photo.get('source_media_id')
                            and proof['source_md5'] == photo.get('source_md5')):
                        if photo.get('_interaction'):
                            proof['raw_xml_sha256'] = fetch.xml_hash(raw)
                        header, data = proof, payload
                        break
                if header is None:
                    header, data = public_image(fetch, photo, external, slot)
                if not data.startswith((b'\xff\xd8\xff', b'\x89PNG\r\n\x1a\n')):
                    encoded=json.dumps(header).encode()
                    private_frame=struct.pack('>I',len(encoded))+encoded+data
                    decoded=subprocess.run([str(runtime/'node-run'),str(scripts.parent/'tools/sns-decrypt/decrypt.cjs'),str(runtime/'wasm')],input=private_frame,capture_output=True,timeout=45)
                    if decoded.returncode:raise ValueError('sns_decode_failed')
                    data=decoded.stdout
                checked=subprocess.run([str(runtime/'validate')],input=data,capture_output=True,timeout=20)
                if checked.returncode:raise ValueError('sns_pixels_failed')
                asset=json.loads(checked.stdout)
                target=objects/asset['sha256']
                if not target.exists():
                    with target.open('xb')as output:output.write(data);output.flush();os.fsync(output.fileno())
                header.pop('seed',None)
                header.update(media=asset,full_pixels=True)
            header.update(event_id=str(event_id),asset_role=role)
            if photo.get('_interaction'):
                header['interaction_id'] = photo['_interaction']
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

#!/usr/bin/env python3
"""Bridge the pinned bundled collectors' complete snapshots to Rust JSONL.

Default action reads completed snapshots only. --collect explicitly runs one
vendored collector cycle, retaining its per-provider cooldowns/platform budgets.
No sessions/configurations are discovered outside the explicit --data-root.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
import fcntl
from datetime import datetime, timezone
import uuid

MAX_FILE = 4 * 1024 * 1024
MAX_FRAME = MAX_FILE - 1024
MAX_SNAPSHOT_JSON = 128 * 1024 * 1024
MEDIA_CACHE={}
SPOOL=None
_formats_spec=importlib.util.spec_from_file_location('monica_collector_formats',Path(__file__).with_name('collector_formats.py'))
formats=importlib.util.module_from_spec(_formats_spec)
_formats_spec.loader.exec_module(formats)

class SpoolWriter:
    """Atomic, immutable IPC pages. A reader acknowledges only after DB commit."""
    def __init__(self,directory,data_root):
        self.directory=directory.resolve();self.data_root=data_root.resolve(strict=True);self.directory.mkdir(parents=True,exist_ok=True)
        self.output=None;self.temporary=None;self.digest=None;self.length=0;self.pages=[]
    def emit(self,frame):
        frame=dict(frame)
        if frame.get('type')=='media':
            path=Path(frame['path']).resolve(strict=True)
            if not path.is_relative_to(self.data_root):raise ValueError('media outside explicit source data root')
            frame['path']=path.relative_to(self.data_root).as_posix()
        data=(json.dumps(frame,ensure_ascii=False,separators=(',',':'))+'\n').encode()
        if len(data)>MAX_FRAME:raise ValueError('collector frame exceeds budget')
        if self.length and self.length+len(data)>4*1024*1024:self.flush()
        if self.output is None:
            fd,name=tempfile.mkstemp(prefix='.page-',suffix='.partial',dir=self.directory)
            self.temporary=Path(name);self.output=os.fdopen(fd,'wb');self.digest=hashlib.sha256()
        self.output.write(data);self.digest.update(data);self.length+=len(data)
    def sequence(self):
        with (self.directory/'.sequence.lock').open('a') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX)
            path=self.directory/'.sequence'
            previous=int(path.read_text()) if path.exists() else 0
            value=max(time.time_ns(),previous+1)
            temporary=self.directory/'.sequence.new'
            with temporary.open('w') as output:output.write(str(value));output.flush();os.fsync(output.fileno())
            os.replace(temporary,path)
            return value
    def flush(self):
        if self.output is None:return
        self.output.flush();os.fsync(self.output.fileno());self.output.close();self.output=None
        digest=self.digest.hexdigest();name=f'{self.sequence():020d}-{digest}.jsonl';path=self.directory/name
        os.replace(self.temporary,path);self.temporary=None
        marker=path.with_suffix('.ready');temporary=marker.with_suffix('.ready.new')
        with temporary.open('w') as output:json.dump(dict(version=1,sha256=digest,byte_length=self.length),output);output.flush();os.fsync(output.fileno())
        os.replace(temporary,marker)
        directory=os.open(self.directory,os.O_RDONLY)
        try:os.fsync(directory)
        finally:os.close(directory)
        self.pages.append(path);self.length=0
    def close(self,complete=True):
        if complete:self.flush()
        if self.output is not None:self.output.close();self.output=None
        if self.temporary is not None:self.temporary.unlink(missing_ok=True)

def emit(frame):
    data=json.dumps(frame,ensure_ascii=False,separators=(',',':'))
    if len(data.encode())>MAX_FRAME:raise ValueError('collector frame exceeds budget')
    if SPOOL is None:print(data,flush=True)
    else:SPOOL.emit(frame)

def load_json(path,max_bytes=MAX_FILE):
    if path.is_symlink() or not path.is_file() or path.stat().st_size>max_bytes:
        raise ValueError('invalid or oversized snapshot JSON')
    before=path.stat()
    document=json.loads(path.read_text(encoding='utf-8'))
    after=path.stat()
    if (before.st_ino,before.st_size,before.st_mtime_ns)!=(after.st_ino,after.st_size,after.st_mtime_ns):
        raise RuntimeError('snapshot changed during read')
    return document

def atomic_document(path,value):
    path.parent.mkdir(parents=True,exist_ok=True)
    fd,name=tempfile.mkstemp(prefix='.'+path.name,dir=path.parent)
    try:
        with os.fdopen(fd,'w') as output:json.dump(value,output,ensure_ascii=False,sort_keys=True,separators=(',',':'));output.flush();os.fsync(output.fileno())
        os.replace(name,path)
        directory=os.open(path.parent,os.O_RDONLY)
        try:os.fsync(directory)
        finally:os.close(directory)
    finally:Path(name).unlink(missing_ok=True)

def completed(destination):
    current=destination/'CURRENT'
    if not current.is_symlink():return None
    snapshot=current.resolve(strict=True)
    snapshots=(destination/'.snapshots').resolve()
    if not snapshot.is_relative_to(snapshots) or not (snapshot/'.crm-ready').is_file():
        raise ValueError('snapshot is incomplete or outside publication root')
    return snapshot

def media(path,entity,external,role,expected=None):
    path=path.resolve(strict=True)
    before=path.stat()
    if not path.is_file() or before.st_size<=0 or before.st_size>512*1024*1024:raise ValueError('invalid media size')
    signature=(str(path),before.st_size,before.st_mtime_ns,before.st_ino)
    digest=MEDIA_CACHE.get(signature)
    if digest is None:
        h=hashlib.sha256()
        with path.open('rb') as source:
            for block in iter(lambda:source.read(1024*1024),b''):h.update(block)
        digest=h.hexdigest()
        if before!=path.stat():raise RuntimeError('snapshot media changed while hashing')
        if expected and digest!=expected:raise ValueError('snapshot media SHA mismatch')
        MEDIA_CACHE[signature]=digest
        emit(dict(type='media',path=str(path),sha256=digest,byte_length=before.st_size))
    return dict(entity_type=entity,external_id=str(external),role=role,sha256=digest)

def record(external,account,kind,content,occurred=None,manifest=None):
    if isinstance(occurred,(float,int)):occurred=datetime.fromtimestamp(occurred,timezone.utc).isoformat()
    if occurred is not None and not isinstance(occurred,str):raise ValueError('invalid source time')
    return dict(external_id=str(external),source_account_external_id=str(account),entity_type=kind,
                occurred_at=occurred,content=content,_manifest=manifest or [])

def strings(value):
    if isinstance(value,str):yield value
    elif isinstance(value,dict):
        for v in value.values():yield from strings(v)
    elif isinstance(value,list):
        for v in value:yield from strings(v)

def linkedin_records(snapshot,owner_account=None):
    document=load_json(snapshot/'linkedin_enrichment.incoming.json',MAX_SNAPSHOT_JSON)
    assets={item['source_url']:item for item in document.get('local_assets',[]) if item.get('source_url')}
    def manifests(slots,entity,external):
        found=[];seen=set()
        for role,url in slots:
            if isinstance(url,str) and url in assets and (role,url) not in seen:
                seen.add((role,url));asset=assets[url];name=asset['local_path']
                if Path(name).name!=name:raise ValueError('invalid LinkedIn media path')
                item=media(snapshot/'assets'/name,entity,external,role,asset.get('content_hash'))
                item['source_url']=url;found.append(item)
        return found
    for item in document.get('profiles',[]):
        account=item.get('provider_user_id')
        if not account:raise ValueError('LinkedIn profile has no stable account')
        profile=item.get('profile') or {}
        content=dict(profile,profile_url=item.get('profile_url'),username=account,
                     display_name=profile.get('name') or profile.get('full_name') or account)
        profile_slots=[('linkedin_avatar',profile.get('avatar_url')),('linkedin_cover',profile.get('background_url'))]
        yield 'profiles',record(account,account,'profile',content,item.get('captured_at'),manifests(profile_slots,'source_account',account))
        for post in item.get('posts',[]):
            external=post.get('provider_post_id') or post.get('urn') or post.get('external_id') or post.get('id') or post.get('source_key') or post.get('url')
            if not external:raise ValueError('LinkedIn post has no stable identity')
            post_media=formats.linkedin_media(post)
            slots=[(m['role'],m['source_url']) for m in post_media]
            slots += [('thumbnail-'+m['role'],m['thumbnail_url']) for m in post_media if m.get('thumbnail_url')!=m['source_url']]
            if not slots:slots=[('media-'+str(i+1),url) for i,url in enumerate(dict.fromkeys(url for url in (post.get('image'),post.get('image_url')) if isinstance(url,str) and url in assets))]
            body=dict(post,text=post.get('text') or post.get('content') or '',source_url=post.get('url'),media=post_media)
            yield 'posts',record(external,account,'post',body,post.get('posted_at') or post.get('occurred_at'),manifests(slots,'social_post',external))
    for conversation in document.get('conversations',[]):
        account=conversation.get('account') or conversation.get('owner_id') or owner_account
        external=conversation.get('id') or conversation.get('external_id') or conversation.get('conversation_id') or conversation.get('conversation_urn')
        if not account or not external:raise ValueError('LinkedIn conversation lacks owner/identity')
        # Newer captures name each participant and give their profile address.
        names=conversation.get('participant_names') or {};urls=conversation.get('participant_urls') or {}
        for message in conversation.get('messages',[]):
            mid=message.get('id') or message.get('external_id') or message.get('message_urn')
            if not mid:raise ValueError('LinkedIn message lacks identity')
            at=message.get('delivered_at') or message.get('occurred_at')
            if isinstance(at,(float,int)):stamp=int(at if at>10**12 else at*1000)
            elif isinstance(at,str):
                parsed=datetime.fromisoformat(at.replace('Z','+00:00'))
                if parsed.tzinfo is None:raise ValueError('LinkedIn delivered time has no timezone')
                stamp=int(parsed.timestamp()*1000)
            else:raise ValueError('LinkedIn message has no delivered time')
            content=dict(message,account=account,conversation=external,body=message.get('body') or message.get('text') or '',sender=message.get('sender_urn') or message.get('sender') or '',sent_at_ms=stamp,
                         participants=[dict(id=person,name=names.get(person) or person,**({'profile_url':urls[person]} if person in urls else {})) if isinstance(person,str) else person for person in conversation.get('participants',[])],conversation_title=conversation.get('title'))
            row=record(mid,account,'message',content,at)
            row['conversation_type']='group' if len(conversation.get('participants',[]))>2 else 'direct'
            yield 'messages',row

def instagram_records(snapshot,kind):
    # The bundled Instaloader writer publishes plain .json profile/post nodes and
    # schema1 story/highlight manifests only after all listed media files exist.
    seen_posts=set();seen_profiles=set()
    # Primary Instaloader documents are authoritative. Raw-node files enrich
    # them and provide a fallback only when their primary document is absent.
    paths=sorted(snapshot.rglob('*.json'),key=lambda p:(p.name.endswith('.raw-node.json'),str(p)))
    for path in paths:
        if path.name.startswith('.') or any(p.is_symlink() for p in path.parents if p!=snapshot):
            continue
        if path.name.endswith('.relationships.json'):continue
        raw=load_json(path)
        if path.name.endswith('.manifest.json'):
            if raw.get('schema_version')!=1 or not raw.get('owner_id') or not raw.get('mediaid'):
                raise ValueError('invalid complete story manifest')
            files=raw.get('files') or []
            if not files or any(Path(name).name!=name or not (path.parent/name).is_file() or (path.parent/name).is_symlink() for name in files):
                raise ValueError('incomplete story media')
            content=formats.normalize_story(raw)
            supported=[name for name in files if Path(name).suffix.lower() in {'.jpg','.jpeg','.png','.webp','.mp4'}]
            if not supported or raw.get('is_video') and not any(Path(name).suffix.lower()=='.mp4' for name in supported):raise ValueError('incomplete story media types')
            attachments=[]
            for name in supported:
                thumbnail=raw.get('is_video') and Path(name).suffix.lower()!='.mp4'
                attachment=media(path.parent/name,'social_story',raw['mediaid'],'thumbnail-1' if thumbnail else 'media-1')
                attachment['source_url']=content['media'][0]['thumbnail_url' if thumbnail else 'source_url']
                attachments.append(attachment)
            yield 'stories',record(raw['mediaid'],raw['owner_id'],'story',content,formats.timestamp(raw.get('date_utc') or raw.get('taken_at')),attachments)
            continue
        node=raw.get('node',raw)
        typename=node.get('__typename') or (raw.get('instaloader') or {}).get('node_type')
        if typename in ('GraphImage','GraphVideo','GraphSidecar','Post') or node.get('shortcode') or node.get('code'):
            code=node.get('shortcode') or node.get('code') or (node.get('iphone_struct') or {}).get('code')
            raw_node=None;relationships={}
            if code:
                if not isinstance(code,str) or not re.fullmatch(r'[A-Za-z0-9_-]+',code):raise ValueError('invalid Instagram shortcode')
                raw_path=path.with_name('post-'+code+'.raw-node.json')
                if raw_path.is_file():
                    raw_node=raw if raw_path==path else load_json(raw_path)
                    extra=raw_node.get('node') or {}
                    if raw_node.get('shortcode')!=code or extra.get('shortcode',code)!=code or extra.get('id') and node.get('id') and str(extra['id'])!=str(node['id']):raise ValueError('Instagram raw sidecar identity mismatch')
                    node={**extra,**node}
                relationship_path=path.with_name('post-'+code+'.relationships.json')
                if relationship_path.is_file():
                    relationships=load_json(relationship_path)
                    if relationships.get('shortcode')!=code:raise ValueError('Instagram relationship sidecar identity mismatch')
            owner=node.get('owner') or node.get('user') or (node.get('iphone_struct') or {}).get('user') or {}
            account=owner.get('id') or owner.get('pk') or owner.get('username')
            external=node.get('id') or node.get('pk') or code
            if not account or not external:raise ValueError('Instagram post lacks stable owner/identity')
            if path.name.endswith('.raw-node.json') and (str(account),str(external)) in seen_posts:continue
            # A post's global shortcode binds the sidecar. Its captured owner
            # username is historical evidence and can predate an account rename.
            normalized=formats.normalize_post(node,relationships)
            files=[p for p in sorted(path.parent.glob(path.stem+'*')) if p.suffix.lower() in {'.jpg','.jpeg','.png','.webp','.mp4'} and not p.is_symlink()]
            attachments=[]
            for file,role,url in formats.instagram_file_slots(path,node,files,normalized):
                if role is None:continue
                attachment=media(file,'social_post',external,role)
                if url:attachment['source_url']=url
                attachments.append(attachment)
            content=dict(normalized,owner=owner,archive_document=raw,archive_node=node,archive_relationships=relationships)
            if raw_node is not None:content['archive_raw_node']=raw_node
            yield 'posts',record(external,account,'post',content,normalized.get('published_at'),attachments)
            seen_posts.add((str(account),str(external)))
        elif typename in ('GraphUser','Profile') or node.get('username'):
            raw_profile=None
            raw_path=path.parent/'profile.raw-node.json'
            if raw_path.is_file():
                raw_profile=raw if raw_path==path else load_json(raw_path)
                extra=raw_profile.get('node') or {}
                original_id=node.get('id') or node.get('userid')
                extra_id=extra.get('id') or extra.get('userid')
                if original_id and extra_id and str(original_id)!=str(extra_id):raise ValueError('Instagram raw profile identity mismatch')
                if not (original_id and extra_id) and node.get('username') and extra.get('username') and node['username'].casefold()!=extra['username'].casefold():raise ValueError('Instagram raw profile owner mismatch')
                node={**extra,**node}
            account=node.get('id') or node.get('userid') or node.get('username')
            if not account:raise ValueError('Instagram profile lacks stable identity')
            if path.name.endswith('.raw-node.json') and str(account) in seen_profiles:continue
            pictures=[p for p in sorted(path.parent.glob('*profile_pic*')) if p.suffix.lower() in {'.jpg','.jpeg','.png','.webp'} and not p.is_symlink()]
            attachments=[media(p,'source_account',account,'instagram_avatar') for p in pictures[-1:]]
            content=dict(node,display_name=node.get('full_name') or node.get('username') or str(account))
            content['archive_document']=raw
            if raw_profile is not None:content['archive_raw_node']=raw_profile
            relationships_path=path.parent/'profile.relationships.json'
            if relationships_path.is_file():
                relationships=load_json(relationships_path);identity=relationships.get('profile') or {}
                if identity.get('userid') and node.get('id') and str(identity['userid'])!=str(node['id']):raise ValueError('Instagram profile sidecar identity mismatch')
                if not (identity.get('userid') and node.get('id')) and identity.get('username') and node.get('username') and identity['username'].casefold()!=node['username'].casefold():raise ValueError('Instagram profile sidecar owner mismatch')
                content['archive_relationships']=relationships
            yield 'profiles',record(account,account,'profile',content,manifest=attachments)
            seen_profiles.add(str(account))

def batches(provider,root,owner_account=None):
    destinations=[('linkedin',root/'raw/linkedin')] if provider=='linkedin' else [(kind,root/'staging'/('instagram-'+kind)) for kind in ('current','stories','highlights')]
    for kind,destination in destinations:
        snapshot=completed(destination)
        if snapshot is None:continue
        records=linkedin_records(snapshot,owner_account) if provider=='linkedin' else instagram_records(snapshot,kind)
        streams={}
        for stream,item in records:
            items=streams.setdefault(stream,[])
            items.append(item)
            # Flush bounded chunks without holding the complete archive in memory.
            if len(items)>=100:yield provider,kind,snapshot,stream,items;streams[stream]=[]
        for stream,items in streams.items():
            if items:yield provider,kind,snapshot,stream,items

def publish(provider,root,owner_account=None):
    count=0
    for provider,kind,snapshot,stream,items in batches(provider,root,owner_account):
        # Split by encoded bytes too; one oversized source object is rejected,
        # never silently truncated. IDs depend on content, independent of run ID.
        pending=[]
        for item in items:
            trial=pending+[item]
            if len(json.dumps(trial,ensure_ascii=False).encode())>2*1024*1024:
                if not pending:raise ValueError('source object exceeds bridge budget')
                publish_batch(provider,kind,snapshot,stream,pending);count+=len(pending);pending=[]
            pending.append(item)
        if pending:publish_batch(provider,kind,snapshot,stream,pending);count+=len(pending)
        emit(dict(type='progress',phase='publishing',processed=count))
    emit(dict(type='progress',phase='complete',processed=count))
    return count

def publish_batch(provider,kind,snapshot,stream,records):
    manifests=[m for item in records for m in item.get('_manifest',[])]
    records=[{k:v for k,v in item.items() if k!='_manifest'} for item in records]
    canonical=json.dumps([snapshot.name,kind,stream,records,manifests],ensure_ascii=False,sort_keys=True,separators=(',',':')).encode()
    digest=hashlib.sha256(canonical).hexdigest()
    # Separate immutable snapshot streams have independent durable cursor chains.
    # Raw snapshot metadata includes the publication identity for replay/audit.
    batch=dict(source=provider,stream=stream,batch_id='collector-'+digest,
               idempotency_key=provider+':collector:'+digest,schema_version=1,
               observed_at=datetime.fromtimestamp(snapshot.stat().st_mtime,timezone.utc).isoformat(),
               records=records,raw_objects=[dict(kind='collector_snapshot',publication=snapshot.name,collector_kind=kind)],media_manifest=manifests)
    emit(dict(type='snapshot',batch=batch))

def publish_messages(root,connection_id=None):
    raw=root/'raw/linkedin/messages';processed=0
    if not raw.is_dir():return 0
    if connection_id is not None:uuid.UUID(connection_id)
    for account_root in sorted(raw.iterdir()):
        if not account_root.is_dir() or account_root.is_symlink():continue
        account=load_json(account_root/'account.json').get('account')
        if not isinstance(account,str) or not account:raise ValueError('LinkedIn message mailbox identity missing')
        pending=account_root/'pending'
        if not pending.is_dir():continue
        for page in sorted(pending.glob('*.json')):
            if processed>=25:return processed
            document=load_json(page);page_hash=hashlib.sha256(page.read_bytes()).hexdigest();ack=account_root/'acks'/(page.stem+'.ack')
            if ack.is_file() and not ack.is_symlink():
                saved=load_json(ack)
                if saved.get('page_sha256')==page_hash and saved.get('receipt_id'):
                    archive=account_root/'archive';archive.mkdir(exist_ok=True)
                    os.replace(page,archive/page.name);continue
            source=document.get('records')
            if not isinstance(source,list) or len(source)>1000:raise ValueError('invalid LinkedIn pending page')
            records=[]
            for item in source:
                external=item.get('external_id');thread=item.get('thread_id')
                if not isinstance(external,str) or not external or not isinstance(thread,str) or not thread:raise ValueError('LinkedIn message lacks stable thread/identity')
                row=record(external,account,'message',item,item.get('occurred_at'));row['conversation_external_id']=thread;row['conversation_type']='group' if item.get('is_group') else 'direct';records.append({k:v for k,v in row.items() if k!='_manifest'})
            digest=hashlib.sha256(json.dumps([account,records],ensure_ascii=False,sort_keys=True,separators=(',',':')).encode()).hexdigest()
            cached=account_root/'batches'/(digest+'.json')
            if cached.exists():batch=load_json(cached)
            else:
                batch=dict(source='linkedin',stream='messages',schema_version=1,batch_id='messages-'+digest,idempotency_key='linkedin:messages:'+digest,observed_at=datetime.fromtimestamp(page.stat().st_mtime,timezone.utc).isoformat(),records=records,raw_objects=[dict(kind='linkedin_message_page',page_sha256=page_hash)],media_manifest=[])
                if connection_id is not None:batch['connection_id']=connection_id
                atomic_document(cached,batch)
            if connection_id is not None and batch.get('connection_id')!=connection_id:raise ValueError('cached LinkedIn connection identity changed')
            emit(dict(type='connection_snapshot',account=account,batch=batch,page_sha256=page_hash,ack_path=ack.relative_to(root).as_posix()))
            processed+=1
    return processed

def collect_messages(root,collector_path):
    # Exactly one bounded cycle; the native/backend supervisor owns recurrence.
    # This lane uses messages-browser, independent of profile collector cooldowns.
    import asyncio
    path=collector_path.parent/'linkedin_messages.py'
    spec=importlib.util.spec_from_file_location('monica_bundled_linkedin_messages',path)
    module=importlib.util.module_from_spec(spec);sys.path.insert(0,str(path.parent));spec.loader.exec_module(module)
    asyncio.run(module.cycle(root/'collectors/linkedin',root/'raw/linkedin'))

def collect(provider,root,collector_path,username=None):
    spec=importlib.util.spec_from_file_location('monica_bundled_collector',collector_path)
    module=importlib.util.module_from_spec(spec)
    sys.path.insert(0,str(collector_path.parent));spec.loader.exec_module(module)
    # Keep the vendored collector's child processes in the managed process group.
    # Its cooldown/rate-limit/login/publication code remains the original code.
    def managed_child(child_root,args,env=None):
        with (child_root/'logs/collector.log').open('ab') as log:
            child=subprocess.Popen([sys.executable,*map(str,args)],cwd=child_root,env=env,stdout=log,stderr=log,start_new_session=False)
            while child.poll() is None:
                if module.STOP.wait(.2):
                    child.terminate()
                    try:child.wait(timeout=5)
                    except subprocess.TimeoutExpired:child.kill();child.wait()
                    raise InterruptedError('Collector stopped')
            if child.returncode:raise RuntimeError('Collector phase failed')
    module.run_child=managed_child
    module.emit=lambda **status:emit(dict(type='progress',phase='collecting',processed=0))
    args=[str(collector_path),'--provider',provider,'--data-root',str(root)]
    if username:args+=['--username',username]
    sys.argv=args+['once'];module.main()

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--provider',choices=['instagram','linkedin','wechat'],required=True)
    parser.add_argument('--data-root',type=Path,required=True)
    parser.add_argument('--collector',type=Path,default=Path(__file__).resolve().parents[1]/'apps/desktop/collectors/collector.py')
    parser.add_argument('--username');parser.add_argument('--account',help='Explicit owning LinkedIn account ID for conversation exports');parser.add_argument('--collect',action='store_true')
    parser.add_argument('--spool-dir',type=Path,help='Publish durable JSONL IPC pages instead of stdout; must be on the shared explicit data volume')
    parser.add_argument('--lane',choices=['profiles','messages'],default='profiles')
    parser.add_argument('--snapshot-root',type=Path,help='Explicit owned Chatlog standalone SQLite export directory')
    parser.add_argument('--self-id',help='Explicit authoritative own WeChat ID; omitted keeps direction unknown')
    parser.add_argument('--connection-id',help='Explicit source connection UUID for independent LinkedIn messages')
    args=parser.parse_args();os.umask(0o077);root=args.data_root.resolve(strict=True)
    if not root.is_absolute():raise ValueError('absolute data root required')
    global SPOOL
    if args.spool_dir is not None:SPOOL=SpoolWriter(args.spool_dir.resolve(),root)
    try:
        emit(dict(type='progress',phase='starting',processed=0))
        failure=None
        if args.provider=='wechat':
            if args.snapshot_root is None or not args.account:raise ValueError('Chatlog requires explicit --snapshot-root and --account')
            path=Path(__file__).with_name('collector-chatlog.py');spec=importlib.util.spec_from_file_location('monica_chatlog_snapshots',path);module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
            more=module.publish(root,args.snapshot_root,args.account,emit,atomic_document,load_json,args.self_id)
            emit(dict(type='progress',phase='collecting' if more else 'waiting',processed=0))
            if SPOOL is not None:SPOOL.close(True)
            return
        if args.lane=='messages' and args.provider!='linkedin':raise ValueError('messages lane requires LinkedIn')
        if args.collect:
            try:
                if args.lane=='messages':collect_messages(root,args.collector.resolve(strict=True))
                else:collect(args.provider,root,args.collector.resolve(strict=True),args.username)
            except Exception as exc:failure=type(exc).__name__
        if args.lane=='messages':publish_messages(root,args.connection_id)
        else:publish(args.provider,root,args.account)
        # Complete source pages are published even if a later collector phase
        # fails. Failed passes keep their honest failure status and checkpoints.
        if failure is not None:
            if SPOOL is not None:SPOOL.flush()
            raise RuntimeError('Collector pass incomplete; completed snapshots retained: '+failure)
    except BaseException:
        if SPOOL is not None:SPOOL.close(False)
        raise
    else:
        if SPOOL is not None:SPOOL.close(True)

if __name__=='__main__':main()

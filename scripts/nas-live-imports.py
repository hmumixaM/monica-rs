#!/usr/bin/env python3
"""Consume existing, read-only NAS collector archives into the isolated Rust copy.

No login, browser, conversation collection or model calls. New SNS image records
also resolve existing files or download their explicitly referenced CDN photos.
Social records
use the signed Rust batch endpoint. Chatlog uses the existing legacy reconciler
so already imported conversations, message IDs and edits are retained.
"""
import argparse
from contextlib import closing, contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import hmac
import http.client
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import sqlite3
import tempfile
import time
from urllib.parse import quote, urlsplit
import uuid

import psycopg

STOP = False
NAMESPACE = uuid.UUID('534b6268-97a1-4d99-a086-f8c9e3955815')


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()


def signature(path):
    s = path.stat()
    return [s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns]


def digest_file(path):
    h = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def atomic(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.' + path.name, dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(name, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        Path(name).unlink(missing_ok=True)


def stamp(value):
    if not value:
        return None
    return datetime.fromisoformat(str(value).replace('Z', '+00:00')).timestamp()


def postgres_content(value, path='', identity=False):
    """Render NUL visibly; the byte-exact source remains in raw-json."""
    if isinstance(value,str):
        if identity and '\0'in value:raise ValueError('source identity contains NUL')
        return value.replace('\0','\u2400'),[path]if'\0'in value else[]
    if isinstance(value,list):
        result=[];changed=[]
        for n,item in enumerate(value):
            normalized,fields=postgres_content(item,path+'/'+str(n),identity);result.append(normalized);changed.extend(fields)
        return result,changed
    if isinstance(value,dict):
        result={};changed=[]
        for key,item in value.items():
            if '\0'in key:raise ValueError('source JSON key contains NUL')
            identifier=key in {'id','pk','urn','sender','account','conversation','thread','username','userid','participants','shortcode','code'}or key.endswith(('_id','_urn','_url'))
            normalized,fields=postgres_content(item,path+'/'+key.replace('~','~0').replace('/','~1'),identifier);result[key]=normalized;changed.extend(fields)
        return result,changed
    return value,[]


def failure_code(error):
    message=str(error)
    if message.startswith('signed import rejected: HTTP '):return 'import_http_'+message.rsplit(' ',1)[-1]
    if message=='source identity contains NUL':return 'source_identity_nul'
    if message=='source JSON key contains NUL':return 'source_json_key_nul'
    if isinstance(error,FileNotFoundError):return 'source_file_missing'
    if isinstance(error,TimeoutError):return 'source_timeout'
    if isinstance(error,ValueError):return 'source_value_invalid'
    return type(error).__name__


def backup_with_progress(source,target,report,stall_seconds=120):
    advanced=time.monotonic();last_report=0;previous=None
    def progress(status,remaining,total):
        nonlocal advanced,last_report,previous
        now=time.monotonic()
        if STOP:raise InterruptedError('Chatlog backup cancelled')
        if status in(sqlite3.SQLITE_OK,sqlite3.SQLITE_DONE)and(previous is None or remaining<previous):advanced=now
        if now-advanced>=stall_seconds:raise TimeoutError('Chatlog backup made no progress')
        if now-last_report>=5:
            report(status,remaining,total);last_report=now
        previous=remaining
    # Pin one read snapshot. Read-only WAL connections can otherwise restart
    # each incremental backup step even when source business bytes are stable.
    source.execute('PRAGMA query_only=ON')
    source.execute('BEGIN')
    try:
        source.execute('PRAGMA schema_version').fetchone()
        source.backup(target,pages=128,sleep=0.05,progress=progress)
    finally:
        source.rollback()


class Consumer:
    def __init__(self, args, db):
        self.args, self.db = args, db
        self.bridge = module('nas_bridge', 'collector-bridge.py')
        self.legacy = module('nas_legacy', 'legacy-chatlog-import.py')
        self.state = args.state
        self.cutoff = stamp(args.since)
        self.journal = sqlite3.connect(self.state / 'consumer.sqlite')
        self.journal.execute('CREATE TABLE IF NOT EXISTS delivered(provider TEXT,stream TEXT,external_id TEXT,account TEXT,digest TEXT,receipt TEXT,PRIMARY KEY(provider,stream,external_id,account))')
        self.journal.execute('CREATE TABLE IF NOT EXISTS source_files(path TEXT PRIMARY KEY,signature TEXT)')
        self.journal.execute('CREATE TABLE IF NOT EXISTS consumer_state(key TEXT PRIMARY KEY,value TEXT)')
        self.last_path = None
        self.last_input_time = None
        self.last_input = None
        self.input_documents = self.input_bytes = 0
        self.last_input_progress = 0
        self.active_provider = None
        self.read_inputs = []
        self.bridge.load_json = self.load
        self.bridge.media = self.deferred_media
        self.counters = dict(batches=0, records=0, unchanged=0, media=0, chatlog_pages=0)
        self.checked_media = set()
        self.provider_status = {}

    def guard(self):
        row = self.db.execute("SELECT system_identifier::text,current_database(),(SELECT oid FROM pg_database WHERE datname=current_database()),(SELECT max(version) FROM _sqlx_migrations WHERE success) FROM pg_control_system()").fetchone()
        if tuple(row[:3]) != ('7707886811981668386', 'monica_rust', 16384) or row[3] not in (86,87,88):
            raise ValueError('isolated Rust target identity changed')
        if not (self.args.media.parent / '.monica-rs-ssd-target').read_bytes() == b'monica-rs-ssd-target-v1\n':
            raise ValueError('isolated SSD marker changed')
        if shutil.disk_usage(self.state).free < 512 * 1024 * 1024:
            raise ValueError('isolated SSD has insufficient space')

    def progress(self, phase, **extra):
        if extra.get('provider'):
            self.provider_status[extra['provider']] = dict(phase=phase,at=time.time(),**extra)
        atomic(self.state / 'status.json', canonical(dict(phase=phase, at=time.time(), pid=os.getpid(), counters=self.counters, providers=self.provider_status, **extra)))

    def load(self, path, max_bytes=4 * 1024 * 1024):
        before = signature(path)
        if path.is_symlink() or not path.is_file() or before[2] > max_bytes:
            raise ValueError('invalid source JSON budget or alias')
        raw = path.read_bytes()
        if signature(path) != before:
            raise ValueError('source JSON changed while reading')
        self.last_path = path
        self.last_input_time = before[3]/1_000_000_000
        key = hashlib.sha256(raw).hexdigest()
        self.last_input=(path,self.last_input_time,key,before)
        self.read_inputs.append(self.last_input)
        self.input_documents+=1;self.input_bytes+=len(raw)
        if time.monotonic()-self.last_input_progress>=5:
            self.progress('reading_source',provider=self.active_provider,input_documents=self.input_documents,input_bytes=self.input_bytes)
            self.last_input_progress=time.monotonic()
        # New captures must survive JSON/generator validation failures too.
        archive=self.state/'raw-json'/(key+'.json')
        if self.last_input_time>=self.cutoff and not archive.exists():atomic(archive,raw)
        try:return json.loads(raw)
        except Exception:
            if not archive.exists():atomic(archive,raw)
            raise

    def archive_inputs(self, inputs, kind):
        objects=[]
        for path,at,key,before in inputs:
            archive=self.state/'raw-json'/(key+'.json')
            if not archive.exists():
                raw=path.read_bytes()
                if signature(path)!=before or hashlib.sha256(raw).hexdigest()!=key:
                    raise ValueError('source JSON changed before archival')
                atomic(archive,raw)
            objects.append(dict(kind='existing_collector_archive',collector_kind=kind,relative_path=path.relative_to(self.args.source).as_posix(),sha256=key,archive_relative_path='raw-json/'+key+'.json'))
        return objects

    def deferred_media(self, path, entity, external, role, expected=None):
        path = path.resolve(strict=True)
        if not path.is_relative_to(self.args.source) or not path.is_file():
            raise ValueError('collector media escapes explicit read-only source')
        return dict(entity_type=entity, external_id=str(external), role=role,
                    _path=str(path), _signature=signature(path), _expected=expected)

    def materialize(self, manifest, provider):
        path = Path(manifest['_path'])
        if signature(path) != manifest['_signature'] or not 0 < path.stat().st_size <= 512 * 1024 * 1024:
            raise ValueError('collector media changed or exceeds budget')
        key = digest_file(path)
        if signature(path) != manifest['_signature'] or manifest['_expected'] and key != manifest['_expected']:
            raise ValueError('collector media checksum differs')
        with self.db.transaction():
            self.db.execute("SELECT pg_advisory_xact_lock(hashtextextended('media-gc:'||%s,0))",(key,))
            relative = 'sha256/' + key[:2] + '/' + key[2:4] + '/' + key
            target = self.args.media / relative
            for directory in (self.args.media/'sha256',self.args.media/'sha256'/key[:2],target.parent):
                try:
                    directory.mkdir(mode=0o755)
                except FileExistsError:
                    if directory.resolve()!=directory or not directory.is_dir():
                        raise ValueError('media CAS directory alias')
                else:
                    # Root's private state umask must not hide public-to-server CAS shards.
                    directory.chmod(0o755)
            if target.resolve() != target:
                raise ValueError('media CAS alias')
            if not target.exists():
                fd, name = tempfile.mkstemp(prefix='.live-', dir=target.parent)
                try:
                    h = hashlib.sha256()
                    with path.open('rb') as source, os.fdopen(fd, 'wb') as output:
                        os.fchmod(output.fileno(),0o644)
                        for block in iter(lambda: source.read(1024 * 1024), b''):
                            output.write(block)
                            h.update(block)
                        output.flush()
                        os.fsync(output.fileno())
                    if h.hexdigest() != key:
                        raise ValueError('collector media changed during copy')
                    # Link is an atomic no-replace publication, including concurrent uploads.
                    try:
                        os.link(name, target)
                    except FileExistsError:
                        pass
                finally:
                    Path(name).unlink(missing_ok=True)
            if key not in self.checked_media:
                if target.stat().st_size != path.stat().st_size or digest_file(target) != key:
                    raise ValueError('existing isolated CAS differs')
                self.checked_media.add(key)
            current = target.parent
            while True:
                directory = os.open(current,os.O_RDONLY|os.O_DIRECTORY)
                try:
                    os.fsync(directory)
                finally:
                    os.close(directory)
                if current==self.args.media:
                    break
                current=current.parent
            with target.open('rb') as source:
                header = source.read(64)
            kind = ('image/jpeg' if header.startswith(b'\xff\xd8\xff') else
                    'image/png' if header.startswith(b'\x89PNG\r\n\x1a\n') else
                    'image/webp' if header[:4] == b'RIFF' and header[8:12] == b'WEBP' else
                    'image/gif' if header.startswith((b'GIF87a', b'GIF89a')) else
                    'video/mp4' if header[4:8] == b'ftyp' else None)
            if kind is None:
                raise ValueError('unsupported local collector media type')
            media = uuid.uuid5(NAMESPACE, 'live-media:' + key)
            with self.db.transaction():
                self.db.execute("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,file_name,source_type,source_record_id,verified_at) VALUES(%s,%s,%s,%s,%s,%s,%s,%s,now()) ON CONFLICT(sha256) DO UPDATE SET media_type=excluded.media_type,file_name=coalesce(media_assets.file_name,excluded.file_name),verified_at=excluded.verified_at WHERE media_assets.media_type='application/octet-stream'", (media,key,kind,target.stat().st_size,relative,path.name,provider,manifest.get('source_url') or key))
                saved = self.db.execute('SELECT id,object_path,byte_length FROM crm.media_assets WHERE sha256=%s', (key,)).fetchone()
                if saved[1] not in (relative,str(target)) or saved[2]!=target.stat().st_size:
                    raise ValueError('registered isolated media identity differs')
                self.db.execute('UPDATE crm.media_assets SET verified_at=now() WHERE id=%s AND verified_at IS NULL', (saved[0],))
            self.counters['media'] += 1
            return {**{k:v for k,v in manifest.items() if not k.startswith('_')}, 'sha256':key}

    def accept(self, batch):
        raw = canonical(batch)
        if len(raw) > 7 * 1024 * 1024:
            raise ValueError('live import batch exceeds request budget')
        stamp_text, nonce = str(int(time.time())), uuid.uuid4().hex
        secret = os.environ['MONICA_IMPORT_HMAC_SECRET'].encode()
        mac = hmac.new(secret, stamp_text.encode()+b'.'+nonce.encode()+b'.'+raw, hashlib.sha256).hexdigest()
        address = urlsplit(self.args.url)
        if address.scheme != 'http' or address.hostname != 'monica-rs-server-1' or address.port != 17494 or address.path not in ('', '/'):
            raise ValueError('explicit isolated Rust endpoint required')
        connection = http.client.HTTPConnection(address.hostname, address.port, timeout=40)
        try:
            connection.request('POST','/api/v1/imports/batches',raw,headers={
                'Host':'nas.tailfb022c.ts.net:9444','Content-Type':'application/json',
                'x-import-timestamp':stamp_text,'x-import-nonce':nonce,'x-import-signature':mac,
                'x-import-token':os.environ['MONICA_IMPORT_TOKEN_'+batch['source'].upper()]})
            response = connection.getresponse()
            body = response.read(1024 * 1024)
            if response.status != 201:
                raise ValueError('signed import rejected: HTTP '+str(response.status))
            receipt = json.loads(body)
            return str(receipt['id'])
        finally:
            connection.close()

    def linkedin_message(self, item):
        connection = getattr(self.args,'linkedin_connection',None)
        own = getattr(self.args,'linkedin_self',None)
        if not connection or not own:
            raise ValueError('LinkedIn messages require verified owner and archive connection')
        if not self.db.execute("SELECT 1 FROM crm.source_connections WHERE id=%s AND provider='linkedin' AND external_account=%s",(connection,own)).fetchone():
            raise ValueError('LinkedIn archive connection owner differs')
        content = dict(item['content'])
        sender = content.get('sender')
        if not isinstance(sender,str) or not sender:
            raise ValueError('LinkedIn message sender required')
        incoming = content.get('incoming')
        if isinstance(incoming,bool) and incoming==(sender==own):
            raise ValueError('LinkedIn sender contradicts captured direction')
        parties = {}
        for person in content.get('participants',[]):
            if isinstance(person,str):
                person = dict(id=person,name=person)
            external = person.get('id') or person.get('urn') or person.get('member_urn')
            if not isinstance(external,str) or not external:
                raise ValueError('LinkedIn participant lacks stable identity')
            parties[external] = dict(person,id=external,is_self=external==own,role='participant')
        parties.setdefault(own,dict(id=own,name='本人',is_self=True,role='participant'))
        parties.setdefault(sender,dict(id=sender,name=sender,is_self=sender==own))
        parties[sender]['role'] = 'sender'
        group = sum(not p['is_self'] for p in parties.values())>1
        content.update(participants=list(parties.values()),direction='sent' if sender==own else 'received',is_group=group)
        return dict(item,content=content,conversation_external_id=content['conversation'],conversation_type='group' if group else 'direct')

    def social(self, provider):
        self.guard()
        self.active_provider=provider
        destinations = [('linkedin', self.args.source/'raw/linkedin')] if provider == 'linkedin' else [(k,self.args.source/'staging'/('instagram-'+k)) for k in ('current','stories','highlights')]
        for kind, destination in destinations:
            if not destination.is_dir():
                continue
            snapshot = self.bridge.completed(destination) or destination
            records = self.bridge.linkedin_records(snapshot,getattr(self.args,'linkedin_self',None)) if provider == 'linkedin' else self.bridge.instagram_records(snapshot, kind)
            while True:
                self.read_inputs = []
                try:
                    stream, item = next(records)
                except StopIteration:
                    break
                except Exception:
                    if self.read_inputs or self.last_input:self.archive_inputs(self.read_inputs or [self.last_input],kind)
                    raise
                if STOP:
                    return
                inputs=self.read_inputs or [self.last_input]
                captured = max(entry[1]for entry in inputs)
                occurred = stamp(item.get('occurred_at'))
                if captured < self.cutoff and (occurred is None or occurred < self.cutoff):
                    continue
                raw_objects=self.archive_inputs(inputs,kind)
                if provider=='linkedin' and stream=='messages':
                    item = self.linkedin_message(item)
                if any('\0'in str(item.get(k,''))for k in('external_id','source_account_external_id','conversation_external_id')):raise ValueError('source identity contains NUL')
                signature_hash = hashlib.sha256(canonical(item)).hexdigest()
                account = item.get('source_account_external_id') or ''
                identity = (provider,stream,item['external_id'],account)
                old = self.journal.execute('SELECT digest FROM delivered WHERE provider=? AND stream=? AND external_id=? AND account=?',identity).fetchone()
                if old and old[0] == signature_hash:
                    self.counters['unchanged'] += 1
                    continue
                manifests = [self.materialize(m,provider) for m in item.get('_manifest',[])]
                record = {k:v for k,v in item.items() if k != '_manifest'}
                record['content'],normalized=postgres_content(record['content'])
                if normalized:record['archive_normalization']=dict(nul='U+2400',fields=normalized)
                observed = datetime.fromtimestamp(captured,timezone.utc).isoformat()
                key = hashlib.sha256(canonical([identity,signature_hash,observed,raw_objects])).hexdigest()
                batch = dict(source=provider,stream=stream,schema_version=1,batch_id='nas-live-v2-'+key,
                             idempotency_key='nas-live:v2:'+key,observed_at=observed,records=[record],
                             raw_objects=raw_objects,media_manifest=manifests)
                if provider=='linkedin' and stream=='messages':
                    batch['connection_id'] = str(self.args.linkedin_connection)
                receipt = self.accept(batch)
                with self.journal:
                    self.journal.execute('INSERT OR REPLACE INTO delivered VALUES(?,?,?,?,?,?)',(*identity,signature_hash,receipt))
                self.counters['batches'] += 1
                self.counters['records'] += 1
                self.progress('social_received',provider=provider,stream=stream)

    def backup_chatlog(self):
        root = self.args.chatlog
        destination = self.state/'chatlog-export'
        paths = [*root.glob('contact/contact.db'),*root.glob('message/message_[0-9]*.db'),*root.glob('message/biz_message_[0-9]*.db'),*root.glob('sns/sns.db')]
        if not paths or len(paths) > 1024:
            raise ValueError('explicit Chatlog export unavailable')
        for path in sorted(paths):
            if STOP:
                return destination
            if path.resolve() != path:
                raise ValueError('Chatlog source alias')
            rel = path.relative_to(root).as_posix()
            state = [signature(path),*[signature(Path(str(path)+s)) if Path(str(path)+s).exists() else None for s in ('-wal','-journal')]]
            prior = self.journal.execute('SELECT signature FROM source_files WHERE path=?',(rel,)).fetchone()
            if prior and json.loads(prior[0]) == state and (destination/rel).is_file():
                continue
            target = destination/rel
            target.parent.mkdir(parents=True,exist_ok=True)
            fd, name = tempfile.mkstemp(prefix='.backup-',dir=target.parent)
            os.close(fd)
            try:
                self.progress('chatlog_consistent_backup',file=rel)
                with closing(sqlite3.connect('file:'+quote(str(path))+'?mode=ro',uri=True,timeout=2)) as source, closing(sqlite3.connect(name)) as backup:
                    backup_with_progress(source,backup,lambda status,remaining,total:self.progress('chatlog_consistent_backup',file=rel,sqlite_status=status,remaining_pages=remaining,total_pages=total))
                    backup.execute('PRAGMA journal_mode=DELETE')
                    if backup.execute('PRAGMA quick_check').fetchall() != [('ok',)]:
                        raise ValueError('Chatlog consistent backup failed integrity check')
                with open(name,'rb') as output:
                    os.fsync(output.fileno())
                os.replace(name,target)
                directory = os.open(target.parent,os.O_RDONLY|os.O_DIRECTORY)
                try:
                    os.fsync(directory)
                finally:
                    os.close(directory)
                with self.journal:
                    self.journal.execute('INSERT OR REPLACE INTO source_files VALUES(?,?)',(rel,canonical(state).decode()))
            finally:
                Path(name).unlink(missing_ok=True)
        return destination

    def wechat(self):
        self.guard()
        if not self.args.chatlog:
            return
        source = self.backup_chatlog()
        if STOP:
            return
        own = self.args.wechat_self
        if not own or not self.db.execute("SELECT 1 FROM crm.legacy_chatlog_snapshots WHERE own_id=%s AND status IN ('converted','verified') LIMIT 1",(own,)).fetchone():
            raise ValueError('verified existing WeChat owner required')
        with self.chatlog_control():
            if getattr(self.args,'sns_runtime',None) and not self.journal.execute("SELECT 1 FROM consumer_state WHERE key='sns-media-bootstrap-v1'").fetchone():
                # One explicit catch-up for pages acknowledged before photos
                # were part of SNS import. No recurring missing-image scan.
                posts=self.db.execute("SELECT e.id FROM crm.timeline_events e WHERE e.provider='wechat' AND e.event_type='post' AND e.occurred_at>=%s AND EXISTS(SELECT 1 FROM jsonb_array_elements(coalesce(e.content->'media','[]'))m WHERE m->>'media_type'='image' AND NOT EXISTS(SELECT 1 FROM crm.media_links l WHERE l.entity_type='social_post' AND l.entity_id=e.id AND l.role=m->>'role')) ORDER BY e.occurred_at,e.id",(self.args.since,)).fetchall()
                for post in posts:
                    if STOP:return
                    self.import_sns_event(source,post[0])
                with self.journal:self.journal.execute("INSERT INTO consumer_state VALUES('sns-media-bootstrap-v1',?)",(self.args.since,))
            manifest = dict(format=1,own_id=own,files=[],kind='continuous_business_pages',source='existing-chatlog-export')
            importer = self.legacy.Importer(self.db,manifest)
            importer.on_message_changed = self.changed_message_media
            reader_state = self.state/'chatlog-reader'
            reader_state.mkdir(parents=True,exist_ok=True)
            self.legacy.bind_reader_target(self.db,reader_state,own,importer.id)
            scan = self.legacy.reader.Reader(reader_state,source,own,self.legacy.atomic,lambda p:json.loads(Path(p).read_text()),own_id=own,allow_unknown_source_sender=True)
            def emit(frame):
                with self.db.transaction():
                    before = self.db.execute("SELECT inserted,changed,unchanged FROM crm.legacy_chatlog_snapshots WHERE id=%s",(importer.id,)).fetchone()
                    importer.page(frame)
                    importer.settle_revocations()
                    # Only current conversation heads change; source facts retain their IDs.
                    talkers = {r['content']['thread_id'] for r in frame['batch']['records'] if r['content'].get('kind')=='message'}
                    for talker in sorted(talkers):
                        self.settle_conversation(importer.conversation(talker))
                    after = self.db.execute("SELECT inserted,changed,unchanged FROM crm.legacy_chatlog_snapshots WHERE id=%s",(importer.id,)).fetchone()
                    inserted,changed,unchanged = (a-b for a,b in zip(after,before))
                    count = len(frame['batch']['records'])
                    run = 'nas-chatlog:'+str(importer.id)+':'+frame['page_sha256']
                    self.db.execute("INSERT INTO crm.import_activity(id,provider,stream,kind,batch_id,state,phase,received_at,completed_at,records,processed,inserted,changed,unchanged) VALUES(%s,'wechat',%s,'page',%s,'complete','existing_archive',now(),now(),%s,%s,%s,%s,%s) ON CONFLICT(id) DO NOTHING",(run,frame['batch']['stream'],frame['page_sha256'],count,count,inserted,changed,unchanged))
                # Source-page replay still enters the image importer even if
                # metadata is already committed. Decoded photos can therefore
                # finish publication before this page is acknowledged.
                for record in frame['batch']['records']:
                    value=record['content']
                    if value.get('kind')!='post':continue
                    root,_=self.legacy.reader.moments.parse_moments_xml(value['raw_content'])
                    obj=root if root.tag=='TimelineObject'else root.find('TimelineObject')
                    author=next(p['id']for p in value['participants']if p['role']=='author')
                    account,_=importer.account(author)
                    post=self.db.execute("SELECT id FROM crm.timeline_events WHERE provider='wechat' AND source_account_id=%s AND event_type='post' AND external_id=%s",(account,obj.findtext('id'))).fetchone()
                    if post:self.import_sns_event(source,post[0])
                self.legacy.atomic(reader_state/frame['ack_path'],dict(page_sha256=frame['page_sha256'],receipt_id=str(importer.id)))
                self.counters['chatlog_pages'] += 1
                self.progress('chatlog_received')
            try:
                while not STOP and scan.step(emit):
                    pass
                importer.settle_revocations()
            finally:
                scan.close()

    def import_sns_event(self, source, event_id):
        runtime=getattr(self.args,'sns_runtime',None)
        if runtime is None:
            if self.db.execute("SELECT 1 FROM crm.timeline_events e CROSS JOIN LATERAL jsonb_array_elements(coalesce(e.content->'media','[]'))m WHERE e.id=%s AND m->>'media_type'='image' LIMIT 1",(event_id,)).fetchone():
                raise ValueError('SNS image runtime must be configured')
            return
        images=module('sns_event_images','sns-media-import.py')
        result=images.import_event(self.db,source/'sns/sns.db',self.args.media,self.state/'sns-media',runtime,getattr(self.args,'sns_cache',None),event_id)
        self.counters['media']+=result['ready']
        self.progress('sns_media_received',provider='wechat',ready=result['ready'],failed=result['failed'])

    def changed_message_media(self, mid):
        # React only to a newly imported source revision. Completed photos are
        # not periodically rechecked. Original bytes and provenance stay intact.
        self.db.execute("DELETE FROM crm.media_links WHERE entity_type='message_event' AND entity_id=%s AND role LIKE 'wechat-group-image:%%'", (mid,))
        self.db.execute("UPDATE crm.wechat_group_images SET state='pending',quality='none',media_id=NULL WHERE message_id=%s", (mid,))

    @contextmanager
    def chatlog_control(self):
        if not self.db.execute("SELECT pg_try_advisory_lock(hashtextextended('monica-offline-chatlog',0))").fetchone()[0]:
            raise ValueError('another legacy reconciliation is active')
        try:
            yield
        finally:
            self.db.execute("SELECT pg_advisory_unlock(hashtextextended('monica-offline-chatlog',0))")

    def settle_conversation(self, conversation):
        with self.db.transaction():
            self.db.execute("SET LOCAL lock_timeout='2s'")
            self.db.execute("UPDATE crm.conversations c SET message_count=coalesce((SELECT sent+received+system+unknown FROM crm.message_stat_counts WHERE conversation_id=c.id AND scope='all'),0),first_message_at=(SELECT occurred_at FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at,message_id LIMIT 1),last_message_at=(SELECT occurred_at FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1),last_message_id=(SELECT message_id FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1),last_message_preview=(SELECT left(body,80) FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1) WHERE id=%s",(conversation,))
            self.db.execute("INSERT INTO crm.conversation_state(provider,account,conversation,message_count,last_at_ms,last_id,preview,generation) SELECT c.provider,c.account,c.external_id,c.message_count,coalesce((extract(epoch FROM c.last_message_at)*1000)::bigint,0),coalesce(m.external_id,''),coalesce(c.last_message_preview,''),1 FROM crm.conversations c LEFT JOIN crm.message_search m ON m.message_id=c.last_message_id WHERE c.id=%s ON CONFLICT(provider,account,conversation) DO UPDATE SET message_count=excluded.message_count,last_at_ms=excluded.last_at_ms,last_id=excluded.last_id,preview=excluded.preview,generation=crm.conversation_state.generation+1 WHERE (conversation_state.message_count,conversation_state.last_at_ms,conversation_state.last_id,conversation_state.preview) IS DISTINCT FROM (excluded.message_count,excluded.last_at_ms,excluded.last_id,excluded.preview)",(conversation,))
            self.db.execute("INSERT INTO crm.conversation_summaries SELECT s.* FROM crm.conversation_state s JOIN crm.conversations c ON (c.provider,c.account,c.external_id)=(s.provider,s.account,s.conversation) WHERE c.id=%s ON CONFLICT(provider,account,conversation) DO UPDATE SET message_count=excluded.message_count,last_at_ms=excluded.last_at_ms,last_id=excluded.last_id,preview=excluded.preview,generation=excluded.generation",(conversation,))

    def close(self):
        self.journal.close()


def main():
    global STOP
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run',action='store_true',required=True)
    parser.add_argument('--source',type=Path,required=True)
    parser.add_argument('--chatlog',type=Path)
    parser.add_argument('--wechat-self')
    parser.add_argument('--linkedin-self')
    parser.add_argument('--linkedin-connection',type=uuid.UUID)
    parser.add_argument('--state',type=Path,required=True)
    parser.add_argument('--media',type=Path,required=True)
    parser.add_argument('--sns-runtime',type=Path)
    parser.add_argument('--sns-cache',type=Path)
    parser.add_argument('--url',default='http://monica-rs-server-1:17494')
    parser.add_argument('--since',required=True)
    parser.add_argument('--once',action='store_true')
    parser.add_argument('--interval',type=int,default=300)
    args = parser.parse_args()
    if os.geteuid()!=10001:
        parser.error('NAS worker must run as the Rust server UID 10001')
    if args.interval < 30 or not args.since.endswith(('Z','+00:00')):
        parser.error('UTC cutoff and interval >=30 seconds required')
    for path in (args.source,args.state,args.media,*([args.chatlog] if args.chatlog else [])):
        if not path.is_absolute() or path.resolve() != path:
            parser.error('canonical absolute roots required')
    if args.source in args.state.parents or args.source in args.media.parents:
        parser.error('collector source and Rust writable roots must be separate')
    if args.state.name!='live-imports' or args.media.name!='media' or args.state.parent != args.media.parent or not (args.media.parent/'.monica-rs-ssd-target').is_file():
        parser.error('state and media must use the existing isolated Rust data root')
    os.umask(0o077)
    args.state.mkdir(parents=True,exist_ok=True)
    def stop(_signal,_frame):
        global STOP
        STOP = True
    signal.signal(signal.SIGTERM,stop)
    signal.signal(signal.SIGINT,stop)
    with (args.state/'control.lock').open('a') as lock, psycopg.connect(os.environ['MONICA_DATABASE_URL'],autocommit=True) as db:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        consumer = Consumer(args,db)
        try:
            failed = False
            while not STOP:
                failed = False
                for provider in ('wechat','linkedin','instagram'):
                    if STOP:
                        break
                    try:
                        consumer.progress('checking',provider=provider)
                        consumer.wechat() if provider=='wechat' else consumer.social(provider)
                        consumer.progress('waiting_for_source',provider=provider)
                    except Exception as error:
                        failed = True
                        # Private original inputs remain available; no payload/secret is logged.
                        consumer.progress('retry_pending',provider=provider,error_type=type(error).__name__,error_code=failure_code(error))
                        if db.closed or db.broken:
                            # The container supervisor restarts with a new connection.
                            return 1
                if args.once:
                    break
                for _ in range(args.interval):
                    if STOP:
                        break
                    time.sleep(1)
        finally:
            consumer.close()
        return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())

#!/usr/bin/env python3
"""Read only Chatlog's explicitly published standalone SQLite snapshots.

No encrypted source, service discovery, SQL-over-HTTP, key extraction or model.
Each bounded source page remains pending until the Rust journal receipt is ACKed.
The active exported inode is pinned across passes so source replacement cannot
shift pagination. A later snapshot is fully reconciled, including corrections.
"""
import base64
from contextlib import closing
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import sqlite3
import time
import errno
import shutil
from datetime import datetime, timezone
import xml.etree.ElementTree as ET

_spec = importlib.util.spec_from_file_location('chatlog_moments', Path(__file__).with_name('chatlog_moments.py'))
moments = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(moments)

LIMIT = 500
MAX_PAGE = 3 * 1024 * 1024
SYSTEM = {'brandsessionholder','exmail_tool','feedsapp','filehelper','floatbottle','fmessage','medianote','mphelper','newsapp','notification_messages','notifymessage','opencustomerservicemsg','qmessage','qqmail','tmessage','voip','weibo','weixin','weixinreminder','weixinteam'}

def encode(value):
    return json.dumps(value,ensure_ascii=False,sort_keys=True,separators=(',',':'))

def stamp(path):
    s=path.stat()
    return encode([s.st_dev,s.st_ino,s.st_size,s.st_mtime_ns])

def safe_file(root,path):
    if path.is_symlink() or not path.is_file() or not path.resolve(strict=True).is_relative_to(root):raise ValueError('Chatlog snapshot escapes explicit root')
    for parent in path.relative_to(root).parents:
        if (root/parent).is_symlink():raise ValueError('Chatlog snapshot directory is a symlink')
    if path.with_name(path.name+'-wal').exists():raise ValueError('Chatlog export is not a standalone SQLite snapshot')

def standalone(path):
    db=sqlite3.connect(path.as_uri()+'?mode=ro&immutable=1',uri=True)
    db.row_factory=sqlite3.Row
    # WCDB can put protobuf bytes in columns declared TEXT. Keep those bytes
    # intact; individual field decoders decide whether UTF-8 is required.
    def text_or_bytes(data):
        try:return data.decode('utf-8')
        except UnicodeDecodeError:return data
    db.text_factory=text_or_bytes
    db.execute('PRAGMA query_only=ON')
    return db

def body(value):
    if isinstance(value,bytes):
        if value.startswith(b'\x28\xb5\x2f\xfd'):
            import zstandard
            value=zstandard.ZstdDecompressor().decompress(value,max_output_size=MAX_PAGE)
        try:return value.decode('utf-8').replace('\0',''),None
        except UnicodeDecodeError:return '',base64.b64encode(value).decode('ascii')
    return str(value or '').replace('\0',''),None

def is_group_identity(value):
    # Some exported Name2Id entries retain trailing whitespace. Preserve the
    # exact identity, while retaining group privacy for the protocol suffix.
    return value.rstrip().endswith('@chatroom')

def contact(row):
    row=dict(row)
    if row.get('username') is not None and not isinstance(row['username'],str):raise ValueError('Chatlog contact identity is not UTF-8')
    external=str(row.get('username') or '').strip()
    if not external:raise ValueError('Chatlog contact has no stable username')
    if external.endswith(('@chatroom','@openim')) or external.startswith(('gh_','@')) or external in SYSTEM or row.get('local_type') in (2,3,5,6):return None
    name=row.get('remark') or row.get('nick_name') or row.get('alias') or external
    fields={k:row.get(k) for k in ['alias','remark','nick_name','description','big_head_url','small_head_url'] if row.get(k)}
    return dict(external_id='contact:'+external,kind='contact',deleted=bool(row.get('delete_flag')),display_name=name,fields=fields,participants=[dict(id=external,name=name,is_self=False,role='contact')])

def message(row,names,talker,file,table,own_id,allow_unknown_source_sender=False):
    row=dict(row);sender=names.get(row.get('real_sender_id'))
    # Offline copies can contain the protocol's empty/zero sender sentinel.
    # Retain the record and its missing attribution explicitly. Arbitrary
    # dangling positive IDs still fail, and the live reader remains strict.
    source_gap=not sender and allow_unknown_source_sender and (row.get('real_sender_id')==0 or row.get('real_sender_id') in names)
    if not sender and not source_gap:
        raise ValueError('Chatlog sender Name2Id identity is missing')
    at=datetime.fromtimestamp(row['create_time'],timezone.utc)
    if not 1990<=at.year<=2100:raise ValueError('Chatlog timestamp outside supported range')
    text,binary=body(row.get('message_content'))
    if not text and not binary:text,binary=body(row.get('compress_content'))
    external='server:'+str(row['server_id']) if row.get('server_id') else file+':'+table+':'+str(row['local_id'])
    # Authoritative Name2Id, never the colliding display name or an XML prefix.
    people=[dict(id=sender,name=sender,role='sender',is_self=own_id is not None and sender==own_id)] if sender else []
    group=is_group_identity(talker)
    if not group and talker!=sender:people.append(dict(id=talker,name=talker,role='participant',is_self=own_id is not None and talker==own_id))
    result=dict(external_id='message:'+talker+':'+external,kind='message',thread_id=talker,body=text,occurred_at=at.isoformat(),message_type=str(row['local_type']),is_group=group,direction='unknown' if own_id is None or not sender else ('sent' if sender==own_id else 'received'),participants=people,raw_message=dict(local_id=row['local_id'],server_id=row.get('server_id'),real_sender_id=row['real_sender_id'],create_time=row['create_time'],local_type=row['local_type']))
    if binary:result['binary_base64']=binary
    if source_gap:result['source_gap']='sender_reference_absent'
    return result

def post(row):
    row=dict(row);external=str(row['tid']);author=str(row.get('user_name') or '')
    raw,binary=body(row.get('content'))
    if binary:raise ValueError('Chatlog Moments XML is not UTF-8')
    root,repairs=moments.parse_moments_xml(raw)
    if root.tag=='TimelineObject':
        wrapper=ET.Element('SnsObject');wrapper.append(root);root=wrapper
    obj=root.find('TimelineObject')
    if obj is None:raise ValueError('Chatlog Moments TimelineObject missing')
    author=author or obj.findtext('username')
    if not author:raise ValueError('Chatlog Moments author is missing')
    epoch=int(obj.findtext('createTime'));at=datetime.fromtimestamp(epoch,timezone.utc)
    if not 1990<=at.year<=2100:raise ValueError('Chatlog Moments timestamp outside supported range')
    parsed=moments.moments_record(external,author,ET.tostring(root,encoding='unicode'),None)['content']
    # The source key retains the stable SQLite identity. Preserve the exact raw
    # XML separately from its parsed rendering and any literal-reference repair.
    parsed['raw_xml_sha256']=hashlib.sha256(raw.encode()).hexdigest()
    if repairs:parsed['xml_literal_character_references']=repairs
    people=[dict(id=author,name=author,role='author',is_self=False)]
    seen={author}
    for item in parsed['interactions']:
        who=item.get('author_external_id')
        if who and who not in seen:
            people.append(dict(id=who,name=item.get('author_name') or who,role='interactor',is_self=False));seen.add(who)
    return dict(parsed,external_id='post:'+external,kind='post',body=parsed['text'],occurred_at=at.isoformat(),participants=people,raw_content=raw,coordinate_format='chatlog_sns_xml')

class Reader:
    def __init__(self,root,snapshots,account,atomic_document,load_json,own_id=None,allow_unknown_source_sender=False,immutable_scan=False):
        if not root.is_absolute() or not snapshots.is_absolute() or not account or len(account)>1024:raise ValueError('Explicit Chatlog snapshot root and account anchor required')
        self.root=root.resolve(strict=True);self.snapshots=snapshots.resolve(strict=True);self.account=account;self.own_id=own_id
        self.allow_unknown_source_sender=allow_unknown_source_sender
        self.immutable_scan=immutable_scan
        self.atomic=atomic_document;self.load=load_json
        digest=hashlib.sha256(account.encode()).hexdigest();self.folder=self.root/'raw/wechat'/digest;self.folder.mkdir(parents=True,exist_ok=True)
        if not self.folder.resolve().is_relative_to(self.root):raise ValueError('Chatlog state escapes data root')
        self.guard=(self.folder/'reader.lock').open('a');fcntl.flock(self.guard,fcntl.LOCK_EX|fcntl.LOCK_NB)
        self.db=sqlite3.connect(self.folder/'reader.sqlite3');self.db.row_factory=sqlite3.Row
        self.db.execute('PRAGMA synchronous=FULL')
        self.db.executescript('''CREATE TABLE IF NOT EXISTS files(name TEXT PRIMARY KEY,stamp TEXT,pin TEXT,cycle TEXT,table_name TEXT DEFAULT '',row_id INTEGER DEFAULT 0,phase TEXT DEFAULT 'scan',done INTEGER DEFAULT 0,served INTEGER DEFAULT 0,pin_stamp TEXT,retry_after REAL DEFAULT 0,error TEXT);
        CREATE TABLE IF NOT EXISTS known(file TEXT,table_name TEXT,row_id INTEGER,external TEXT,hash TEXT,cycle TEXT,deleted INTEGER,context TEXT,PRIMARY KEY(file,table_name,row_id));
        CREATE TABLE IF NOT EXISTS names(file TEXT,id INTEGER,username TEXT,digest TEXT,PRIMARY KEY(file,id));
        CREATE INDEX IF NOT EXISTS names_digest ON names(file,digest);
        CREATE TABLE IF NOT EXISTS pending(id INTEGER PRIMARY KEY CHECK(id=1),frame TEXT,change TEXT);
        CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT);''')
        self.db.execute('CREATE TABLE IF NOT EXISTS scan_counts(file TEXT,table_name TEXT,records INTEGER NOT NULL,PRIMARY KEY(file,table_name))')
        mode=self.db.execute("SELECT value FROM meta WHERE key='immutable_scan'").fetchone()
        if mode and mode[0]!=str(int(immutable_scan)):raise ValueError('Chatlog scan policy changed; use a distinct state directory')
        if not mode and immutable_scan and self.db.execute('SELECT 1 FROM files LIMIT 1').fetchone():raise ValueError('immutable scan requires its own state directory')
        self.db.execute("INSERT OR IGNORE INTO meta VALUES('immutable_scan',?)",(str(int(immutable_scan)),))
        previous=self.db.execute("SELECT value FROM meta WHERE key='own_id'").fetchone()
        policy=own_id or ''
        if previous and previous[0]!=policy:raise ValueError('Chatlog self identity changed; use a distinct account anchor')
        self.db.execute("INSERT OR IGNORE INTO meta VALUES('own_id',?)",(policy,));self.db.commit()
    def close(self):
        self.db.close();self.guard.close()
    def apply(self,change):
        with self.db:
            if self.immutable_scan:
                counts={}
                for row in change['known']:counts[(row[0],row[1])]=counts.get((row[0],row[1]),0)+1
                for (file,table),count in counts.items():self.db.execute('INSERT INTO scan_counts VALUES(?,?,?) ON CONFLICT(file,table_name) DO UPDATE SET records=scan_counts.records+excluded.records',(file,table,count))
            else:
                for row in change['known']:
                    self.db.execute('INSERT INTO known VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(file,table_name,row_id) DO UPDATE SET external=excluded.external,hash=excluded.hash,cycle=excluded.cycle,deleted=excluded.deleted,context=excluded.context',row)
            f=change['file'];self.db.execute('UPDATE files SET table_name=?,row_id=?,phase=?,done=?,served=? WHERE name=? AND cycle=?',(f['table_name'],f['row_id'],f['phase'],f['done'],f['served'],f['name'],f['cycle']))
            self.db.execute('DELETE FROM pending')
    def count_records(self,file,table):
        if self.immutable_scan:
            row=self.db.execute('SELECT records FROM scan_counts WHERE file=? AND table_name=?',(file,table)).fetchone()
            return row[0] if row else 0
        return self.db.execute('SELECT count(*) FROM known WHERE file=? AND table_name=? AND NOT deleted',(file,table)).fetchone()[0]
    def pending(self,emit):
        pending=self.db.execute('SELECT frame,change FROM pending').fetchone()
        if not pending:return False
        frame=json.loads(pending['frame']);ack=self.root/frame['ack_path']
        if ack.exists():
            saved=self.load(ack)
            if saved.get('page_sha256')!=frame['page_sha256'] or not saved.get('receipt_id'):raise ValueError('Chatlog receipt ACK mismatch')
            self.apply(json.loads(pending['change']));return False
        emit(frame);return True
    def discover(self):
        found=[*self.snapshots.glob('contact/contact.db'),*self.snapshots.glob('message/message_[0-9]*.db'),*self.snapshots.glob('message/biz_message_[0-9]*.db'),*self.snapshots.glob('sns/sns.db')]
        if len(found)>1024:raise ValueError('Chatlog exported shard budget exceeded')
        for path in sorted(found):
            safe_file(self.snapshots,path);name=path.relative_to(self.snapshots).as_posix();current=stamp(path)
            row=self.db.execute('SELECT * FROM files WHERE name=?',(name,)).fetchone()
            if self.immutable_scan and row and row['stamp']!=current:raise ValueError('frozen Chatlog input changed')
            if row and (not row['done'] or row['stamp']==current):continue
            # ExportDatabase installs a standalone consistent SQLite by rename.
            # Pin that inode so another export cannot shift an active scan.
            pin=self.folder/'snapshots'/hashlib.sha256((name+current).encode()).hexdigest();pin.parent.mkdir(exist_ok=True)
            if not pin.exists():
                try:os.link(path,pin)
                except OSError as exc:
                    if exc.errno!=errno.EXDEV:raise
                    temporary=pin.with_suffix('.partial')
                    try:
                        with path.open('rb') as original,temporary.open('wb') as output:
                            shutil.copyfileobj(original,output,length=1024*1024);output.flush();os.fsync(output.fileno())
                        if stamp(path)!=current:raise ValueError('Chatlog export changed while copying')
                        os.replace(temporary,pin)
                    finally:temporary.unlink(missing_ok=True)
            directory=os.open(pin.parent,os.O_RDONLY)
            try:os.fsync(directory)
            finally:os.close(directory)
            with closing(standalone(pin)) as source:
                source.execute('SELECT name FROM sqlite_master LIMIT 1').fetchall()
            cycle=hashlib.sha256((name+current).encode()).hexdigest()
            with self.db:
                self.db.execute("INSERT INTO files(name,stamp,pin,cycle,pin_stamp,phase) VALUES(?,?,?,?,?,'names') ON CONFLICT(name) DO UPDATE SET stamp=excluded.stamp,pin=excluded.pin,cycle=excluded.cycle,table_name='',row_id=0,phase='names',done=0,pin_stamp=excluded.pin_stamp,retry_after=0,error=NULL",(name,current,str(pin),cycle,stamp(pin)))
                self.db.execute('DELETE FROM names WHERE file=?',(name,))
            if row:Path(row['pin']).unlink(missing_ok=True)
    def step(self,emit):
        if self.pending(emit):return True
        self.discover()
        state=self.db.execute('SELECT * FROM files WHERE done=0 AND retry_after<=? ORDER BY served,name LIMIT 1',(time.time(),)).fetchone()
        if state is None:
            if self.db.execute('SELECT count(*) FROM files WHERE done=0 AND error IS NOT NULL').fetchone()[0]:raise RuntimeError('Chatlog independent snapshot retry pending')
            return False
        state=dict(state);source=standalone(Path(state['pin']))
        try:
            return self.scan(source,state,emit)
        except Exception:
            with self.db:self.db.execute("UPDATE files SET retry_after=?,error='snapshot_read_failed',served=(SELECT coalesce(max(served),0)+1 FROM files) WHERE name=?",(time.time()+30,state['name']))
            raise
        finally:source.close()
    def scan(self,source,state,emit):
        file=state['name'];cycle=state['cycle']
        state['served']=self.db.execute('SELECT coalesce(max(served),0)+1 FROM files').fetchone()[0]
        tables=[r[0] for r in source.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
        if state['phase']=='names':
            if file.startswith('message/'):
                if 'Name2Id' not in tables:
                    if tables:raise ValueError('Nonempty Chatlog shard missing Name2Id')
                    state['done']=1
                else:
                    names=source.execute('SELECT rowid AS _name_id,user_name FROM Name2Id WHERE rowid>? ORDER BY rowid LIMIT 1000',(state['row_id'],)).fetchall()
                    with self.db:
                        for row in names:
                            name=row['user_name']
                            if name is not None and not isinstance(name,str):raise ValueError('Chatlog Name2Id identity is not UTF-8')
                            self.db.execute('INSERT OR REPLACE INTO names VALUES(?,?,?,?)',(file,row['_name_id'],name,hashlib.md5(name.encode()).hexdigest() if name else None))
                    if names:state['row_id']=names[-1]['_name_id']
                    if len(names)==1000:self.apply(dict(file=state,known=[]));return True
            state.update(phase='scan',row_id=0)
        if file.startswith('contact/') and 'contact' not in tables:raise ValueError('Chatlog contact schema is missing')
        if file.startswith('sns/') and 'SnsTimeLine' not in tables:raise ValueError('Chatlog Moments schema is missing')
        allowed=[t for t in tables if (t=='contact' and file.startswith('contact/')) or (re.fullmatch('Msg_[0-9a-fA-F]{32}',t) and file.startswith('message/')) or (t=='SnsTimeLine' and file.startswith('sns/'))]
        if state['phase']=='scan':
            candidates=[t for t in allowed if t>=state['table_name']]
            if not candidates:
                state.update(phase='delete',table_name='',row_id=0)
            else:
                table=candidates[0]
                if table!=state['table_name']:state.update(table_name=table,row_id=0)
                # Bound bytes BEFORE hydrating source bodies. LIMIT alone would
                # materialize 500 multi-MiB messages and evade runtime admission.
                columns=[r[1] for r in source.execute('PRAGMA table_info("'+table+'")')]
                if len(columns)>64:raise ValueError('Chatlog source column budget exceeded')
                lengths=['coalesce(length(CAST("'+column.replace('"','""')+'" AS BLOB)),0)' for column in columns]
                extra=',real_sender_id' if table.startswith('Msg_') else ''
                metadata=source.execute('SELECT rowid AS _row_id,'+'+'.join(lengths)+' AS _bytes'+extra+' FROM "'+table+'" WHERE rowid>? ORDER BY rowid LIMIT ?',(state['row_id'],LIMIT)).fetchall()
                selected=[];hydration_bytes=0
                for item in metadata:
                    if item['_bytes']>MAX_PAGE:raise ValueError('Single Chatlog raw record exceeds page budget')
                    if hydration_bytes+item['_bytes']>MAX_PAGE:break
                    hydration_bytes+=item['_bytes'];selected.append(item)
                talker=None;names={}
                if table.startswith('Msg_'):
                    entry=self.db.execute('SELECT username FROM names WHERE file=? AND digest=?',(file,table[4:].lower())).fetchone()
                    if not entry:
                        if not selected:
                            state.update(table_name=table+'\0',row_id=0)
                            return self.page(state,[],[],emit)
                        raise ValueError('Chatlog conversation table has no stable talker')
                    talker=entry['username'];ids=[r['real_sender_id'] for r in selected]
                    if ids:
                        names={r['id']:r['username'] for r in self.db.execute('SELECT id,username FROM names WHERE file=? AND id IN ('+','.join('?' for _ in ids)+')',[file,*ids])}
                changed=[];known=[];size=0
                rows=source.execute('SELECT rowid AS _row_id,* FROM "'+table+'" WHERE rowid IN ('+','.join('?' for _ in selected)+') ORDER BY rowid',[item['_row_id'] for item in selected]) if selected else []
                for row in rows:
                    value=contact(row) if table=='contact' else post(row) if table=='SnsTimeLine' else message(row,names,talker,file,table,self.own_id,self.allow_unknown_source_sender)
                    row_size=len(encode(value).encode()) if value else 0
                    if row_size>MAX_PAGE:raise ValueError('Single Chatlog record exceeds page budget')
                    if size+row_size>MAX_PAGE:break
                    size+=row_size;state['row_id']=row['_row_id']
                    if value is None:continue
                    digest=hashlib.sha256(encode(value).encode()).hexdigest()
                    old=self.db.execute('SELECT hash,external,context,deleted FROM known WHERE file=? AND table_name=? AND row_id=?',(file,table,row['_row_id'])).fetchone()
                    context={k:value[k] for k in ['kind','thread_id','occurred_at','message_type','is_group'] if k in value}
                    known.append([file,table,row['_row_id'],value['external_id'],digest,cycle,int(value.get('deleted',False)),encode(context)])
                    if old and old['external']!=value['external_id'] and not old['deleted']:
                        changed.append(dict(json.loads(old['context']),external_id=old['external'],deleted=True,participants=[]))
                    if not old or old['hash']!=digest:changed.append(value)
                if not metadata or state['row_id']==metadata[-1]['_row_id'] and len(metadata)<LIMIT:
                    state.update(table_name=table+'\0',row_id=0)
                return self.page(state,changed,known,emit)
        if state['phase']=='delete':
            obsolete=self.db.execute('SELECT * FROM known WHERE file=? AND cycle<>? AND deleted=0 ORDER BY table_name,row_id LIMIT ?',(file,cycle,LIMIT)).fetchall()
            changed=[dict(json.loads(r['context']),external_id=r['external'],deleted=True,participants=[]) for r in obsolete]
            known=[[file,r['table_name'],r['row_id'],r['external'],hashlib.sha256(encode(value).encode()).hexdigest(),cycle,1,r['context']] for r,value in zip(obsolete,changed)]
            if not obsolete:state['done']=1
            return self.page(state,changed,known,emit)
        self.apply(dict(file=state,known=[]));return True
    def page(self,state,values,known,emit):
        if stamp(Path(state['pin']))!=state['pin_stamp']:raise ValueError('Published Chatlog inode changed in place')
        change=dict(file=state,known=known)
        if not values:self.apply(change);return True
        # Content-addressed payload and captured time stay byte-identical on replay.
        rows=[]
        for value in values:
            row=dict(external_id=value['external_id'],entity_type=value['kind'],content=value)
            if value['kind']=='message':
                row.update(conversation_external_id=value['thread_id'],conversation_type='group' if value['is_group'] else 'direct',occurred_at=value['occurred_at'],message_type=value['message_type'])
            elif value.get('occurred_at'):row['occurred_at']=value['occurred_at']
            rows.append(row)
        digest=hashlib.sha256(encode([self.account,rows,state['cycle'],state['table_name'],state['row_id'],state['phase']]).encode()).hexdigest()
        batch=dict(source='wechat',stream='chatlog',schema_version=1,batch_id='chatlog-'+digest,idempotency_key='wechat:chatlog:'+digest,observed_at=datetime.now(timezone.utc).isoformat(),records=rows,media_manifest=[],raw_objects=[])
        frame=dict(type='connection_snapshot',account=self.account,batch=batch,page_sha256=digest,ack_path=(self.folder/'acks'/(digest+'.ack')).relative_to(self.root).as_posix())
        with self.db:self.db.execute('INSERT INTO pending VALUES(1,?,?)',(encode(frame),encode(change)))
        emit(frame);return True

def publish(root,snapshots,account,emit,atomic_document,load_json,own_id=None):
    reader=Reader(root,snapshots,account,atomic_document,load_json,own_id)
    try:return reader.step(emit)
    finally:reader.close()

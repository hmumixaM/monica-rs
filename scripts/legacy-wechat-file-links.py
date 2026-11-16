"""Register decoded indexed files and attach proven current message references.

This is the indexed-file stage. References lacking indexed local files remain
pending filesystem resolution; they are not called missing source data here.
"""
import argparse
from contextlib import closing
import hashlib,importlib.util,json,os,re
from pathlib import Path
import sqlite3,uuid
import psycopg
# SSD guard only: actual worker owns the shared inode until exit.
import fcntl as _ssd_fcntl
_ssd_copy_lock = open('/copy.lock', 'r')
_ssd_fcntl.flock(_ssd_copy_lock, _ssd_fcntl.LOCK_EX | _ssd_fcntl.LOCK_NB)

spec=importlib.util.spec_from_file_location('legacy',Path.cwd()/'scripts/legacy-chatlog-import.py')
legacy=importlib.util.module_from_spec(spec);spec.loader.exec_module(legacy)
EXPECTED='7488327f3a7ed5869f490dca5fd77fd2da86b3d515dd9b482f7d81a5a8e1405a'
def sha(path):
    value=hashlib.sha256()
    with path.open('rb') as stream:
        for part in iter(lambda:stream.read(1024*1024),b''):value.update(part)
    return value.hexdigest()
def batches(rows,count=100):
    pending=[]
    for row in rows:
        pending.append(row)
        if len(pending)==count:yield pending;pending=[]
    if pending:yield pending
def asset_file(root,asset):
    key=asset['sha256'];relative='sha256/'+key[:2]+'/'+key[2:4]+'/'+key
    if len(key)!=64 or any(c not in '0123456789abcdef' for c in key) or asset['object_path']!=relative:raise ValueError('invalid copied CAS record')
    path=root/relative
    if path.resolve()!=path or not path.is_file() or path.stat().st_size!=asset['byte_length']:raise ValueError('copied runtime file differs')
    # The completed copier already hashed every published immutable CAS file.
    # Reuse its receipt and check presence/size instead of rereading 32 GB.
def prepare_media_keys(index,state,identity):
    # Old Chatlog resolves both hardlink.md5 and file_name prefixes. Build a
    # disk-backed lookup only for files proven ready by the completed copier.
    # This is derived state in the independent Rust root, never source edits.
    path=state/'indexed-media-keys.sqlite'
    for candidate in [path,*[path.with_name(path.name+s) for s in ['-journal','-wal','-shm']]]:
        if candidate.resolve()!=candidate:raise ValueError('media key lookup alias')
    index.execute('ATTACH DATABASE ? AS media_keys',(str(path),))
    index.execute('PRAGMA media_keys.cache_size=-4096')
    index.executescript('CREATE TABLE IF NOT EXISTS media_keys.metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE IF NOT EXISTS media_keys.keys(kind TEXT,md5 TEXT,source_key TEXT,PRIMARY KEY(kind,md5,source_key)) WITHOUT ROWID;')
    expected={'snapshot':identity,'input_sha256':EXPECTED,'format':'1'}
    prior=dict(index.execute('SELECT * FROM media_keys.metadata'))
    if prior and prior!=expected:raise ValueError('media lookup belongs to another snapshot')
    if not prior and index.execute('SELECT 1 FROM media_keys.keys LIMIT 1').fetchone():raise ValueError('media lookup has no source identity')
    with index:
        index.executemany('INSERT OR IGNORE INTO media_keys.metadata VALUES(?,?)',expected.items())
        index.execute('DELETE FROM media_keys.keys')
        for kind,digest,key,encoded,path in index.execute("SELECT f.kind,f.md5,f.source_key,f.candidates,a.source_path FROM files f JOIN copied.assets a USING(source_key) WHERE a.status='ready'"):
            if path not in json.loads(encoded):raise ValueError('copied media source is not an indexed candidate')
            values={(kind,digest,key)}
            match=re.fullmatch(r'([0-9a-fA-F]{32})(?:[._].*)?',Path(path).name)
            if kind in ('image','video') and match:values.add((kind,match.group(1).lower(),key))
            index.executemany('INSERT OR IGNORE INTO media_keys.keys VALUES(?,?,?)',values)
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['index','assets','manifest','media','state']:
        parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--expected-schema',type=int,required=True)
    parser.add_argument('--allow-partial',action='store_true',help='link ready local files while retaining failed/pending records')
    args=parser.parse_args()
    if args.expected_schema<67:raise ValueError('unsupported target schema')
    expected={'index':Path('/index'),'assets':Path('/assets'),'manifest':Path('/work/manifest.json'),'media':Path('/media'),'state':Path('/state')}
    if any(getattr(args,k)!=v for k,v in expected.items()):raise ValueError('dedicated new-instance container mounts required')
    for p in [args.index,args.assets,args.manifest,args.media,args.state]:
        if not p.is_absolute() or p.resolve()!=p:raise ValueError('canonical absolute import paths required')
    if sha(args.index)!=EXPECTED:raise ValueError('source file index differs')
    manifest=json.loads(args.manifest.read_text())
    identity=hashlib.sha256(legacy.reader.encode(manifest).encode()).hexdigest()
    snapshot=uuid.uuid5(legacy.NS,'chatlog-snapshot:'+identity)
    args.state.mkdir(mode=0o700,parents=True,exist_ok=True)
    with closing(sqlite3.connect(args.index.as_uri()+'?mode=ro&immutable=1',uri=True)) as index,closing(sqlite3.connect(args.assets.as_uri()+'?mode=ro&immutable=1',uri=True)) as files,psycopg.connect(os.environ['MONICA_DATABASE_URL'],autocommit=True) as db:
        source=dict(index.execute('SELECT * FROM metadata'))
        converted=dict(files.execute('SELECT * FROM metadata'))
        if source.get('snapshot')!=identity or source.get('own_id')!=manifest['own_id'] or source.get('complete')!='1':raise ValueError('message reference source identity differs')
        if converted!={'snapshot':identity,'own_id':manifest['own_id'],'input_sha256':EXPECTED}:raise ValueError('copied file state belongs to another input')
        if files.execute('SELECT count(*) FROM assets').fetchone()[0]!=index.execute('SELECT count(*) FROM files').fetchone()[0]:raise ValueError('indexed file copy has not finished')
        failures=files.execute("SELECT count(*) FROM assets WHERE status NOT IN ('ready','source_missing')").fetchone()[0]
        if failures and not args.allow_partial:raise ValueError('some copied files still need decoding; explicit partial link pass required')
        system,name,version=db.execute("SELECT system_identifier::text,current_database(),(SELECT max(version) FROM _sqlx_migrations WHERE success) FROM pg_control_system()").fetchone()
        if (system,name,version)!=('7707886811981668386','monica_rust',args.expected_schema):raise ValueError('isolated target identity differs')
        if db.execute("SELECT oid FROM pg_database WHERE datname=current_database()").fetchone()[0]!=16384:raise ValueError('isolated target database OID differs')
        if not db.execute("SELECT 1 FROM crm.legacy_chatlog_snapshots WHERE id=%s AND status IN ('converted','verified')",(snapshot,)).fetchone():raise ValueError('complete Chatlog conversion is required')
        if not db.execute("SELECT pg_try_advisory_lock(hashtextextended('monica-offline-chatlog',0))").fetchone()[0]:raise ValueError('another offline media operation is active')
        legacy.bind_state(db,args.state,snapshot)
        registered=0
        for group in batches(files.execute("SELECT source_key,kind,source_path,asset FROM assets WHERE status='ready' ORDER BY source_key")):
            for source_key,kind,path,encoded in group:asset_file(args.media,json.loads(encoded))
            with db.transaction():
                for source_key,kind,path,encoded in group:
                    asset=json.loads(encoded);provenance=str(snapshot)+':file:'+source_key
                    file_name=Path(path).name if kind!='image' else 'image.'+asset['media_type'].split('/')[-1].replace('jpeg','jpg')
                    db.execute("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,file_name,width,height,source_type,source_record_id,verified_at) VALUES(%s,%s,%s,%s,%s,%s,%s,%s,'wechat',%s,now()) ON CONFLICT(sha256) DO NOTHING",(uuid.uuid5(legacy.NS,'chatlog-media:'+asset['sha256']),asset['sha256'],asset['media_type'],asset['byte_length'],asset['object_path'],file_name,asset.get('width'),asset.get('height'),provenance))
                    saved=db.execute('SELECT id,byte_length FROM crm.media_assets WHERE sha256=%s',(asset['sha256'],)).fetchone()
                    if not saved or saved[1]!=asset['byte_length']:raise ValueError('registered media differs from its copied file')
                    db.execute("INSERT INTO crm.media_provenance(media_id,source,source_record_id) VALUES(%s,'wechat',%s) ON CONFLICT DO NOTHING",(saved[0],provenance))
                    registered+=1
            print(json.dumps({'phase':'indexed_file_assets','registered_records':registered}),flush=True)
        # Index and copied-state sources stay read-only. The fixed ATTACH URI
        # is data, never an interpolated SQL identifier or a write destination.
        index.execute('ATTACH DATABASE ? AS copied',(args.assets.as_uri()+'?mode=ro&immutable=1',))
        prepare_media_keys(index,args.state,identity)
        linked=pending=missing_current=0
        previous=None;selected=None
        for group in batches(index.execute('SELECT source_key,kind,md5,chat,create_time,server_id,local_id,message_type FROM refs ORDER BY kind,md5,source_key')):
            with db.transaction():
                for key,kind,digest,chat,at,server_id,local_id,message_type in group:
                    if previous!=(kind,digest):
                        selected=index.execute("SELECT a.asset FROM media_keys.keys k JOIN files f USING(source_key) JOIN copied.assets a USING(source_key) WHERE k.kind=? AND k.md5=? AND a.status='ready' ORDER BY f.tier DESC,f.expected_bytes DESC,f.modified_at DESC,f.source_key LIMIT 1",(kind,digest)).fetchone()
                        previous=(kind,digest)
                    if not selected:pending+=1;continue
                    asset=json.loads(selected[0]);asset_file(args.media,asset)
                    source_id='server:'+str(server_id) if server_id else (key[8:] if key.startswith('message/') else key)
                    conversation=('group:' if legacy.reader.is_group_identity(chat) else 'direct:')+chat
                    rows=db.execute("SELECT m.message_id FROM crm.conversations c JOIN crm.message_search m ON m.conversation_id=c.id JOIN crm.legacy_message_evidence e USING(message_id) WHERE c.provider='wechat' AND c.account=%s AND c.external_id=%s AND m.message_type=%s AND m.occurred_at=to_timestamp(%s) AND e.source_record_id=%s",(manifest['own_id'],conversation,message_type,at,source_id)).fetchall()
                    if len(rows)>1:raise ValueError('ambiguous message media association; no link guessed')
                    if not rows:missing_current+=1;continue
                    media_id=db.execute('SELECT id FROM crm.media_assets WHERE sha256=%s',(asset['sha256'],)).fetchone()[0]
                    # Only the message owns this link. Account/person ownership
                    # remains governed by its existing attribution rules.
                    db.execute("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) SELECT 'message_event',%s,%s,%s WHERE NOT EXISTS(SELECT 1 FROM crm.media_links WHERE entity_type='message_event' AND entity_id=%s AND media_id=%s) ON CONFLICT DO NOTHING",(rows[0][0],media_id,kind,rows[0][0],media_id))
                    linked+=1
            print(json.dumps({'phase':'indexed_message_media','references_linked':linked,'pending_file_resolution':pending,'unmatched_current_messages':missing_current}),flush=True)
        print(json.dumps({'phase':'indexed_message_media_registered','registered_file_records':registered,'message_references_linked':linked,'pending_file_resolution':pending,'unmatched_current_messages':missing_current,'all_chatlog_media_complete':False,'unresolved_file_records':failures,'partial':bool(failures)}),flush=True)
if __name__=='__main__':main()

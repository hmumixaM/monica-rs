#!/usr/bin/env python3
"""Opt-in WeChat group-photo recovery from an explicitly mounted read-only cache.

No Chatlog HTTP calls: some image GET handlers write decrypted files into the
source cache. This worker only reads source files, decodes into Rust's own CAS,
and attaches media to existing message IDs. It never creates people/messages.
"""
import argparse
from contextlib import closing
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import subprocess
import sys
import time
import uuid
import xml.etree.ElementTree as ET

MARKER = 'monica-rust-isolated-media-v1\n'
MAX_IMAGE = 64 * 1024 * 1024
NIL = uuid.UUID(int=0)
NAME = re.compile(r'^([0-9a-f]{32})(.*)$', re.I)
ATTRIBUTES = ('md5', 'hdmd5', 'hevc_mid_size_md5', 'thumbmd5')
# Resolve raw content from the current CRM revision, never an undelivered head.
SCOPE = '''m.provider='wechat' AND c.provider='wechat' AND c.account=%s AND c.conversation_type='group'
 AND NOT m.tombstone AND (lower(m.message_type) IN ('image','photo') OR
 CASE WHEN m.message_type ~ '^-?[0-9]{1,20}$' THEN mod(mod(m.message_type::numeric,4294967296)+4294967296,4294967296)=3 ELSE false END)'''
DISCOVER = '''SELECT m.message_id,m.conversation_id,(m.occurred_at AT TIME ZONE 'UTC')::date
 FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE ''' + SCOPE
CURRENT = '''SELECT m.message_id,m.conversation_id,(m.occurred_at AT TIME ZONE 'UTC')::date,
 m.body,coalesce(v.payload_json,o.payload_json),r.payload
 FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id
 LEFT JOIN crm.legacy_message_evidence e ON e.message_id=m.message_id
 LEFT JOIN crm.content_objects o ON o.id=e.content_object_id
 LEFT JOIN crm.legacy_chatlog_projection p ON p.entity_type='message' AND p.entity_id=m.message_id
 LEFT JOIN crm.legacy_chatlog_versions lv ON lv.entity_type='message' AND lv.entity_id=m.message_id AND lv.revision=p.revision
 LEFT JOIN crm.content_objects v ON v.id=lv.content_object_id
 LEFT JOIN crm.connection_evidence ce ON ce.message_id=m.message_id
 LEFT JOIN wechat.connection_payloads r ON r.content_hash=ce.content_hash
 WHERE (ce.id IS NULL OR (ce.included AND NOT ce.deleted)) AND ''' + SCOPE


class Paused(Exception):
    pass


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(256 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def image_references(*values):
    """Only image XML attributes are identities; never guess hashes from prose/URLs."""
    references = []
    def visit(value):
        if isinstance(value, dict):
            for key in ('text', 'body', 'raw_body', 'chatlog_original', 'content'):
                if key in value:
                    visit(value[key])
        elif isinstance(value, str) and len(value) <= 8 * 1024 * 1024:
            start = value.find('<msg')
            if start < 0:
                start = value.find('<img')
            if start < 0 or '<!DOCTYPE' in value.upper() or '<!ENTITY' in value.upper():
                return
            try:
                root = ET.fromstring(value[start:])
            except ET.ParseError:
                return
            for node in root.iter('img'):
                for attr in ATTRIBUTES:
                    key = node.get(attr, '').lower()
                    if re.fullmatch('[0-9a-f]{32}', key) and key != '0' * 32 and key not in references:
                        references.append(key)
    for value in values:
        visit(value)
    return references


def quality(name):
    """Filename tier is evidence, dimensions alone cannot prove an original."""
    stem = Path(name).stem.lower()
    if stem.endswith(('_original', '.original')):
        return 'original', 5
    if stem.endswith(('_h', '.h', '_hd', '.hd')):
        return 'high', 4
    if stem.endswith(('_b', '.b')):
        return 'standard', 3
    if stem.endswith(('_t', '.t', '_thumb', '.thumb')):
        return 'thumbnail', 1
    return 'unknown', 2


def canonical(path):
    path = Path(os.path.abspath(path))
    if path.resolve(strict=True) != path:
        raise ValueError('canonical paths without symlinks required')
    return path


def roots(source, media, state):
    source, media, state = map(canonical, (source, media, state))
    for a, b in ((source, media), (source, state), (media, state)):
        if a == b or a in b.parents or b in a.parents:
            raise ValueError('independent disjoint roots required')
    if (media / '.monica-rust-migration-media').read_text() != MARKER:
        raise ValueError('Rust media identity marker required')
    if sys.platform.startswith('linux') and not os.statvfs(source).f_flag & os.ST_RDONLY:
        raise ValueError('source must be mounted read-only')
    return source, media, state


def safe_file(root, relative):
    relative = Path(relative)
    if relative.is_absolute() or any(p in ('..', '.') for p in relative.parts):
        raise ValueError('invalid relative source path')
    path = root / relative
    if path.resolve(strict=True) != path or not path.is_file():
        raise ValueError('regular source file without symlinks required')
    return path


def verify_asset(media, asset):
    key = asset['sha256']
    if not re.fullmatch('[0-9a-f]{64}', key):
        raise ValueError('invalid decoded digest')
    relative = f'sha256/{key[:2]}/{key[2:4]}/{key}'
    if asset['object_path'] != relative or not asset['media_type'].startswith('image/'):
        raise ValueError('invalid decoded image')
    path = safe_file(media, relative)
    if path.stat().st_size != asset['byte_length'] or digest(path) != key:
        raise ValueError('decoded image checksum differs')
    # The helper syncs the blob. Persist directory entries before recording a link.
    parent = path.parent
    while True:
        fd = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
        if parent == media:
            break
        parent = parent.parent


def catalog(source, ledger, guard):
    """Disk-backed index covers every cache directory, including unlinked groups."""
    ledger.execute('DELETE FROM files')
    count = 0
    def walk_error(_):
        raise ValueError('image cache enumeration failed')
    for directory, dirs, files in os.walk(source, followlinks=False, onerror=walk_error):
        dirs[:] = [d for d in dirs if not (Path(directory) / d).is_symlink()]
        for name in files:
            match = NAME.match(name)
            # Image attachment folders only; exclude avatars/video thumbnails.
            path = Path(directory) / name
            if not match or 'img' not in [p.lower() for p in path.relative_to(source).parts[:-1]]:
                continue
            if path.suffix.lower() not in ('.dat', '.jpg', '.jpeg', '.png', '.gif', '.webp', '.bmp', ''):
                continue
            if path.is_symlink() or not path.is_file():
                continue
            info = path.stat()
            q, tier = quality(name)
            ledger.execute('INSERT OR IGNORE INTO files VALUES(?,?,?,?,?,?)',
                           (match[1].lower(), str(path.relative_to(source)), q, tier, info.st_size, info.st_mtime_ns))
            count += 1
            if count % 100 == 0:
                guard()
                ledger.commit()
        guard()
    ledger.commit()
    return count


def metadata_aliases(source, ledger, index_path, account, guard):
    """Chatlog hardlink MD5 and filename MD5 can differ; honor both identities."""
    index_path = canonical(index_path)
    with closing(sqlite3.connect(index_path.as_uri() + '?mode=ro&immutable=1', uri=True)) as index:
        if index.execute('PRAGMA quick_check').fetchone() != ('ok',):
            raise ValueError('invalid frozen metadata index')
        metadata = dict(index.execute('SELECT * FROM metadata'))
        if metadata.get('own_id') != account or metadata.get('complete') != '1':
            raise ValueError('file metadata account differs or inventory is incomplete')
        for kind, key, encoded in index.execute("SELECT kind,md5,candidates FROM files WHERE kind='image'"):
            guard()
            if not re.fullmatch('[0-9a-fA-F]{32}', key):
                continue
            for relative in json.loads(encoded):
                parts = Path(relative).parts
                if not parts or parts[0] != 'msg':
                    raise ValueError('metadata image path must be below msg')
                try:
                    path = safe_file(source, str(Path(*parts[1:])))
                except FileNotFoundError:
                    continue
                q, tier = quality(path.name)
                info = path.stat()
                ledger.execute('INSERT OR IGNORE INTO files VALUES(?,?,?,?,?,?)',
                               (key.lower(), str(path.relative_to(source)), q, tier, info.st_size, info.st_mtime_ns))
    ledger.commit()


def candidates(ledger, references):
    found = []
    for key in references:
        found.extend(ledger.execute('SELECT path,quality,tier,bytes,mtime FROM files WHERE md5=?', (key,)))
    # Prefer high/original but retain all fallbacks when the best file cannot decode.
    return sorted(set(found), key=lambda r: (-r[2], -r[3], r[0]))


def decode(source, media, decoder, relative, env):
    with subprocess.Popen([str(decoder), '--source-root', str(source), '--target-root', str(media)],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                          env=env, start_new_session=True) as proc:
        try:
            output, _ = proc.communicate(json.dumps({'path': relative, 'kind': 'image'}).encode(), timeout=45)
            if proc.returncode != 0:
                raise ValueError('image decoder failed')
        finally:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    if len(output) > 8192:
        raise ValueError('invalid codec result')
    asset = json.loads(output)
    verify_asset(media, asset)
    return asset


class Worker:
    def __init__(self, db, ledger, args, paths, env):
        self.db, self.ledger, self.args = db, ledger, args
        self.source, self.media, self.state = paths
        self.env = env
        self.run_id = None
        self.last_guard = 0

    def guard(self, force=False):
        if not force and time.monotonic() - self.last_guard < 1:
            return
        self.last_guard = time.monotonic()
        control = self.db.execute('SELECT enabled AND NOT paused FROM crm.wechat_image_control').fetchone()
        if not control or not control[0]:
            raise Paused()
        if sys.platform.startswith('linux'):
            available = next(int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemAvailable:'))
            if available < 768 * 1024:
                raise Paused()
        if self.run_id:
            self.db.execute('UPDATE crm.wechat_image_runs SET heartbeat_at=now() WHERE id=%s', (self.run_id,))

    def open(self):
        identity = self.db.execute('SELECT current_database(),system_identifier::text FROM pg_control_system()').fetchone()
        if identity != (self.args.expected_database, self.args.expected_system_id):
            raise ValueError('target database identity differs')
        if not self.db.execute("SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=68 AND success)").fetchone()[0]:
            raise ValueError('Rust group-image migration required')
        if not self.db.execute("SELECT pg_try_advisory_lock(hashtextextended('monica-wechat-group-images',0))").fetchone()[0]:
            raise ValueError('another group-image worker is active')
        connections = self.db.execute("SELECT DISTINCT c.connection_id,s.external_account FROM crm.conversations c JOIN crm.source_connections s ON s.id=c.connection_id WHERE c.provider='wechat' AND c.account=%s AND c.conversation_type='group'", (self.args.account,)).fetchall()
        self.source_owner = self.args.source_owner or self.args.account
        if connections:
            if len(connections) != 1 or str(connections[0][0]) != self.args.account or not self.args.source_owner or connections[0][1] != self.source_owner:
                raise ValueError('explicit source owner must match the target WeChat connection')
        elif self.source_owner != self.args.account:
            raise ValueError('legacy target account must match the source owner')
        self.guard(True)
        self.ledger.executescript('''CREATE TABLE IF NOT EXISTS identity(value TEXT NOT NULL,run_id TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS files(md5 TEXT,path TEXT,quality TEXT,tier INTEGER,bytes INTEGER,mtime INTEGER,PRIMARY KEY(md5,path));
 CREATE INDEX IF NOT EXISTS files_hash ON files(md5,tier);
 CREATE TABLE IF NOT EXISTS decoded(path TEXT PRIMARY KEY,raw_sha256 TEXT NOT NULL,asset TEXT NOT NULL);''')
        stamp = json.dumps([*identity, self.args.account, self.source_owner, str(self.source), str(self.media)], separators=(',', ':'))
        prior = self.ledger.execute('SELECT value,run_id FROM identity').fetchone()
        if prior and prior[0] != stamp:
            raise ValueError('recovery state belongs to another target or source')
        self.run_id = uuid.UUID(prior[1]) if prior else uuid.uuid4()
        if not prior:
            self.ledger.execute('INSERT INTO identity VALUES(?,?)', (stamp, str(self.run_id)))
            self.ledger.commit()
        self.db.execute("INSERT INTO crm.wechat_image_runs(id,account) VALUES(%s,%s) ON CONFLICT(id) DO UPDATE SET status='indexing',discovery_complete=false,error_code=NULL,completed_at=NULL,heartbeat_at=now()", (self.run_id, self.args.account))

    def discover(self):
        after = NIL
        while True:
            self.guard()
            rows = self.db.execute(DISCOVER + ' AND m.message_id>%s ORDER BY m.message_id LIMIT 500', (self.args.account, after)).fetchall()
            if not rows:
                break
            with self.db.transaction():
                for mid, group, day, *_ in rows:
                    self.db.execute('INSERT INTO crm.wechat_group_images(run_id,message_id,conversation_id,day) VALUES(%s,%s,%s,%s) ON CONFLICT(run_id,message_id) DO UPDATE SET conversation_id=excluded.conversation_id,day=excluded.day WHERE (crm.wechat_group_images.conversation_id,crm.wechat_group_images.day) IS DISTINCT FROM (excluded.conversation_id,excluded.day)', (self.run_id, mid, group, day))
            after = rows[-1][0]
        self.db.execute("UPDATE crm.wechat_image_runs SET discovery_complete=true,status='running',heartbeat_at=now() WHERE id=%s", (self.run_id,))

    def item(self, mid):
        self.guard()
        row = self.db.execute(CURRENT + ' AND m.message_id=%s', (self.args.account, mid)).fetchone()
        state, q, asset = 'source_changed', 'none', None
        if row:
            refs = image_references(*row[3:])
            reference_hash = hashlib.sha256(json.dumps(sorted(refs), separators=(',', ':')).encode()).hexdigest()
            provenance_prefix = 'group-image:' + str(mid) + ':' + reference_hash
            choices = candidates(self.ledger, refs)
            state = 'no_reference' if not refs else 'source_missing'
            # Reuse assets published by the original import, as well as this worker.
            # An opaque filename never earns an original/high quality claim.
            best = None
            ranks = {'original': 5, 'high': 4, 'standard': 3, 'unknown': 2, 'thumbnail': 1}
            proofs = [provenance_prefix + ':' + value for value in ranks]
            existing = self.db.execute("SELECT to_jsonb(a),p.source_record_id FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id JOIN crm.media_provenance p ON p.media_id=a.id AND p.source='wechat' AND p.source_record_id=ANY(%s) WHERE l.entity_type='message_event' AND l.entity_id=%s AND a.media_type LIKE 'image/%%' ORDER BY a.id", (proofs, mid)).fetchall()
            for saved, proof in existing:
                try:
                    verify_asset(self.media, saved)
                    saved_q = proof.rsplit(':', 1)[-1]
                    if best is None or ranks[saved_q] > ranks[best[1]]:
                        best = saved, saved_q
                except (OSError, ValueError, KeyError):
                    continue
            if best:
                asset, q = best
                state = 'available'
                choices = [choice for choice in choices if choice[2] > ranks[q]]
                self.db.execute('UPDATE crm.wechat_image_runs SET reused_files=reused_files+1 WHERE id=%s', (self.run_id,))
            for relative, q0, _, size, _ in choices:
                self.guard()
                if size > MAX_IMAGE:
                    if asset is None:
                        state = 'decode_failed'
                    continue
                try:
                    path = safe_file(self.source, relative)
                    raw_hash = digest(path)
                    cached = self.ledger.execute('SELECT asset FROM decoded WHERE path=? AND raw_sha256=?', (relative, raw_hash)).fetchone()
                    if cached:
                        try:
                            verify_asset(self.media, json.loads(cached[0]))
                        except (ValueError, OSError, KeyError, TypeError):
                            # The receipt is derived state, not a reason to skip
                            # a healthy source forever. Rebuild it with the codec.
                            self.ledger.execute('DELETE FROM decoded WHERE path=?', (relative,))
                            self.ledger.commit()
                            cached = None
                    if cached:
                        asset = json.loads(cached[0])
                        self.db.execute('UPDATE crm.wechat_image_runs SET reused_files=reused_files+1 WHERE id=%s', (self.run_id,))
                    else:
                        started = time.monotonic()
                        asset = decode(self.source, self.media, self.args.decoder, relative, self.env)
                        if digest(path) != raw_hash:
                            raise ValueError('source changed during decode')
                        self.ledger.execute('INSERT OR REPLACE INTO decoded VALUES(?,?,?)', (relative, raw_hash, json.dumps(asset)))
                        self.ledger.commit()
                        self.db.execute('UPDATE crm.wechat_image_runs SET copied_bytes=copied_bytes+%s WHERE id=%s', (asset['byte_length'], self.run_id))
                        # Pace copied input/output bytes at 2 MiB/s; checksum
                        # reads are additional. Pause checks also run while pacing.
                        until = started + (size + asset['byte_length']) / (2 * 1024 * 1024)
                        while time.monotonic() < until:
                            self.guard(True)
                            time.sleep(min(1, max(0, until - time.monotonic())))
                    state, q = 'available', q0
                    break
                except (ValueError, OSError, subprocess.SubprocessError, KeyError):
                    state, q, asset = ('available', best[1], best[0]) if best else ('decode_failed', 'none', None)
        with self.db.transaction():
            self.db.execute('SET TRANSACTION ISOLATION LEVEL READ COMMITTED')
            # Fence pause and source retractions/edits before attaching a decoded file.
            allowed = self.db.execute('SELECT enabled AND NOT paused FROM crm.wechat_image_control FOR SHARE').fetchone()
            if not allowed or not allowed[0]:
                raise Paused()
            # Acquire row locks in a separate statement. After waiting for a
            # projector, a joined locking SELECT can retain old outer-join
            # payloads via EvalPlanQual. The next statement needs a fresh snapshot.
            # The projector locks conversation before message. Only the message
            # is needed here: image/raw/lifecycle/position changes write it in
            # the same transaction. Do not introduce the reverse m -> c order.
            self.db.execute("SELECT message_id FROM crm.message_search WHERE message_id=%s FOR SHARE", (mid,)).fetchone()
            current = self.db.execute(CURRENT + ' AND m.message_id=%s', (self.args.account, mid)).fetchone()
            if current != row:
                state, q, asset = 'source_changed', 'none', None
            media_id = None
            if asset:
                key = asset['sha256']
                # Share MediaStore's orphan-collection fence. Earlier file
                # checks precede the message-lock wait and cannot protect an
                # old, unregistered CAS blob from concurrent GC.
                self.db.execute("SELECT pg_advisory_xact_lock(hashtextextended('media-gc:'||%s,0))", (key,))
                verify_asset(self.media, asset)
                media_id = uuid.uuid5(uuid.NAMESPACE_URL, 'monica:wechat-group-image:' + key)
                self.db.execute("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,width,height,source_type,verified_at) VALUES(%s,%s,%s,%s,%s,%s,%s,'wechat',now()) ON CONFLICT(sha256) DO NOTHING", (media_id, key, asset['media_type'], asset['byte_length'], asset['object_path'], asset.get('width'), asset.get('height')))
                media_id, stored_bytes = self.db.execute('SELECT id,byte_length FROM crm.media_assets WHERE sha256=%s', (key,)).fetchone()
                if stored_bytes != asset['byte_length']:
                    raise ValueError('registered image size differs')
                self.db.execute("INSERT INTO crm.media_provenance(media_id,source,source_record_id) VALUES(%s,'wechat',%s) ON CONFLICT DO NOTHING", (media_id, provenance_prefix + ':' + q))
                # Only retire this feature's superseded public attachments.
                # Immutable blobs/provenance and imported/manual links remain.
                self.db.execute("DELETE FROM crm.media_links WHERE entity_type='message_event' AND entity_id=%s AND role LIKE 'wechat-group-image:%%' AND media_id<>%s", (mid, media_id))
                self.db.execute("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) SELECT 'message_event',%s,%s,%s WHERE NOT EXISTS(SELECT 1 FROM crm.media_links WHERE entity_type='message_event' AND entity_id=%s AND media_id=%s) ON CONFLICT DO NOTHING", (mid, media_id, 'wechat-group-image:' + q, mid, media_id))
            else:
                self.db.execute("DELETE FROM crm.media_links WHERE entity_type='message_event' AND entity_id=%s AND role LIKE 'wechat-group-image:%%'", (mid,))
            self.db.execute('UPDATE crm.wechat_group_images SET state=%s,quality=%s,media_id=%s,conversation_id=coalesce(%s,conversation_id),day=coalesce(%s,day),attempts=attempts+1,updated_at=now() WHERE run_id=%s AND message_id=%s', (state, q, media_id, current[1] if current else None, current[2] if current else None, self.run_id, mid))

    def run(self):
        self.open()
        count = catalog(self.source, self.ledger, self.guard)
        if self.args.file_index:
            metadata_aliases(self.source, self.ledger, self.args.file_index, self.source_owner, self.guard)
        self.db.execute('UPDATE crm.wechat_image_runs SET indexed_files=%s WHERE id=%s', (count, self.run_id))
        self.discover()
        if self.args.retry:
            self.db.execute("UPDATE crm.wechat_group_images SET state='pending',quality='none' WHERE run_id=%s AND state<>'pending'", (self.run_id,))
        while True:
            self.guard()
            rows = self.db.execute("SELECT message_id FROM crm.wechat_group_images WHERE run_id=%s AND state='pending' ORDER BY message_id LIMIT 100", (self.run_id,)).fetchall()
            if not rows:
                break
            for (mid,) in rows:
                self.item(mid)
            self.report()
        self.db.execute("UPDATE crm.wechat_image_runs SET status='complete',heartbeat_at=now(),completed_at=now() WHERE id=%s", (self.run_id,))
        self.report()

    def report(self):
        row = self.db.execute('SELECT status,counts,discovery_complete FROM crm.wechat_image_runs WHERE id=%s', (self.run_id,)).fetchone()
        print(json.dumps({'run_id': str(self.run_id), 'state': row[0], 'counts': row[1], 'discovery_complete': row[2]}, sort_keys=True), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', help='explicitly execute; otherwise print usage only')
    parser.add_argument('--retry', action='store_true', help='recheck all items, including thumbnail/unknown quality, with existing blobs reused')
    parser.add_argument('--file-index', type=Path, help='optional frozen Chatlog hardlink index; accepts aliases only for the selected account')
    parser.add_argument('--source-owner', help='source WeChat wxid; required for a native target connection UUID and checked against its saved external_account')
    for name in ('source-root', 'media-root', 'state-root', 'decoder'):
        parser.add_argument('--' + name, type=Path)
    for name in ('account', 'expected-database', 'expected-system-id'):
        parser.add_argument('--' + name)
    args = parser.parse_args()
    if not args.run:
        parser.print_help()
        return
    if any(getattr(args, x) is None for x in ('source_root', 'media_root', 'state_root', 'decoder', 'account', 'expected_database', 'expected_system_id')):
        parser.error('execution requires all explicit source and target identity arguments')
    def stop(_signal, _frame):
        raise Paused()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    # Dependencies/credentials are accessed only on explicit execution.
    import psycopg
    paths = roots(args.source_root, args.media_root, args.state_root)
    args.decoder = canonical(args.decoder)
    if not args.decoder.is_file():
        raise ValueError('dedicated codec executable required')
    lock_path = paths[2] / 'worker.lock'
    if lock_path.is_symlink():
        raise ValueError('state lock alias')
    with lock_path.open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        state_path = paths[2] / 'group-images.sqlite'
        if state_path.is_symlink():
            raise ValueError('state database alias')
        with closing(sqlite3.connect(state_path)) as ledger, psycopg.connect(os.environ['MONICA_DATABASE_URL'], autocommit=True) as db:
            child_env = {k: v for k, v in os.environ.items() if k in ('PATH', 'TMPDIR', 'LD_LIBRARY_PATH', 'MONICA_MIGRATION_IMAGE_KEY', 'MONICA_MIGRATION_IMAGE_XOR')}
            worker = Worker(db, ledger, args, paths, child_env)
            try:
                worker.run()
            except Paused:
                if worker.run_id:
                    db.execute("UPDATE crm.wechat_image_runs SET status='paused',heartbeat_at=now() WHERE id=%s", (worker.run_id,))
                    worker.report()
            except Exception:
                if worker.run_id:
                    db.execute("UPDATE crm.wechat_image_runs SET status='failed',error_code='worker_failed',heartbeat_at=now() WHERE id=%s", (worker.run_id,))
                raise


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Exception:
        # Do not expose database credentials, source keys, message bodies or paths.
        print('{"state":"failed","error_code":"worker_failed"}', file=sys.stderr)
        sys.exit(1)

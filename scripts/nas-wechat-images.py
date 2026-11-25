"""Continuously attach recent WeChat photos from a read-only, same-account cache.

No network, source mutation, full cache walk, model call or new message/person.
The existing image worker owns decoding, publication fences and current-row checks.
"""
import argparse
from collections import defaultdict
from contextlib import closing, nullcontext
from datetime import datetime, timedelta
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import sys
import time
from monica_imports.sqlite_snapshot import SourceChanged, publish_snapshot, source_signature

spec = importlib.util.spec_from_file_location('group_images', Path(__file__).with_name('wechat-group-images.py'))
images = importlib.util.module_from_spec(spec)
spec.loader.exec_module(images)


def component(value):
    if not isinstance(value, str) or value in ('', '.', '..') or '/' in value or '\\' in value or '\0' in value:
        raise ValueError('invalid cache path component')
    return value


def requested_catalog(source, metadata, ledger, requested, guard):
    """Scan the small metadata table once, stat only requested same-account files."""
    ledger.execute('DELETE FROM files')
    count = 0
    for index, row in enumerate(metadata.execute('SELECT m.md5,m.file_name,d1.username,d2.username,m.extra_buffer FROM image_hardlink_info_v4 m LEFT JOIN dir2id d1 ON d1.rowid=m.dir1 LEFT JOIN dir2id d2 ON d2.rowid=m.dir2')):
        if index % 500 == 0:
            guard()
        key, name, one, two, extra = row
        identities = set()
        if isinstance(key, str) and re.fullmatch('[0-9a-fA-F]{32}', key):
            identities.add(key.lower())
        filename = images.NAME.match(name) if isinstance(name, str) else None
        if filename:
            identities.add(filename[1].lower())
        matched = identities & requested
        if not matched:
            continue
        name, one, two = map(component, (name, one, two))
        base = Path('attach')/one/two
        if isinstance(extra, bytes):
            extra = extra.decode('utf-8', errors='ignore')
        extra = re.sub('[^a-zA-Z0-9]', '', extra or '')
        choices = [base/'Rec'/extra/'Img'/name, base/'Img'/name] if extra else [base/'Img'/name]
        for relative in choices:
            try:
                candidate = source/relative
                if not candidate.exists() and not candidate.is_symlink():
                    continue
                path = images.safe_file(source, str(relative))
                info = path.stat()
            except FileNotFoundError:
                continue
            quality, tier = images.quality(name)
            for identity in sorted(matched):
                ledger.execute('INSERT OR IGNORE INTO files VALUES(?,?,?,?,?,?)', (identity, str(relative), quality, tier, info.st_size, info.st_mtime_ns))
                count += 1
    ledger.commit()
    return count


def snapshot(source, target, guard):
    publish_snapshot(source, target, progress=lambda *_: guard())


def resource_hash(metadata, peer, record, server, timestamp):
    if metadata is None:
        return None
    value = server or (record or '').removeprefix('server:')
    if not peer or not re.fullmatch(r'-?[0-9]{1,20}', value):
        return None
    server_id = int(value)
    if 2**63 <= server_id < 2**64:
        server_id -= 2**64
    if server_id == 0 or not -2**63 <= server_id < 2**63:
        return None
    rows = metadata.execute("""
        SELECT m.packed_info FROM ChatName2Id c JOIN MessageResourceInfo m ON m.chat_id=c.rowid
        WHERE c.user_name=? AND m.message_svr_id=? AND m.message_create_time=?
          AND (m.message_local_type & 4294967295)=3 LIMIT 2
    """, (peer, server_id, timestamp)).fetchall()
    if len(rows) != 1 or not isinstance(rows[0][0], bytes):
        return None
    # WeChat stores the cache key in field 2, containing a field 1 string.
    # Match that record shape rather than harvesting unrelated hex strings.
    match = re.fullmatch(rb'\x12\x22\x0a\x20([0-9A-Fa-f]{32})', rows[0][0])
    if match and match[1] != b'0' * 32:
        return match[1].decode('ascii').lower()
    return None


def supports_resources(db):
    if db is None:
        return False
    required = {
        'ChatName2Id': {'user_name'},
        'MessageResourceInfo': {
            'chat_id', 'message_svr_id', 'message_create_time', 'message_local_type', 'packed_info',
        },
    }
    return all(columns <= {row[1] for row in db.execute(f'PRAGMA table_info({table})')}
               for table, columns in required.items())


def cache_folders(peer, day):
    identity = hashlib.md5(peer.encode('utf-8')).hexdigest()
    first = day.replace(day=1)
    months = {first, (first - timedelta(days=1)).replace(day=1),
              (first + timedelta(days=32)).replace(day=1)}
    return [Path('attach') / identity / month.strftime('%Y-%m') for month in sorted(months)]


def resource_catalog(source, ledger, folders, guard):
    for relative, wanted in folders.items():
        guard()
        folder = source / relative
        if not folder.is_dir() or folder.resolve() != folder:
            continue
        for directory in (folder / 'Img', *folder.glob('Rec/*/Img')):
            if not directory.is_dir() or directory.resolve() != directory:
                continue
            for file in directory.iterdir():
                match = images.NAME.match(file.name)
                if not match or match[1].lower() not in wanted or file.suffix.lower() not in (
                    '.dat', '.jpg', '.jpeg', '.png', '.gif', '.webp', '.bmp', '',
                ):
                    continue
                path = str(file.relative_to(source))
                try:
                    info = images.safe_file(source, path).stat()
                except (OSError, ValueError):
                    continue
                quality, tier = images.quality(file.name)
                ledger.execute('INSERT OR IGNORE INTO files VALUES(?,?,?,?,?,?)',
                               (match[1].lower(), path, quality, tier, info.st_size, info.st_mtime_ns))
    ledger.commit()


class RecentImages(images.Worker):
    def __init__(self, db, ledger, args, paths, env):
        super().__init__(db, ledger, args, paths, env)
        self.current_sql = self.current_sql.replace('r.payload\n FROM', '''r.payload,
 regexp_replace(coalesce(nullif(c.source_external_id,''),c.external_id),'^(direct|group|room):',''),
 e.source_record_id,coalesce(e.original->>'server_id',e.original->>'server_message_id',e.original->>'serverId'),
 floor(extract(epoch from m.occurred_at))::bigint
 FROM''')
        self.resources = None
        self.resource_signature = None

    def references(self, row):
        references = images.image_references(*row[3:6])
        key = resource_hash(self.resources, *row[6:])
        if key and key not in references:
            references.append(key)
        return references

    def candidate_files(self, row, references):
        original = images.candidates(self.ledger, images.image_references(*row[3:6]))
        identity = hashlib.md5(row[6].encode('utf-8')).hexdigest()
        same_chat = [candidate for candidate in images.candidates(self.ledger, references)
                     if Path(candidate[0]).parts[:2] == ('attach', identity)]
        return sorted(set(original + same_chat), key=lambda row: (-row[2], -row[3], row[0]))

    def run(self):
        self.open()
        try:
            self.run_with_resources()
        finally:
            self.db.execute("SELECT pg_advisory_unlock(hashtextextended('monica-wechat-group-images',0))")

    def resource_database(self):
        path = self.args.hardlink_db.parent.parent / 'message/message_resource.db'
        if path.is_file():
            target = self.state / 'message-resource.sqlite'
            if self.resource_signature != source_signature(path) or not target.is_file():
                self.resource_signature = publish_snapshot(path, target, progress=lambda *_: self.guard())
            return target
        return None

    def hardlink_database(self):
        target = self.state/'hardlink.sqlite'
        snapshot(self.args.hardlink_db, target, self.guard)
        return target

    def run_with_resources(self):
        path = self.resource_database()
        connection = nullcontext(None)
        if path is not None:
            connection = closing(sqlite3.connect(path.as_uri() + '?mode=ro&immutable=1', uri=True))
        with connection as resource:
            self.resources = resource if supports_resources(resource) else None
            try:
                self.scan_resources()
            finally:
                self.resources = None

    def scan_resources(self):
        complete = self.discover()
        batch_size = getattr(self.args, 'batch_size', None)
        if batch_size:
            batch = [mid for (mid,) in self.db.execute("""SELECT message_id FROM crm.wechat_group_images
                WHERE run_id=%s AND (state='pending' OR
                  (state IN ('source_missing','no_reference','decode_failed') AND updated_at<now()-interval '30 minutes'))
                ORDER BY attempts,updated_at,message_id LIMIT %s""", (self.run_id, batch_size))]
            scoped = ' AND m.message_id=ANY(%s)'
            scope_args = (*self.query_args, batch)
        else:
            # Reuse successful links; previously missing cache files may arrive.
            self.db.execute("UPDATE crm.wechat_group_images SET state='pending' WHERE run_id=%s AND state<>'available' AND state<>'pending'", (self.run_id,))
            batch = None
            scoped = " AND EXISTS(SELECT FROM crm.wechat_group_images i WHERE i.run_id=%s AND i.message_id=m.message_id AND i.state='pending')"
            scope_args = (*self.query_args, self.run_id)
        requested = set()
        folders = defaultdict(set)
        after = images.NIL
        while True:
            self.guard()
            rows = self.db.execute(self.current_sql + scoped + " AND m.message_id>%s ORDER BY m.message_id LIMIT 100", (*scope_args, after)).fetchall()
            if not rows:
                break
            for row in rows:
                references = self.references(row)
                requested.update(references)
                for folder in cache_folders(row[6], row[2]):
                    folders[folder].update(references)
            after = rows[-1][0]
        if requested:
            target = self.hardlink_database()
            if target is not None:
                with closing(sqlite3.connect(target.as_uri()+'?mode=ro&immutable=1', uri=True)) as metadata:
                    requested_catalog(self.source, metadata, self.ledger, requested, self.guard)
            else:
                self.ledger.execute('DELETE FROM files')
                self.ledger.commit()
            resource_catalog(self.source, self.ledger, folders, self.guard)
            count = self.ledger.execute('SELECT count(*) FROM files').fetchone()[0]
        else:
            count = 0
        self.db.execute('UPDATE crm.wechat_image_runs SET indexed_files=%s WHERE id=%s', (count, self.run_id))
        if batch is not None:
            for mid in batch:
                self.item(mid)
        else:
            while True:
                self.guard()
                rows = self.db.execute("SELECT message_id FROM crm.wechat_group_images WHERE run_id=%s AND state='pending' ORDER BY message_id LIMIT 100", (self.run_id,)).fetchall()
                if not rows:
                    break
                for (mid,) in rows:
                    self.item(mid)
                self.report()
        if complete and (batch is None or len(batch) < batch_size):
            self.db.execute("UPDATE crm.wechat_image_runs SET status='complete',heartbeat_at=now(),completed_at=now() WHERE id=%s", (self.run_id,))
        else:
            self.db.execute('UPDATE crm.wechat_image_runs SET heartbeat_at=now() WHERE id=%s', (self.run_id,))
        self.report()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', required=True)
    for name in ('source-root','media-root','state-root','decoder','hardlink-db'):
        parser.add_argument('--'+name, type=Path, required=True)
    for name in ('account','expected-system-id','since'):
        parser.add_argument('--'+name, required=True)
    parser.add_argument('--expected-database', default='monica_rust')
    parser.add_argument('--expected-database-oid', type=int, required=True)
    parser.add_argument('--interval', type=int, default=300)
    args = parser.parse_args()
    if os.geteuid() != 10001 or args.interval < 30:
        raise ValueError('UID10001 and interval >=30 required')
    cutoff = datetime.fromisoformat(args.since.replace('Z','+00:00'))
    if cutoff.tzinfo is None or cutoff.utcoffset().total_seconds() != 0:
        raise ValueError('explicit UTC cutoff required')
    args.include_direct, args.source_owner = True, None
    args.hardlink_db, args.decoder = images.canonical(args.hardlink_db), images.canonical(args.decoder)
    paths = images.roots(args.source_root, args.media_root, args.state_root)
    policy = [args.account, cutoff.isoformat(), str(args.hardlink_db), str(paths[0]), str(paths[1])]
    marker = paths[2]/'recent-policy.json'
    if marker.is_symlink():
        raise ValueError('media policy alias')
    if marker.exists():
        if json.loads(marker.read_text()) != policy:
            raise ValueError('recent media policy changed')
    else:
        with marker.open('x') as output:
            json.dump(policy, output); output.flush(); os.fsync(output.fileno())
    # Credentials arrive only over a private stdin pipe, never Docker Env/argv,
    # a key file, a status payload or an exception message.
    frame = sys.stdin.buffer.readline(4097)
    if len(frame) > 4096:
        raise ValueError('invalid private credential frame')
    credentials = json.loads(frame)
    key, xor = credentials['image_key'], int(credentials['image_xor'])
    if not isinstance(key, str) or not 1 <= len(key) <= 128 or not 0 <= xor <= 255:
        raise ValueError('invalid private credential frame')
    child_env = {k:v for k,v in os.environ.items() if k in ('PATH','TMPDIR','LD_LIBRARY_PATH')}
    child_env.update(MONICA_MIGRATION_IMAGE_KEY=key, MONICA_MIGRATION_IMAGE_XOR=str(xor))
    del credentials, key, frame
    import psycopg
    def stop(_signal, _frame):
        raise images.Paused()
    signal.signal(signal.SIGTERM, stop); signal.signal(signal.SIGINT, stop)
    with (paths[2]/'worker.lock').open('a') as lock, closing(sqlite3.connect(paths[2]/'group-images.sqlite')) as ledger, psycopg.connect(os.environ['MONICA_DATABASE_URL'], autocommit=True) as db:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        target = db.execute("SELECT system_identifier::text,(SELECT oid FROM pg_database WHERE datname=current_database()),(SELECT max(version) FROM _sqlx_migrations WHERE success) FROM pg_control_system()").fetchone()
        if target[:2] != (args.expected_system_id, args.expected_database_oid) or target[2] is None or target[2] < 86:
            raise ValueError('isolated target differs')
        worker = RecentImages(db, ledger, args, paths, child_env)
        try:
            while True:
                try:
                    worker.run()
                except SourceChanged:
                    print(json.dumps({'state': 'retrying', 'reason': 'sqlite_source_changed'}), flush=True)
                time.sleep(args.interval)
        except images.Paused:
            if worker.run_id:
                db.execute("UPDATE crm.wechat_image_runs SET status='paused',heartbeat_at=now() WHERE id=%s", (worker.run_id,))


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Exception as error:
        print(json.dumps(dict(error_code=type(error).__name__)), flush=True)
        raise SystemExit(1)

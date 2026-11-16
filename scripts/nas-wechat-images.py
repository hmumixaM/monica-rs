"""Continuously attach recent WeChat photos from a read-only, same-account cache.

No network, source mutation, full cache walk, model call or new message/person.
The existing image worker owns decoding, publication fences and current-row checks.
"""
import argparse
from contextlib import closing
from datetime import datetime
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import sys
import time
from urllib.parse import quote

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
        if not isinstance(key, str) or key.lower() not in requested:
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
            ledger.execute('INSERT OR IGNORE INTO files VALUES(?,?,?,?,?,?)', (key.lower(), str(relative), quality, tier, info.st_size, info.st_mtime_ns))
            count += 1
    ledger.commit()
    return count


def snapshot(source, target, guard):
    temporary = target.with_suffix('.partial')
    if source.resolve() != source or source.is_symlink() or target.is_symlink() or temporary.is_symlink():
        raise ValueError('metadata snapshot alias')
    temporary.unlink(missing_ok=True)
    last = time.monotonic()
    remaining_before = None
    def progress(status, remaining, total):
        nonlocal last, remaining_before
        guard()
        now = time.monotonic()
        if remaining_before is None or remaining < remaining_before:
            last = now
        if now-last > 120:
            raise TimeoutError('metadata snapshot stalled')
        remaining_before = remaining
    try:
        with closing(sqlite3.connect('file:'+quote(str(source))+'?mode=ro', uri=True, timeout=2)) as reader, closing(sqlite3.connect(temporary)) as output:
            reader.execute('PRAGMA query_only=ON'); reader.execute('BEGIN')
            try:
                reader.execute('PRAGMA schema_version').fetchone()
                reader.backup(output, pages=128, progress=progress, sleep=.05)
            finally:
                reader.rollback()
            if output.execute('PRAGMA quick_check').fetchone() != ('ok',):
                raise ValueError('metadata snapshot integrity failed')
            output.execute('PRAGMA journal_mode=DELETE')
        os.replace(temporary, target)
    finally:
        temporary.unlink(missing_ok=True)


class RecentImages(images.Worker):
    def run(self):
        self.open()
        self.discover()
        # Ready publications are reused. Previously unavailable files can arrive
        # later even when the message and its source JSON have not changed.
        self.db.execute("UPDATE crm.wechat_group_images SET state='pending' WHERE run_id=%s AND state<>'available' AND state<>'pending'", (self.run_id,))
        requested = set()
        after = images.NIL
        while True:
            self.guard()
            rows = self.db.execute(self.current_sql + " AND m.message_id>%s AND EXISTS(SELECT 1 FROM crm.wechat_group_images i WHERE i.run_id=%s AND i.message_id=m.message_id AND i.state='pending') ORDER BY m.message_id LIMIT 100", (*self.query_args, after, self.run_id)).fetchall()
            if not rows:
                break
            for row in rows:
                requested.update(images.image_references(*row[3:]))
            after = rows[-1][0]
        target = self.state/'hardlink.sqlite'
        if requested:
            snapshot(self.args.hardlink_db, target, self.guard)
            with closing(sqlite3.connect(target.as_uri()+'?mode=ro&immutable=1', uri=True)) as metadata:
                count = requested_catalog(self.source, metadata, self.ledger, requested, self.guard)
        else:
            count = 0
        self.db.execute('UPDATE crm.wechat_image_runs SET indexed_files=%s WHERE id=%s', (count, self.run_id))
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', required=True)
    for name in ('source-root','media-root','state-root','decoder','hardlink-db'):
        parser.add_argument('--'+name, type=Path, required=True)
    for name in ('account','expected-system-id','since'):
        parser.add_argument('--'+name, required=True)
    parser.add_argument('--interval', type=int, default=300)
    args = parser.parse_args()
    if os.geteuid() != 10001 or args.interval < 30:
        raise ValueError('UID10001 and interval >=30 required')
    cutoff = datetime.fromisoformat(args.since.replace('Z','+00:00'))
    if cutoff.tzinfo is None or cutoff.utcoffset().total_seconds() != 0:
        raise ValueError('explicit UTC cutoff required')
    args.include_direct, args.expected_database, args.source_owner = True, 'monica_rust', None
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
        if target != (args.expected_system_id, 16384, 86):
            raise ValueError('isolated target differs')
        worker = RecentImages(db, ledger, args, paths, child_env)
        try:
            while True:
                worker.run()
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

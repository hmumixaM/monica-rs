#!/usr/bin/env python3
"""Process a bounded batch of media from the desktop-owned Chatlog connection."""
import importlib.util
import json
from contextlib import closing
import os
from pathlib import Path
import re
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import uuid
from types import SimpleNamespace
import sys

import psycopg
from monica_imports.media_store import MARKER, MediaStore


class EventDeadline(BaseException):
    pass


def deadline_expired(_signum, _frame):
    raise EventDeadline()


def clean_staging(db, state, account):
    stages = []
    for path in state.iterdir():
        if not path.is_dir() or path.is_symlink():
            continue
        try:
            event_id = uuid.UUID(path.name)
        except ValueError:
            continue
        stages.append((path.stat().st_mtime_ns, event_id, path))
    queue = dict(db.execute("""SELECT q.event_id,q.version FROM crm.native_moments_media q
        JOIN crm.source_connections c ON c.id=q.connection_id
        WHERE c.provider='wechat' AND c.external_account=%s AND q.event_id=ANY(%s)""",
        (account, [event_id for _, event_id, _ in stages])).fetchall())
    kept = 0
    for _, event_id, path in sorted(stages, reverse=True):
        if event_id not in queue or kept >= 2:
            shutil.rmtree(path)
        else:
            for revision in path.iterdir():
                if revision.is_dir() and not revision.is_symlink() and revision.name != str(queue[event_id]):
                    shutil.rmtree(revision)
            kept += 1


def run(db, source, media, state, runtime, account, cache=None):
    spec = importlib.util.spec_from_file_location('sns_event_media', Path(__file__).with_name('sns-media-import.py'))
    images = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(images)
    if not db.execute("SELECT pg_try_advisory_lock(hashtextextended('monica-native-moments-media',0))").fetchone()[0]:
        return
    clean_staging(db, state, account)
    if shutil.disk_usage(media).free < images.NATIVE_MIN_FREE:
        db.execute("""UPDATE crm.native_moments_media q SET retry_at=now()+interval '5 minutes'
            FROM crm.source_connections c WHERE c.id=q.connection_id AND c.provider='wechat'
              AND c.external_account=%s AND q.retry_at<=now()""", (account,))
        return
    due = db.execute("""
        SELECT q.event_id, q.connection_id, q.version, q.attempts
        FROM crm.native_moments_media q JOIN crm.source_connections s ON s.id=q.connection_id
        WHERE s.provider='wechat' AND s.external_account=%s AND s.enabled AND q.retry_at<=now()
        ORDER BY q.retry_at,q.event_id LIMIT 1
    """, (account,)).fetchone()
    if not due:
        return
    event_id, connection, version, attempts = due
    delay = min(24 * 3600, 300 * 2**min(attempts, 9))
    # Record backoff before doing any I/O, including hard interruption or quit.
    if not db.execute("""UPDATE crm.native_moments_media SET attempts=attempts+1,
        retry_at=now()+make_interval(secs=>%s) WHERE event_id=%s AND version=%s RETURNING 1""",
        (delay, event_id, version)).fetchone():
        return
    stage = state / str(event_id)
    stage.mkdir(exist_ok=True)
    if stage.resolve() != stage:
        raise ValueError('invalid desktop media staging path')
    previous_handler = signal.signal(signal.SIGALRM, deadline_expired)
    signal.alarm(360)
    try:
        result = images.import_event(db, source, media, stage/str(version), runtime, cache, event_id,
                                     native_event=(connection, version))
    except (EventDeadline, OSError, ValueError, OverflowError, KeyError, psycopg.Error):
        if db.closed or db.broken:
            raise
        return
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, previous_handler)
    if not result['failed']:
        shutil.rmtree(stage)
        db.execute('DELETE FROM crm.native_moments_media WHERE event_id=%s AND version=%s', (event_id, version))


def recover_chat_images(db, snapshots, media, state, runtime, account):
    cache = Path(os.environ['MONICA_NATIVE_WECHAT_CACHE'])
    if not cache.is_dir():
        return
    spec = importlib.util.spec_from_file_location('recent_images', Path(__file__).with_name('nas-wechat-images.py'))
    recent = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(recent)
    connection = db.execute("SELECT id FROM crm.source_connections WHERE provider='wechat' AND external_account=%s AND enabled", (account,)).fetchone()
    if not connection:
        return
    connection = connection[0]

    credentials = {}

    class DesktopImages(recent.RecentImages):
        def decode_image(self, relative):
            image = recent.images.safe_file(self.source, relative)
            with tempfile.TemporaryDirectory(prefix='.chat-image-', dir=self.state) as directory:
                path = Path(directory) / 'decoded'
                subprocess.run([str(self.args.decoder), 'image', str(image), str(path), '--credentials-stdin'],
                               input=json.dumps(credentials).encode(), env=self.env, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=150, check=True)
                probe = subprocess.run([str(runtime/'ffprobe'), '-v', 'error', '-select_streams', 'v:0',
                                        '-show_entries', 'stream=codec_name,width,height', '-of', 'json', str(path)],
                                       capture_output=True, env=self.env, timeout=15, check=True)
                stream = json.loads(probe.stdout)['streams'][0]
                formats = {'mjpeg':'jpeg', 'png':'png', 'gif':'gif', 'webp':'webp', 'bmp':'bmp', 'tiff':'tiff'}
                kind = formats.get(stream['codec_name'])
                if not kind:
                    raise ValueError('invalid decoded image format')
                digest, length = recent.images.digest(path), path.stat().st_size
                asset = dict(sha256=digest, byte_length=length, media_type='image/'+kind,
                             width=stream['width'], height=stream['height'],
                             object_path=f'sha256/{digest[:2]}/{digest[2:4]}/{digest}')
                recent.images.verify_pixels(self.media, asset, self.env, path=path)
                MediaStore(self.media).publish(path, digest, length)
                return asset

        def control_allowed(self, locked=False):
            suffix = ' FOR SHARE' if locked else ''
            return (self.db.execute('SELECT NOT paused FROM crm.scheduler_control WHERE singleton' + suffix).fetchone() == (True,)
                    and self.db.execute("SELECT enabled AND NOT paused FROM crm.adapter_controls WHERE provider='wechat'" + suffix).fetchone() == (True,)
                    and self.db.execute('SELECT enabled FROM crm.source_connections WHERE id=%s' + suffix, (connection,)).fetchone() == (True,)
                    and super().control_allowed(locked))

        def resource_database(self):
            return self.snapshot('message/message_resource.db')

        def hardlink_database(self):
            return self.snapshot('hardlink/hardlink.db')

        def snapshot(self, relative):
            path = snapshots / relative
            if path.is_file() and path.resolve() == path and not path.with_name(path.name+'-wal').exists():
                return path
            return None

    scope = state / 'chat-pictures'
    scope.mkdir(exist_ok=True)
    args = SimpleNamespace(account=str(connection), source_owner=account, expected_database=db.info.dbname,
                           expected_system_id=db.execute('SELECT system_identifier::text FROM pg_control_system()').fetchone()[0],
                           decoder=runtime/'chatlog-beta-media', include_direct=True, batch_size=100)
    env = {'PATH': str(runtime) + ':/usr/bin:/bin'}
    key_path = snapshots.parents[1] / 'chatlog-image-key.json'
    if key_path.is_file() and key_path.resolve() == key_path:
        saved = json.loads(key_path.read_text())
        if saved.get('account') == account:
            if saved.get('image_key'):
                credentials['image_key'] = saved['image_key']
            if saved.get('image_xor') is not None:
                credentials['image_xor'] = saved['image_xor']
    with closing(sqlite3.connect(scope/'group-images.sqlite')) as ledger:
        worker = DesktopImages(db, ledger, args, recent.images.roots(cache, media, scope), env)
        # The native connection keeps the server ID in its raw JSON envelope.
        worker.current_sql = worker.current_sql.replace("e.original->>'serverId'", "e.original->>'serverId',r.payload#>>'{raw_message,server_id}'")
        try:
            worker.run()
        except recent.images.Paused:
            if worker.run_id:
                db.execute("UPDATE crm.wechat_image_runs SET status='paused',heartbeat_at=now() WHERE id=%s", (worker.run_id,))


def main():
    os.umask(0o077)
    root = Path.cwd().resolve(strict=True)
    snapshots = Path(os.environ['MONICA_CHATLOG_SNAPSHOT_ROOT'])
    source = snapshots / 'sns/sns.db'
    mode = sys.argv[1]
    media = Path(os.environ['MONICA_MEDIA_ROOT'])
    account = os.environ['MONICA_CHATLOG_ACCOUNT']
    if not snapshots.is_dir() or (mode == 'moments' and not source.is_file()):
        return
    if (not re.fullmatch(r'native-chatlog-[0-9a-f]{64}', account) or snapshots.resolve() != snapshots or (source.exists() and source.resolve() != source)
            or media.resolve(strict=True) != media or root.parent != media.parent or root.name != 'collector-data'):
        raise ValueError('invalid desktop source or media scope')
    state = root / 'moments-media' / account
    state.mkdir(parents=True, exist_ok=True)
    if state.resolve() != state or shutil.disk_usage(media).free < 512 * 1024**2:
        raise ValueError('invalid or full desktop media storage')
    marker = media / '.monica-rust-migration-media'
    if not marker.exists():
        try:
            with marker.open('x') as output:
                output.write(MARKER)
        except FileExistsError:
            pass
    runtime = Path(__file__).resolve().parents[2] / 'tools/bin'
    with psycopg.connect(os.environ['MONICA_DATABASE_URL'], autocommit=True, connect_timeout=5,
                          options='-c statement_timeout=15000 -c lock_timeout=2000') as db:
        if mode == 'moments':
            run(db, source, media, state, runtime, account)
        elif mode == 'images':
            recover_chat_images(db, snapshots, media, state, runtime, account)
        else:
            raise ValueError('unknown desktop media worker')


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(type(error).__name__, file=sys.stderr)
        raise SystemExit(1)

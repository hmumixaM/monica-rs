#!/usr/bin/env python3
"""Retry failed indexed-media copies after their original worker has exited.

Runs only the pinned offline codec in an isolated, bounded container. Source
archives and successful records are retained. No PostgreSQL, collector or AI
connection is made. Credentials enter through stdin, never Docker configuration.
"""
import argparse
from contextlib import closing, contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import selectors
import signal
import sqlite3
import stat
import subprocess
import sys
import time
import uuid

MARKER = 'monica-rust-isolated-media-v1\n'
CODES = {'image_conversion_failed', 'wxgf_conversion_failed',
         'decoded_image_invalid', 'voice_conversion_failed', 'media_decode_failed'}


class Failure(RuntimeError):
    pass


def require(value, code):
    if not value:
        raise Failure(code)


def canonical(path, directory=False, independent=True):
    require(path.is_absolute() and path.resolve(strict=True) == path, 'path_alias_or_missing')
    info = path.stat()
    require(stat.S_ISDIR(info.st_mode) if directory else
            stat.S_ISREG(info.st_mode) and (not independent or info.st_nlink == 1), 'unexpected_file_identity')
    return info


def checksum(path, independent=True):
    canonical(path, independent=independent)
    with os.fdopen(os.open(str(path), os.O_RDONLY | os.O_NOFOLLOW), 'rb') as source:
        before = os.fstat(source.fileno())
        h = hashlib.sha256()
        for part in iter(lambda: source.read(1024 * 1024), b''):
            h.update(part)
        after = os.fstat(source.fileno())
    require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
            (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
            'file_changed_during_read')
    return h.hexdigest()


def read_json(path):
    canonical(path)
    require(path.stat().st_size <= 2 * 1024 * 1024, 'metadata_too_large')
    return json.loads(path.read_text())


def digest(value):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value) is not None


def capture(argv, input_data=None, deadline=120, limit=128 * 1024):
    """Bound process output without retaining or printing upstream diagnostics."""
    with subprocess.Popen(argv, stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True) as child:
        try:
            streams = {child.stdout: bytearray(), child.stderr: bytearray()}
            with selectors.DefaultSelector() as selector:
                for stream in streams:
                    selector.register(stream, selectors.EVENT_READ)
                offset = 0
                if input_data:
                    os.set_blocking(child.stdin.fileno(), False)
                    selector.register(child.stdin, selectors.EVENT_WRITE)
                elif child.stdin is not None:
                    child.stdin.close()
                end = time.monotonic() + deadline
                while selector.get_map():
                    require(time.monotonic() < end, 'codec_command_timeout')
                    for event, _ in selector.select(min(0.25, max(0, end - time.monotonic()))):
                        if event.fileobj is child.stdin:
                            try:
                                offset += os.write(child.stdin.fileno(), input_data[offset:offset + 4096])
                            except BrokenPipeError:
                                raise Failure('codec_input_closed') from None
                            if offset == len(input_data):
                                selector.unregister(child.stdin)
                                child.stdin.close()
                            continue
                        data = os.read(event.fileobj.fileno(), 4096)
                        if data:
                            require(sum(map(len, streams.values())) + len(data) <= limit,
                                    'codec_output_limit')
                            streams[event.fileobj].extend(data)
                        else:
                            selector.unregister(event.fileobj)
                child.wait(timeout=max(0.1, end - time.monotonic()))
            return child.returncode, bytes(streams[child.stdout]), bytes(streams[child.stderr])
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=10)


class DockerCodec:
    def __init__(self, args):
        self.args = args
        self.runtime_id = hashlib.sha256((args.codec_sha256 + '\n' + args.image).encode()).hexdigest()

    def command(self, args, **kwargs):
        return capture([str(self.args.docker)] + args, **kwargs)

    def inspect(self, target, image=False):
        code, output, _ = self.command(['image' if image else 'container', 'inspect', target], deadline=45)
        require(code == 0, 'container_inspection_failed')
        values = json.loads(output)
        require(len(values) == 1 and values[0]['Id'] == target, 'container_identity_changed')
        return values[0]

    def mounts(self):
        return {'/codec': (str(self.args.codec), False), '/raw': (str(self.args.raw), False),
                '/media': (str(self.args.media), True)}

    def recover(self, inspect_only=False):
        receipt = self.args.state / 'codec-retry-container.json'
        if not receipt.exists():
            return
        prior = read_json(receipt)
        require(digest(prior.get('id')) and re.fullmatch('[0-9a-f]{32}', prior.get('token', ''))
                and re.fullmatch('sha256:[0-9a-f]{64}', prior.get('image', '')),
                'retry_creation_requires_inspection')
        mounts = {k: tuple(v) for k, v in prior['mounts'].items()}
        require(mounts.get('/raw') == (str(self.args.raw), False)
                and mounts.get('/media') == (str(self.args.media), True), 'prior_retry_roots_changed')
        item = self.own(prior['id'], prior['token'], mounts, prior['image'])
        if item['State']['Running']:
            require(not inspect_only, 'prior_retry_still_running')
            code, _, _ = self.command(['stop', '--time=5', prior['id']], deadline=45)
            require(code == 0 and not self.own(prior['id'], prior['token'], mounts,
                                            prior['image'])['State']['Running'],
                    'owned_retry_cleanup_required')

    def terminal(self):
        a = self.args
        proc = Path('/proc') / str(a.copy_pid)
        if proc.exists():
            words = (proc / 'stat').read_text().rsplit(') ', 1)[1].split()
            require(words[19] != a.copy_start_ticks or words[0] == 'Z', 'original_copy_still_running')
        item = self.inspect(a.copy_container)
        state = item['State']
        labels = item['Config'].get('Labels') or {}
        require(state['Status'] == 'exited' and not state['Running'] and not state.get('OOMKilled')
                and state['ExitCode'] in (0, 1), 'original_copy_not_terminal')
        require(labels.get('com.docker.compose.project') == 'monica-rs'
                and labels.get('monica.migration.ssd-stage') == 'copy'
                and labels.get('monica.migration.ssd-baseline') == a.copy_manifest,
                'original_copy_owner_changed')
        mounts = {v['Destination']: v for v in item['Mounts']}
        require(mounts.get('/copy.lock', {}).get('Source') == str(a.copy_lock),
                'original_copy_lock_changed')
        image = self.inspect(a.image, image=True)
        require(image['Architecture'] == 'amd64' and image['Os'] == 'linux', 'codec_runtime_changed')

    def own(self, cid, token, mounts, image=None):
        item = self.inspect(cid)
        labels = item['Config'].get('Labels') or {}
        require(item['Image'] == (image or self.args.image) and labels.get('monica.media-retry') == token,
                'retry_container_owner_changed')
        actual = {v['Destination']: (v['Source'], v['RW']) for v in item['Mounts']}
        require(actual == mounts and set(item['NetworkSettings']['Networks']) == {'none'},
                'retry_container_mounts_changed')
        return item

    def convert(self, requests, credentials):
        a = self.args
        token = uuid.uuid4().hex
        mounts = self.mounts()
        argv = ['create', '-i', '--name', 'monica-rs-media-retry-' + token,
                '--label', 'com.docker.compose.project=monica-rs',
                '--label', 'monica.media-retry=' + token, '--log-driver=none',
                '--network=none', '--read-only', '--cap-drop=ALL',
                '--security-opt=no-new-privileges', '--ulimit', 'core=0',
                '--memory=384m', '--memory-swap=384m', '--cpuset-cpus=3', '--cpu-shares=64',
                '--user=' + str(a.owner_uid) + ':' + str(a.owner_gid),
                '--tmpfs', '/tmp:rw,noexec,nosuid,size=64m']
        for target, (source, writable) in mounts.items():
            require(not any(c in source for c in ',\n\r'), 'unsupported_mount_path')
            argv += ['--mount', 'type=bind,src=' + source + ',dst=' + target + ('' if writable else ',readonly')]
        argv += ['--entrypoint=/codec', a.image, '--source-root', '/raw', '--target-root', '/media',
                 '--batch', '--credentials-stdin']
        # Persist intent before a create timeout can leave an unknown owned container.
        receipt = a.state / 'codec-retry-container.json'
        atomic_json(receipt, {'id': None, 'token': token, 'image': a.image, 'mounts': mounts,
                              'name': 'monica-rs-media-retry-' + token, 'state': 'creation_requested'})
        code, output, _ = self.command(argv, deadline=90)
        require(code == 0, 'retry_container_create_failed')
        cid = output.decode().strip()
        require(digest(cid), 'retry_container_identity_invalid')
        try:
            self.own(cid, token, mounts)
            # Only safe container identity is persisted for crash recovery.
            atomic_json(receipt, {'id': cid, 'token': token, 'image': a.image, 'mounts': mounts})
            frame = bytearray((json.dumps(credentials) + '\n' + ''.join(
                json.dumps(r) + '\n' for r in requests)).encode())
            try:
                code, output, errors = self.command(['start', '--attach', '--interactive', cid],
                                                     input_data=frame, deadline=120 * len(requests) + 30)
            finally:
                frame[:] = b'\0' * len(frame)
            item = self.own(cid, token, mounts)
            require(code == 0 and not errors and item['State']['Status'] == 'exited'
                    and item['State']['ExitCode'] == 0 and not item['State'].get('OOMKilled'),
                    'codec_batch_process_failed')
            return output
        finally:
            item = self.own(cid, token, mounts)
            if item['State']['Running']:
                code, _, _ = self.command(['stop', '--time=5', cid], deadline=45)
                require(code == 0 and not self.own(cid, token, mounts)['State']['Running'],
                        'owned_retry_cleanup_required')
            # Keep the stopped, labelled container as evidence. Never remove a service.


def atomic_json(path, value):
    require(path.resolve() == path, 'retry_receipt_alias')
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.partial')
    with os.fdopen(os.open(str(temporary), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), 'w') as out:
        json.dump(value, out, sort_keys=True)
        out.flush()
        os.fsync(out.fileno())
    os.replace(str(temporary), str(path))
    sync_directory(path.parent)


def sync_directory(path):
    fd = os.open(str(path), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def recover_published_temporaries(media):
    """Remove only fully accounted codec aliases; keep every underlying byte."""
    groups = {}
    for path in media.iterdir():
        if re.fullmatch(r'\.migration-media-[0-9]{1,20}', path.name):
            info = canonical(path, independent=False)
            if info.st_nlink > 1:
                groups.setdefault((info.st_dev, info.st_ino), []).append(path)
    require(sum(map(len, groups.values())) <= 256, 'temporary_recovery_requires_inspection')
    recovered = 0
    for identity, aliases in groups.items():
        info = aliases[0].stat()
        require(info.st_nlink == len(aliases) + 1 and 0 < info.st_size <= 512 * 1024 * 1024,
                'unaccounted_media_hardlinks')
        key = checksum(aliases[0], independent=False)
        target = media / 'sha256' / key[:2] / key[2:4] / key
        current = canonical(target, independent=False)
        require((current.st_dev, current.st_ino) == identity and current.st_nlink == info.st_nlink,
                'unaccounted_media_hardlinks')
        # Make the canonical name durable before removing any redundant alias.
        with target.open('rb') as source:
            os.fsync(source.fileno())
        for parent in list(target.parents)[:4]:
            sync_directory(parent)
        for offset, alias in enumerate(aliases):
            now, other = alias.lstat(), target.lstat()
            require((now.st_dev, now.st_ino) == (other.st_dev, other.st_ino) == identity
                    and other.st_nlink == len(aliases) - offset + 1, 'temporary_recovery_changed')
            alias.unlink()
            recovered += 1
        sync_directory(media)
        canonical(target)  # Unrelated hardlinks still fail the independence check.
        with target.open('rb') as source:
            os.fsync(source.fileno())
    return recovered


def finalize(db, database, state, summary):
    require(db.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0] == 0,
            'retry_checkpoint_pending')
    with database.open('rb') as data:
        os.fsync(data.fileno())
    sync_directory(state)
    atomic_json(state / 'codec-retry-status.json', summary)
    return summary


@contextmanager
def copy_guard(path):
    canonical(path)
    with os.fdopen(os.open(str(path), os.O_RDONLY | os.O_NOFOLLOW), 'rb') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Failure('original_media_operation_active') from None
        yield


def asset_file(media, asset, owner):
    require(isinstance(asset, dict) and not set(asset) -
            {'sha256', 'byte_length', 'media_type', 'object_path', 'width', 'height'}, 'invalid_codec_asset')
    key = asset.get('sha256')
    require(digest(key), 'invalid_codec_asset')
    relative = 'sha256/' + key[:2] + '/' + key[2:4] + '/' + key
    require(asset.get('object_path') == relative and type(asset.get('byte_length')) is int
            and 0 < asset['byte_length'] <= 128 * 1024 * 1024
            and re.fullmatch('image/[A-Za-z0-9.+-]+', asset.get('media_type', '')) is not None,
            'invalid_codec_asset')
    require(all(type(asset.get(k, 0)) is int and asset.get(k, 0) >= 0 for k in ('width', 'height')),
            'invalid_codec_dimensions')
    path = media / relative
    require(canonical(path).st_size == asset['byte_length'] and checksum(path) == key,
            'decoded_asset_checksum_mismatch')
    for parent in [path] + list(path.parents)[:3]:
        info = parent.stat()
        if (info.st_uid, info.st_gid) != owner:
            os.chown(str(parent), *owner)
        parent.chmod(0o640 if parent == path else 0o2750)
        if parent == path:
            with path.open('rb') as data:
                os.fsync(data.fileno())
        else:
            sync_directory(parent)
    sync_directory(media)


def retry(args, executor, credentials=None):
    for path in (args.raw, args.media, args.state):
        canonical(path, directory=True)
    require(not any(a == b or a in b.parents or b in a.parents
                    for a, b in [(args.raw, args.media), (args.state, args.raw), (args.state, args.media)]),
            'overlapping_retry_roots')
    require((args.media / '.monica-rust-migration-media').read_text() == MARKER, 'wrong_media_root')
    canonical(args.codec)
    require(checksum(args.codec) == args.codec_sha256 and args.codec.stat().st_mode & 0o111
            and not args.codec.stat().st_mode & 0o022,
            'codec_binary_changed')
    with copy_guard(args.copy_lock):
        executor.terminal()
        executor.recover(inspect_only=args.action == 'inspect')
        require(checksum(args.index) == args.index_sha256, 'frozen_index_changed')
        require(not args.index.stat().st_mode & 0o022, 'frozen_index_writable')
        database = args.state / 'wechat-file-assets.sqlite'
        raw_database = args.state / 'wechat-files.sqlite'
        for path in (database, raw_database):
            canonical(path)
            for suffix in ('-wal', '-shm', '-journal'):
                sidecar = path.with_name(path.name + suffix)
                if sidecar.exists():
                    canonical(sidecar)
        mode = '?mode=rw' if args.action == 'retry' else '?mode=ro'
        with closing(sqlite3.connect(database.as_uri() + mode, uri=True, timeout=0.1)) as db, \
                closing(sqlite3.connect(raw_database.as_uri() + '?mode=ro', uri=True)) as raw, \
                closing(sqlite3.connect(args.index.as_uri() + '?mode=ro&immutable=1', uri=True)) as index:
            info = dict(index.execute('SELECT key,value FROM metadata'))
            expected = {'snapshot': info['snapshot'], 'own_id': info['own_id'], 'input_sha256': args.index_sha256}
            require(info.get('complete') == '1' and dict(db.execute('SELECT key,value FROM metadata')) == expected,
                    'media_snapshot_identity_changed')
            total = index.execute('SELECT count(*) FROM files').fetchone()[0]
            counts = dict(db.execute('SELECT status,count(*) FROM assets GROUP BY status'))
            require(total == args.expected_records and sum(counts.values()) == total
                    and set(counts) <= {'ready', 'decode_failed', 'source_missing'}, 'indexed_copy_incomplete')
            status = read_json(args.state / 'copy-status-wechat-files.json')
            require(status.get('processed_records') == total and status.get('indexed_records') == total
                    and status.get('state') in ('paused', 'needs_attention', 'complete'), 'indexed_copy_incomplete')
            require(dict(raw.execute('SELECT name,value FROM policy')) ==
                    {'source': '/source', 'target': '/target/snapshots/wechat-files/msg'}, 'raw_receipt_identity_changed')
            has_journal = db.execute("SELECT 1 FROM sqlite_master WHERE name='codec_retry_attempts'").fetchone()
            skipped = set(db.execute('SELECT source_key,raw_sha256 FROM codec_retry_attempts WHERE runtime_id=?',
                                     (executor.runtime_id,))) if has_journal and not args.repeat else set()
            rows = []
            for row in db.execute("SELECT source_key,kind,md5,source_path,raw_sha256 FROM assets WHERE status='decode_failed' ORDER BY source_key"):
                if (row[0], row[4]) not in skipped:
                    rows.append(row)
                    if len(rows) == args.limit:
                        break
            requests = []
            for key, kind, md5, source_path, raw_sha in rows:
                source = PurePosixPath(source_path or '')
                require(kind == 'image' and digest(raw_sha) and str(source) == source_path
                        and len(source.parts) > 1 and source.parts[0] == 'msg'
                        and not source.is_absolute() and '..' not in source.parts and '\\' not in source_path,
                        'invalid_failed_record')
                entry = index.execute('SELECT kind,md5,candidates FROM files WHERE source_key=?', (key,)).fetchone()
                require(entry and (kind, md5) == entry[:2] and source_path in json.loads(entry[2]),
                        'failed_record_not_in_frozen_index')
                relative = '/'.join(source.parts[1:])
                receipt = raw.execute('SELECT sha256,bytes,mode FROM files WHERE path=?', (relative,)).fetchone()
                path = args.raw / relative
                require(receipt and receipt[0] == raw_sha and receipt[2] == 'file'
                        and 0 < canonical(path).st_size == receipt[1] <= 64 * 1024 * 1024
                        and checksum(path) == raw_sha, 'raw_copy_checksum_mismatch')
                requests.append({'id': hashlib.sha256(key.encode()).hexdigest(), 'path': relative, 'kind': kind})
            summary = {'selected': len(rows), 'remaining_decode_failed': counts.get('decode_failed', 0),
                       'ready': counts.get('ready', 0), 'source_missing': counts.get('source_missing', 0),
                       'indexed_records': total, 'runtime_id': executor.runtime_id, 'recovered': 0, 'codes': {}}
            if args.action == 'inspect':
                return summary
            summary['recovered_temporary_links'] = recover_published_temporaries(args.media)
            if not rows:
                return finalize(db, database, args.state, summary)
            require(isinstance(credentials, dict) and set(credentials) <= {'image_key', 'image_xor'}
                    and all(isinstance(v, str) for v in credentials.values()), 'invalid_credential_frame')
            output = executor.convert(requests, credentials)
            values = [json.loads(line) for line in output.splitlines()]
            expected_ids = {r['id'] for r in requests}
            require(len(values) == len(expected_ids) and {v.get('id') for v in values} == expected_ids,
                    'invalid_batch_results')
            results = {}
            for value in values:
                require(value.get('codec_commit') == args.codec_commit, 'codec_build_identity_changed')
                if 'asset' in value:
                    require(set(value) == {'id', 'asset', 'codec_commit'}, 'invalid_batch_results')
                    asset_file(args.media, value['asset'], (args.owner_uid, args.owner_gid))
                else:
                    require(set(value) == {'id', 'error_code', 'codec_commit'} and value['error_code'] in CODES,
                            'invalid_batch_results')
                results[value['id']] = value
            db.execute('PRAGMA synchronous=FULL')
            db.execute('''CREATE TABLE IF NOT EXISTS codec_retry_attempts(
 id TEXT PRIMARY KEY,source_key TEXT NOT NULL,raw_sha256 TEXT NOT NULL,runtime_id TEXT NOT NULL,
 codec_commit TEXT NOT NULL,code TEXT NOT NULL,asset TEXT,finished_at REAL NOT NULL)''')
            with db:
                for row, request in zip(rows, requests):
                    value = results[request['id']]
                    asset = value.get('asset')
                    code = value.get('error_code', 'ready')
                    if asset:
                        changed = db.execute("UPDATE assets SET status='ready',asset=? WHERE source_key=? AND raw_sha256=? AND status='decode_failed'",
                                             (json.dumps(asset, sort_keys=True), row[0], row[4])).rowcount
                        require(changed == 1, 'failed_record_changed')
                        summary['recovered'] += 1
                    else:
                        summary['codes'][code] = summary['codes'].get(code, 0) + 1
                    db.execute('INSERT INTO codec_retry_attempts VALUES(?,?,?,?,?,?,?,?)',
                               (str(uuid.uuid4()), row[0], row[4], executor.runtime_id,
                                args.codec_commit, code, json.dumps(asset, sort_keys=True) if asset else None, time.time()))
            summary['ready'] += summary['recovered']
            summary['remaining_decode_failed'] -= summary['recovered']
            return finalize(db, database, args.state, summary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['inspect', 'retry'])
    for name in ['raw', 'media', 'state', 'index', 'codec', 'copy-lock', 'docker']:
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ['index-sha256', 'codec-sha256', 'codec-commit', 'image', 'copy-container', 'copy-manifest', 'copy-start-ticks']:
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--copy-pid', type=int, required=True)
    parser.add_argument('--expected-records', type=int, required=True)
    parser.add_argument('--limit', type=int, default=16)
    parser.add_argument('--repeat', action='store_true', help='explicitly retry an already recorded failed attempt')
    parser.add_argument('--owner-uid', type=int, default=10001)
    parser.add_argument('--owner-gid', type=int, default=10001)
    args = parser.parse_args()
    require(1 <= args.limit <= 128 and args.expected_records > 0 and args.copy_pid > 0,
            'invalid_retry_bounds')
    require(all(digest(getattr(args, n)) for n in ['index_sha256', 'codec_sha256', 'copy_container', 'copy_manifest'])
            and re.fullmatch('[0-9a-f]{40}', args.codec_commit)
            and re.fullmatch('sha256:[0-9a-f]{64}', args.image)
            and args.copy_start_ticks.isdigit(), 'invalid_retry_identity')
    os.umask(0o077)
    credentials = None
    try:
        if args.action == 'retry':
            line = sys.stdin.buffer.readline(8193)
            require(len(line) <= 8192, 'invalid_credential_frame')
            credentials = json.loads(line)
        result = retry(args, DockerCodec(args), credentials)
        print(json.dumps(result, sort_keys=True), flush=True)
    finally:
        if isinstance(credentials, dict):
            credentials.clear()


if __name__ == '__main__':
    try:
        main()
    except BaseException as error:
        if isinstance(error, SystemExit):
            raise
        code = error.args[0] if isinstance(error, Failure) else 'media_retry_failed'
        print(json.dumps({'error_code': code}), flush=True)
        sys.exit(1)

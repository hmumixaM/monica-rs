#!/usr/bin/env python3
"""Publish exact same-account source matches without modifying frozen archives.

Input is a private staged manifest, not an arbitrary filename mapping. Every
source must match the frozen index's kind, content MD5 and byte length, as well
as the independently recorded SHA-256. Only source_missing rows may change.
The original row and recovery provenance are retained in an append-only table.
No PostgreSQL connection, collector, remote download or codec is used.
"""
import argparse
from contextlib import closing
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import struct
import sys
import time
import uuid

spec = importlib.util.spec_from_file_location('media_retry', Path(__file__).with_name('retry-wechat-media.py'))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def require(value):
    if not value:
        raise ValueError('verified_source_contract_failed')


def rows_digest(db, keys):
    h = hashlib.sha256()
    count = 0
    for row in db.execute('SELECT * FROM assets ORDER BY source_key'):
        if row[0] in keys:
            h.update((json.dumps(row, separators=(',', ':')) + '\n').encode())
            count += 1
    return count, h.hexdigest()


def publish(source, media, digest, length, owner):
    relative = Path('sha256') / digest[:2] / digest[2:4] / digest
    parent = media
    for part in relative.parts[:-1]:
        parent = parent / part
        try:
            parent.mkdir(mode=0o2750)
        except FileExistsError:
            helper.canonical(parent, directory=True)
        else:
            os.chown(parent, *owner)
            parent.chmod(0o2750)
            helper.sync_directory(parent.parent)
    target = media / relative
    if target.exists() and target.stat().st_nlink > 1:
        # An interrupted link publication can retain a temporary alias. Only
        # remove same-inode aliases after accounting for every hard link.
        info = helper.canonical(target, independent=False)
        require(info.st_size == length and helper.checksum(target, independent=False) == digest)
        aliases = []
        for path in parent.iterdir():
            if not re.fullmatch(r'\.source-recovery-[0-9a-f]{32}', path.name):
                continue
            candidate = path.lstat()
            if (candidate.st_dev, candidate.st_ino) == (info.st_dev, info.st_ino):
                require(path.is_file() and not path.is_symlink())
                aliases.append(path)
        require(info.st_nlink == len(aliases) + 1 and
                (target.stat().st_ino, target.stat().st_nlink) == (info.st_ino, info.st_nlink))
        with target.open('rb') as f:
            os.fsync(f.fileno())
        helper.sync_directory(parent)
        for path in aliases:
            path.unlink()
        helper.sync_directory(parent)
    if not target.exists():
        temporary = parent / ('.source-recovery-' + uuid.uuid4().hex)
        try:
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o640)
            with os.fdopen(fd, 'wb') as output, source.open('rb') as input_file:
                os.fchown(output.fileno(), *owner)
                shutil.copyfileobj(input_file, output, 1024 * 1024)
                output.flush()
                os.fsync(output.fileno())
            require(helper.checksum(temporary) == digest)
            try:
                os.link(temporary, target)
            except FileExistsError:
                pass
        finally:
            temporary.unlink(missing_ok=True)
        helper.sync_directory(parent)
    require(helper.canonical(target).st_size == length and helper.checksum(target) == digest)
    return relative.as_posix()


def restore(args, terminal):
    for p in (args.stage, args.media, args.state):
        helper.canonical(p, directory=True)
    require(not any(a == b or a in b.parents or b in a.parents for a, b in
                    [(args.stage, args.media), (args.state, args.media), (args.stage, args.state)]))
    require((args.media / '.monica-rust-migration-media').read_text() == helper.MARKER)
    require((args.stage / '.verified-source-stage').read_text() == 'monica-verified-source-stage-v1\n')
    manifest = helper.read_json(args.stage / 'manifest.json')
    require(set(manifest) == {'version', 'index_sha256', 'metadata', 'entries'} and manifest['version'] == 1)
    require(helper.checksum(args.index) == args.index_sha256 == manifest['index_sha256'])
    entries = manifest['entries']
    require(isinstance(entries, list) and 0 < len(entries) <= 1400)
    require(len({e['key'] for e in entries}) == len(entries))
    require(sum(e['bytes'] for e in entries) <= 512 * 1024 * 1024)
    with helper.copy_guard(args.copy_lock):
        terminal()
        database = args.state / 'wechat-file-assets.sqlite'
        helper.canonical(database)
        with closing(sqlite3.connect(database.as_uri() + '?mode=rw', uri=True, timeout=0.1)) as db, \
                closing(sqlite3.connect(args.index.as_uri() + '?mode=ro&immutable=1', uri=True)) as index:
            metadata = dict(index.execute('SELECT key,value FROM metadata'))
            require(metadata.get('complete') == '1' and metadata == manifest['metadata'])
            expected_metadata = {k: metadata[k] for k in ('snapshot', 'own_id')}
            expected_metadata['input_sha256'] = args.index_sha256
            require(dict(db.execute('SELECT key,value FROM metadata')) == expected_metadata)
            require(db.execute('PRAGMA integrity_check').fetchall() == [('ok',)])
            require(db.execute('SELECT count(*) FROM assets').fetchone()[0] == args.expected_records)
            old_ready = {r[0] for r in db.execute("SELECT source_key FROM assets WHERE status='ready'")}
            ready_before = rows_digest(db, old_ready)
            updates = []
            for entry in entries:
                require(set(entry) == {'key', 'kind', 'md5', 'bytes', 'sha256', 'scope', 'media_type', 'width', 'height'})
                require(entry['scope'] == 'same_account' and helper.digest(entry['sha256'])
                        and re.fullmatch('[0-9a-f]{32}', entry['md5'])
                        and type(entry['bytes']) is int and 0 < entry['bytes'] <= 32 * 1024 * 1024)
                key = entry['key']
                frozen = index.execute('SELECT kind,md5,expected_bytes,candidates FROM files WHERE source_key=?', (key,)).fetchone()
                require(frozen and frozen[:3] == (entry['kind'], entry['md5'], entry['bytes']))
                require(all(Path(p).name != '.DS_Store' for p in json.loads(frozen[3])))
                source = args.stage / entry['sha256']
                require(helper.canonical(source).st_size == entry['bytes'] and helper.checksum(source) == entry['sha256'])
                with source.open('rb') as f:
                    require(hashlib.md5(f.read()).hexdigest() == entry['md5'])
                if entry['kind'] == 'image':
                    require(entry['media_type'] == 'image/png')
                    with source.open('rb') as f:
                        header = f.read(24)
                    require(header[:16] == b'\x89PNG\r\n\x1a\n\0\0\0\rIHDR'
                            and struct.unpack('>II', header[16:24]) == (entry['width'], entry['height']))
                else:
                    require(entry['kind'] in ('file', 'video') and entry['width'] == entry['height'] == 0
                            and entry['media_type'] == ('video/mp4' if entry['kind'] == 'video' else 'application/octet-stream'))
                old = db.execute('SELECT * FROM assets WHERE source_key=?', (key,)).fetchone()
                require(old and old[1:3] == frozen[:2])
                if old[6] == 'ready':
                    asset = json.loads(old[5])
                    require(old[4] == entry['sha256'] and asset['sha256'] == entry['sha256']
                            and asset['byte_length'] == entry['bytes'] and asset['media_type'] == entry['media_type'])
                    target = args.media / 'sha256' / entry['sha256'][:2] / entry['sha256'][2:4] / entry['sha256']
                    require(helper.canonical(target).st_size == entry['bytes'] and helper.checksum(target) == entry['sha256'])
                    continue
                require(old[6] == 'source_missing' and old[3:6] == (None, None, None))
                relative = publish(source, args.media, entry['sha256'], entry['bytes'], (args.owner_uid, args.owner_gid))
                asset = dict(sha256=entry['sha256'], byte_length=entry['bytes'], media_type=entry['media_type'], object_path=relative)
                if entry['kind'] == 'image':
                    asset.update(width=entry['width'], height=entry['height'])
                updates.append((old, json.dumps(asset, sort_keys=True), entry))
            db.execute('BEGIN IMMEDIATE')
            try:
                db.execute('''CREATE TABLE IF NOT EXISTS verified_source_recoveries(
                    source_key TEXT PRIMARY KEY, original_row TEXT NOT NULL, asset TEXT NOT NULL,
                    raw_sha256 TEXT NOT NULL, index_sha256 TEXT NOT NULL, scope TEXT NOT NULL, recovered_at REAL NOT NULL)''')
                for old, asset, entry in updates:
                    require(db.execute('SELECT * FROM assets WHERE source_key=?', (old[0],)).fetchone() == old)
                    db.execute('INSERT INTO verified_source_recoveries VALUES(?,?,?,?,?,?,?)',
                               (old[0], json.dumps(old), asset, entry['sha256'], args.index_sha256, entry['scope'], time.time()))
                    # No source path is invented inside the untouched frozen archive.
                    require(db.execute("UPDATE assets SET status='ready',raw_sha256=?,asset=? WHERE source_key=? AND status='source_missing'",
                                       (entry['sha256'], asset, old[0])).rowcount == 1)
                require(rows_digest(db, old_ready) == ready_before)
                db.commit()
            except BaseException:
                db.rollback()
                raise
            require(db.execute('PRAGMA integrity_check').fetchall() == [('ok',)])
            require(db.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0] == 0)
            with database.open('rb') as f:
                os.fsync(f.fileno())
            helper.sync_directory(args.state)
            return dict(recovered=len(updates), entries=len(entries), previous_ready_rows_unchanged=True,
                        counts=dict(db.execute('SELECT status,count(*) FROM assets GROUP BY status')),
                        sqlite_integrity='ok', frozen_archives_unchanged=True)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('stage', 'media', 'state', 'index', 'copy-lock', 'docker'):
        p.add_argument('--' + name, type=Path, required=True)
    for name in ('index-sha256', 'image', 'copy-container', 'copy-manifest', 'copy-start-ticks'):
        p.add_argument('--' + name, required=True)
    p.add_argument('--copy-pid', type=int, required=True)
    p.add_argument('--expected-records', type=int, required=True)
    p.add_argument('--owner-uid', type=int, default=10001)
    p.add_argument('--owner-gid', type=int, default=10001)
    args = p.parse_args()
    args.codec_sha256 = '0' * 64  # terminal inspection does not execute a codec
    os.umask(0o077)
    print(json.dumps(restore(args, helper.DockerCodec(args).terminal), sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print(json.dumps({'error_code': 'verified_source_recovery_failed'}))
        sys.exit(1)

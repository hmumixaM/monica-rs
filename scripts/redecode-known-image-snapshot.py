#!/usr/bin/env python3
"""Build a separate correction set from verified original image bytes.

The input is a private snapshot manifest with rows [key, source_path, raw_sha,
old_asset_json]. The original source and NAS ledgers are never written. A pinned
codec reads credentials from stdin and writes only to a marked correction CAS.
Pillow independently decodes every output frame before a success is recorded.
Requires Pillow. Intended for the local recovery workstation, not a service.
"""
import argparse
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import sqlite3
import sys
import time
import warnings
from PIL import Image

spec = importlib.util.spec_from_file_location('media_retry', Path(__file__).with_name('retry-wechat-media.py'))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def require(value):
    if not value:
        raise ValueError('correction_contract_failed')


def full_pixels(target, asset):
    sha = asset['sha256']
    require(helper.digest(sha))
    relative = 'sha256/' + sha[:2] + '/' + sha[2:4] + '/' + sha
    require(asset['object_path'] == relative)
    path = target / relative
    require(helper.canonical(path, independent=False).st_size == asset['byte_length']
            and helper.checksum(path, independent=False) == sha)
    with warnings.catch_warnings():
        warnings.simplefilter('error', Image.DecompressionBombWarning)
        with Image.open(path) as image:
            frames = getattr(image, 'n_frames', 1)
            pixels = image.width * image.height
            require(0 < frames <= 1200 and 0 < pixels <= 64 * 1024 * 1024
                    and pixels * frames <= 1024 * 1024 * 1024)
            require(image.size == (asset.get('width'), asset.get('height')))
            expected = {'JPEG': 'image/jpeg', 'PNG': 'image/png', 'GIF': 'image/gif'}
            require(expected.get(image.format) == asset['media_type'])
            for frame in range(frames):
                image.seek(frame)
                image.load()
    return frames


def run(args, credentials):
    for p in (args.source, args.target):
        helper.canonical(p, directory=True)
    require(args.source != args.target and args.source not in args.target.parents
            and args.target not in args.source.parents)
    helper.canonical(args.journal.parent, directory=True)
    require(args.journal.is_absolute() and args.journal.resolve() == args.journal
            and args.source not in args.journal.parents
            and args.journal not in (args.input, args.codec))
    require((args.target / '.monica-rust-migration-media').read_text() == helper.MARKER)
    require(args.target.stat().st_mode & 0o077 == 0)
    require(helper.checksum(args.codec) == args.codec_sha256)
    require(re.fullmatch('[0-9a-f]{40}', args.codec_commit))
    require(helper.canonical(args.input).st_size <= 16 * 1024 * 1024)
    manifest = json.loads(args.input.read_text())
    rows = manifest['rows']
    require(isinstance(rows, list) and 0 < len(rows) <= 10000)
    require(len({r[0] for r in rows}) == len(rows))
    metadata = dict(source_manifest_sha256=helper.checksum(args.input),
                    codec_commit=args.codec_commit, binary_sha256=args.codec_sha256,
                    source_root=str(args.source), target_root=str(args.target),
                    index_metadata_json=json.dumps(manifest['metadata'], sort_keys=True))
    require(set(credentials) == {'image_key', 'image_xor'} and
            all(isinstance(v, str) for v in credentials.values()) and
            len(credentials['image_key']) <= 256 and len(credentials['image_xor']) <= 8)
    os.umask(0o077)
    if args.journal.exists():
        helper.canonical(args.journal)
    with sqlite3.connect(args.journal) as db:
        db.executescript('''CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE IF NOT EXISTS results(source_key TEXT PRIMARY KEY,raw_sha256 TEXT NOT NULL,
            old_asset_sha256 TEXT NOT NULL,code TEXT NOT NULL,asset TEXT,frames INT,finished_at REAL NOT NULL);''')
        existing = dict(db.execute('SELECT key,value FROM metadata'))
        require(not existing or existing == metadata)
        if not existing:
            require(db.execute('SELECT count(*) FROM results').fetchone()[0] == 0)
            db.executemany('INSERT INTO metadata VALUES(?,?)', metadata.items())
            db.commit()
        done = {r[0] for r in db.execute('SELECT source_key FROM results')}
        require(done <= {r[0] for r in rows})
        for asset, frames in db.execute("SELECT asset,frames FROM results WHERE code='ready'"):
            require(full_pixels(args.target, json.loads(asset)) == frames)
        cache = {}
        started, last = time.monotonic(), time.monotonic()

        def record(row, code, asset=None, frames=0):
            db.execute('INSERT INTO results VALUES(?,?,?,?,?,?,?)',
                       (row[0], row[2], json.loads(row[3])['sha256'], code,
                        json.dumps(asset) if asset else None, frames, time.time()))

        for start in range(0, len(rows), args.batch_size):
            pending = [r for r in rows[start:start + args.batch_size] if r[0] not in done]
            lookup, requests = {}, []
            for row in pending:
                require(len(row) == 4 and helper.digest(row[2]))
                relative = PurePosixPath(row[1])
                require(not relative.is_absolute() and '..' not in relative.parts and '\\' not in row[1]
                        and str(relative) == row[1])
                source = args.source / row[1]
                try:
                    require(0 < helper.canonical(source).st_size <= 64 * 1024 * 1024)
                    matching = helper.checksum(source) == row[2]
                except (OSError, helper.Failure):
                    matching = False
                if not matching:
                    record(row, 'local_source_unavailable_or_changed')
                    continue
                ident = hashlib.sha256(row[0].encode()).hexdigest()
                require(ident not in lookup)
                lookup[ident] = row
                requests.append(dict(id=ident, path=row[1], kind='image'))
            if requests:
                frame = bytearray((json.dumps(credentials) + '\n' + ''.join(
                    json.dumps(r) + '\n' for r in requests)).encode())
                # The codec also clears its credential variables before FFmpeg;
                # no workstation/NAS secrets are inherited by this subprocess.
                saved = os.environ.copy()
                try:
                    os.environ.clear()
                    os.environ.update(PATH=args.codec_path, TMPDIR='/tmp')
                    code, output, errors = helper.capture(
                        [str(args.codec), '--source-root', str(args.source), '--target-root', str(args.target),
                         '--credentials-stdin', '--batch'], input_data=frame, deadline=180)
                finally:
                    os.environ.clear()
                    os.environ.update(saved)
                    frame[:] = b'\0' * len(frame)
                require(code == 0 and not errors)
                seen = set()
                for line in output.splitlines():
                    response = json.loads(line)
                    ident = response.get('id')
                    require(ident in lookup and ident not in seen and response.get('codec_commit') == args.codec_commit)
                    seen.add(ident)
                    row = lookup[ident]
                    if 'asset' not in response:
                        record(row, response.get('error_code', 'codec_failed'))
                        continue
                    require(helper.checksum(args.source / row[1]) == row[2])
                    asset = response['asset']
                    try:
                        if asset['sha256'] not in cache:
                            cache[asset['sha256']] = full_pixels(args.target, asset)
                        else:
                            # A same-content alias still must carry the same dimensions and MIME.
                            require(cache[asset['sha256']] == full_pixels(args.target, asset))
                        record(row, 'ready', asset, cache[asset['sha256']])
                    except Exception:
                        record(row, 'full_pixel_validation_failed')
                require(seen == set(lookup))
            db.commit()
            now = time.monotonic()
            if now - last >= 15 or start + args.batch_size >= len(rows):
                counts = dict(db.execute('SELECT code,count(*) FROM results GROUP BY code'))
                print(json.dumps(dict(total=len(rows),processed=sum(counts.values()),counts=counts,
                                      elapsed_seconds=round(now-started, 1))), flush=True)
                last = now
        require(db.execute('PRAGMA integrity_check').fetchall() == [('ok',)])
        return dict(total=len(rows), counts=dict(db.execute('SELECT code,count(*) FROM results GROUP BY code')),
                    codec_commit=args.codec_commit, binary_sha256=args.codec_sha256,
                    sqlite_integrity='ok', original_ledger_modified=False)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('input', 'source', 'target', 'codec', 'journal'):
        p.add_argument('--' + name, type=Path, required=True)
    for name in ('codec-sha256', 'codec-commit', 'codec-path'):
        p.add_argument('--' + name, required=True)
    p.add_argument('--batch-size', type=int, default=32)
    args = p.parse_args()
    require(1 <= args.batch_size <= 128)
    line = sys.stdin.buffer.readline(8193)
    require(len(line) <= 8192)
    credentials = json.loads(line)
    try:
        print(json.dumps(run(args, credentials), sort_keys=True), flush=True)
    finally:
        credentials.clear()


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print(json.dumps({'error_code': 'image_snapshot_correction_failed'}), flush=True)
        sys.exit(1)

#!/usr/bin/env python3
"""Publish verified correction objects and update only a separate shadow ledger.

The primary assets ledger and frozen index are read-only. Original source bytes
and old CAS objects remain untouched. The manifest, three result journals and
their source manifests bind each corrected object to a source key and codec.
"""
import argparse
from contextlib import closing
import hashlib
import importlib.util
import json
import os
import re
from pathlib import Path, PurePosixPath
import sqlite3
import sys
import time

spec = importlib.util.spec_from_file_location('source_restore', Path(__file__).with_name('restore-verified-wechat-sources.py'))
restorer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(restorer)
helper = restorer.helper


def require(value):
    if not value:
        raise ValueError('image_correction_publish_contract_failed')


def file_in(root, relative):
    p = PurePosixPath(relative)
    require(not p.is_absolute() and '..' not in p.parts and str(p) == relative and '\\' not in relative)
    result = root / relative
    helper.canonical(result)
    return result


def digest_rows(db, excluded=None):
    h = hashlib.sha256()
    for row in db.execute('SELECT * FROM assets ORDER BY source_key'):
        if excluded and row[0] in excluded:
            continue
        h.update((json.dumps(row, separators=(',', ':')) + '\n').encode())
    return h.hexdigest()


def publish(args):
    for path in (args.bundle, args.media, args.raw):
        helper.canonical(path, directory=True)
    require(args.bundle != args.media and args.bundle not in args.media.parents and args.media not in args.bundle.parents)
    require(all(a != b and a not in b.parents and b not in a.parents
                for a, b in ((args.raw, args.media), (args.raw, args.bundle))))
    require(args.primary != args.shadow and args.primary.resolve() != args.shadow.resolve())
    require(args.raw not in args.shadow.parents and args.shadow != args.index)
    require((args.media / '.monica-rust-migration-media').read_text() == helper.MARKER)
    require((args.bundle / '.monica-image-correction-bundle').read_text() == 'monica-image-correction-bundle-v1\n')
    manifest_path = file_in(args.bundle, 'manifest.json')
    require(manifest_path.stat().st_size <= 32 * 1024 * 1024 and helper.checksum(manifest_path) == args.manifest_sha256)
    manifest = json.loads(manifest_path.read_text())
    require(manifest['version'] == 1 and manifest['index_sha256'] == args.index_sha256)
    require(re.fullmatch('[0-9a-f]{40}', manifest['codec_commit']) and helper.digest(manifest['codec_sha256']))
    require(helper.checksum(args.index) == args.index_sha256)
    records = manifest['records']
    require(0 < len(records) <= 10000 and len({r['key'] for r in records}) == len(records))
    attested = {}
    for packet in manifest['receipts']:
        receipt = file_in(args.bundle, packet['path'])
        source_manifest = file_in(args.bundle, packet['input'])
        require(helper.checksum(receipt) == packet['sha256'] and helper.checksum(source_manifest) == packet['input_sha256'])
        require(source_manifest.stat().st_size <= 16 * 1024 * 1024)
        source = json.loads(source_manifest.read_text())
        require(source['metadata'] == manifest['account_metadata'])
        inputs = {r[0]: r for r in source['rows']}
        require(len(inputs) == len(source['rows']))
        with closing(sqlite3.connect(receipt.as_uri() + '?mode=ro', uri=True)) as db:
            require(db.execute('PRAGMA integrity_check').fetchall() == [('ok',)])
            meta = dict(db.execute('SELECT key,value FROM metadata'))
            require(meta['source_manifest_sha256'] == packet['input_sha256']
                    and meta['codec_commit'] == manifest['codec_commit']
                    and meta['binary_sha256'] == manifest['codec_sha256']
                    and json.loads(meta['index_metadata_json']) == manifest['account_metadata'])
            for key, raw, old, code, asset, frames, _ in db.execute('SELECT * FROM results'):
                require(key in inputs)
                if code != 'ready':
                    continue
                prior = json.loads(inputs[key][3])
                require(raw == inputs[key][2] and old == (prior['sha256'] if prior else None))
                value = dict(input_sha256=raw, old_sha256=old, asset=json.loads(asset), frames=frames)
                require(key not in attested or attested[key] == value)
                attested[key] = value
    require(set(attested) == {r['key'] for r in records})
    for path in (args.primary, args.shadow, args.index):
        helper.canonical(path)
    with helper.copy_guard(args.copy_lock), helper.copy_guard(args.recovery_lock), \
            closing(sqlite3.connect(args.primary.as_uri() + '?mode=ro', uri=True)) as primary, \
            closing(sqlite3.connect(args.shadow.as_uri() + '?mode=rw', uri=True)) as shadow, \
            closing(sqlite3.connect(args.index.as_uri() + '?mode=ro&immutable=1', uri=True)) as index:
        require(digest_rows(primary) == manifest['primary_digest'])
        account = dict(primary.execute('SELECT key,value FROM metadata'))
        require(account == manifest['account_metadata'] == dict(shadow.execute('SELECT key,value FROM metadata'))
                and account['input_sha256'] == args.index_sha256)
        require(primary.execute('SELECT count(*) FROM assets').fetchone()[0] == args.expected_records)
        require(shadow.execute('SELECT count(*) FROM assets').fetchone()[0] == args.expected_records)
        changed_keys = {r['key'] for r in records}
        untouched = digest_rows(primary, changed_keys)
        require(digest_rows(shadow, changed_keys) == untouched)
        has_receipts = shadow.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='image_correction_receipts'").fetchone()
        prepared, verified_objects, verified_raw = [], set(), set()
        for record in records:
            key, asset = record['key'], record['asset']
            proof = attested[key]
            require({k: record[k] for k in ('input_sha256', 'old_sha256', 'asset', 'frames')} == proof)
            sha = asset['sha256']
            require(helper.digest(sha) and type(asset['byte_length']) is int and 0 < asset['byte_length'] <= 256 * 1024 * 1024
                    and asset['media_type'] in ('image/jpeg', 'image/png', 'image/gif')
                    and type(asset.get('width')) is int and type(asset.get('height')) is int
                    and asset['width'] > 0 and asset['height'] > 0 and type(record['frames']) is int and record['frames'] > 0
                    and asset['object_path'] == 'sha256/' + sha[:2] + '/' + sha[2:4] + '/' + sha)
            source_object = file_in(args.bundle, 'objects/' + sha)
            if sha not in verified_objects:
                require(source_object.stat().st_size == asset['byte_length'] and helper.checksum(source_object) == sha)
                verified_objects.add(sha)
            old = primary.execute('SELECT * FROM assets WHERE source_key=?', (key,)).fetchone()
            current = shadow.execute('SELECT * FROM assets WHERE source_key=?', (key,)).fetchone()
            frozen = index.execute('SELECT kind,md5,expected_bytes FROM files WHERE source_key=?', (key,)).fetchone()
            require(old and current and frozen and old[1:3] == frozen[:2] and old[1] == 'image')
            raw_sha = record['source_raw_sha256']
            require(helper.digest(raw_sha))
            if record['category'] == 'bmp':
                require(old[6] == 'ready' and json.loads(old[5])['media_type'] == 'image/bmp'
                        and json.loads(old[5])['sha256'] == record['old_sha256'] != sha
                        and old[4] == raw_sha == record['input_sha256'])
                p = PurePosixPath(old[3])
                require(p.parts[0] == 'msg' and not p.is_absolute() and '..' not in p.parts and str(p) == old[3])
                raw_identity = (old[3], raw_sha)
                if raw_identity not in verified_raw:
                    require(helper.checksum(args.raw / old[3], independent=False) == raw_sha)
                    verified_raw.add(raw_identity)
                require(current[:5] == old[:5] and current[6] in ('decode_failed', 'ready'))
            else:
                require(record['category'] == 'source_missing' and old[3:7] == (None, None, None, 'source_missing')
                        and record['old_sha256'] is None)
                plain = file_in(args.bundle, 'sources/plain/' + record['input_sha256'])
                encoded = file_in(args.bundle, 'sources/raw/' + raw_sha)
                require(helper.checksum(plain) == record['input_sha256'] and plain.stat().st_size == frozen[2]
                        and hashlib.md5(plain.read_bytes()).hexdigest() == frozen[1]
                        and helper.checksum(encoded) == raw_sha)
                require(current[:4] == old[:4] and current[6] in ('source_missing', 'ready'))
            initial = old[:6] + ('decode_failed',) if record['category'] == 'bmp' else old
            if current != initial:
                receipt = shadow.execute('SELECT asset,source_raw_sha256,manifest_sha256 FROM image_correction_receipts WHERE source_key=?', (key,)).fetchone() if has_receipts else None
                expected_asset = json.dumps(asset, sort_keys=True)
                require(receipt == (expected_asset, raw_sha, args.manifest_sha256)
                        and current == old[:4] + (raw_sha, expected_asset, 'ready'))
            prepared.append((record, old, current, source_object))
        # New CAS names are append-only; existing objects are checked and retained.
        for n, (record, old, current, source) in enumerate(prepared, 1):
            a = record['asset']
            restorer.publish(source, args.media, a['sha256'], a['byte_length'], (args.owner_uid, args.owner_gid))
            if n % 256 == 0:
                print(json.dumps(dict(phase='publishing_verified_cas',processed=n,total=len(prepared))), flush=True)
        shadow.execute('BEGIN IMMEDIATE')
        try:
            shadow.execute('''CREATE TABLE IF NOT EXISTS image_correction_receipts(
                source_key TEXT PRIMARY KEY,category TEXT NOT NULL,original_row TEXT NOT NULL,
                asset TEXT NOT NULL,source_raw_sha256 TEXT NOT NULL,input_sha256 TEXT NOT NULL,
                codec_commit TEXT NOT NULL,manifest_sha256 TEXT NOT NULL,frames INT NOT NULL,corrected_at REAL NOT NULL)''')
            for record, old, current, source in prepared:
                key = record['key']
                asset = json.dumps(record['asset'], sort_keys=True)
                prior = shadow.execute('SELECT asset,source_raw_sha256,manifest_sha256 FROM image_correction_receipts WHERE source_key=?', (key,)).fetchone()
                if prior:
                    require(prior == (asset, record['source_raw_sha256'], args.manifest_sha256)
                            and current[4:7] == (record['source_raw_sha256'], asset, 'ready'))
                    continue
                require(shadow.execute('SELECT * FROM assets WHERE source_key=?', (key,)).fetchone() == current)
                shadow.execute('INSERT INTO image_correction_receipts VALUES(?,?,?,?,?,?,?,?,?,?)',
                               (key,record['category'],json.dumps(old),asset,record['source_raw_sha256'],record['input_sha256'],
                                manifest['codec_commit'],args.manifest_sha256,record['frames'],time.time()))
                shadow.execute("UPDATE assets SET raw_sha256=?,asset=?,status='ready' WHERE source_key=?",
                               (record['source_raw_sha256'],asset,key))
            require(digest_rows(primary) == manifest['primary_digest'])
            require(digest_rows(shadow, changed_keys) == untouched)
            require(shadow.execute('SELECT count(*) FROM assets').fetchone()[0] == args.expected_records)
            shadow.commit()
        except BaseException:
            shadow.rollback()
            raise
        require(shadow.execute('PRAGMA integrity_check').fetchall() == [('ok',)])
        require(shadow.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0] == 0)
        with args.shadow.open('rb') as f:
            os.fsync(f.fileno())
        helper.sync_directory(args.shadow.parent)
        return dict(records=len(records),unique_objects=len(verified_objects),primary_rows_unchanged=True,
                    shadow_counts=dict(shadow.execute('SELECT status,count(*) FROM assets GROUP BY status')),
                    sqlite_integrity='ok',manifest_sha256=args.manifest_sha256)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('bundle','media','raw','primary','shadow','index','copy-lock','recovery-lock'):
        p.add_argument('--'+name,type=Path,required=True)
    for name in ('manifest-sha256','index-sha256'):
        p.add_argument('--'+name,required=True)
    p.add_argument('--expected-records',type=int,required=True)
    p.add_argument('--owner-uid',type=int,default=10001)
    p.add_argument('--owner-gid',type=int,default=10001)
    args=p.parse_args();os.umask(0o077)
    print(json.dumps(publish(args),sort_keys=True),flush=True)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print(json.dumps({'error_code':'image_correction_publish_failed'}),flush=True)
        sys.exit(1)

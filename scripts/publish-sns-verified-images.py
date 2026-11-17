"""Publish an explicitly verified SNS import batch without removing any media.

The stage has only decoded images and public source identities, never image keys.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import uuid


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def require(value):
    if not value:
        raise ValueError('sns_publication_contract')


def publish(db, args):
    restorer = module('sns_atomic_restorer', 'restore-verified-wechat-sources.py')
    helper = restorer.helper
    for path in (args.stage, args.media, args.state):
        helper.canonical(path, directory=True)
    require(not any(a == b or a in b.parents or b in a.parents for a, b in
                    ((args.stage, args.media), (args.state, args.media), (args.stage, args.state))))
    require((args.media / '.monica-rust-migration-media').read_text() == helper.MARKER)
    manifest = args.stage / 'manifest.json'
    require(helper.checksum(manifest) == args.manifest_sha256)
    rows = json.loads(manifest.read_text())['rows']
    require(len(rows) == args.expected_records and 0 < len(rows) <= 1000)
    require(len({(r['event_id'], r.get('asset_role', 'media-'+str(r['slot']))) for r in rows}) == len(rows))
    identity = db.execute("SELECT system_identifier::text,current_database(),(SELECT oid::text FROM pg_database WHERE datname=current_database()),(SELECT max(version) FROM _sqlx_migrations WHERE success) FROM pg_control_system()").fetchone()
    require(identity == (args.expected_system_id, 'monica_rust', '16384', args.expected_schema))
    for row in rows:
        asset = row['media']
        require(str(uuid.UUID(row['event_id'])) == row['event_id'] and row['external_id'].isdigit())
        require(type(row['slot']) is int and 1 <= row['slot'] <= 100)
        video = asset['media_type'] == 'video/mp4'
        require(row.get('full_video') is True if video else row.get('full_pixels') is True)
        require(re.fullmatch('[0-9a-f]{64}', asset['sha256']) and
                0 < asset['byte_length'] <= (512 if video else 32) * 1024 * 1024 and
                asset['media_type'] in ('image/jpeg', 'image/png', 'video/mp4'))
        source = args.stage / 'objects' / asset['sha256']
        require(helper.canonical(source).st_size == asset['byte_length'])
        if video:
            verifier=module('sns_video_validation','sns-video.py')
            actual=verifier.validate(source,args.runtime)
        else:
            checked = subprocess.run([str(args.validator)], input=source.read_bytes(),
                                     capture_output=True, timeout=15,
                                     env={'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'})
            require(checked.returncode == 0 and len(checked.stdout) < 4096)
            actual = json.loads(checked.stdout)
        require(all(actual.get(k) == asset[k] for k in
                    ('sha256', 'byte_length', 'media_type', 'width', 'height')))
    owner = (args.media.stat().st_uid, args.media.stat().st_gid)
    with db.transaction():
        require(db.execute("SELECT pg_try_advisory_xact_lock(hashtextextended('sns-verified-publication',0))").fetchone()[0])
        for event_id, role in sorted((r['event_id'], r.get('asset_role', 'media-'+str(r['slot']))) for r in rows):
            if not db.execute("SELECT pg_try_advisory_xact_lock(hashtextextended(%s,0))", ('media-slot:social_post:' + event_id + ':' + role,)).fetchone()[0]:
                raise BlockingIOError('sns_publication_busy')
        # Legacy additive writers omit slot locks. Acquire the table lock
        # without waiting, so mixed lock orders cannot deadlock publication.
        # A pending decoded batch is retried through its original source page.
        db.execute('LOCK TABLE crm.media_links IN SHARE ROW EXCLUSIVE MODE NOWAIT')
        for digest in sorted({r['media']['sha256'] for r in rows}):
            # Share the service publisher/GC exclusion contract until commit.
            db.execute("SELECT pg_advisory_xact_lock(hashtextextended('media-gc:'||%s,0))", (digest,))
        events = {}
        for event_id in sorted({r['event_id'] for r in rows}):
            saved = db.execute('SELECT provider,external_id,content,source_account_id FROM crm.timeline_events WHERE id=%s FOR UPDATE', (event_id,)).fetchone()
            require(saved and saved[0] == 'wechat')
            events[event_id] = saved
        before = db.execute('SELECT entity_type,entity_id::text,media_id::text,role FROM crm.media_links WHERE entity_id=ANY(%s::uuid[]) ORDER BY entity_id,role,media_id', (list(events),)).fetchall()
        for row in rows:
            provider, external, content, source_account = events[row['event_id']]
            require(external == row['external_id'])
            original = content['media'][row['slot'] - 1]
            role = row.get('asset_role', 'media-' + str(row['slot']))
            require(role in ('media-'+str(row['slot']), 'media-'+str(row['slot'])+'-poster'))
            source_url = original.get('thumbnail_url') if role.endswith('-poster') else original.get('source_url') or original.get('thumbnail_url') or ''
            require(original.get('role') == 'media-' + str(row['slot']) and
                    source_url and source_url.replace('http://', 'https://', 1) == row['source_url'] and
                    original.get('source_media_id') == row['source_media_id'] and
                    original.get('source_md5') == row['source_md5'])
            asset = row['media']
            digest = asset['sha256']
            path = restorer.publish(args.stage / 'objects' / digest, args.media,
                                    digest, asset['byte_length'], owner)
            media_id = uuid.uuid5(uuid.NAMESPACE_URL, 'monica:sns:' + digest)
            provenance = 'sns-restore:' + external + ':' + str(row['slot']) + ':' + row['raw_xml_sha256']
            db.execute("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,width,height,source_type,source_record_id,duration_ms,verified_at) VALUES(%s,%s,%s,%s,%s,%s,%s,'wechat',%s,%s,now()) ON CONFLICT(sha256) DO UPDATE SET media_type=excluded.media_type,byte_length=excluded.byte_length,object_path=excluded.object_path,width=excluded.width,height=excluded.height,duration_ms=coalesce(excluded.duration_ms,crm.media_assets.duration_ms),verified_at=excluded.verified_at", (media_id, digest, asset['media_type'], asset['byte_length'], path, asset['width'], asset['height'], provenance,asset.get('duration_ms')))
            media_id, length = db.execute('SELECT id,byte_length FROM crm.media_assets WHERE sha256=%s', (digest,)).fetchone()
            require(length == asset['byte_length'])
            db.execute("INSERT INTO crm.media_provenance(media_id,source,source_record_id,source_url) VALUES(%s,'wechat',%s,%s) ON CONFLICT DO NOTHING", (media_id, provenance, source_url))
            # The unique slot arbitrates concurrent publishers. RETURNING must
            # confirm the winner before inserting a potentially duplicate link.
            existing = db.execute("INSERT INTO crm.media_slots(entity_type,entity_id,media_id,role) VALUES('social_post',%s,%s,%s) ON CONFLICT(entity_type,entity_id,role) DO UPDATE SET media_id=crm.media_slots.media_id RETURNING media_id", (row['event_id'], media_id, role)).fetchone()
            require(existing[0] == media_id)
            linked = db.execute("SELECT media_id FROM crm.media_links WHERE entity_type='social_post' AND entity_id=%s AND role=%s FOR UPDATE", (row['event_id'], role)).fetchall()
            require(all(r[0] == media_id for r in linked))
            db.execute("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role,source_account_id) VALUES('social_post',%s,%s,%s,%s) ON CONFLICT DO NOTHING", (row['event_id'], media_id, role, source_account))
        from psycopg.types.json import Jsonb
        metadata = dict(manifest_sha256=args.manifest_sha256, records=len(rows), before_links=before,
                        files_deleted=0, references_deleted=0)
        db.execute("INSERT INTO crm.audit_events(action,entity_type,metadata) VALUES('media.sns_import_repaired','social_post',%s)", (Jsonb(metadata),))
    result = dict(complete=True, records=len(rows), unique_files=len({r['media']['sha256'] for r in rows}),
                  manifest_sha256=args.manifest_sha256, files_deleted=0, references_deleted=0)
    helper.atomic_json(args.state / 'result.json', result)
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--stage', type=Path, required=True)
    p.add_argument('--media', type=Path, required=True)
    p.add_argument('--state', type=Path, required=True)
    p.add_argument('--validator', type=Path, required=True)
    p.add_argument('--runtime', type=Path)
    p.add_argument('--manifest-sha256', required=True)
    p.add_argument('--expected-records', type=int, required=True)
    p.add_argument('--expected-system-id', required=True)
    p.add_argument('--expected-schema', type=int, required=True)
    args = p.parse_args()
    import psycopg
    with psycopg.connect(os.environ['MONICA_DATABASE_URL'], autocommit=True) as db:
        print(json.dumps(publish(db, args)), flush=True)


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Exception as error:
        print(json.dumps(dict(error_type=type(error).__name__)), flush=True)
        raise SystemExit(1)

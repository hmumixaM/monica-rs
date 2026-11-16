"""Restore archived URL-to-media identities without downloading or guessing slots."""
import argparse
import hashlib
import json
import os
from pathlib import Path
from urllib.parse import urlsplit
import uuid


def restore_page(db, media, migration, after, limit=250):
    rows = db.execute("SELECT record_key,payload FROM crm.legacy_history WHERE migration_id=%s AND store='crm' AND table_name='remote_media_sources' AND record_key>%s ORDER BY record_key LIMIT %s", (migration, after, limit)).fetchall()
    inserted = missing = 0
    with db.transaction():
        db.execute("SET LOCAL lock_timeout='2s'")
        for key, value in rows:
            mid, url = uuid.UUID(value['media_id']), value['source_url']
            parsed = urlsplit(url)
            if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password:
                raise ValueError('invalid archived media identity')
            asset = db.execute('SELECT sha256,byte_length,source_type FROM crm.media_assets WHERE id=%s', (mid,)).fetchone()
            if not asset:
                raise ValueError('archived media ID has no migrated asset')
            digest, size, provider = asset
            if len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest):
                raise ValueError('invalid migrated media hash')
            path = media/'sha256'/digest[:2]/digest[2:4]/digest
            try:
                available = path.resolve() == path and path.is_file() and path.stat().st_size == size
            except OSError:
                available = False
            if not available:
                missing += 1
                continue
            proof = 'legacy-remote:' + str(migration) + ':' + hashlib.sha256(url.encode()).hexdigest()
            result = db.execute("INSERT INTO crm.media_provenance(media_id,source,source_record_id,source_url) VALUES(%s,%s,%s,%s) ON CONFLICT DO NOTHING", (mid, provider or 'legacy', proof, url))
            inserted += result.rowcount
    return dict(after=rows[-1][0] if rows else after, processed=len(rows), inserted=inserted, missing_files=missing)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', required=True)
    parser.add_argument('--media', type=Path, required=True)
    parser.add_argument('--state', type=Path, required=True)
    parser.add_argument('--migration', type=uuid.UUID, required=True)
    parser.add_argument('--expected-system-id', required=True)
    parser.add_argument('--expected-schema', type=int, required=True)
    args = parser.parse_args()
    import fcntl
    import psycopg
    if args.media.resolve(strict=True) != args.media or args.state.resolve(strict=True) != args.state or args.media == args.state or args.media in args.state.parents or args.state in args.media.parents:
        raise ValueError('canonical independent roots required')
    if (args.media/'.monica-rust-migration-media').read_bytes() != b'monica-rust-isolated-media-v1\n':
        raise ValueError('isolated media marker differs')
    with (args.state/'control.lock').open('a') as lock, psycopg.connect(os.environ['MONICA_DATABASE_URL'], autocommit=True) as db:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        identity = db.execute("SELECT system_identifier::text,current_database(),(SELECT oid::text FROM pg_database WHERE datname=current_database()),(SELECT max(version) FROM _sqlx_migrations WHERE success) FROM pg_control_system()").fetchone()
        if identity != (args.expected_system_id, 'monica_rust', '16384', args.expected_schema):
            raise ValueError('isolated target identity differs')
        if not db.execute("SELECT 1 FROM crm.legacy_migration_tables WHERE migration_id=%s AND store='crm' AND table_name='remote_media_sources' AND status='complete' AND expected=processed", (args.migration,)).fetchone():
            raise ValueError('archived URL inventory is not complete')
        policy = [*identity, str(args.migration), str(args.media)]
        checkpoint = args.state/'status.json'
        if checkpoint.is_symlink() or checkpoint.with_suffix('.partial').is_symlink():
            raise ValueError('checkpoint alias')
        state = json.loads(checkpoint.read_text()) if checkpoint.exists() else dict(policy=policy, after='', processed=0, inserted=0, missing_files=0, complete=False)
        if state['policy'] != policy:
            raise ValueError('checkpoint target differs')
        while not state['complete']:
            page = restore_page(db, args.media, args.migration, state['after'])
            state['after'] = page['after']
            for key in ('processed', 'inserted', 'missing_files'):
                state[key] += page[key]
            state['complete'] = not page['processed']
            temporary = checkpoint.with_suffix('.partial')
            with temporary.open('w') as output:
                json.dump(state, output); output.flush(); os.fsync(output.fileno())
            os.replace(temporary, checkpoint)
            directory = os.open(args.state, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
            print(json.dumps({key:state[key] for key in ('processed','inserted','missing_files','complete')}), flush=True)


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Exception as error:
        print(json.dumps(dict(error_code=type(error).__name__)), flush=True)
        raise SystemExit(1)

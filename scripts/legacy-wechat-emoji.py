#!/usr/bin/env python3
"""Link type-47 emoji from already-copied local assets. Never fetch a URL.

Run explicitly after indexed-file registration. A shared MD5 cache reuses one
CAS object across messages. Missing references remain visible and are retried at
most daily, unless --retry-missing is explicitly supplied. Type-3 pictures are
outside this worker, even if they look like stickers.
"""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import uuid
import xml.etree.ElementTree as ET

MARKER = 'monica-rust-isolated-media-v1\n'
NIL = '00000000-0000-0000-0000-000000000000'


def references(*values):
    result = []
    def visit(value):
        if isinstance(value, dict):
            for key in ('text', 'body', 'raw_body', 'chatlog_original', 'content'):
                if key in value:
                    visit(value[key])
        elif isinstance(value, str) and len(value) <= 8 * 1024 * 1024:
            start = value.find('<msg')
            if start < 0:
                start = value.find('<emoji')
            if start < 0 or '<!DOCTYPE' in value.upper() or '<!ENTITY' in value.upper():
                return
            try:
                root = ET.fromstring(value[start:])
            except ET.ParseError:
                return
            for node in root.iter('emoji'):
                key = node.get('md5', '').lower()
                if re.fullmatch('[0-9a-f]{32}', key) and key != '0' * 32 and key not in result:
                    result.append(key)
    for value in values:
        visit(value)
    return result


def canonical(path):
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise ValueError('canonical path required')
    return path


def asset_file(root, asset):
    key = asset['sha256']
    if not re.fullmatch('[0-9a-f]{64}', key):
        raise ValueError('invalid digest')
    relative = 'sha256/' + key[:2] + '/' + key[2:4] + '/' + key
    if asset['object_path'] != relative or not asset['media_type'].startswith('image/'):
        raise ValueError('invalid copied asset')
    path = canonical(root / relative)
    if not path.is_file() or path.stat().st_size != asset['byte_length']:
        raise ValueError('copied bytes missing')
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    if digest.hexdigest() != key:
        raise ValueError('copied bytes differ')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true')
    parser.add_argument('--retry-missing', action='store_true')
    parser.add_argument('--after', default=NIL, type=uuid.UUID)
    parser.add_argument('--limit', default=500, type=int)
    for key in ('assets', 'local-index', 'media'):
        parser.add_argument('--' + key, type=Path)
    for key in ('expected-database', 'expected-system-id'):
        parser.add_argument('--' + key)
    args = parser.parse_args()
    if not args.run:
        parser.print_help()
        return
    if not 1 <= args.limit <= 1000 or any(getattr(args, key) is None for key in ('assets', 'local_index', 'media', 'expected_database', 'expected_system_id')):
        parser.error('explicit local paths and isolated database identity required')
    assets, index, media = map(canonical, (args.assets, args.local_index, args.media))
    if (media / '.monica-rust-migration-media').read_text() != MARKER:
        raise ValueError('wrong media destination')
    import psycopg
    with closing(sqlite3.connect(index.as_uri() + '?mode=ro&immutable=1', uri=True)) as local, closing(sqlite3.connect(assets.as_uri() + '?mode=ro', uri=True)) as ledger, psycopg.connect(os.environ['MONICA_DATABASE_URL'], autocommit=True) as db:
        if db.execute('SELECT current_database(),system_identifier::text FROM pg_control_system()').fetchone() != (args.expected_database, args.expected_system_id):
            raise ValueError('wrong target database')
        if not db.execute('SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=86 AND success)').fetchone()[0]:
            raise ValueError('emoji migration required')
        # Only the mutable worker control is reserved; no source fact is locked.
        if not db.execute("SELECT pg_try_advisory_lock(hashtextextended('monica-local-emoji-linker',0))").fetchone()[0]:
            raise ValueError('another emoji linker is active')
        a = dict(ledger.execute('SELECT * FROM metadata'))
        b = dict(local.execute('SELECT * FROM metadata'))
        if a.get('snapshot') != b.get('snapshot') or a.get('input_sha256') != b.get('input_sha256'):
            raise ValueError('local cache identities differ')
        local.execute('ATTACH DATABASE ? AS copied', (assets.as_uri() + '?mode=ro',))
        rows = db.execute("""SELECT m.message_id,m.body,coalesce(v.payload_json,o.payload_json),
 md5(m.body||coalesce(v.sha256,o.sha256,'')||coalesce(p.revision::text,'')) fingerprint
 FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id
 LEFT JOIN crm.legacy_message_evidence e ON e.message_id=m.message_id LEFT JOIN crm.content_objects o ON o.id=e.content_object_id
 LEFT JOIN crm.legacy_chatlog_projection p ON p.entity_type='message' AND p.entity_id=m.message_id
 LEFT JOIN crm.legacy_chatlog_versions lv ON lv.entity_type='message' AND lv.entity_id=m.message_id AND lv.revision=p.revision
 LEFT JOIN crm.content_objects v ON v.id=lv.content_object_id
 WHERE m.provider='wechat' AND m.message_type='47' AND NOT m.tombstone AND m.message_id>%s
 ORDER BY m.message_id LIMIT %s""", (args.after, args.limit)).fetchall()
        counts = {'cached': 0, 'not_in_local_archive': 0, 'no_reference': 0}
        for mid, body, raw, fingerprint in rows:
            keys = references(body, raw)
            # A single emoji message must have one unambiguous content identity.
            key = keys[0] if len(keys) == 1 else None
            state, media_id = 'no_reference', None
            if key:
                prior = db.execute('SELECT state,media_id,retry_after>now() FROM crm.wechat_emoji_cache WHERE md5=%s', (key,)).fetchone()
                if prior and (prior[0] == 'cached' or (prior[2] and not args.retry_missing)):
                    state, media_id = prior[:2]
                else:
                    saved = local.execute("SELECT a.asset FROM keys k JOIN copied.assets a USING(source_key) WHERE k.kind='image' AND k.md5=? AND a.status='ready' ORDER BY k.source_key LIMIT 1", (key,)).fetchone()
                    state = 'not_in_local_archive'
                    if saved:
                        asset = json.loads(saved[0])
                        asset_file(media, asset)
                        media_id = db.execute('SELECT id FROM crm.media_assets WHERE sha256=%s AND byte_length=%s', (asset['sha256'], asset['byte_length'])).fetchone()
                        if not media_id:
                            raise ValueError('indexed media registration must finish first')
                        media_id, state = media_id[0], 'cached'
                    db.execute("INSERT INTO crm.wechat_emoji_cache(md5,media_id,state) VALUES(%s,%s,%s) ON CONFLICT(md5) DO UPDATE SET media_id=excluded.media_id,state=excluded.state,checked_at=now(),retry_after=now()+interval '1 day'", (key, media_id, state))
            with db.transaction():
                # This is a derived link, never the immutable original message.
                db.execute("DELETE FROM crm.media_links WHERE entity_type='message_event' AND entity_id=%s AND role LIKE 'wechat-emoji:%%' AND media_id IS DISTINCT FROM %s", (mid, media_id))
                if media_id:
                    db.execute("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) VALUES('message_event',%s,%s,%s) ON CONFLICT DO NOTHING", (mid, media_id, 'wechat-emoji:' + key))
                db.execute("INSERT INTO crm.wechat_emoji_messages(message_id,reference_fingerprint,md5,state) VALUES(%s,%s,%s,%s) ON CONFLICT(message_id) DO UPDATE SET reference_fingerprint=excluded.reference_fingerprint,md5=excluded.md5,state=excluded.state,checked_at=now()", (mid, fingerprint, key, state))
            counts[state] += 1
        print(json.dumps({'phase': 'local_emoji_links', 'counts': counts, 'processed': len(rows), 'after': str(rows[-1][0]) if rows else str(args.after), 'more': len(rows) == args.limit, 'network_requests': 0}), flush=True)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print(json.dumps({'error_code': 'local_emoji_link_failed'}), flush=True)
        raise SystemExit(1) from None

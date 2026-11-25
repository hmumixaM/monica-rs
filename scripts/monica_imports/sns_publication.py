"""Publish verified source media through the normal import/storage contract."""
from dataclasses import dataclass
import importlib.util
import json
from pathlib import Path
from typing import Optional
import re
import subprocess
import uuid

from .files import atomic_json, canonical, checksum
from .media_store import MediaStore


@dataclass(frozen=True)
class DatabaseTarget:
    system_id: str
    database: str
    oid: str

    @property
    def values(self):
        return self.system_id, self.database, self.oid

    @classmethod
    def read(cls, db):
        return cls(*db.execute("SELECT system_identifier::text,current_database(),(SELECT oid::text FROM pg_database WHERE datname=current_database()) FROM pg_control_system()").fetchone())


@dataclass(frozen=True)
class Publication:
    stage: Path
    media: Path
    state: Path
    validator: Path
    manifest_sha256: str
    expected_records: int
    target: DatabaseTarget
    runtime: Optional[Path] = None
    native_event: Optional[tuple[uuid.UUID, int]] = None


def module(name, filename):
    path = Path(__file__).resolve().parents[1] / filename
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def require(value):
    if not value:
        raise ValueError('sns_publication_contract')


def publication_entity(row):
    return ('social_interaction', row['interaction_id']) if row.get('interaction_id') else ('social_post', row['event_id'])


def publish(db, request: Publication):
    store = MediaStore(request.media)
    for path in (request.stage, request.media, request.state):
        canonical(path, directory=True)
    require(not any(a == b or a in b.parents or b in a.parents for a, b in
                    ((request.stage, request.media), (request.state, request.media), (request.stage, request.state))))
    manifest = request.stage / 'manifest.json'
    require(checksum(manifest) == request.manifest_sha256)
    rows = json.loads(manifest.read_text())['rows']
    require(len(rows) == request.expected_records and 0 < len(rows) <= 1000)
    require(len({(*publication_entity(r), r.get('asset_role', 'media-'+str(r['slot']))) for r in rows}) == len(rows))
    require(DatabaseTarget.read(db) == request.target)
    for row in rows:
        asset = row['media']
        require(str(uuid.UUID(row['event_id'])) == row['event_id'] and row['external_id'].isdigit())
        if row.get('interaction_id'):
            require(str(uuid.UUID(row['interaction_id'])) == row['interaction_id'] and asset['media_type'].startswith('image/'))
        require(type(row['slot']) is int and 1 <= row['slot'] <= 100)
        video = asset['media_type'] == 'video/mp4'
        require(row.get('full_video') is True if video else row.get('full_pixels') is True)
        require(re.fullmatch('[0-9a-f]{64}', asset['sha256']) and
                0 < asset['byte_length'] <= (512 if video else 32) * 1024 * 1024 and
                asset['media_type'] in ('image/jpeg', 'image/png', 'video/mp4'))
        source = request.stage / 'objects' / asset['sha256']
        require(canonical(source).st_size == asset['byte_length'])
        if video:
            verifier=module('sns_video_validation','sns-video.py')
            source_md5 = row.get('source_md5') if row.get('verified_cached_video') else None
            require(not row.get('verified_cached_video') or
                    (isinstance(source_md5, str) and re.fullmatch(r'[a-fA-F0-9]{32}', source_md5)))
            actual=verifier.validate(source,request.runtime,source_md5=source_md5)
        else:
            checked = subprocess.run([str(request.validator)], input=source.read_bytes(),
                                     capture_output=True, timeout=15,
                                     env={'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'})
            require(checked.returncode == 0 and len(checked.stdout) < 4096)
            actual = json.loads(checked.stdout)
        require(all(actual.get(k) == asset[k] for k in
                    ('sha256', 'byte_length', 'media_type', 'width', 'height')))
    with db.transaction():
        if request.native_event:
            connection, version = request.native_event
            require(db.execute("SELECT NOT paused FROM crm.scheduler_control WHERE singleton FOR SHARE").fetchone() == (True,))
            require(db.execute("SELECT enabled AND NOT paused FROM crm.adapter_controls WHERE provider='wechat' FOR SHARE").fetchone() == (True,))
            require(db.execute("SELECT enabled FROM crm.source_connections WHERE id=%s FOR SHARE", (connection,)).fetchone() == (True,))
        require(db.execute("SELECT pg_try_advisory_xact_lock(hashtextextended('sns-verified-publication',0))").fetchone()[0])
        for entity_type, entity_id, role in sorted((*publication_entity(r), r.get('asset_role', 'media-'+str(r['slot']))) for r in rows):
            if not db.execute("SELECT pg_try_advisory_xact_lock(hashtextextended(%s,0))", (f'media-slot:{entity_type}:{entity_id}:{role}',)).fetchone()[0]:
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
            saved = db.execute("SELECT provider,external_id,content,source_account_id,connection_id,version FROM crm.timeline_events WHERE id=%s AND status='active' FOR UPDATE", (event_id,)).fetchone()
            require(saved and saved[0] == 'wechat')
            if request.native_event:
                require(saved[4:] == (connection, version))
            events[event_id] = saved[:4]
        before = db.execute('SELECT entity_type,entity_id::text,media_id::text,role FROM crm.media_links WHERE entity_id=ANY(%s::uuid[]) ORDER BY entity_id,role,media_id', ([publication_entity(r)[1] for r in rows],)).fetchall()
        for row in rows:
            provider, external, content, source_account = events[row['event_id']]
            if request.native_event:
                from chatlog_moments import parse_moments_xml
                root, _ = parse_moments_xml(content['raw_content'])
                xml_id = root.findtext('id' if root.tag == 'TimelineObject' else 'TimelineObject/id')
                require(xml_id == row['external_id'] and external == 'post:' + content['source_tid'])
            else:
                require(external == row['external_id'])
            entity_type, entity_id = publication_entity(row)
            if entity_type == 'social_interaction':
                interaction = db.execute("""
                    SELECT metadata,author_account_id FROM crm.social_interactions
                    WHERE id=%s AND post_id=%s AND provider='wechat'
                      AND interaction_type IN ('comment','reply') AND NOT is_deleted FOR UPDATE
                """, (entity_id, row['event_id'])).fetchone()
                require(interaction is not None)
                content, source_account = interaction
            slot = row['slot']
            original = next((item for item in content['media'] if item.get('role') == f'media-{slot}'), None)
            require(original is not None)
            role = row.get('asset_role', f'media-{slot}')
            live = role == f'live-media-{slot}'
            if live:
                require(row['media']['media_type'] == 'video/mp4' and original.get('media_type') == 'image'
                        and original.get('source_media_id') == row['parent_media_id']
                        and content.get('raw_xml_sha256') == row['raw_xml_sha256'])
                source_url = row['source_url']
            else:
                require(role in (f'media-{slot}', f'media-{slot}-poster'))
                source_url = original.get('thumbnail_url') if role.endswith('-poster') else original.get('source_url') or original.get('thumbnail_url') or ''
                require(source_url and source_url.replace('http://', 'https://', 1) == row['source_url'] and
                        original.get('source_media_id') == row['source_media_id'] and
                        original.get('source_md5') == row['source_md5'])
            asset = row['media']
            digest = asset['sha256']
            path = store.publish(request.stage / 'objects' / digest, digest, asset['byte_length'])
            media_id = uuid.uuid5(uuid.NAMESPACE_URL, 'monica:sns:' + digest)
            provenance_owner = (row['event_id'] if request.native_event else external) if entity_type == 'social_post' else entity_id
            provenance_role = role if role.startswith('live-media-') else str(slot)
            proof = row.get('raw_xml_sha256') or row.get('source_reference_sha256')
            require(isinstance(proof, str) and re.fullmatch('[0-9a-f]{64}', proof))
            provenance = 'sns-import:' + provenance_owner + ':' + provenance_role + ':' + proof
            db.execute("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path,width,height,source_type,source_record_id,duration_ms,verified_at) VALUES(%s,%s,%s,%s,%s,%s,%s,'wechat',%s,%s,now()) ON CONFLICT(sha256) DO UPDATE SET media_type=excluded.media_type,byte_length=excluded.byte_length,object_path=excluded.object_path,width=excluded.width,height=excluded.height,duration_ms=coalesce(excluded.duration_ms,crm.media_assets.duration_ms),verified_at=excluded.verified_at", (media_id, digest, asset['media_type'], asset['byte_length'], path, asset['width'], asset['height'], provenance,asset.get('duration_ms')))
            media_id, length = db.execute('SELECT id,byte_length FROM crm.media_assets WHERE sha256=%s', (digest,)).fetchone()
            require(length == asset['byte_length'])
            db.execute("INSERT INTO crm.media_provenance(media_id,source,source_record_id,source_url) VALUES(%s,'wechat',%s,%s) ON CONFLICT DO NOTHING", (media_id, provenance, source_url))
            # The unique slot arbitrates concurrent publishers. RETURNING must
            # confirm the winner before inserting a potentially duplicate link.
            native = entity_type == 'social_post' and role == f'media-{slot}' and asset['media_type'] == 'video/mp4'
            if native:
                require(original.get('media_type') == 'video')
                superseded = db.execute("""
                    SELECT a.id,a.media_type FROM crm.media_assets a WHERE a.id IN (
                        SELECT media_id FROM crm.media_slots WHERE entity_type=%s AND entity_id=%s AND role=%s
                        UNION SELECT media_id FROM crm.media_links WHERE entity_type=%s AND entity_id=%s AND role=%s
                    )
                """, (entity_type, entity_id, role, entity_type, entity_id, role)).fetchall()
                require(all(old_id == media_id or kind.startswith('image/') for old_id, kind in superseded))
            if request.native_event:
                # A revision can replace this worker's attachment. Preserve
                # slots written by other import paths or by the owner.
                prefix = 'sns-import:' + provenance_owner + ':'
                existing = db.execute("""
                    SELECT l.media_id, EXISTS (SELECT FROM crm.media_provenance p
                      WHERE p.media_id=l.media_id AND p.source='wechat' AND starts_with(p.source_record_id,%s))
                    FROM crm.media_links l WHERE l.entity_type=%s AND l.entity_id=%s AND l.role=%s
                """, (prefix, entity_type, entity_id, role)).fetchall()
                require(all(old_id == media_id or owned for old_id, owned in existing))
                for previous, _ in existing:
                    if previous != media_id:
                        db.execute("DELETE FROM crm.media_slots WHERE entity_type=%s AND entity_id=%s AND role=%s AND media_id=%s",
                                   (entity_type, entity_id, role, previous))
                        db.execute("DELETE FROM crm.media_links WHERE entity_type=%s AND entity_id=%s AND role=%s AND media_id=%s",
                                   (entity_type, entity_id, role, previous))
            if live or native:
                # Legacy importers sometimes stored an image under a native
                # video role. Keep its link, and advance the verified clip slot.
                db.execute("""INSERT INTO crm.media_slots(entity_type,entity_id,media_id,role)
                    VALUES('social_post',%s,%s,%s) ON CONFLICT(entity_type,entity_id,role) DO UPDATE SET
                    media_id=excluded.media_id,generation=crm.media_slots.generation+1,updated_at=now()
                    WHERE crm.media_slots.media_id<>excluded.media_id""", (row['event_id'], media_id, role))
            else:
                existing = db.execute("INSERT INTO crm.media_slots(entity_type,entity_id,media_id,role) VALUES(%s,%s,%s,%s) ON CONFLICT(entity_type,entity_id,role) DO UPDATE SET media_id=crm.media_slots.media_id RETURNING media_id", (entity_type, entity_id, media_id, role)).fetchone()
                require(existing[0] == media_id)
                linked = db.execute("SELECT media_id FROM crm.media_links WHERE entity_type=%s AND entity_id=%s AND role=%s FOR UPDATE", (entity_type, entity_id, role)).fetchall()
                require(all(r[0] == media_id for r in linked))
            db.execute("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role,source_account_id) VALUES(%s,%s,%s,%s,%s) ON CONFLICT DO NOTHING", (entity_type, entity_id, media_id, role, source_account))
        from psycopg.types.json import Jsonb
        metadata = dict(manifest_sha256=request.manifest_sha256, records=len(rows), before_links=before,
                        files_deleted=0, references_deleted=0)
        db.execute("INSERT INTO crm.audit_events(action,entity_type,metadata) VALUES('media.sns_import_published','social_post',%s)", (Jsonb(metadata),))
    result = dict(complete=True, records=len(rows), unique_files=len({r['media']['sha256'] for r in rows}),
                  manifest_sha256=request.manifest_sha256, files_deleted=0, references_deleted=0)
    atomic_json(request.state / 'result.json', result)
    return result

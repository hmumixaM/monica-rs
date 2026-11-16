"""Bind changed SNS URLs through archived stable image identities.

This repairs import data. It never guesses by a numbered slot alone and never
removes assets, references, or files. Call on an explicit source revision only.
"""
import hashlib
from pathlib import Path
import re


def link_existing(db, media_root, event_id, role, source_url):
    """Reuse the unique verified file already bound to this exact URL."""
    from psycopg.types.json import Jsonb
    assets=db.execute("SELECT DISTINCT a.id,a.sha256,a.byte_length FROM crm.media_provenance p JOIN crm.media_assets a ON a.id=p.media_id WHERE p.source_url=%s AND a.media_type LIKE 'image/%%'",(source_url,)).fetchall()
    if len(assets)!=1:return False
    mid,sha,size=assets[0]
    if not re.fullmatch('[0-9a-f]{64}',sha):raise ValueError('sns_asset_digest_invalid')
    path=Path(media_root)/'sha256'/sha[:2]/sha[2:4]/sha
    if not path.is_file():return False  # Keep references; try the source cache/CDN.
    if path.is_symlink() or path.resolve()!=path or path.stat().st_size!=size:
        raise ValueError('sns_cas_requires_recovery')
    digest=hashlib.sha256()
    with path.open('rb')as image:
        for block in iter(lambda:image.read(1024*1024),b''):digest.update(block)
    if digest.hexdigest()!=sha:raise ValueError('sns_cas_digest_mismatch')
    with db.transaction():
        row=db.execute("SELECT content,source_account_id FROM crm.timeline_events WHERE id=%s AND provider='wechat' FOR UPDATE",(event_id,)).fetchone()
        if not row or not any(m.get('role')==role and m.get('source_url')==source_url for m in row[0].get('media',[])):
            raise ValueError('sns_source_revision_changed')
        if not db.execute("SELECT pg_try_advisory_xact_lock(hashtextextended(%s,0))",('media-slot:social_post:'+str(event_id)+':'+role,)).fetchone()[0]:
            raise BlockingIOError('sns_slot_busy')
        prior=db.execute("SELECT media_id FROM crm.media_links WHERE entity_type='social_post' AND entity_id=%s AND role=%s",(event_id,role)).fetchall()
        if any(r[0]!=mid for r in prior):raise ValueError('sns_slot_identity_conflict')
        chosen=db.execute("INSERT INTO crm.media_slots(entity_type,entity_id,role,media_id) VALUES('social_post',%s,%s,%s) ON CONFLICT(entity_type,entity_id,role) DO UPDATE SET media_id=crm.media_slots.media_id RETURNING media_id",(event_id,role,mid)).fetchone()[0]
        if chosen!=mid:raise ValueError('sns_slot_identity_conflict')
        inserted=db.execute("INSERT INTO crm.media_links(entity_type,entity_id,role,media_id,source_account_id) VALUES('social_post',%s,%s,%s,%s) ON CONFLICT DO NOTHING",(event_id,role,mid,row[1])).rowcount
        if inserted:db.execute("INSERT INTO crm.audit_events(action,entity_type,entity_id,metadata) VALUES('media.sns_existing_file_linked','social_post',%s,%s)",(event_id,Jsonb(dict(role=role,media_id=str(mid),source_url=source_url,sha256=sha,files_deleted=0,references_deleted=0))))
    return True


def repair_event(db, media_root, event_id):
    from psycopg.types.json import Jsonb
    inserted = []
    with db.transaction():
        event = db.execute("SELECT content FROM crm.timeline_events WHERE id=%s AND provider='wechat' FOR UPDATE", (event_id,)).fetchone()
        if not event:
            raise ValueError('sns_event_not_found')
        history = db.execute("SELECT v.id,o.payload_json->'media' FROM crm.legacy_social_versions v JOIN crm.content_objects o ON o.id=v.content_object_id WHERE v.event_id=%s ORDER BY v.observed_at,v.id", (event_id,)).fetchall()
        for item in event[0].get('media', []):
            identity = item.get('source_media_id')
            digest = item.get('source_md5')
            role, url = item.get('role'), item.get('source_url')
            if not identity or not role or not url:
                continue
            if digest is not None and not re.fullmatch('[0-9a-fA-F]{32}', digest):
                continue
            old = [(str(version), photo['source_url']) for version, photos in history for photo in (photos or [])
                   if photo.get('source_media_id') == identity and photo.get('source_md5') == digest
                   and photo.get('role') == role and photo.get('media_type') == item.get('media_type')
                   and (digest is not None or
                        (str(identity).isdigit() and all(item.get(k) and photo.get(k) == item[k]
                         for k in ('width', 'height', 'byte_length'))))
                   and photo.get('source_url') and photo['source_url'] != url]
            if not old:
                continue
            matches = db.execute("SELECT DISTINCT a.id,a.sha256,a.byte_length FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id JOIN crm.media_provenance p ON p.media_id=a.id WHERE l.entity_type='social_post' AND l.entity_id=%s AND l.role=%s AND p.source_url=ANY(%s)", (event_id, role, [u for _, u in old])).fetchall()
            if len(matches) != 1:
                continue
            mid, sha, size = matches[0]
            if not re.fullmatch('[0-9a-f]{64}', sha):
                raise ValueError('sns_asset_digest_invalid')
            db.execute("SELECT pg_advisory_xact_lock(hashtextextended('media-gc:'||%s,0))", (sha,))
            path = Path(media_root)/'sha256'/sha[:2]/sha[2:4]/sha
            # A failure here preserves every reference for source recovery.
            # It is not evidence that the original photograph no longer exists.
            if path.is_symlink() or path.resolve(strict=True) != path or path.stat().st_size != size:
                raise ValueError('sns_cas_requires_recovery')
            actual = hashlib.sha256()
            with path.open('rb') as source:
                for block in iter(lambda: source.read(1024*1024), b''):
                    actual.update(block)
            if actual.hexdigest() != sha:
                raise ValueError('sns_cas_digest_mismatch')
            record = 'sns-url-revision:' + str(event_id) + ':' + identity + ':' + hashlib.sha256(url.encode()).hexdigest()
            added = db.execute("INSERT INTO crm.media_provenance(media_id,source,source_record_id,source_url) VALUES(%s,'wechat',%s,%s) ON CONFLICT DO NOTHING", (mid, record, url)).rowcount
            if added:
                inserted.append(dict(role=role, media_id=str(mid), source_media_id=identity,
                                     sha256=sha, source_url=url, historical_versions=[v for v, _ in old]))
        if inserted:
            db.execute("INSERT INTO crm.audit_events(action,entity_type,entity_id,metadata) VALUES('media.sns_url_identity_repaired','social_post',%s,%s)", (event_id, Jsonb(dict(bindings=inserted,files_deleted=0,references_deleted=0))))
    return dict(event_id=str(event_id), inserted=len(inserted), files_deleted=0, references_deleted=0)

"""Reuse a verified file for an exact source URL during normal ingestion."""
from pathlib import Path
import re

from .files import canonical, checksum


def matches_source(media, role, source_url):
    if media.get('role') == role:
        return media.get('source_url') == source_url
    if role.endswith('-poster') and media.get('role') == role.removesuffix('-poster'):
        return media.get('thumbnail_url') == source_url
    return False


def link_existing(db, media_root, event_id, role, source_url, media_kind='image', *, interaction_id=None):
    """Reuse the unique verified file already bound to this exact URL."""
    from psycopg.types.json import Jsonb

    assets = db.execute(
        'SELECT DISTINCT a.id,a.sha256,a.byte_length FROM crm.media_provenance p '
        'JOIN crm.media_assets a ON a.id=p.media_id '
        'WHERE p.source_url=%s AND a.media_type LIKE %s',
        (source_url, media_kind + '/%'),
    ).fetchall()
    if len(assets) != 1:
        return False
    media_id, digest, size = assets[0]
    if not re.fullmatch('[0-9a-f]{64}', digest):
        raise ValueError('sns_asset_digest_invalid')
    path = Path(media_root) / 'sha256' / digest[:2] / digest[2:4] / digest
    if not path.is_file():
        return False
    if canonical(path).st_size != size or checksum(path) != digest:
        raise ValueError('sns_asset_file_invalid')

    with db.transaction():
        row = db.execute(
            "SELECT content,source_account_id FROM crm.timeline_events "
            "WHERE id=%s AND provider='wechat' AND status='active' FOR UPDATE", (event_id,),
        ).fetchone()
        entity_type, entity_id = 'social_post', event_id
        if row and interaction_id is not None:
            entity_type, entity_id = 'social_interaction', interaction_id
            row = db.execute("""
                SELECT metadata,author_account_id FROM crm.social_interactions
                WHERE id=%s AND post_id=%s AND provider='wechat'
                  AND interaction_type IN ('comment','reply') AND NOT is_deleted FOR UPDATE
            """, (entity_id, event_id)).fetchone()
        if not row or not any(matches_source(item, role, source_url) for item in row[0].get('media', [])):
            raise ValueError('sns_source_revision_changed')
        lock = f'media-slot:{entity_type}:{entity_id}:{role}'
        if not db.execute('SELECT pg_try_advisory_xact_lock(hashtextextended(%s,0))', (lock,)).fetchone()[0]:
            raise BlockingIOError('sns_slot_busy')
        prior = db.execute("""
            SELECT a.id,a.media_type FROM crm.media_assets a WHERE a.id IN (
                SELECT media_id FROM crm.media_links WHERE entity_type=%s AND entity_id=%s AND role=%s
                UNION SELECT media_id FROM crm.media_slots WHERE entity_type=%s AND entity_id=%s AND role=%s
            )
        """, (entity_type, entity_id, role, entity_type, entity_id, role)).fetchall()
        conflicts = [kind for linked_id, kind in prior if linked_id != media_id]
        if conflicts:
            if media_kind == 'video' and entity_type == 'social_post' and all(kind.startswith('image/') for kind in conflicts):
                return False  # The verified publisher can advance the old still slot.
            raise ValueError('sns_slot_identity_conflict')
        chosen = db.execute(
            "INSERT INTO crm.media_slots(entity_type,entity_id,role,media_id) "
            "VALUES(%s,%s,%s,%s) ON CONFLICT(entity_type,entity_id,role) "
            "DO UPDATE SET media_id=crm.media_slots.media_id RETURNING media_id",
            (entity_type, entity_id, role, media_id),
        ).fetchone()[0]
        if chosen != media_id:
            raise ValueError('sns_slot_identity_conflict')
        inserted = db.execute(
            "INSERT INTO crm.media_links(entity_type,entity_id,role,media_id,source_account_id) "
            "VALUES(%s,%s,%s,%s,%s) ON CONFLICT DO NOTHING",
            (entity_type, entity_id, role, media_id, row[1]),
        ).rowcount
        if inserted:
            metadata = dict(role=role, media_id=str(media_id), source_url=source_url,
                            sha256=digest, files_deleted=0, references_deleted=0)
            db.execute(
                "INSERT INTO crm.audit_events(action,entity_type,entity_id,metadata) "
                "VALUES('media.sns_existing_file_linked',%s,%s,%s)",
                (entity_type, entity_id, Jsonb(metadata)),
            )
    return True

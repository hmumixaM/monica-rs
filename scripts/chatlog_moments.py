"""Pure Moments XML contract from the pinned ordinary NAS baseline.

Only parsing functions are included; no SQL, network, or credentials.
"""
from __future__ import annotations
import hashlib
import re
import xml.etree.ElementTree as ET
from collections import Counter
from datetime import datetime, timezone
from typing import Any
UTC = timezone.utc
# Source: hmumixaM/monica-next-crm, 6d59cfef4b1a12f678d809a00e37d8332be70b06
# source_time.py: sha256 a6892468e7af28179f85612882b74e65768e105dcfa786c196ab467abcb5656a
# moments_xml.py: sha256 7b6e818a07e65bac35869ce75a6e736a162ea4163d27a484fcfa07be9948b530
# moments_posts.py: sha256 f44acf7299d2b8d0acf67a0c032387c8278804d551f83c60013987f9899441cb

def iso(value: Any) -> str | None:
    if value in (None, ''):
        return None
    if isinstance(value, (int, float)):
        try:
            return datetime.fromtimestamp(value, UTC).isoformat()
        except (OverflowError, OSError, ValueError):
            return None
    value = str(value).strip()
    if not value:
        return None
    if value.isdigit() or (value.startswith('-') and value[1:].isdigit()):
        try:
            return datetime.fromtimestamp(int(value), UTC).isoformat()
        except (OverflowError, OSError, ValueError):
            return None
    try:
        return datetime.fromisoformat(value).isoformat()
    except ValueError:
        return None

def parse_moments_xml(value):
    try:
        return (ET.fromstring(value or ''), {})
    except ET.ParseError:
        repairs = Counter()

        def escape(match):
            token = match.group(1)
            if token is None:
                return match.group(0)
            digits = token.removeprefix('x')
            digits = digits.lstrip('0') or '0'
            number = int(digits, 16 if token.startswith('x') else 10) if len(digits) <= 8 else 1114112
            if number in (9, 10, 13) or 32 <= number <= 55295 or 57344 <= number <= 65533 or (65536 <= number <= 1114111):
                return match.group(0)
            repairs[match.group(0)] += 1
            return '&amp;' + match.group(0)[1:]
        escaped = re.sub('<!\\[CDATA\\[.*?\\]\\]>|<!--.*?-->|<\\?.*?\\?>|&#(x[0-9a-fA-F]+|[0-9]+);', escape, value or '', flags=re.DOTALL)
        return (ET.fromstring(escaped), dict(repairs))

def shared_link(obj):
    """Extract the shared destination, never confuse its cover with the post."""
    from urllib.parse import urlsplit
    content = obj.find('ContentObject')
    if content is None or content.findtext('type') not in ('3', '4', '5'):
        return None
    url = (content.findtext('contentUrl') or '').strip()
    try:
        parsed = urlsplit(url)
        if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password:
            return None
    except ValueError:
        return None
    result = {'url': url, 'title': (content.findtext('title') or '').strip(), 'description': (content.findtext('description') or '').strip(), 'source_name': (obj.findtext('sourceNickName') or '').strip()}
    if content.findtext('type') in ('4', '5'):
        result['kind'] = {'4':'music','5':'video'}[content.findtext('type')]
    return result

def finder_media(obj):
    """Keep Channels metadata and queue only its poster, never an unverified video."""
    from urllib.parse import urlsplit
    feed = obj.find('ContentObject/finderFeed')
    if feed is None:
        return []

    def number(value):
        try:
            result = float(value or 0)
            return int(result) if 0 < result < 10000000 else None
        except (ValueError, OverflowError):
            return None
    rows = []
    for (index, media) in enumerate(feed.findall('mediaList/media'), 1):
        cover = media.findtext('coverUrl') or media.findtext('thumbUrl') or ''
        try:
            parsed = urlsplit(cover)
            if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password:
                cover = None
        except ValueError:
            cover = None
        duration = number(media.findtext('videoPlayDuration'))
        rows.append({'role': f'media-{index}', 'media_type': 'video', 'source_url': None, 'thumbnail_url': cover, 'source_kind': 'wechat_channels', 'source_media_id': feed.findtext('objectId'), 'author_name': feed.findtext('nickname'), 'description': feed.findtext('desc'), 'duration_ms': duration * 1000 if duration else None, 'width': number(media.findtext('width')), 'height': number(media.findtext('height'))})
    return rows

def comment_images(item: ET.Element) -> list[ET.Element]:
    """Normalize comment imageinfo into the same cache contract as post media."""
    images = []
    for image in item.findall('imagelist/imageinfo'):
        media = ET.Element('media')
        for (tag, source, key, token) in (('url', 'url', 'key', 'token'), ('thumb', 'thumb_url', 'thumb_key', 'thumb_url_token')):
            if not image.findtext(source):
                continue
            node = ET.SubElement(media, tag, key=image.findtext(key) or '', token=image.findtext(token) or '')
            node.text = image.findtext(source)
        ET.SubElement(media, 'id').text = image.findtext('media_id')
        ET.SubElement(media, 'type').text = '2'
        ET.SubElement(media, 'size', width=image.findtext('width') or '0', height=image.findtext('height') or '0')
        images.append(media)
    return images

def comment_media(item: ET.Element) -> list[dict[str, Any]]:
    obj = ET.Element('TimelineObject')
    media_list = ET.SubElement(ET.SubElement(obj, 'ContentObject'), 'mediaList')
    media_list.extend(comment_images(item))
    return moments_media(obj)

def live_node(media):
    live = media.find('LivePhoto')
    if live is None:
        return None
    nested = live.find('liveMedia')
    return nested if nested is not None else live


def moments_media(obj: ET.Element) -> list[dict[str, Any]]:
    media_rows: list[dict[str, Any]] = finder_media(obj)
    media_list = obj.find('ContentObject/mediaList')
    if media_list is None:
        return media_rows
    for (index, media) in enumerate(media_list.findall('media'), start=len(media_rows)):
        size = media.find('size')
        media_type = media.findtext('type') or ''
        source_url = media.findtext('url') or ''
        thumbnail_url = media.findtext('thumb') or ''
        if not source_url and (not thumbnail_url):
            continue
        try:
            width = int(size.attrib.get('width', '0')) if size is not None else None
        except ValueError:
            width = None
        try:
            height = int(size.attrib.get('height', '0')) if size is not None else None
        except ValueError:
            height = None
        try:
            total_size = int(size.attrib.get('totalSize', '0')) if size is not None else None
        except ValueError:
            total_size = None
        try:
            duration = float(media.findtext('videoDuration') or '0')
            duration_ms = int(duration * 1000) if duration else None
        except ValueError:
            duration_ms = None
        url_element = media.find('url')
        media_rows.append({'role': f'media-{index + 1}', 'source_media_id': media.findtext('id'), 'source_md5': url_element.attrib.get('md5') if url_element is not None else None, 'media_type': 'video' if media_type in {'6', '15'} else 'image', 'source_url': source_url or None, 'thumbnail_url': thumbnail_url or None, 'width': width, 'height': height, 'byte_length': total_size, 'duration_ms': duration_ms, 'description': media.findtext('description') or None})
        motion = live_node(media)
        if media_rows[-1]['media_type'] == 'image':
            media_rows[-1]['live_motion_url'] = motion.findtext('url') if motion is not None else None
    parent_type = obj.findtext('ContentObject/type')
    if parent_type == '5':
        for item in media_rows:
            if item['media_type'] == 'image':
                item['source_media_type'] = 'video'
                item['source_kind'] = 'wechat_video_share'
    return media_rows

def moments_interactions(root_node: ET.Element, post_id: str) -> list[dict[str, Any]]:
    local = root_node.find('LocalExtraInfo')
    if local is None:
        return []
    rows: list[dict[str, Any]] = []
    owner = root_node.findtext('TimelineObject/username')
    for (list_name, default_type) in (('like_user_list', 'like'), ('comment_user_list', 'comment'), ('with_user_list', 'mention')):
        parent = local.find(list_name)
        if parent is None:
            continue
        for (index, item) in enumerate(parent.findall('user_comment')):
            ref_id = next((v for v in (item.findtext('ref_comment_64id'), item.findtext('ref_comment_id')) if v and v != '0'), '')
            interaction_type = default_type
            if default_type == 'comment' and ref_id not in {'', '0'}:
                interaction_type = 'reply'
            username = item.findtext('username') or ''
            create_time = item.findtext('create_time')
            if default_type == 'mention' and create_time in {None, '', '0'}:
                create_time = root_node.findtext('TimelineObject/createTime')
            rows.append({'interaction_type': interaction_type, 'external_id': f'{post_id}:{default_type}:' + next((v for v in (item.findtext('comment_64id'), item.findtext('comment_id')) if v and v != '0'), f'{username}:{create_time or index}'), 'author_external_id': (owner if default_type == 'mention' else username) or None, 'author_name': (None if default_type == 'mention' else item.findtext('nickname')) or None, 'text': item.findtext('content') or None, **({'media': comment_media(item)} if item.findall('imagelist/imageinfo') else {}), 'occurred_at': iso(create_time), 'create_time': create_time, 'ref_external_id': ref_id or item.findtext('ref_comment_64id') or item.findtext('ref_comment_id') or None, **({'ref_author_external_id': item.findtext('ref_username')} if ref_id else {}), **({'target_external_id': username} if default_type == 'mention' else {}), 'source': list_name, 'is_deleted': item.findtext('b_deleted') == '1'})
    return rows

def moments_record(tid, username, xml, profile_id):
    """The import record for one post, or None without a TimelineObject.

    Raises ET.ParseError for XML that cannot be read even with literal
    character-reference preservation.
    """
    (root, repairs) = parse_moments_xml(xml)
    obj = root.find('TimelineObject')
    if obj is None:
        return None
    external = obj.findtext('id') or str(tid)
    interactions = moments_interactions(root, external)
    location = obj.find('location')
    content = {'text': obj.findtext('contentDesc') or '', 'username': username, 'occurred_at': iso(obj.findtext('createTime')), 'location': dict(location.attrib) if location is not None else None, 'media': moments_media(obj), 'like_count': sum((item['interaction_type'] == 'like' for item in interactions)), 'comment_count': sum((item['interaction_type'] in {'comment', 'reply'} for item in interactions)), 'interaction_count': len(interactions), 'interactions': interactions, 'source_tid': str(tid), 'raw_xml_sha256': hashlib.sha256((xml or '').encode()).hexdigest()}
    link = shared_link(obj)
    if link:
        content['shared_link'] = link
    if repairs:
        content['xml_literal_character_references'] = repairs
    return {'external_id': external, 'profile_id': profile_id, 'entity_type': 'post', 'occurred_at': content['occurred_at'], 'content': content, 'repairs': bool(repairs)}

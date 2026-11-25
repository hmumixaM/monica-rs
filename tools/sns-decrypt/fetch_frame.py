"""Read one explicit SNS slot and stream a private seed/ciphertext frame.

No credentials, frame, or request URL is written to disk or diagnostic output.
This is a one-shot fetch, not a watcher or recurring audit.
"""
import argparse
from contextlib import closing, contextmanager, nullcontext
import hashlib
import json
import re
from pathlib import Path
import sqlite3
import struct
import sys
import xml.etree.ElementTree as ET
from urllib.parse import quote_plus, urlsplit
from urllib.request import Request, HTTPRedirectHandler, build_opener
scripts = str(Path(__file__).resolve().parents[2] / 'scripts')
if scripts not in sys.path:
    sys.path.insert(0, scripts)
from chatlog_moments import live_node, parse_moments_xml


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


@contextmanager
def read_post(source, external, source_tid=None, *, missing_ok=False):
    source = Path(source)
    if source.resolve(strict=True) != source or not external.isdigit():
        raise ValueError('invalid source selection')
    with closing(sqlite3.connect(source.as_uri() + '?mode=ro', uri=True)) as db:
        number = int(source_tid if source_tid is not None else external)
        signed = number - 2 ** 64 if number >= 2 ** 63 else number
        row = db.execute('SELECT content FROM SnsTimeLine WHERE tid IN(?,?)', (external, signed)).fetchone()
        if not row and not missing_ok:
            raise ValueError('source record absent')
        yield source_xml(row[0]) if row else None


def source_xml(value):
    if isinstance(value, bytes):
        if value.startswith(b'\x28\xb5\x2f\xfd'):
            import zstandard
            value = zstandard.ZstdDecompressor().decompress(value, max_output_size=32 * 1024 * 1024)
        value = value.decode('utf-8')
    return (value or '').replace('\0', '')


def post_xml(xml):
    root, _ = parse_moments_xml(xml)
    obj = root if root.tag == 'TimelineObject' else root.find('TimelineObject')
    return root, obj


def xml_hash(xml):
    return hashlib.sha256(xml.encode() if isinstance(xml, str) else xml).hexdigest()


def live_assets(obj):
    finder_count = len(obj.findall('ContentObject/finderFeed/mediaList/media'))
    for slot, media in enumerate(obj.findall('ContentObject/mediaList/media'), 1 + finder_count):
        motion = live_node(media)
        url = motion.find('url') if motion is not None else None
        if url is not None and url.text:
            yield dict(role=f'live-media-{slot}', _slot=slot, media_type='video',
                       source_url=url.text, source_media_id=motion.findtext('id'),
                       source_md5=url.get('md5'), parent_media_id=media.findtext('id'))


def legacy_video_keys(address, token, seeds):
    """Recognize the URL and optional key spellings used by Chatlog's video cache."""
    urls = list(dict.fromkeys((address.strip(), address.strip().replace('http://', 'https://', 1),
                              address.strip().replace('https://', 'http://', 1))))
    if token and 'token=' not in address:
        query_token = 'token=' + quote_plus(token) + '&idx=1'
        for value in tuple(urls):
            path, sep, query = value.partition('?')
            urls.extend((path + '?' + query_token + ('&' + query if sep else ''),
                         value + ('&' if sep else '?') + query_token))
    return (hashlib.md5((url + '|' + key).encode()).hexdigest() + '.bin'
            for url in urls for key in seeds)


def verified_video_cache(path, expected_md5):
    with path.open('rb') as data:
        if data.read(8)[4:] != b'ftyp':
            return False
        data.seek(0)
        digest = hashlib.md5()
        for block in iter(lambda: data.read(1024 * 1024), b''):
            digest.update(block)
        return digest.hexdigest() == expected_md5


def frame(source, external, slot, cache_root=None, source_tid=None, destination=None, thumbnail=False,
          source_media_id=None, source_kind=None, live_motion=False, raw_xml=None):
    if not 1 <= slot <= 100:
        raise ValueError('invalid source selection')
    source = Path(source)
    context = read_post(source, external, source_tid) if raw_xml is None else nullcontext(raw_xml)
    with context as xml:
        root, obj = post_xml(xml)
        if obj is None or obj.findtext('id') != external:
            raise ValueError('source identity mismatch')
        regular = obj.findall('ContentObject/mediaList/media')
        finder = obj.find('ContentObject/finderFeed')
        finder_rows = finder.findall('mediaList/media') if finder is not None else []
        finder_cover = source_kind == 'wechat_channels' and slot <= len(finder_rows)
        parent_media_id = None
        parent = None
        if live_motion:
            index = slot - len(finder_rows) - 1
            if not 0 <= index < len(regular):
                raise ValueError('live photo slot absent')
            parent = regular[index]
            parent_media_id = parent.findtext('id')
            media = live_node(parent)
            if media is None or media.findtext('id') != source_media_id:
                raise ValueError('live photo motion identity mismatch')
        elif finder_cover:
            if finder.findtext('objectId') != source_media_id:
                raise ValueError('source Channels identity mismatch')
            original_cover = finder_rows[slot - 1]
            media = ET.Element('media')
            ET.SubElement(media, 'id').text = source_media_id
            cover = original_cover.find('coverUrl')
            if cover is None or not cover.text:
                cover = original_cover.find('thumbUrl')
            if cover is None or not cover.text:
                raise ValueError('source Channels cover unavailable')
            node = ET.SubElement(media, 'thumb', **cover.attrib)
            node.text = cover.text
        elif source_media_id is not None:
            matches = [m for m in regular if m.findtext('id') == source_media_id]
            if len(matches) == 1:
                media = matches[0]
            else:
                media = regular[slot - len(finder_rows) - 1]
                if media.findtext('id') != source_media_id:
                    raise ValueError('source media identity mismatch')
        else:
            media = regular[slot - 1]
        url, thumb = media.find('url'), media.find('thumb')
        original = media.findtext('url') or media.findtext('thumb') or ''
        def cdn(value):
            host = urlsplit(value).hostname or ''
            return host in ('mmsns.c2c.wechat.com', 'mmsns.hk.wechat.com', 'snsvideo.c2c.wechat.com', 'snsvideo.hk.wechat.com', 'wxapp.tc.qq.com', 'vweixinthumb.tc.qq.com', 'qpic.cn', 'qlogo.cn') or host.endswith(('.qpic.cn', '.qlogo.cn', '.video.qq.com'))
        # Shared articles carry their webpage in <url> and the actual image in
        # <thumb>. Keep the source slot identity; fetch its real cover bytes.
        selected = thumb if thumbnail else url if url is not None and url.text and cdn(url.text) else thumb
        fetch_url = selected.text if selected is not None else ''
        attrs = ({**(thumb.attrib if thumb is not None else {}), **url.attrib}
                 if selected is url and url is not None else
                 selected.attrib if selected is not None else {})
        seed, token = attrs.get('key', ''), attrs.get('token', '')
        enc = media.find('.//enc')
        if enc is None:
            enc = root.find('.//enc')
        if (not seed or seed == '0') and enc is not None:
            seed = enc.get('key', '')
        native_video = live_motion or media.findtext('type') in ('6', '15')
        video = native_video and not thumbnail
        if video and destination is None:
            raise ValueError('video requires a streaming destination')
        if video and selected is not url:
            raise ValueError('native video source unavailable')
        fetch = (fetch_url or '') if native_video or finder_cover else (fetch_url or '').replace('http://', 'https://', 1)
        if not video:
            fetch = fetch.replace('/150?', '/0?')
            if fetch.endswith('/150'):
                fetch = fetch[:-4] + '/0'
        if token and 'token=' not in fetch:
            query_token = 'token=' + quote_plus(token) + '&idx=1'
            if video:
                prefix, sep, query = fetch.partition('?')
                fetch = prefix + '?' + query_token + ('&' + query if sep else '')
            else:
                fetch += ('&' if '?' in fetch else '?') + query_token
        parsed = urlsplit(fetch)
        if (parsed.scheme not in (('http', 'https') if native_video or finder_cover else ('https',)) or not parsed.hostname or
                not cdn(fetch) or
                parsed.username or parsed.password or parsed.port not in (None, 80 if parsed.scheme == 'http' else 443)):
            raise ValueError('source is outside WeChat media CDN')
        # Existing cache identity is exactly the pinned Chatlog URL|seed rule.
        cache = Path(cache_root) if cache_root is not None else source.parent.parent.parent / '.chatlog_sns_cache'
        # Monica Next also cached the tokenized original HTTP address. Keep
        # that exact filename identity while network downloads use HTTPS.
        next_url = (fetch_url or '').strip().replace('/150?', '/0?')
        if next_url.endswith('/150'):
            next_url = next_url[:-4] + '/0'
        if token and 'token=' not in next_url:
            next_url += ('&' if '?' in next_url else '?') + 'token=' + quote_plus(token) + '&idx=1'
        cache_urls = dict.fromkeys([fetch, next_url, fetch_url or ''])
        cache_seeds = ['', seed] if seed == '0' else [seed]
        filenames = [hashlib.md5((u + '|' + key).encode()).hexdigest() + '.bin'
                     for u in cache_urls for key in cache_seeds]
        expected_md5 = (url.get('md5') or '').lower() if url is not None else ''
        source_digest = expected_md5 if video and re.fullmatch(r'[a-f0-9]{32}', expected_md5) else None
        if source_digest:
            enc_nodes = (media.find('.//enc'), parent.find('.//enc') if parent is not None else None)
            seeds = list(dict.fromkeys([seed, attrs.get('key', ''), '',
                                       '0', *[node.get('key', '') for node in enc_nodes if node is not None]]))
            filenames.extend(legacy_video_keys(fetch_url, token, seeds))
        limit = 512 * 1024 * 1024 if video else 32 * 1024 * 1024
        candidate = None
        for filename in dict.fromkeys(filenames):
            path = cache / filename
            if path.is_symlink() or not path.is_file():
                continue
            if not 0 < path.stat().st_size <= limit:
                continue
            if source_digest and not verified_video_cache(path, source_digest):
                continue
            candidate = path
            break
        cached = candidate is not None
        if candidate is not None:
            if candidate.stat().st_size > limit:
                raise ValueError('cached object too large')
            stream = candidate.open('rb')
        else:
            request = Request(fetch, headers={'User-Agent': 'MicroMessenger Client', 'Accept': '*/*'})
            stream = build_opener(NoRedirect()).open(request, timeout=20)
        digest = hashlib.sha256()
        original_digest = hashlib.md5() if source_digest else None
        total = 0
        data = bytearray() if destination is None else None
        with stream:
            output = Path(destination).open('wb') if destination is not None else None
            try:
                for block in iter(lambda: stream.read(1024 * 1024), b''):
                    total += len(block)
                    if total > limit:raise ValueError('object too large')
                    digest.update(block)
                    if original_digest is not None:
                        original_digest.update(block)
                    if output is not None:output.write(block)
                    else:data.extend(block)
            finally:
                if output is not None:output.close()
        if total == 0:raise ValueError('empty media object')
        return dict(seed=seed, size=total, external_id=external, slot=slot,
                    source_url=(media.findtext('thumb') if thumbnail else original).replace('http://', 'https://', 1),
                    source_md5=url.attrib.get('md5') if url is not None else None,
                    source_media_id=media.findtext('id'),
                    raw_xml_sha256=xml_hash(xml), parent_media_id=parent_media_id,
                    downloaded_sha256=digest.hexdigest(), cached=cached,
                    downloaded_md5=original_digest.hexdigest() if original_digest is not None else None,
                    verified_cached_video=bool(cached and source_digest)), bytes(data) if data is not None else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--external', required=True)
    parser.add_argument('--slot', type=int, required=True)
    args = parser.parse_args()
    header, data = frame(args.source, args.external, args.slot)
    encoded = json.dumps(header).encode()
    if len(encoded) > 4096:
        raise ValueError('frame metadata too large')
    sys.stdout.buffer.write(struct.pack('>I', len(encoded)) + encoded + data)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        # HTTP exceptions and XML can contain per-image keys. Never stringify.
        sys.stderr.write('sns_source_fetch_failed\n')
        raise SystemExit(1)

"""Read one explicit SNS slot and stream a private seed/ciphertext frame.

No credentials, frame, or request URL is written to disk or diagnostic output.
This is a one-shot fetch, not a watcher or recurring audit.
"""
import argparse
from contextlib import closing
import hashlib
import json
from pathlib import Path
import sqlite3
import struct
import sys
from urllib.parse import quote, quote_plus, urlsplit
from urllib.request import Request, HTTPRedirectHandler, build_opener
import xml.etree.ElementTree as ET


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def frame(source, external, slot, cache_root=None, source_tid=None):
    source = Path(source)
    if source.resolve(strict=True) != source or not external.isdigit() or not 1 <= slot <= 100:
        raise ValueError('invalid source selection')
    with closing(sqlite3.connect('file:' + quote(str(source)) + '?mode=ro', uri=True)) as db:
        db.execute('PRAGMA query_only=ON')
        db.execute('BEGIN')
        number = int(source_tid if source_tid is not None else external)
        signed = number - 2 ** 64 if number >= 2 ** 63 else number
        row = db.execute('SELECT content FROM SnsTimeLine WHERE tid IN(?,?)', (external, signed)).fetchone()
        if not row:
            raise ValueError('source record absent')
        root = ET.fromstring(row[0])
        obj = root if root.tag == 'TimelineObject' else root.find('TimelineObject')
        if obj is None or obj.findtext('id') != external:
            raise ValueError('source identity mismatch')
        media = obj.findall('ContentObject/mediaList/media')[slot - 1]
        url, thumb = media.find('url'), media.find('thumb')
        original = media.findtext('url') or media.findtext('thumb') or ''
        def cdn(value):
            host = urlsplit(value).hostname or ''
            return host == 'mmsns.c2c.wechat.com' or host == 'qpic.cn' or host.endswith('.qpic.cn')
        # Shared articles carry their webpage in <url> and the actual image in
        # <thumb>. Keep the source slot identity; fetch its real cover bytes.
        selected = url if url is not None and url.text and cdn(url.text) else thumb
        fetch_url = selected.text if selected is not None else ''
        attrs = ({**(thumb.attrib if thumb is not None else {}), **url.attrib}
                 if selected is url and url is not None else
                 selected.attrib if selected is not None else {})
        seed, token = attrs.get('key', ''), attrs.get('token', '')
        enc = root.find('.//enc')
        if (not seed or seed == '0') and enc is not None:
            seed = enc.get('key', '')
        if media.findtext('type') in ('6', '15'):
            raise ValueError('video requires its own codec')
        fetch = (fetch_url or '').replace('http://', 'https://', 1).replace('/150?', '/0?')
        if fetch.endswith('/150'):
            fetch = fetch[:-4] + '/0'
        if token and 'token=' not in fetch:
            fetch += ('&' if '?' in fetch else '?') + 'token=' + quote_plus(token) + '&idx=1'
        parsed = urlsplit(fetch)
        if (parsed.scheme != 'https' or not parsed.hostname or
                not cdn(fetch) or
                parsed.username or parsed.password or parsed.port not in (None, 443)):
            raise ValueError('source is outside WeChat media CDN')
        # Existing cache identity is exactly the pinned Chatlog URL|seed rule.
        cache = Path(cache_root) if cache_root is not None else source.parent.parent.parent / '.chatlog_sns_cache'
        candidate = cache / (hashlib.md5((fetch + '|' + seed).encode()).hexdigest() + '.bin')
        if candidate.is_file() and not candidate.is_symlink():
            if candidate.stat().st_size > 32 * 1024 * 1024:
                raise ValueError('cached object too large')
            data = candidate.read_bytes()
            cached = True
        else:
            request = Request(fetch, headers={'User-Agent': 'MicroMessenger Client', 'Accept': '*/*'})
            with build_opener(NoRedirect()).open(request, timeout=20) as response:
                data = response.read(32 * 1024 * 1024 + 1)
            cached = False
        if not 0 < len(data) <= 32 * 1024 * 1024:
            raise ValueError('invalid object size')
        return dict(seed=seed, size=len(data), external_id=external, slot=slot,
                    source_url=original.replace('http://', 'https://', 1),
                    source_md5=url.attrib.get('md5') if url is not None else None,
                    source_media_id=media.findtext('id'),
                    raw_xml_sha256=hashlib.sha256(row[0].encode() if isinstance(row[0], str) else row[0]).hexdigest(),
                    downloaded_sha256=hashlib.sha256(data).hexdigest(), cached=cached), data


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

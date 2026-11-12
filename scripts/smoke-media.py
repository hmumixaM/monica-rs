"""Real subprocess decoder and transfer tests. Only generated PNG/text bytes."""
import hashlib
import json
import struct
import urllib.error
import urllib.request
import zlib


def run(base, opener, csrf):
    def request(path, body=None, method=None, status=200, headers=None):
        h = {'Origin': base, 'X-CSRF-Token': csrf}
        h.update(headers or {})
        if isinstance(body, dict):
            body = json.dumps(body).encode()
            h['Content-Type'] = 'application/json'
        req = urllib.request.Request(base + path, data=body, method=method, headers=h)
        try:
            response = opener.open(req, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            data = response.read()
            assert response.status == status, (path, response.status, status, data[:300])
            return response.headers, data

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 16, 8, 8, 2, 0, 0, 0))
    png += chunk(b'IDAT', zlib.compress((b'\0' + bytes([70, 110, 190]) * 16) * 8)) + chunk(b'IEND', b'')

    def upload(content, name, status=201):
        boundary = 'synthetic-media-contract'
        body = (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{name}"\r\n'
                'Content-Type: application/octet-stream\r\n\r\n').encode() + content + f'\r\n--{boundary}--\r\n'.encode()
        _, response = request('/api/v1/media', body, status=status,
                              headers={'Content-Type': f'multipart/form-data; boundary={boundary}'})
        return json.loads(response)

    asset = upload(png, 'synthetic.png')
    assert asset['width'] == 16 and asset['height'] == 8 and asset['media_type'] == 'image/png'
    assert upload(png, 'duplicate.png')['id'] == asset['id']
    upload(png[:45], 'truncated.png', 422)
    upload(b'not a valid photograph', 'fake.jpeg', 422)
    bomb = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 16000, 16000, 8, 6, 0, 0, 0))
    upload(bomb, 'oversized.png', 422)
    path = '/api/v1/media/' + asset['id']
    headers, body = request(path)
    assert body == png
    etag = headers['ETag']
    assert request(path, headers={'Range': 'bytes=4-9', 'If-Range': etag}, status=206)[1] == png[4:10]
    assert request(path, headers={'Range': 'bytes=4-9', 'If-Range': '"stale"'})[1] == png
    assert request(path, headers={'Range': 'bytes=-7'}, status=206)[1] == png[-7:]
    request(path, headers={'Range': 'bytes=999999-'}, status=416)
    request(path, headers={'If-None-Match': etag}, status=304)
    preview_headers, preview = request(path + '/preview')
    assert preview[:4] == b'RIFF' and preview[8:12] == b'WEBP'
    request(path + '/preview', headers={'If-None-Match': preview_headers['ETag']}, status=304)
    request(path + '/ocr', status=404)
    assert json.loads(request('/api/v1/faces/media/' + asset['id'] + '/reading')[1]) == {'readings': []}
    _, body = request('/api/v1/imports/media/initiate', {
        'expected_sha256': hashlib.sha256(png).hexdigest(), 'expected_byte_length': len(png),
        'media_type': 'text/plain', 'file_name': 'resumable.png'}, status=201)
    upload_id = json.loads(body)['upload_id']
    path = '/api/v1/imports/media/' + upload_id
    assert json.loads(request(path + '/chunks/0', png[:30], 'PUT')[1])['next_chunk'] == 1
    request(path + '/chunks/0', png[:30], 'PUT')
    request(path + '/chunks/0', b'different', 'PUT', 409)
    request(path + '/complete', b'', 'POST', 422)
    request(path + '/chunks/1', png[30:], 'PUT')
    complete = json.loads(request(path + '/complete', b'', 'POST', 201)[1])
    assert complete['id'] == asset['id'] and complete['media_type'] == 'image/png'
    assert json.loads(request(path + '/complete', b'', 'POST', 201)[1])['id'] == asset['id']
    assert json.loads(request(path)[1])['status'] == 'complete'
    print('Media smoke passed: real isolated PNG decode, limits, preview, dedupe, Range/If-Range, resumable replay')

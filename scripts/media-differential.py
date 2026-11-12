#!/usr/bin/env python3
"""Real pinned Python/Rust media protocol parity on new synthetic databases.

Compares stable public fields and bytes. Storage paths, generated IDs/timestamps,
additional Rust dimensions, stricter validation text and replay improvements are
not equalized into fabricated parity; their boundaries are tested separately.
"""
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import time
import urllib.error
import urllib.request
import zlib
from reference_backend import PG, ReferenceBackend


def scenario(base):
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    csrf = ''
    observations = []

    def request(path, body=None, method=None, expected=200, headers=None):
        h = {'Origin': base, 'X-CSRF-Token': csrf, **(headers or {})}
        if isinstance(body, dict):
            body = json.dumps(body).encode()
            h['Content-Type'] = 'application/json'
        req = urllib.request.Request(base + path, data=body, method=method, headers=h)
        try:
            response = opener.open(req, timeout=20)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            data = response.read()
            assert response.status == expected, (path, response.status, expected, data[:500])
            return response.headers, data

    request('/api/v1/auth/bootstrap', {'email': 'fixture@example.invalid', 'password': 'synthetic-password-only'})
    csrf = json.loads(request('/api/v1/auth/csrf')[1])['csrf_token']
    request('/api/v1/auth/login', {'email': 'fixture@example.invalid', 'password': 'synthetic-password-only'})
    def chunk(kind, data):
        return struct.pack('>I',len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind+data))
    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR',struct.pack('>IIBBBBB',16,8,8,2,0,0,0))
    png += chunk(b'IDAT',zlib.compress((b'\0'+bytes([70,110,190])*16)*8)) + chunk(b'IEND',b'')
    def upload(data, name, expected=201):
        boundary = 'synthetic-media-contract'
        body = (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{name}"\r\nContent-Type: application/octet-stream\r\n\r\n').encode()+data+f'\r\n--{boundary}--\r\n'.encode()
        return json.loads(request('/api/v1/media',body,expected=expected,headers={'Content-Type':f'multipart/form-data; boundary={boundary}'})[1])
    asset = upload(png,'synthetic.png')
    observations.append({key:asset[key] for key in ('sha256','media_type','byte_length','file_name')})
    observations.append({'deduplicated':upload(png,'synthetic.png')['id']==asset['id']})
    upload(b'not a photograph','fake.jpeg',422)
    observations.append({'invalid_image':422})
    path = '/api/v1/media/'+asset['id']
    for headers, expected in [({},200),({'Range':'bytes=4-9'},206),({'Range':'bytes=-7'},206),({'Range':'bytes=999999-'},416)]:
        h, body = request(path,headers=headers,expected=expected)
        observations.append({'status':expected,'bytes':body.hex() if expected!=416 else None,
                             'headers':{k:h.get(k) for k in ('ETag','Content-Range','Cache-Control','X-Content-Type-Options')} if expected!=416 else {}})
    request(path,headers={'If-None-Match':'"'+hashlib.sha256(png).hexdigest()[:32]+'"'},expected=304)
    observations.append({'conditional':304})
    _, missing = request(path+'/ocr',expected=404)
    observations.append(json.loads(missing))
    observations.append(json.loads(request('/api/v1/faces/media/'+asset['id']+'/reading')[1]))
    init = json.loads(request('/api/v1/imports/media/initiate',{'expected_sha256':hashlib.sha256(png).hexdigest(),'expected_byte_length':len(png),'media_type':'image/png'},expected=201)[1])
    path = '/api/v1/imports/media/'+init['upload_id']
    observations.append({'next_chunk':init['next_chunk']})
    for number, data in enumerate((png[:30],png[30:])):
        result=json.loads(request(path+f'/chunks/{number}',data,'PUT',headers={'Content-Type':'application/octet-stream'})[1])
        observations.append({key:result[key] for key in ('next_chunk','received_bytes')})
    status=json.loads(request(path)[1])
    observations.append({key:status[key] for key in ('expected_sha256','media_type','expected_byte_length','next_chunk','received_bytes','status','completed_at')})
    result=json.loads(request(path+'/complete',b'','POST',201)[1])
    observations.append({key:result[key] for key in ('sha256','media_type','byte_length')})
    return observations


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline',choices=('crm','desktop'),default='crm')
    args=parser.parse_args()
    with ReferenceBackend(args.baseline) as reference:
        expected=scenario(reference.url)
        subprocess.run([str(PG/'createdb'),'-h',str(reference.root),'rust'],check=True)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
        env={'PATH':os.defpath,'MONICA_DATABASE_URL':f'postgresql:///rust?host={reference.root}','MONICA_SESSION_SECRET':'synthetic-reference-session-secret-32bytes','MONICA_MEDIA_ROOT':str(reference.root/'rust-media')}
        binary=str(Path('target/debug/monica-server').resolve())
        subprocess.run([binary,'migrate'],env=env,check=True)
        with (reference.root/'rust.log').open('wb') as log:
            rust=subprocess.Popen([binary,'serve','--listen',f'127.0.0.1:{port}'],env=env,stdout=log,stderr=log)
            try:
                url=f'http://127.0.0.1:{port}'
                for _ in range(200):
                    if rust.poll() is not None:raise RuntimeError((reference.root/'rust.log').read_text())
                    try:urllib.request.urlopen(url+'/health',timeout=1).close();break
                    except OSError:time.sleep(.05)
                actual=scenario(url)
                assert actual==expected,json.dumps({'baseline':expected,'rust':actual},indent=2)
                print(f'Real {args.baseline}/Rust media differential passed: {len(actual)} observations')
            finally:
                rust.send_signal(signal.SIGTERM)
                try:rust.wait(timeout=100)
                except subprocess.TimeoutExpired:rust.kill();rust.wait(timeout=5)

if __name__=='__main__':main()

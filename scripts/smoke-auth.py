"""Black-box auth regression against a freshly bootstrapped synthetic instance."""
import base64
import hashlib
import hmac
import http.cookiejar
import json
import struct
import time
import urllib.error
import urllib.request


def run(base):
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    csrf = None

    def request(path, payload=None, status=200, extra=None, method=None):
        headers = {'Content-Type': 'application/json', 'Origin': base}
        if csrf:
            headers['X-CSRF-Token'] = csrf
        headers.update(extra or {})
        req = urllib.request.Request(base + path, data=None if payload is None else json.dumps(payload).encode(), headers=headers, method=method)
        try:
            response = opener.open(req, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            assert response.status == status, (path, response.status, status)
            return None if response.status == 204 else json.load(response)

    credentials = {'email': 'fixture@example.invalid', 'password': 'synthetic-password-only'}
    request('/api/v1/me', status=401)
    request('/api/v1/auth/bootstrap', credentials)
    request('/api/v1/auth/bootstrap', credentials, status=409)
    csrf = request('/api/v1/auth/csrf')['csrf_token']
    request('/api/v1/auth/login', credentials)
    assert request('/api/v1/me')['email'] == credentials['email']
    profile = request('/api/v1/profiles', {'display_name': '测试联系人', 'summary': 'Synthetic'}, status=201)
    profile_id = profile['id']
    assert request('/api/v1/profiles?q=Synthetic')[0]['id'] == profile_id
    field_path = f'/api/v1/profiles/{profile_id}/fields'
    original = request(field_path, {'field_key': 'display_name', 'value': 'A'}, method='PATCH')
    request(field_path, {'field_key': 'display_name', 'value': 'B'}, method='PATCH')
    request(field_path, {'field_key': 'display_name', 'value': 'A'}, method='PATCH')
    request(field_path, {'field_key': 'display_name', 'value': 'A'}, method='PATCH')
    assert len(request(f'/api/v1/profiles/{profile_id}/history')) == 3
    assert request('/api/v1/profiles?q=A')[0]['display_name'] == 'A'
    request(f'/api/v1/profiles/{profile_id}/history/{original["id"]}/restore', {})
    request(field_path, {'field_key': 'display_name', 'value': ''}, status=422, method='PATCH')
    tag = request('/api/v1/tags', {'name': 'test-tag'}, status=201)
    assert request('/api/v1/tags', {'name': 'test-tag'}, status=201)['id'] == tag['id']
    tag_path = f'/api/v1/profiles/{profile_id}/tags/{tag["id"]}'
    request(tag_path, {})
    assert request('/api/v1/tags')[0]['people'] == 1
    assert request(f'/api/v1/profiles?tag={tag["id"]}')[0]['id'] == profile_id
    request(tag_path, {}, status=204, method='DELETE')
    assert request('/api/v1/tags')[0]['people'] == 0
    request(tag_path, {}, status=404, method='DELETE')
    request(tag_path, {})
    assert request('/api/v1/tags')[0]['people'] == 1
    request('/api/v1/me', status=403, extra={'Host': 'attacker.invalid'})
    request('/api/v1/auth/logout', {}, status=403, extra={'X-CSRF-Token': 'wrong'})
    request('/api/v1/auth/logout', {}, status=403, extra={'Origin': 'https://attacker.invalid'})
    secret = request('/api/v1/auth/totp/setup', {})['secret']
    request('/api/v1/auth/totp/verify', {'code': 'invalid'}, status=422)

    def otp():
        raw = base64.b32decode(secret)
        digest = hmac.new(raw, struct.pack('>Q', int(time.time()) // 30), hashlib.sha1).digest()
        offset = digest[-1] & 15
        return f'{(struct.unpack(">I", digest[offset:offset + 4])[0] & 0x7fffffff) % 1000000:06d}'

    codes = request('/api/v1/auth/totp/verify', {'code': otp()})['recovery_codes']
    assert len(codes) == len(set(codes)) == 8
    assert request('/api/v1/auth/totp/status')['enabled'] is True
    request('/api/v1/auth/totp/setup', {}, status=409)
    request('/api/v1/auth/logout', {})
    request('/api/v1/me', status=401)
    request('/api/v1/auth/login', credentials, status=401)
    request('/api/v1/auth/login', dict(credentials, recovery_code=codes[0]))
    request('/api/v1/auth/logout', {})
    request('/api/v1/auth/login', dict(credentials, recovery_code=codes[0]), status=401)
    request('/api/v1/auth/login', dict(credentials, totp_code=otp()))
    sessions = request('/api/v1/auth/sessions')
    current = next(s['id'] for s in sessions if s['current'])
    assert request('/api/v1/audit/events')
    request(f'/api/v1/auth/sessions/{current}/revoke', {})
    request('/api/v1/me', status=401)
    for _ in range(5):
        request('/api/v1/auth/login', dict(credentials, password='wrong-password-value'), status=401)
    request('/api/v1/auth/login', credentials, status=429)
    print('Auth smoke passed: bootstrap, session, CSRF, Host/Origin, TOTP, recovery, revocation, throttling')

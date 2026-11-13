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
    nested = {'z': 'a\x00b', 'a': [1.0, 1e-7, '合成']}
    safe_nested = {'a': [1.0, 1e-7, '合成'], 'z': 'ab'}
    first = request(field_path, {'field_key': 'synthetic.nested', 'value': nested}, method='PATCH')
    expected_hash = hashlib.sha256(json.dumps(
        safe_nested, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    assert first['content_hash'] == expected_hash
    assert first['value'] is None
    assert request(f'/api/v1/profiles/{profile_id}/history?field_key=synthetic.nested')[0]['value'] == safe_nested
    unchanged = request(field_path, {'field_key': 'synthetic.nested', 'value': safe_nested}, method='PATCH')
    assert unchanged['id'] == first['id']
    request(field_path, {'field_key': 'synthetic.nested', 'value': 'B'}, method='PATCH')
    reverted = request(field_path, {'field_key': 'synthetic.nested', 'value': safe_nested}, method='PATCH')
    assert reverted['id'] != first['id']
    assert reverted['content_hash'] == first['content_hash']
    string_zero = request(field_path, {'field_key': 'synthetic.scalar', 'value': '0'}, method='PATCH')
    number_zero = request(field_path, {'field_key': 'synthetic.scalar', 'value': 0}, method='PATCH')
    assert string_zero['content_hash'] == hashlib.sha256(b'0').hexdigest()
    assert number_zero['content_hash'] == hashlib.sha256(b'json\x000').hexdigest()
    assert request(field_path, {'field_key': 'synthetic.scalar', 'value': 0}, method='PATCH')['id'] == number_zero['id']
    restored_zero = request(field_path, {'field_key': 'synthetic.scalar', 'value': '0'}, method='PATCH')
    assert restored_zero['content_hash'] == string_zero['content_hash'] and restored_zero['id'] != string_zero['id']
    number_first = request(field_path, {'field_key': 'synthetic.reverse', 'value': 42}, method='PATCH')
    string_second = request(field_path, {'field_key': 'synthetic.reverse', 'value': '42'}, method='PATCH')
    assert number_first['content_hash'] == hashlib.sha256(b'42').hexdigest()
    assert string_second['content_hash'] == hashlib.sha256(b'json\x00"42"').hexdigest()
    details_path = f'/api/v1/profiles/{profile_id}/details'
    request(details_path, {'field_key': ' City ', 'value': ' Synthetic Town '}, method='PUT')
    before = request(f'/api/v1/profiles/{profile_id}/history')
    request(details_path, {'field_key': 'Renamed City', 'value': 'New Town', 'replaces': [
        {'field_key': 'City'}, {'field_key': 'Missing'}]}, method='PUT', status=404)
    assert request(f'/api/v1/profiles/{profile_id}/history') == before, 'failed rename must roll back every revision'
    request(details_path, {'field_key': 'Renamed City', 'value': 'New Town',
                          'replaces': [{'field_key': 'City'}]}, method='PUT')
    request(details_path, {'field_key': 'display_name', 'value': 'Wrong editor'}, method='PUT', status=422)
    request(field_path, {'field_key': 'monica.custom_fields', 'value': {'Keep': 'A', 'Drop': 'B'}}, method='PATCH')
    request(details_path, {'refs': [{'field_key': 'monica.custom_fields', 'item': 'Drop'}]}, method='DELETE', status=204)
    latest = request(f'/api/v1/profiles/{profile_id}/history?field_key=monica.custom_fields')[0]
    assert latest['value'] == {'Keep': 'A'}
    request(details_path, {'refs': [{'field_key': 'monica.custom_fields', 'item': 'Keep'}]}, method='DELETE', status=204)
    assert request(f'/api/v1/profiles/{profile_id}/history?field_key=monica.custom_fields')[0]['value'] is None
    request(field_path, {'field_key': 'linkedin.experience', 'value': [
        {'company_name': 'SyntheticCompany', 'title': 'Engineer', 'start_date': '2020-01-01'}]}, method='PATCH')
    request(field_path, {'field_key': 'linkedin.education', 'value': [
        {'school_name': 'SyntheticSchool', 'degree': 'Synthetic degree'}]}, method='PATCH')
    assert request('/api/v1/profiles?company=SyntheticCompany') == []
    assert request('/api/v1/profiles?school=SyntheticSchool') == []
    assert request('/api/v1/profiles?company=UnrelatedCompany') == []
    detail = request(f'/api/v1/profiles/{profile_id}')
    assert detail['id'] == profile_id
    assert detail['structured_facts'] == []  # Generic manual fields are not typed source facts.
    assert detail['presentation']['facts'] == []
    compact = request(f'/api/v1/profiles/{profile_id}/summary')
    assert compact['profile']['id'] == profile_id and 'fields' not in compact['profile']
    assert compact['has_posts'] is False and compact['overview']['messages'] is None
    assert {f['field_key']: f['value'] for f in detail['fields']}['Renamed City'] == 'New Town'
    assert request(f'/api/v1/profiles/{profile_id}/structured-history') == []
    dates_path = f'/api/v1/profiles/{profile_id}/dates'
    leap = {'kind': 'birthday', 'precision': 'month_day', 'month': 2, 'day': 29, 'label': 'ignored'}
    birthday = request(dates_path, leap, status=201)
    assert birthday['label'] is None and birthday['next']
    assert request(dates_path, leap, status=201)['id'] == birthday['id']
    request(dates_path, {'kind': 'birthday', 'precision': 'exact', 'year': 2000, 'month': 2, 'day': 29}, status=409)
    request(dates_path, {'kind': 'other', 'precision': 'exact', 'year': 1900, 'month': 2, 'day': 29}, status=422)
    updated = request(dates_path + '/' + birthday['id'], {
        'kind': 'birthday', 'precision': 'exact', 'year': 2000, 'month': 2, 'day': 29}, method='PUT')
    assert updated['years_exact'] is True
    assert request(f'/api/v1/profiles/{profile_id}')['birthday']['id'] == birthday['id']
    peer = request('/api/v1/profiles', {'display_name': 'SyntheticPeer'}, status=201)
    group = request('/api/v1/profiles', {'display_name': 'SyntheticGroup', 'profile_type': 'group'}, status=201)
    request(f'/api/v1/profiles/{group["id"]}/dates', leap, status=422)
    edge_path = f'/api/v1/profiles/{profile_id}/relationships'
    edge_data = {'to_profile_id': peer['id'], 'relationship_type': 'friend', 'note': 'Synthetic'}
    edge = request(edge_path, edge_data, status=201)
    assert request(edge_path, edge_data, status=201)['id'] == edge['id']
    assert request(f'/api/v1/profiles/{peer["id"]}/relationships')[0]['related_profile_id'] == profile_id
    request(edge_path, {'to_profile_id': profile_id, 'relationship_type': 'friend'}, status=422)
    members_path = f'/api/v1/groups/{group["id"]}/members'
    member = {'person_profile_id': profile_id, 'role': 'owner'}
    request(members_path, member, status=201)
    assert request(members_path)[0]['person_profile_id'] == profile_id
    assert request(f'/api/v1/profiles/{group["id"]}')['group_owners'][0]['person_profile_id'] == profile_id
    assert request(f'/api/v1/profiles/{profile_id}')['linked_group_count'] == 1
    request(members_path, {**member, 'left_at': '2026-01-01T00:00:00Z'}, status=201)
    assert request(members_path) == []
    assert request(f'/api/v1/profiles/{profile_id}')['linked_group_count'] == 0
    request(dates_path + '/' + birthday['id'], {}, method='DELETE', status=204)
    assert request(f'/api/v1/profiles/{profile_id}')['birthday'] is None
    tag = request('/api/v1/tags', {'name': 'test-tag'}, status=201)
    assert request('/api/v1/tags', {'name': 'test-tag'}, status=201)['id'] == tag['id']
    tag_path = f'/api/v1/profiles/{profile_id}/tags/{tag["id"]}'
    request(tag_path, {})
    assert request('/api/v1/tags')[0]['people'] == 1
    content = b'Synthetic media body\n' * 5000
    boundary = 'monica-synthetic-boundary'
    body = (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="fixture.txt"\r\nContent-Type: text/plain\r\n\r\n'.encode()
            + content + f'\r\n--{boundary}--\r\n'.encode())

    def upload():
        req = urllib.request.Request(base + '/api/v1/media', data=body, headers={
            'Content-Type': f'multipart/form-data; boundary={boundary}', 'X-CSRF-Token': csrf, 'Origin': base})
        with opener.open(req, timeout=10) as response:
            assert response.status == 201
            return json.load(response)

    media = upload()
    assert media['sha256'] == hashlib.sha256(content).hexdigest()
    assert upload()['id'] == media['id']
    media_path = base + '/api/v1/media/' + media['id']
    with opener.open(media_path) as response:
        assert response.read() == content
        assert response.headers['Content-Disposition'].startswith('attachment;')
        etag = response.headers['ETag']
    with opener.open(urllib.request.Request(media_path, headers={'Range': 'bytes=0-3'})) as response:
        assert response.status == 206
        assert response.read() == content[:4]
    try:
        opener.open(urllib.request.Request(media_path, headers={'If-None-Match': etag}))
        raise AssertionError('expected 304')
    except urllib.error.HTTPError as response:
        assert response.code == 304
        response.close()
    photos = f'/api/v1/profiles/{profile_id}/photos'
    request(photos, {'media_ids': [media['id']]}, status=201)
    assert request(photos)[0]['media_id'] == media['id']
    request(photos + '/' + media['id'], {}, status=204, method='DELETE')
    assert request(photos) == []
    activity_data = {'activity_type': 'note', 'occurred_at': '2026-01-01T12:00:00Z',
                     'title': 'Synthetic note', 'body': '**A synthetic note**',
                     'participant_profile_ids': [profile_id, peer['id']], 'media_ids': [media['id']]}
    activity = request('/api/v1/activities', activity_data, status=201)
    activity_path = '/api/v1/activities/' + activity['id']
    notes = request('/api/v1/activities?profile_id=' + profile_id)
    assert notes[0]['id'] == activity['id'] and notes[0]['media'][0]['id'] == media['id']
    assert len(notes[0]['participants']) == 2
    request(activity_path, {**activity_data, 'participant_profile_ids': [
        '00000000-0000-0000-0000-000000000099']}, method='PUT', status=404)
    assert request('/api/v1/activities?profile_id=' + profile_id)[0]['title'] == 'Synthetic note'
    task_data = {**activity_data, 'activity_type': 'task', 'title': 'Synthetic task', 'media_ids': []}
    request(activity_path, task_data, method='PUT')
    request(activity_path, task_data, method='PUT')
    request(activity_path + '/state', {'state': 'done'}, method='PATCH')
    request(activity_path + '/state', {'state': 'done'}, method='PATCH')
    tasks = request('/api/v1/activities?profile_id=' + profile_id)
    assert tasks[0]['state'] == 'done' and tasks[0]['media'] == []
    manual_feed = request('/api/v1/timeline?provider=manual')
    assert sum(item['id'] == activity['id'] for item in manual_feed) == 1
    assert request(f'/api/v1/profiles?tag={tag["id"]}')[0]['id'] == profile_id
    request(tag_path, {}, status=204, method='DELETE')
    assert request('/api/v1/tags')[0]['people'] == 0
    request(tag_path, {}, status=404, method='DELETE')
    request(tag_path, {})
    assert request('/api/v1/tags')[0]['people'] == 1
    request('/api/v1/me', status=403, extra={'Host': 'attacker.invalid'})
    request('/api/v1/auth/logout', {}, status=403, extra={'X-CSRF-Token': 'wrong'})
    request('/api/v1/auth/logout', {}, status=403, extra={'Origin': 'https://attacker.invalid'})
    import runpy
    runpy.run_path('scripts/smoke-media.py')['run'](base, opener, csrf)
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


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description='Exercise a fresh isolated Monica HTTP instance.')
    parser.add_argument('base_url')
    run(parser.parse_args().base_url.rstrip('/'))

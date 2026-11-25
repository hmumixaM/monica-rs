#!/usr/bin/env python3
"""Drive the packaged daemon, scheduler, Chatlog and media store with private fixtures."""
from contextlib import closing, contextmanager
import hashlib
from http.cookiejar import CookieJar
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
from urllib.request import HTTPCookieProcessor, ProxyHandler, Request, build_opener

import psycopg


def request(client, url, endpoint, body=None, token=None):
    headers = {} if endpoint.startswith('/internal/') else {'Origin': url}
    if body is not None:
        headers['Content-Type'] = 'application/json'
        if 'Origin' in headers:
            headers['X-CSRF-Token'] = request(client, url, '/api/v1/auth/csrf')['csrf_token']
    if token:
        headers['Authorization'] = 'Bearer ' + token
    encoded = json.dumps(body).encode() if body is not None else None
    with client.open(Request(url + endpoint, data=encoded, headers=headers), timeout=3) as response:
        return json.load(response)


def wait(process, action, label, timeout=65):
    stopped_at = time.monotonic() + timeout
    while process.poll() is None and time.monotonic() < stopped_at:
        try:
            if result := action():
                return result
        except (OSError, psycopg.OperationalError):
            pass
        time.sleep(0.25)
    raise RuntimeError(f'{label} did not complete')


@contextmanager
def launch(app, home):
    env = {key: value for key, value in os.environ.items() if not key.startswith('MONICA_')}
    env['HOME'] = str(home)
    with (home/'app.log').open('ab') as log:
        process = subprocess.Popen([app/'Contents/MacOS/monica-rust-desktop'], env=env, stdout=log, stderr=log)
    try:
        yield process
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            process.wait(timeout=185)
        assert process.returncode == 0
        assert not (home/'Library/Application Support/Monica Rust/postgres/postmaster.pid').exists()


def fixture_sqlite(path, script, insert=None):
    path.parent.mkdir(parents=True, exist_ok=True)
    with closing(sqlite3.connect(path)) as db, db:
        db.executescript(script)
        if insert:
            db.execute(*insert)


def fixtures(root, config, tools):
    now = int(time.time())
    original = root/'wechat'
    storage = original/'db_storage'
    still = root/'test.png'
    subprocess.run([tools/'ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'color=red:size=32x32',
                    '-frames:v', '1', '-y', still], check=True)
    photo = still.read_bytes()
    clip = root/'test.mp4'
    subprocess.run([tools/'ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'color=black:size=32x32:duration=0.2',
                    '-c:v', 'mpeg4', '-y', clip], check=True)
    motion = clip.read_bytes()
    md5 = hashlib.md5(motion).hexdigest()
    still_url = 'https://image.qpic.cn/monica-fixture/0'
    video_url = 'https://fixture.video.qq.com/monica-fixture.mp4'
    xml = f'''<SnsObject><TimelineObject><id>1001</id><username>synthetic-person</username>
<createTime>{now}</createTime><contentDesc>Native fixture</contentDesc><ContentObject><type>1</type><mediaList>
<media><id>imageref</id><type>2</type><url>{still_url}</url><size width="32" height="32"/>
<LivePhoto><liveMedia><id>clipref</id><type>6</type><url md5="{md5}">{video_url}</url></liveMedia></LivePhoto>
</media></mediaList></ContentObject></TimelineObject></SnsObject>'''
    fixture_sqlite(storage/'sns/sns.db', 'CREATE TABLE SnsTimeLine(tid INTEGER PRIMARY KEY,user_name TEXT,content TEXT);',
                   ('INSERT INTO SnsTimeLine VALUES(?,?,?)', (888, 'synthetic-person', xml)))
    fixture_sqlite(storage/'contact/contact.db', "CREATE TABLE contact(username,local_type,remark); INSERT INTO contact VALUES('synthetic-person',0,'Synthetic Person');")
    cache = config/'work/.chatlog_sns_cache'
    cache.mkdir(parents=True)
    for url, payload in [(still_url, photo), (video_url, motion)]:
        (cache/(hashlib.md5((url+'|').encode()).hexdigest()+'.bin')).write_bytes(payload)

    peer = 'synthetic@chatroom'
    conversation = hashlib.md5(peer.encode()).hexdigest()
    key = 'f'*32
    folder = original/'msg/attach'/conversation/time.strftime('%Y-%m', time.gmtime(now))/'Img'
    folder.mkdir(parents=True)
    (folder/(key+'_h.dat')).write_bytes(photo)
    table = 'Msg_' + conversation
    fixture_sqlite(storage/'message/message_0.db', f"""
CREATE TABLE Name2Id(user_name TEXT); INSERT INTO Name2Id VALUES('{peer}'),('synthetic-person');
CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,create_time INTEGER,
                     local_type INTEGER,real_sender_id INTEGER,message_content BLOB,compress_content BLOB);
""", (f'INSERT INTO {table} VALUES(?,?,?,?,?,?,?)', (1, 123, now, 3, 2, b'<msg><img /></msg>', None)))
    fixture_sqlite(storage/'message/message_resource.db', f"""
CREATE TABLE ChatName2Id(user_name TEXT); INSERT INTO ChatName2Id VALUES('{peer}');
CREATE TABLE MessageResourceInfo(chat_id,message_svr_id,message_create_time,message_local_type,packed_info);
""", ('INSERT INTO MessageResourceInfo VALUES(?,?,?,?,?)', (1, 123, now, 3, bytes.fromhex('12220a20')+key.encode())))
    fixture_sqlite(storage/'hardlink/hardlink.db', """
CREATE TABLE dir2id(username TEXT); CREATE TABLE image_hardlink_info_v4(md5,file_name,dir1,dir2,extra_buffer);
""")
    return original


def source_bytes(original):
    return {str(path.relative_to(original)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in original.rglob('*') if path.is_file()}


def check_media(app):
    client = build_opener(ProxyHandler({}), HTTPCookieProcessor(CookieJar()))
    with tempfile.TemporaryDirectory(prefix='monica-desktop-media-') as directory:
        root = Path(directory).resolve()
        home = root/'home'
        data = home/'Library/Application Support/Monica Rust'
        config = data/'chatlog'
        config.mkdir(parents=True, mode=0o700)
        original = fixtures(root, config, app/'Contents/Resources/runtime/tools/bin')
        before = source_bytes(original)
        prefs = config/'preferences.json'
        prefs.write_text(json.dumps({'enabled': False, 'port': 0}))
        server_config = config/'chatlog-server.json'
        server_config.write_text(json.dumps({'platform':'darwin', 'version':4, 'desktop_mode':True,
                           'data_dir':str(original), 'data_key':'1'*64, 'work_dir':str(config/'work'),
                           'http_addr':'127.0.0.1:0'}))
        server_config.chmod(0o600)

        def ready():
            url = 'http://127.0.0.1:' + str(json.loads((data/'runtime-preferences.json').read_text())['web_port'])
            return url if request(client, url, '/api/v1/ready') else None

        with launch(app, home) as process:
            url = wait(process, ready, 'packaged server startup')
            secrets = json.loads((data/'runtime-secrets.json').read_text())
            request(client, url, '/internal/adapter-state', {'provider':'wechat', 'enabled':True, 'paused':False}, secrets['control'])
            credentials = {'email':'desktop-fixture@example.invalid', 'password':'synthetic-test-password-42'}
            request(client, url, '/api/v1/auth/bootstrap', credentials)
            request(client, url, '/api/v1/auth/login', credentials)
            request(client, url, '/api/v1/imports/wechat-images', {'action':'enable'})
        prefs.write_text(json.dumps({'enabled':True, 'port':0}))

        with launch(app, home) as process:
            assert wait(process, ready, 'upgraded server startup') == url
            port = int((data/'postgres/postmaster.pid').read_text().splitlines()[3])
            with psycopg.connect(host='127.0.0.1', port=port, user='monica', password=secrets['postgres'],
                                  dbname='monica_rust', autocommit=True) as db:
                expect = [('message_event','wechat-group-image:high'), ('social_post','live-media-1'), ('social_post','media-1')]
                def linked_media():
                    rows = db.execute("""
                        SELECT entity_type,role FROM crm.media_links
                        WHERE role IN ('media-1','live-media-1','wechat-group-image:high') ORDER BY 1,2
                    """).fetchall()
                    return rows == expect and not db.execute('SELECT 1 FROM crm.native_moments_media').fetchone()

                try:
                    wait(process, linked_media, 'native still, live motion and resource-linked chat photo')
                except RuntimeError:
                    for sql in ('SELECT status, progress FROM crm.adapter_runs ORDER BY created_at DESC LIMIT 10',
                                'SELECT entity_type,role FROM crm.media_links',
                                'SELECT status,counts FROM crm.wechat_image_runs',
                                'SELECT event_id,attempts FROM crm.native_moments_media'):
                        print(db.execute(sql).fetchall(), flush=True)
                    for path in (data/'collector-data/moments-media').rglob('photos.json'):
                        print([(slot['status'],slot.get('code')) for slot in json.loads(path.read_text()).values()], flush=True)
                    raise
                event_id = db.execute("SELECT id FROM crm.timeline_events WHERE external_id='post:888'").fetchone()[0]
                post = request(client, url, '/api/v1/timeline/' + str(event_id))
                assert post['media'][0]['live_media_id']
                assert source_bytes(original) == before

                storage = original/'db_storage'
                with closing(sqlite3.connect(storage/'sns/sns.db')) as source, source:
                    xml = source.execute('SELECT content FROM SnsTimeLine WHERE tid=888').fetchone()[0]
                    start, end = xml.index('<mediaList>'), xml.index('</mediaList>') + len('</mediaList>')
                    source.execute('UPDATE SnsTimeLine SET content=? WHERE tid=888', (xml[:start]+xml[end:],))
                with closing(sqlite3.connect(storage/'message/message_0.db')) as source, source:
                    table = 'Msg_' + hashlib.md5(b'synthetic@chatroom').hexdigest()
                    source.execute(f'UPDATE {table} SET local_type=1,message_content=? WHERE server_id=123', ('Fixture updated',))
                before = source_bytes(original)

                def revised_media_removed():
                    return db.execute("""SELECT EXISTS(SELECT FROM crm.message_search WHERE body='Fixture updated')
                        AND EXISTS(SELECT FROM crm.timeline_events WHERE id=%s AND content->'media'='[]'::jsonb)
                        AND NOT EXISTS(SELECT FROM crm.media_links WHERE entity_type IN ('social_post','message_event'))
                        AND EXISTS(SELECT FROM crm.media_assets)""", (event_id,)).fetchone()[0]

                wait(process, revised_media_removed, 'retiring replaced source attachments', timeout=100)
        assert source_bytes(original) == before
        print('Packaged desktop imported and retired Moments and chat media across source revisions. The fixture bytes, saved media and local loopback origin stayed intact.')


if __name__ == '__main__':
    check_media(Path(sys.argv[1]).resolve(strict=True))

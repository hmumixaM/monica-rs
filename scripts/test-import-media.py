"""Normal source-media publication against a disposable PostgreSQL database."""
import base64
from dataclasses import replace
import hashlib
import importlib.util
import json
from pathlib import Path
import sqlite3
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid

from psycopg.types.json import Jsonb
from monica_imports.media_store import MARKER
from monica_imports.sns_publication import DatabaseTarget, Publication, publish

spec = importlib.util.spec_from_file_location('live_fixture', Path(__file__).with_name('test-nas-live-imports.py'))
fixtures = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixtures)


class PublicationContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        fixtures.LiveImportContract.setUpClass.__func__(cls)

    def setUp(self):
        fixtures.LiveImportContract.setUp(self)
        (self.args.media/'.monica-rust-migration-media').write_text(MARKER)
        self.event = uuid.uuid4()
        self.url = 'https://mmsns.qpic.cn/synthetic/0'
        self.content = {'username':'synthetic-author','media':[{'role':'media-1','source_url':self.url,'source_media_id':'101','source_md5':'a'*32,'media_type':'image'}]}
        self.db.execute("INSERT INTO crm.timeline_events(id,provider,object_id,external_id,event_type,content,content_hash,version) VALUES(%s,'wechat',%s,'100','post',%s,'synthetic',1)", (self.event,uuid.uuid4(),Jsonb(self.content)))
        image = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII=')
        self.asset = dict(sha256=hashlib.sha256(image).hexdigest(),byte_length=len(image),media_type='image/png',width=1,height=1)
        stage = Path(self.temp.name).resolve()/'stage'
        (stage/'objects').mkdir(parents=True)
        (stage/'objects'/self.asset['sha256']).write_bytes(image)
        row = dict(event_id=str(self.event),external_id='100',slot=1,asset_role='media-1',source_url=self.url,source_media_id='101',source_md5='a'*32,raw_xml_sha256='b'*64,full_pixels=True,media=self.asset)
        manifest = stage/'manifest.json'
        manifest.write_text(json.dumps(dict(rows=[row])))
        state = Path(self.temp.name).resolve()/'publication'
        state.mkdir()
        self.request = Publication(stage=stage,media=self.args.media,state=state,validator=Path('/unused-validator'),manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest(),expected_records=1,target=DatabaseTarget.read(self.db))
        self.validator = SimpleNamespace(returncode=0,stdout=json.dumps(self.asset).encode())

    def test_verified_publication_and_replay_preserve_single_asset_slot_and_source(self):
        original = self.db.execute('SELECT content FROM crm.timeline_events WHERE id=%s',(self.event,)).fetchone()[0]
        with patch('monica_imports.sns_publication.subprocess.run',return_value=self.validator):
            publish(self.db,self.request)
            publish(self.db,self.request)
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_assets').fetchone()[0],1)
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_links').fetchone()[0],1)
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_slots').fetchone()[0],1)
        self.assertEqual(self.db.execute('SELECT content FROM crm.timeline_events WHERE id=%s',(self.event,)).fetchone()[0],original)
        path = self.args.media/'sha256'/self.asset['sha256'][:2]/self.asset['sha256'][2:4]/self.asset['sha256']
        self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(),self.asset['sha256'])
        self.assertTrue(json.loads((self.request.state/'result.json').read_text())['complete'])

    def test_live_motion_publication_is_bound_to_the_static_slot_and_xml_revision(self):
        motion = b'synthetic validated MP4'
        asset = dict(self.asset, sha256=hashlib.sha256(motion).hexdigest(),
                     byte_length=len(motion), media_type='video/mp4', duration_ms=1500)
        (self.request.stage/'objects'/asset['sha256']).write_bytes(motion)
        manifest = self.request.stage/'manifest.json'
        row = json.loads(manifest.read_text())['rows'][0]
        row.update(asset_role='live-media-1', parent_media_id='101', source_media_id='motion-id',
                   source_url='https://szzjwxsns.video.qq.com/motion', full_video=True, media=asset)
        manifest.write_text(json.dumps(dict(rows=[row])))
        request = replace(self.request, manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest())
        fake_video = SimpleNamespace(validate=lambda *args, **kwargs: asset)
        with patch('monica_imports.sns_publication.module', return_value=fake_video):
            with self.assertRaises(ValueError):
                publish(self.db, request)
            self.content['raw_xml_sha256'] = row['raw_xml_sha256']
            self.db.execute('UPDATE crm.timeline_events SET content=%s WHERE id=%s',
                            (Jsonb(self.content), self.event))
            publish(self.db, request)
            publish(self.db, request)
        self.assertEqual(self.db.execute('SELECT role FROM crm.media_links WHERE entity_id=%s',
                                         (self.event,)).fetchall(), [('live-media-1',)])

    def test_verified_native_video_can_advance_the_legacy_poster_slot(self):
        with patch('monica_imports.sns_publication.subprocess.run', return_value=self.validator):
            publish(self.db, self.request)
        old = self.db.execute('SELECT media_id FROM crm.media_slots WHERE entity_id=%s', (self.event,)).fetchone()[0]
        self.content['media'][0]['media_type'] = 'video'
        self.db.execute('UPDATE crm.timeline_events SET content=%s WHERE id=%s', (Jsonb(self.content), self.event))
        clip = b'verified native MP4'
        asset = dict(self.asset, sha256=hashlib.sha256(clip).hexdigest(),
                     byte_length=len(clip), media_type='video/mp4')
        (self.request.stage/'objects'/asset['sha256']).write_bytes(clip)
        manifest = self.request.stage/'manifest.json'
        row = json.loads(manifest.read_text())['rows'][0]
        row.update(media=asset,full_video=True)
        manifest.write_text(json.dumps(dict(rows=[row])))
        request = replace(self.request, manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest())
        verifier = SimpleNamespace(validate=lambda *args, **kwargs: asset)
        with patch('monica_imports.sns_publication.module', return_value=verifier):
            publish(self.db, request)
        self.assertEqual(self.db.execute('SELECT a.media_type FROM crm.media_slots s JOIN crm.media_assets a ON a.id=s.media_id WHERE entity_id=%s',
                                         (self.event,)).fetchone(), ('video/mp4',))
        self.assertIsNotNone(self.db.execute('SELECT 1 FROM crm.media_links WHERE entity_id=%s AND media_id=%s',
                                            (self.event,old)).fetchone())

    def test_native_owned_video_can_be_replaced_then_retired(self):
        connection = uuid.uuid4()
        self.db.execute("""INSERT INTO crm.source_connections
            (id,provider,external_account,display_name,connection_method,state,enabled)
            VALUES(%s,'wechat','native-chatlog-test','Fixture','collector','connected',true)""", (connection,))
        self.db.execute("UPDATE crm.scheduler_control SET paused=false")
        self.db.execute("UPDATE crm.adapter_controls SET enabled=true,paused=false WHERE provider='wechat'")
        self.content.update(source_tid='90', coordinate_format='chatlog_sns_xml',
                            raw_content='<TimelineObject><id>100</id></TimelineObject>')
        self.content['media'][0]['media_type'] = 'video'
        self.db.execute("UPDATE crm.timeline_events SET connection_id=%s,external_id='post:90',content=%s,version=2 WHERE id=%s",
                        (connection, Jsonb(self.content), self.event))
        manifest = self.request.stage/'manifest.json'
        row = json.loads(manifest.read_text())['rows'][0]
        for clip in (b'first verified clip', b'second verified clip'):
            asset = dict(self.asset, sha256=hashlib.sha256(clip).hexdigest(), byte_length=len(clip), media_type='video/mp4')
            (self.request.stage/'objects'/asset['sha256']).write_bytes(clip)
            row.update(media=asset, full_video=True)
            manifest.write_text(json.dumps(dict(rows=[row])))
            request = replace(self.request, manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest(),
                              native_event=(connection, 2))
            with patch('monica_imports.sns_publication.module', return_value=SimpleNamespace(validate=lambda *_args, **_kwargs: asset)):
                publish(self.db, request)
        attached = self.db.execute("SELECT a.sha256 FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id WHERE l.entity_id=%s", (self.event,)).fetchall()
        self.assertEqual(attached, [(asset['sha256'],)])
        self.content['media'] = []
        self.db.execute('UPDATE crm.timeline_events SET content=%s,version=3 WHERE id=%s', (Jsonb(self.content), self.event))
        for table in ('media_slots', 'media_links'):
            self.assertFalse(self.db.execute(f'SELECT 1 FROM crm.{table} WHERE entity_id=%s', (self.event,)).fetchone())
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_assets').fetchone(), (2,))

    def test_wrong_database_or_changed_source_never_publishes(self):
        with self.assertRaises(ValueError):
            publish(self.db,replace(self.request,target=replace(self.request.target,system_id='another-cluster')))
        self.content['media'][0]['source_md5'] = 'c'*32
        self.db.execute('UPDATE crm.timeline_events SET content=%s WHERE id=%s',(Jsonb(self.content),self.event))
        with patch('monica_imports.sns_publication.subprocess.run',return_value=self.validator):
            with self.assertRaises(ValueError):
                publish(self.db,self.request)
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_assets').fetchone()[0],0)
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_links').fetchone()[0],0)
        self.assertFalse((self.request.state/'result.json').exists())

    def test_comment_only_post_uses_archived_xml_after_the_live_snapshot_loses_it(self):
        comment = uuid.uuid4()
        media = {**self.content['media'][0], 'source_md5': None}
        self.db.execute('UPDATE crm.timeline_events SET content=%s WHERE id=%s',
                        (Jsonb({'source_tid': '100', 'media': []}), self.event))
        self.db.execute("""
            INSERT INTO crm.social_interactions
                (id,post_id,provider,external_id,interaction_type,author_external_id,occurred_at,content,metadata)
            VALUES(%s,%s,'wechat','100:comment:77','reply','peer','2026-11-14','{}',%s)
        """, (comment, self.event, Jsonb({'media': [media]})))
        raw = ('<SnsObject><TimelineObject><id>100</id></TimelineObject><LocalExtraInfo>'
               '<comment_user_list><user_comment><comment_64id>77</comment_64id><username>peer</username>'
               f'<imagelist><imageinfo><media_id>101</media_id><url>{self.url}</url></imageinfo></imagelist>'
               '</user_comment></comment_user_list></LocalExtraInfo></SnsObject>')
        evidence = uuid.uuid4()
        self.db.execute("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json) VALUES (%s,'social_post',%s,2,'{}')",
                        (evidence, 'a'*64))
        self.db.execute("""
            INSERT INTO crm.legacy_social_versions(id,event_id,content_object_id,content_hash,observed_at,original)
            VALUES(%s,%s,%s,'synthetic',now(),%s)
        """, (uuid.uuid4(), self.event, evidence, Jsonb({'raw_content': raw})))
        snapshot = self.args.chatlog/'sns/sns.db'
        snapshot.parent.mkdir()
        with sqlite3.connect(snapshot) as source:
            source.execute('CREATE TABLE SnsTimeLine(tid INTEGER PRIMARY KEY,content TEXT)')
        self.args.sns_runtime = self.request.stage.parent/'runtime'
        self.args.sns_cache = self.request.stage.parent/'cache'
        self.args.sns_cache.mkdir()
        name = hashlib.md5((self.url+'|').encode()).hexdigest()+'.bin'
        (self.args.sns_cache/name).write_bytes((self.request.stage/'objects'/self.asset['sha256']).read_bytes())
        with patch('monica_imports.sns_publication.subprocess.run', return_value=self.validator):
            self.assertEqual(self.consumer.import_sns_media(self.args.chatlog),
                             {'sns_pending': 0, 'sns_ready': 1, 'sns_failed': 0})
            self.assertEqual(self.consumer.import_sns_media(self.args.chatlog),
                             {'sns_pending': 0, 'sns_ready': 0, 'sns_failed': 0})
        self.assertEqual(self.db.execute('SELECT entity_type,entity_id,role FROM crm.media_links').fetchall(),
                         [('social_interaction', comment, 'media-1')])
        self.assertEqual(self.consumer.journal.execute("SELECT count(*) FROM sns_queue_seeds WHERE name='moments-and-comments'").fetchone()[0], 1)
        images = fixtures.live.module('sns_reuse_comment_test', 'sns-media-import.py')
        (self.args.sns_cache/name).unlink()
        with patch.object(images, 'archived_xml', side_effect=AssertionError('must reuse the verified asset')):
            self.assertEqual(images.import_event(self.db, snapshot, self.args.media, self.request.stage.parent/'fresh-state',
                                                self.args.sns_runtime, self.args.sns_cache, self.event),
                             {'ready': 1, 'failed': 0})
        self.assertEqual(self.db.execute('SELECT count(*) FROM crm.media_links').fetchone()[0], 1)


if __name__ == '__main__':
    unittest.main()

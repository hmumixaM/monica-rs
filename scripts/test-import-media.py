"""Normal source-media publication against a disposable PostgreSQL database."""
import base64
from dataclasses import replace
import hashlib
import importlib.util
import json
from pathlib import Path
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


if __name__ == '__main__':
    unittest.main()

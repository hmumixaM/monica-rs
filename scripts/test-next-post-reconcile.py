"""Regression cases for Next post identity and cached media import."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch
import uuid

import psycopg
from psycopg.types.json import Jsonb

ROOT=Path(__file__).resolve().parents[1]
def module(name,path):
    spec=importlib.util.spec_from_file_location(name,ROOT/path);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value

reconcile=module('next_reconcile','scripts/reconcile-next-posts.py')
contracts=module('next_live_fixture','scripts/test-nas-live-imports.py')
moments=module('next_moments','scripts/chatlog_moments.py')
fetch=module('next_cache_fetch','tools/sns-decrypt/fetch_frame.py')
enrichment=module('next_source_semantics','scripts/enrich-moments-media.py')


class SourceContracts(unittest.TestCase):
    def test_original_next_http_token_cache_is_used_without_network(self):
        with tempfile.TemporaryDirectory()as directory:
            root=Path(directory).resolve();source=root/'sns.db';cache=root/'cache';cache.mkdir()
            xml='<root><enc key="wrong-root-key"/><TimelineObject><id>42</id><ContentObject><type>1</type><mediaList><media><id>photo-42</id><type>2</type><enc key="123"/><url key="0" token="synthetic">http://mmsns.qpic.cn/unique-photo/150</url></media></mediaList></ContentObject></TimelineObject></root>'
            with sqlite3.connect(source)as db:db.execute('CREATE TABLE SnsTimeLine(tid,content)');db.execute('INSERT INTO SnsTimeLine VALUES(42,?)',(xml,))
            url='http://mmsns.qpic.cn/unique-photo/0?token=synthetic&idx=1'
            filename=hashlib.md5((url+'|123').encode()).hexdigest()+'.bin';body=b'\xff\xd8\xffsynthetic'
            (cache/filename).write_bytes(body)
            with patch.object(fetch,'build_opener',side_effect=AssertionError('network must not be used')):
                header,actual=fetch.frame(source,'42',1,cache_root=cache)
            self.assertTrue(header['cached']);self.assertEqual(actual,body);self.assertEqual(header['seed'],'123')

    def test_shared_video_keeps_title_link_and_cover_semantics(self):
        xml='<root><TimelineObject><id>42</id><ContentObject><type>5</type><title>Source video title</title><description>Source description</description><contentUrl>https://www.xiaohongshu.com/explore/synthetic</contentUrl><mediaList><media><id>cover-42</id><type>4</type><url>https://mmsns.qpic.cn/cover/0</url></media></mediaList></ContentObject></TimelineObject></root>'
        record=moments.moments_record(42,'synthetic-author',xml,None)['content']
        self.assertEqual(record['shared_link']['kind'],'video');self.assertEqual(record['shared_link']['title'],'Source video title')
        self.assertEqual(record['media'][0]['media_type'],'image');self.assertEqual(record['media'][0]['source_media_type'],'video')
        self.assertEqual(record['media'][0]['source_kind'],'wechat_video_share')


    def test_semantics_patch_keeps_next_caption_and_original_media_reference(self):
        obj=moments.parse_moments_xml('<TimelineObject><id>42</id><ContentObject><type>5</type><title>Video</title><contentUrl>https://www.xiaohongshu.com/explore/synthetic</contentUrl><mediaList><media><id>cover-42</id><type>4</type><url>https://mmsns.qpic.cn/new-address/0</url></media></mediaList></ContentObject></TimelineObject>')[0]
        current=dict(text='Next caption',media=[dict(role='media-1',source_media_id='cover-42',source_url='http://mmsns.c2c.wechat.com/original-address/0',media_type='image')])
        result=enrichment.enriched(current,obj)
        self.assertEqual(result['text'],current['text'])
        self.assertEqual(result['media'][0]['source_url'],current['media'][0]['source_url'])
        self.assertEqual(result['media'][0]['source_media_type'],'video')
        self.assertNotIn('source_media_type',current['media'][0])


class NextIdentityContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        contracts.LiveImportContract.setUpClass.__func__(cls)

    def setUp(self):
        contracts.LiveImportContract.setUp(self)
        self.owner=uuid.uuid4();self.account=uuid.uuid4();self.keep=uuid.uuid4();self.extra=uuid.uuid4();self.obj=uuid.uuid4()
        self.db.execute("INSERT INTO crm.profiles(id,profile_type,display_name)VALUES(%s,'person','Synthetic Next owner')",(self.owner,))
        self.db.execute("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name)VALUES(%s,'wechat','synthetic-author','person','Synthetic')",(self.account,))
        content=dict(username='synthetic-author',text='Next caption',media=[dict(role='media-1',source_url='https://mmsns.qpic.cn/synthetic/0',media_type='image')])
        self.db.execute("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,content,content_hash,version)VALUES(%s,'wechat',%s,%s,'post','42',%s,'next',1)",(self.keep,self.keep,self.owner,Jsonb(content)))

    def test_adoption_retains_next_id_and_original_baseline(self):
        selected=self.db.execute("SELECT crm.adopt_legacy_timeline_identity('wechat','post','42',%s,%s,%s)",(self.account,self.owner,self.obj)).fetchone()[0]
        self.assertEqual(selected,self.keep)
        self.assertEqual(self.db.execute('SELECT id,source_account_id,object_id FROM crm.timeline_events').fetchone(),(self.keep,self.account,self.obj))
        original,current=self.db.execute("SELECT original_row,current_row FROM crm.legacy_chatlog_projection WHERE entity_id=%s",(self.keep,)).fetchone()
        self.assertIsNone(original['source_account_id']);self.assertEqual(original['object_id'],str(self.keep));self.assertEqual(current['object_id'],str(self.obj))
        self.assertEqual(self.db.execute("SELECT crm.adopt_legacy_timeline_identity('wechat','post','42',%s,%s,%s)",(self.account,self.owner,self.obj)).fetchone()[0],self.keep)

    def test_same_source_author_keeps_next_owner_when_account_link_differs(self):
        other=uuid.uuid4()
        self.db.execute("INSERT INTO crm.profiles(id,profile_type,display_name)VALUES(%s,'person','Other source owner')",(other,))
        chosen=self.db.execute("SELECT crm.adopt_legacy_timeline_identity('wechat','post','42',%s,%s,%s)",(self.account,other,self.obj)).fetchone()[0]
        self.assertEqual(chosen,self.keep)
        self.assertEqual(self.db.execute('SELECT profile_id FROM crm.timeline_events WHERE id=%s',(self.keep,)).fetchone()[0],self.owner)

    def test_source_enrichment_appends_history_and_preserves_next_reference(self):
        obj=moments.parse_moments_xml('<TimelineObject><id>42</id><username>synthetic-author</username><ContentObject><type>5</type><title>Video</title><contentUrl>https://www.xiaohongshu.com/explore/synthetic</contentUrl><mediaList><media><id>cover</id><type>4</type><url>https://mmsns.qpic.cn/synthetic/0</url></media></mediaList></ContentObject></TimelineObject>')[0]
        self.assertTrue(enrichment.apply(self.db,self.keep,'42',obj,'b'*64))
        self.assertFalse(enrichment.apply(self.db,self.keep,'42',obj,'b'*64))
        content=self.db.execute('SELECT content FROM crm.timeline_events WHERE id=%s',(self.keep,)).fetchone()[0]
        self.assertEqual(content['text'],'Next caption')
        self.assertEqual(content['shared_link']['kind'],'video')
        self.assertEqual(content['media'][0]['source_url'],'https://mmsns.qpic.cn/synthetic/0')
        self.assertEqual(self.db.execute('SELECT count(*)FROM crm.legacy_social_versions WHERE event_id=%s',(self.keep,)).fetchone()[0],1)

    def test_merge_keeps_next_attachments_interactions_readings_and_history(self):
        content=dict(username='synthetic-author',text='Other source caption',media=[])
        self.db.execute("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,content,content_hash,version)VALUES(%s,'wechat',%s,%s,%s,'post','42',%s,'source',1)",(self.extra,self.obj,self.account,self.owner,Jsonb(content)))
        media=[]
        for index in range(3):
            mid=uuid.uuid4();media.append(mid);self.db.execute("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path)VALUES(%s,%s,'image/jpeg',1,'synthetic')",(mid,str(index+1)*64))
        for post,mid,role in[(self.keep,media[0],'media-1'),(self.extra,media[1],'media-1'),(self.extra,media[2],'media-2')]:
            self.db.execute("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role)VALUES('social_post',%s,%s,%s)",(post,mid,role))
        retained=uuid.uuid4();overlap=uuid.uuid4();unique=uuid.uuid4()
        for iid,post,external in[(retained,self.keep,'same-like'),(overlap,self.extra,'same-like'),(unique,self.extra,'new-like')]:
            self.db.execute("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_profile_id,occurred_at,content)VALUES(%s,%s,'wechat',%s,'like',%s,now(),'{}')",(iid,post,external,self.owner))
        co=uuid.uuid4();version=uuid.uuid4()
        self.db.execute("INSERT INTO crm.content_objects(id,content_kind,sha256,byte_length,payload_json)VALUES(%s,'post',%s,2,'{}')",(co,'f'*64))
        self.db.execute("INSERT INTO crm.legacy_social_versions(id,event_id,content_object_id,content_hash,observed_at,original)VALUES(%s,%s,%s,'source',now(),'{}')",(version,self.extra,co))
        self.db.execute("INSERT INTO crm.media_readings(entity_id,entity_type,status,summary)VALUES(%s,'social_post','complete','Saved reading')",(self.extra,))
        self.assertTrue(reconcile.merge_pair(self.db,self.keep,self.extra))
        self.assertFalse(reconcile.merge_pair(self.db,self.keep,self.extra))
        self.assertEqual(self.db.execute('SELECT id,source_account_id,object_id,content->>\'text\'FROM crm.timeline_events WHERE status=\'active\'').fetchone(),(self.keep,self.account,self.obj,'Next caption'))
        self.assertEqual(set(self.db.execute("SELECT media_id,role FROM crm.media_links WHERE entity_id=%s",(self.keep,)).fetchall()),{(media[0],'media-1'),(media[2],'media-2')})
        self.assertEqual(self.db.execute('SELECT count(*)FROM crm.media_assets').fetchone()[0],3)
        self.assertEqual(self.db.execute("SELECT count(*)FROM crm.media_links WHERE entity_id=%s",(self.extra,)).fetchone()[0],2)
        self.assertEqual({r[0]for r in self.db.execute('SELECT id FROM crm.social_interactions')},{retained,unique})
        self.assertEqual(self.db.execute('SELECT interaction_id FROM crm.social_interaction_aliases WHERE alias_id=%s',(overlap,)).fetchone()[0],retained)
        self.assertEqual(self.db.execute('SELECT event_id FROM crm.legacy_social_versions WHERE id=%s',(version,)).fetchone()[0],self.extra)
        self.assertEqual(self.db.execute('SELECT summary FROM crm.media_readings WHERE entity_id=%s',(self.keep,)).fetchone()[0],'Saved reading')
        self.assertEqual(self.db.execute('SELECT sum(count)FROM crm.interaction_months WHERE post_id=%s',(self.keep,)).fetchone()[0],2)
        original=self.db.execute('SELECT original_row,preserved_data FROM crm.timeline_event_aliases WHERE alias_id=%s',(self.extra,)).fetchone()
        self.assertEqual(original[0]['content']['text'],'Other source caption');self.assertEqual(len(original[1]['attachments']),2)


if __name__=='__main__':
    unittest.main()

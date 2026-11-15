#!/usr/bin/env python3
"""Generated SQLite only. No Chatlog process, real source, network or credentials."""
import importlib.util
import hashlib
import json
from pathlib import Path
import os
import sqlite3
import tempfile
import unittest

BASE=Path(os.environ.get('MONICA_TEST_COLLECTOR_ROOT',str(Path(__file__).parent))).resolve()

def module(name,file):
    print(json.dumps({'fixture_module':str(BASE/file),'sha256':hashlib.sha256((BASE/file).read_bytes()).hexdigest()}),flush=True)
    spec=importlib.util.spec_from_file_location(name,BASE/file);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
bridge=module('bridge','collector-bridge.py');chatlog=module('chatlog','collector-chatlog.py')

class MomentsContractTests(unittest.TestCase):
    def test_photos_links_locations_and_social_interactions_survive_snapshot(self):
        xml="""<SnsObject><TimelineObject><id>12</id><username>wxid_author</username><createTime>1767225600</createTime><contentDesc>合成 &amp; 原文</contentDesc><location latitude="48.85" longitude="2.35" city="Paris"/><ContentObject><type>3</type><contentUrl>https://example.invalid/shared</contentUrl><title>Synthetic link</title><mediaList><media><id>photo</id><type>2</type><url>https://example.invalid/photo.jpg</url><thumb>https://example.invalid/thumb.jpg</thumb><size width="640" height="480"/></media></mediaList></ContentObject></TimelineObject><LocalExtraInfo><like_user_list><user_comment><username>wxid_peer</username><nickname>Synthetic Peer</nickname><create_time>1767225601</create_time></user_comment></like_user_list><comment_user_list><user_comment><username>wxid_peer</username><comment_64id>99</comment_64id><content>合成评论</content><create_time>1767225602</create_time></user_comment></comment_user_list></LocalExtraInfo></SnsObject>"""
        record=chatlog.post(dict(tid=12,user_name='wxid_author',content=xml))
        self.assertEqual(record['external_id'],'post:12')
        self.assertEqual(record['media'][0]['source_url'],'https://example.invalid/photo.jpg')
        self.assertEqual(record['location']['latitude'],'48.85')
        self.assertEqual(record['shared_link']['title'],'Synthetic link')
        self.assertEqual((record['like_count'],record['comment_count']),(1,1))
        self.assertEqual(record['interactions'][1]['text'],'合成评论')
        self.assertEqual(record['raw_content'],xml)
        self.assertEqual(record['raw_xml_sha256'],hashlib.sha256(xml.encode()).hexdigest())
        repaired=chatlog.post(dict(tid=12,user_name='wxid_author',content=xml.replace('合成 &amp; 原文','literal &#2;')))
        self.assertIn('&#2;',repaired['body'])
        self.assertEqual(repaired['xml_literal_character_references'],{'&#2;':1})

class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='monica-chatlog-synthetic-',dir='/tmp');self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name).resolve();self.snapshots=self.root/'owned-snapshots';self.snapshots.mkdir();self.data=self.root/'data';self.data.mkdir();self.frames=[]
    def create(self,name,sql,rows=()):
        path=self.snapshots/name;path.parent.mkdir(parents=True,exist_ok=True)
        temporary=path.with_suffix('.new')
        db=sqlite3.connect(temporary);db.executescript(sql)
        for query,values in rows:db.executemany(query,values)
        db.commit();db.close();os.replace(temporary,path);return path
    def reader(self):return chatlog.Reader(self.data,self.snapshots,'synthetic-source-anchor',bridge.atomic_document,bridge.load_json,own_id='wxid_synthetic_own')
    def step(self,ack=True):
        before=len(self.frames);r=self.reader()
        try:result=r.step(self.frames.append)
        finally:r.close()
        if ack and len(self.frames)>before:
            f=self.frames[-1];bridge.atomic_document(self.data/f['ack_path'],dict(receipt_id='11111111-1111-4111-8111-111111111111',page_sha256=f['page_sha256']))
        return result
    def drain(self):
        for _ in range(100):
            if not self.step():return
        self.fail('reader did not settle within bounded fixture turns')
    def contact_snapshot(self,values):
        return self.create('contact/contact.db','CREATE TABLE contact(id INTEGER PRIMARY KEY,username TEXT,local_type INTEGER,delete_flag INTEGER,alias TEXT,remark TEXT,nick_name TEXT,description TEXT);',[('INSERT INTO contact VALUES(?,?,?,?,?,?,?,?)',values)])
    def messages(self,values):
        talker='wxid_synthetic_peer';table='Msg_'+hashlib.md5(talker.encode()).hexdigest()
        self.create('message/message_0.db','CREATE TABLE Name2Id(user_name TEXT);CREATE TABLE "'+table+'"(local_id INTEGER PRIMARY KEY,server_id INTEGER,create_time INTEGER,local_type INTEGER,real_sender_id INTEGER,message_content BLOB,compress_content BLOB);',[('INSERT INTO Name2Id VALUES(?)',[(talker,),('wxid_synthetic_own',)]),('INSERT INTO "'+table+'" VALUES(?,?,?,?,?,?,?)',values)])
        return table
    def test_receipt_before_cursor_and_identical_crash_replay(self):
        self.contact_snapshot([(1,'wxid_synthetic_peer',0,0,'alias','Synthetic Friend','Nick','note')])
        self.assertTrue(self.step(False));initial=self.frames[-1];self.assertTrue(self.step(False));self.assertEqual(initial,self.frames[-1])
        reader=self.reader();self.assertEqual(reader.db.execute('SELECT count(*) FROM known').fetchone()[0],0);self.assertEqual(reader.db.execute('SELECT row_id FROM files').fetchone()[0],0);reader.close()
        bridge.atomic_document(self.data/initial['ack_path'],dict(receipt_id='11111111-1111-4111-8111-111111111111',page_sha256=initial['page_sha256']))
        self.drain();self.assertEqual(len(self.frames),2);self.assertEqual(initial['batch']['records'][0]['content']['display_name'],'Synthetic Friend')
    def test_new_snapshot_keeps_active_inode_then_corrects_deletes_and_appends(self):
        old=chatlog.LIMIT;chatlog.LIMIT=2;self.addCleanup(setattr,chatlog,'LIMIT',old)
        rows=[(i,100+i,1767225600+i,1,1,('Message '+str(i)).encode(),None) for i in range(1,6)]
        self.messages(rows);self.step();first=self.frames[-1];self.assertEqual(len(first['batch']['records']),2)
        # Replace the export after page1: active old inode must finish first.
        changed=[rows[0],(2,102,1767225602,1,1,b'Corrected two',None),rows[3],rows[4],(6,106,1767225606,1,2,b'New own message',None)]
        self.messages(changed);self.drain()
        values=[r['content'] for f in self.frames for r in f['batch']['records']]
        self.assertTrue(any(v.get('body')=='Message 3' for v in values));self.assertTrue(any(v.get('body')=='Corrected two' for v in values));self.assertTrue(any(v.get('deleted') and v['external_id'].endswith('server:103') for v in values));self.assertTrue(any(v.get('direction')=='sent' for v in values));self.assertTrue(all(v['kind']=='message' for v in values));self.assertTrue(all('occurred_at' in v for v in values));self.assertTrue(all(v.get('participants',[])==[] or v['participants'][0]['id'] in ['wxid_synthetic_peer','wxid_synthetic_own'] for v in values))
        before=len(self.frames);self.drain();self.assertEqual(before,len(self.frames),'unchanged exports must not reread or republish source pages')
    def test_contact_filter_and_deletion_reconcile(self):
        self.contact_snapshot([(1,'wxid_synthetic_peer',0,0,'','Friend','',''),(2,'gh_public',0,0,'','Official','',''),(3,'synthetic@chatroom',2,0,'','Group','',''),(4,'wxid_stranger',3,0,'','Stranger','','')]);self.drain()
        values=[r['content'] for f in self.frames for r in f['batch']['records']];self.assertEqual([v['external_id'] for v in values],['contact:wxid_synthetic_peer','contact:gh_public'])
        self.contact_snapshot([]);self.drain();self.assertTrue(self.frames[-1]['batch']['records'][0]['content']['deleted'])
    def test_official_evidence_retained_and_enterprise_not_filtered(self):
        for external,flag,kind in [('wxid_synthetic_public',24,1),('gh_synthetic_public',8,1),('synthetic@openim',0,5),('wxid_synthetic_person',0,1)]:
            with self.subTest(external=external):
                value=chatlog.contact(dict(username=external,verify_flag=flag,local_type=kind,nick_name='Synthetic'))
                self.assertEqual(value['participants'][0]['id'],external)
                self.assertEqual(value['raw_contact'],dict(verify_flag=flag,local_type=kind))
    def test_group_sender_uses_name2id_and_private_binary_retained(self):
        room='synthetic@chatroom';table='Msg_'+hashlib.md5(room.encode()).hexdigest()
        self.create('message/message_1.db','CREATE TABLE Name2Id(user_name TEXT);CREATE TABLE "'+table+'"(local_id INTEGER PRIMARY KEY,server_id INTEGER,create_time INTEGER,local_type INTEGER,real_sender_id INTEGER,message_content BLOB,compress_content BLOB);',[('INSERT INTO Name2Id VALUES(?)',[(room,),('wxid_synthetic_sender',)]),('INSERT INTO "'+table+'" VALUES(?,?,?,?,?,?,?)',[(1,0,1767225600,1,2,b'colliding-display:\nReal text',None),(2,0,1767225601,3,2,b'\xff\xfe',None)])]);self.drain()
        rows=self.frames[0]['batch']['records'];self.assertEqual(rows[0]['content']['participants'][0]['id'],'wxid_synthetic_sender');self.assertEqual(len(rows[0]['content']['participants']),1);self.assertTrue(rows[0]['content']['is_group']);self.assertEqual(rows[1]['content']['binary_base64'],'//4=')
    def test_moments_exact_xml_and_source_time(self):
        self.create('sns/sns.db','CREATE TABLE SnsTimeLine(tid INTEGER PRIMARY KEY,user_name TEXT,content TEXT,pack_info_buf BLOB);',[('INSERT INTO SnsTimeLine VALUES(?,?,?,?)',[(12,'wxid_synthetic_author','<TimelineObject><username>wxid_synthetic_author</username><createTime>1767225600</createTime><contentDesc>Synthetic Moments</contentDesc></TimelineObject>',None)])]);self.drain();row=self.frames[0]['batch']['records'][0];self.assertEqual(row['entity_type'],'post');self.assertEqual(row['content']['body'],'Synthetic Moments');self.assertEqual(row['content']['occurred_at'],'2026-01-01T00:00:00+00:00');self.assertIn('<TimelineObject>',row['content']['raw_content'])
    def test_path_escape_bad_sender_and_ack_fail_closed(self):
        outside=self.root/'outside';outside.mkdir();(self.snapshots/'message').symlink_to(outside,target_is_directory=True)
        source=sqlite3.connect(outside/'message_1.db');source.execute('CREATE TABLE test(x)');source.close()
        with self.assertRaises(ValueError):self.step()
        (self.snapshots/'message').unlink()
        self.messages([(1,100,1767225600,1,99,b'Unknown sender',None)])
        with self.assertRaises(ValueError):self.step()
        r=self.reader();self.assertEqual(r.db.execute('SELECT row_id FROM files').fetchone()[0],0);self.assertEqual(r.db.execute('SELECT count(*) FROM pending').fetchone()[0],0);r.close()
    def test_reused_contact_rowid_tombstones_former_identity(self):
        self.contact_snapshot([(1,'wxid_former',0,0,'','Former','','')]);self.drain()
        self.contact_snapshot([(1,'wxid_replacement',0,0,'','Replacement','','')]);self.drain()
        values=[r['content'] for f in self.frames[1:] for r in f['batch']['records']]
        self.assertTrue(any(v['external_id']=='contact:wxid_former' and v.get('deleted') for v in values));self.assertTrue(any(v['external_id']=='contact:wxid_replacement' and not v.get('deleted') for v in values))
    def test_bad_shard_does_not_starve_other_sources_and_mismatched_ack_rejected(self):
        self.create('contact/contact.db','CREATE TABLE wrong_schema(x);')
        self.messages([(1,100,1767225600,1,1,b'Independent good shard',None)])
        with self.assertRaises(ValueError):self.step()
        self.step(False);self.assertEqual(self.frames[-1]['batch']['records'][0]['content']['body'],'Independent good shard')
        frame=self.frames[-1];bridge.atomic_document(self.data/frame['ack_path'],dict(receipt_id='synthetic',page_sha256='bad'))
        with self.assertRaises(ValueError):self.step()
    def test_cross_filesystem_export_is_copied_atomically_and_stays_readonly(self):
        from unittest import mock
        self.contact_snapshot([(1,'wxid_synthetic_peer',0,0,'','Friend','','')]);before=(self.snapshots/'contact/contact.db').read_bytes()
        with mock.patch.object(chatlog.os,'link',side_effect=OSError(chatlog.errno.EXDEV,'synthetic cross filesystem')):self.drain()
        self.assertEqual((self.snapshots/'contact/contact.db').read_bytes(),before);self.assertEqual(len(self.frames),1);self.assertFalse(list(self.data.rglob('*.partial')))
    def test_page_byte_limit_and_missing_self_stays_unknown(self):
        self.messages([(1,100,1767225600,1,1,b'x'*(2*1024*1024),None),(2,101,1767225601,1,1,b'y'*(2*1024*1024),None)])
        r=chatlog.Reader(self.data,self.snapshots,'another-anchor',bridge.atomic_document,bridge.load_json)
        try:r.step(self.frames.append)
        finally:r.close()
        self.assertEqual(len(self.frames[0]['batch']['records']),1);self.assertEqual(self.frames[0]['batch']['records'][0]['content']['direction'],'unknown');self.assertLess(len(json.dumps(self.frames[0]).encode()),4*1024*1024)

if __name__=='__main__':unittest.main()

#!/usr/bin/env python3
"""Offline synthetic publication fixtures; never imports collector runtimes."""
import base64
import contextlib
import importlib.util
import io
import json
import os
import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest import mock

MODULE_ROOT=Path(os.environ.get('MONICA_TEST_COLLECTOR_ROOT',str(Path(__file__).parent))).resolve()
MODULE_PATH=MODULE_ROOT/'collector-bridge.py'
print(json.dumps({'fixture_module':str(MODULE_PATH),'sha256':hashlib.sha256(MODULE_PATH.read_bytes()).hexdigest()}),flush=True)
spec=importlib.util.spec_from_file_location('bridge',MODULE_PATH)
bridge=importlib.util.module_from_spec(spec);spec.loader.exec_module(bridge)
PNG=base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN1cAAAAASUVORK5CYII=')

class BridgeTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='monica-synthetic-bridge-',dir='/tmp');self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name);bridge.MEDIA_CACHE.clear()
    def snapshot(self,relative):
        root=self.root/relative;path=root/'.snapshots/synthetic-v1';path.mkdir(parents=True);(path/'.crm-ready').write_text('synthetic-ready');(root/'CURRENT').symlink_to('.snapshots/synthetic-v1',target_is_directory=True);return path
    def frames(self,provider):
        output=io.StringIO()
        with mock.patch.object(bridge.subprocess,'Popen',side_effect=AssertionError('No real collector authorized')),contextlib.redirect_stdout(output):bridge.publish(provider,self.root)
        return [json.loads(line) for line in output.getvalue().splitlines()]
    def test_linkedin_profiles_posts_media_and_replay(self):
        snapshot=self.snapshot('raw/linkedin');asset=snapshot/'assets/synthetic.png';asset.parent.mkdir();asset.write_bytes(PNG);digest=bridge.hashlib.sha256(PNG).hexdigest()
        document=dict(local_assets=[dict(source_url='https://synthetic.invalid/photo.png',local_path='synthetic.png',content_hash=digest)],profiles=[dict(provider_user_id='synthetic-person',profile_url='https://linkedin.com/in/synthetic-person',captured_at='2026-01-01T00:00:00Z',profile=dict(name='Synthetic Person',avatar_url='https://synthetic.invalid/photo.png'),posts=[dict(provider_post_id='urn:li:activity:synthetic',source_key='fallback-key',text='Synthetic content',image='https://synthetic.invalid/photo.png')]),dict(provider_user_id='cover-only',profile=dict(name='Cover only',background_url='https://synthetic.invalid/photo.png'),posts=[])],export_metadata='x'*(4*1024*1024))
        (snapshot/'linkedin_enrichment.incoming.json').write_text(json.dumps(document))
        frames=self.frames('linkedin');batches=[frame['batch'] for frame in frames if frame['type']=='snapshot'];self.assertEqual(len(batches),2);self.assertEqual({b['stream'] for b in batches},{'profiles','posts'});self.assertEqual(len([f for f in frames if f['type']=='media']),1)
        self.assertEqual(batches[0]['media_manifest'][0]['sha256'],digest);self.assertEqual(batches[1]['media_manifest'][0]['entity_type'],'social_post')
        self.assertEqual(batches[1]['records'][0]['external_id'],'urn:li:activity:synthetic')
        self.assertEqual([m['role'] for m in batches[0]['media_manifest'] if m['external_id']=='cover-only'],['linkedin_cover'])
        again=[f['batch'] for f in self.frames('linkedin') if f['type']=='snapshot'];self.assertEqual(batches,again)
    def test_instagram_complete_story_and_post(self):
        current=self.snapshot('staging/instagram-current');profile=current/'profiles/synthetic';profile.mkdir(parents=True)
        comment=dict(id='1101',owner=dict(id='42',username='synthetic_friend'),text='Collected comment',created_at=1767225600,edge_threaded_comments=dict(edges=[dict(node=dict(id='1102',owner=dict(id='43',username='synthetic_reply'),text='Collected reply',created_at=1767225600))]))
        node=dict(__typename='GraphImage',id='synthetic-post',shortcode='SyntheticPost',owner=dict(id='123',username='synthetic_owner'),taken_at_timestamp=1767225600,edge_media_to_caption=dict(edges=[dict(node=dict(text='Synthetic post'))]),edge_media_to_parent_comment=dict(count=1,edges=[dict(node=comment)]))
        (profile/'post.json').write_text(json.dumps(dict(node=node)))
        (profile/'post-SyntheticPost.raw-node.json').write_text(json.dumps(dict(shortcode='SyntheticPost',node=node,captured_at=1767225601)))
        preview=dict(id=1101,owner=dict(userid=42,username='synthetic_friend'),created_at='2026-01-01T00:00:00+00:00',likes_count=0,mentions=[],answers=[dict(id=1102,owner=dict(userid=43,username='synthetic_reply'),created_at='2026-01-01T00:00:00+00:00',likes_count=0,mentions=[])])
        (profile/'post-SyntheticPost.relationships.json').write_text(json.dumps(dict(schema_version=1,shortcode='SyntheticPost',owner_username='older_owner_name',comments=dict(count=1,policy='collected',records=[preview]))))
        (profile/'profile.json').write_text(json.dumps(dict(userid=123,username='synthetic_owner',full_name='Synthetic profile')))
        (profile/'profile.raw-node.json').write_text(json.dumps(dict(captured_at=1767225601,node=dict(id='123',username='synthetic_owner',edge_mutual_followed_by=dict(count=7,edges=[])))))
        (profile/'profile.relationships.json').write_text(json.dumps(dict(schema_version=1,profile=dict(userid=123,username='older_owner_name'))))
        (profile/'post.jpg').write_bytes(PNG)
        stories=self.snapshot('staging/instagram-stories');owner=stories/'synthetic';owner.mkdir();(owner/'story.jpg').write_bytes(PNG);(owner/'story.manifest.json').write_text(json.dumps(dict(schema_version=1,owner_id='123',owner_username='synthetic_owner',mediaid='456',files=['story.jpg'],taken_at=1767225600,caption='Archived Story',expiring_at='2026-01-02T00:00:00+00:00',raw_metadata=dict(iphone_struct=dict(story_locations=[dict(location=dict(id='place-1',lat=1,lng=2),x=.5,y=.5)])))))
        frames=self.frames('instagram');batches=[f['batch'] for f in frames if f['type']=='snapshot'];self.assertEqual({b['stream'] for b in batches},{'profiles','posts','stories'});self.assertTrue(all(b['media_manifest'] for b in batches if b['stream']!='profiles'))
        posts=next(b for b in batches if b['stream']=='posts');self.assertEqual(len(posts['records']),1)
        self.assertEqual(posts['records'][0]['content']['text'],'Synthetic post')
        self.assertEqual({i['external_id']:i['content']['text'] for i in posts['records'][0]['content']['interactions']},{'1101':'Collected comment','1102':'Collected reply'})
        self.assertEqual(posts['records'][0]['content']['archive_raw_node']['captured_at'],1767225601)
        profiles=next(b for b in batches if b['stream']=='profiles')['records'];self.assertEqual(len(profiles),1)
        self.assertEqual(profiles[0]['content']['edge_mutual_followed_by']['count'],7)
        self.assertEqual(profiles[0]['content']['archive_raw_node']['captured_at'],1767225601)
        story=next(b for b in batches if b['stream']=='stories')['records'][0]
        self.assertEqual(story['content']['text'],'Archived Story');self.assertEqual(story['content']['expires_at'],'2026-01-02T00:00:00+00:00');self.assertEqual(story['content']['location']['latitude'],1)
        self.assertEqual(story['occurred_at'],'2026-01-01T00:00:00+00:00')
    def test_instagram_sparse_carousel_preserves_positions_and_thumbnails(self):
        snapshot=self.snapshot('staging/instagram-current')
        children=[dict(id=str(i),is_video=i==2,display_url=f'https://synthetic.invalid/{i}.jpg',video_url=f'https://synthetic.invalid/{i}.mp4' if i==2 else None) for i in range(1,11)]
        node=dict(__typename='GraphSidecar',id='carousel',owner=dict(id='synthetic-owner'),edge_sidecar_to_children=dict(edges=[dict(node=c) for c in children]))
        (snapshot/'post.json').write_text(json.dumps(dict(node=node)))
        for name in ['post_2.jpg','post_2.mp4','post_10.jpg']:(snapshot/name).write_bytes(PNG)
        batch=next(f['batch'] for f in self.frames('instagram') if f['type']=='snapshot')
        self.assertEqual({m['role'] for m in batch['media_manifest']},{'thumbnail-2','media-2','media-10'})
    def test_incomplete_and_escape_publications_rejected(self):
        snapshot=self.snapshot('raw/linkedin');(snapshot/'.crm-ready').unlink()
        with self.assertRaises(ValueError):self.frames('linkedin')
        current=self.root/'raw/linkedin/CURRENT';current.unlink();outside=self.root/'outside';outside.mkdir();(outside/'.crm-ready').touch();current.symlink_to(outside,target_is_directory=True)
        with self.assertRaises(ValueError):self.frames('linkedin')
    def test_unknown_and_incomplete_source_facts_fail(self):
        snapshot=self.snapshot('raw/linkedin');(snapshot/'linkedin_enrichment.incoming.json').write_text(json.dumps(dict(profiles=[dict(profile=dict(name='Missing ID'))])))
        with self.assertRaises(ValueError):self.frames('linkedin')
    def test_instagram_incomplete_manifest_never_published(self):
        snapshot=self.snapshot('staging/instagram-stories');(snapshot/'bad.manifest.json').write_text(json.dumps(dict(schema_version=1,owner_id='123',mediaid='456',files=['missing.jpg'])))
        with self.assertRaises(ValueError):self.frames('instagram')
    def test_linkedin_exact_conversation_keys_require_explicit_owner(self):
        snapshot=self.snapshot('raw/linkedin');document=dict(profiles=[],conversations=[dict(conversation_urn='urn:li:conversation:synthetic',participants=[dict(id='synthetic-peer')],messages=[dict(message_urn='urn:li:message:synthetic',sender_urn='urn:li:member:synthetic',delivered_at='2026-01-01T01:02:03+00:00',body='Synthetic message',incoming=True)])]);(snapshot/'linkedin_enrichment.incoming.json').write_text(json.dumps(document))
        with self.assertRaises(ValueError):self.frames('linkedin')
        rows=list(bridge.linkedin_records(snapshot,'synthetic-own-account'));self.assertEqual(rows[0][0],'messages');self.assertEqual(rows[0][1]['content']['account'],'synthetic-own-account');self.assertEqual(rows[0][1]['content']['sender'],'urn:li:member:synthetic');self.assertEqual(rows[0][1]['content']['sent_at_ms'],1767229323000)
    def test_linkedin_profiles_carry_mutual_connections_and_messages_name_participants(self):
        snapshot=self.snapshot('raw/linkedin');mutuals=dict(count=1,is_complete=True,connections=[dict(display_name='Synthetic Peer',profile_url='https://www.linkedin.com/in/synthetic-peer')])
        document=dict(profiles=[dict(provider_user_id='synthetic-contact',profile_url='https://www.linkedin.com/in/synthetic-contact',profile=dict(name='Synthetic Contact'),mutuals=mutuals),
                                dict(provider_user_id='synthetic-other',profile=dict(name='Synthetic Other'),mutuals=dict(count=4,connections=[]))],
                      conversations=[dict(conversation_urn='urn:li:conversation:synthetic',participants=['urn:li:member:peer'],participant_names={'urn:li:member:peer':'Synthetic Peer'},participant_urls={'urn:li:member:peer':'https://www.linkedin.com/in/synthetic-peer'},
                                          messages=[dict(message_urn='urn:li:message:synthetic',sender_urn='urn:li:member:peer',delivered_at='2026-01-01T01:02:03+00:00',body='Synthetic message',incoming=True)])])
        (snapshot/'linkedin_enrichment.incoming.json').write_text(json.dumps(document))
        rows=[row['content'] for _,row in bridge.linkedin_records(snapshot,'synthetic-own-account')]
        self.assertEqual(rows[0]['mutuals'],mutuals);self.assertNotIn('mutuals',rows[1])
        self.assertEqual(rows[2]['participants'],[dict(id='urn:li:member:peer',name='Synthetic Peer',profile_url='https://www.linkedin.com/in/synthetic-peer')])
    def test_spool_pages_are_fsynced_hashed_and_media_paths_are_relative(self):
        media=self.root/'synthetic.png';media.write_bytes(PNG);writer=bridge.SpoolWriter(self.root/'spool',self.root)
        writer.emit(dict(type='media',path=str(media),sha256=bridge.hashlib.sha256(PNG).hexdigest(),byte_length=len(PNG)))
        writer.emit(dict(type='snapshot',batch=dict(source='linkedin',records=[])));writer.close(True)
        self.assertEqual(len(writer.pages),1);page=writer.pages[0];marker=json.loads(page.with_suffix('.ready').read_text());self.assertEqual(marker['sha256'],bridge.hashlib.sha256(page.read_bytes()).hexdigest());self.assertEqual(marker['byte_length'],page.stat().st_size);self.assertEqual(json.loads(page.read_text().splitlines()[0])['path'],'synthetic.png');self.assertFalse(list(page.parent.glob('*.partial')))
        next_writer=bridge.SpoolWriter(self.root/'spool',self.root);next_writer.emit(dict(type='progress',phase='complete'));next_writer.close(True);self.assertLess(page.name,next_writer.pages[0].name)
        cancelled=bridge.SpoolWriter(self.root/'cancelled-spool',self.root);cancelled.emit(dict(type='progress'));cancelled.close(False);self.assertFalse(list(cancelled.directory.glob('*.jsonl')))
    def test_independent_message_outbox_reuses_batch_and_archives_only_after_ack(self):
        folder=self.root/'raw/linkedin/messages'/('a'*64);pending=folder/'pending';pending.mkdir(parents=True);(folder/'account.json').write_text(json.dumps(dict(account='urn:li:member:synthetic-own')))
        page=dict(records=[dict(external_id='urn:li:message:fixture',thread_id='urn:li:conversation:fixture',kind='message',body='Synthetic independent lane',occurred_at='2026-01-01T00:00:00Z',direction='received',participants=[dict(id='synthetic-peer',role='sender',is_self=False),dict(id='synthetic-own',role='participant',is_self=True)])])
        first=pending/'00000000000000000001.json';first.write_text(json.dumps(page));capture=io.StringIO()
        with contextlib.redirect_stdout(capture):self.assertEqual(bridge.publish_messages(self.root),1)
        initial=json.loads(capture.getvalue());self.assertEqual(initial['type'],'connection_snapshot');self.assertEqual(initial['account'],'urn:li:member:synthetic-own');self.assertEqual(initial['batch']['records'][0]['content']['direction'],'received')
        second=pending/'00000000000000000002.json';second.write_text(json.dumps(page));bridge.os.utime(second,(first.stat().st_mtime+100,first.stat().st_mtime+100))
        ack=self.root/initial['ack_path'];ack.parent.mkdir();ack.write_text(json.dumps(dict(page_sha256=initial['page_sha256'],receipt_id='11111111-1111-4111-8111-111111111111')))
        capture=io.StringIO()
        with contextlib.redirect_stdout(capture):self.assertEqual(bridge.publish_messages(self.root),1)
        again=json.loads(capture.getvalue());self.assertEqual(initial['batch'],again['batch'],'identical previews must not create different observed_at under one idempotency key');self.assertFalse(first.exists());self.assertTrue((folder/'archive'/first.name).exists());self.assertTrue(second.exists(),'unacknowledged source pages must remain replayable')

if __name__=='__main__':unittest.main()

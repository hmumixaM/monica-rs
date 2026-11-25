"""Source identity and streaming contracts, with no network or real source data."""
import hashlib,importlib.util,json,sqlite3,struct,subprocess,tempfile,unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('frame',ROOT/'tools/sns-decrypt/fetch_frame.py');fetch=importlib.util.module_from_spec(spec);spec.loader.exec_module(fetch)
spec=importlib.util.spec_from_file_location('moments',ROOT/'scripts/chatlog_moments.py');moments=importlib.util.module_from_spec(spec);spec.loader.exec_module(moments)
from unittest.mock import patch
class SourceMedia(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name).resolve();self.cache=self.root/'cache';self.cache.mkdir();self.db=self.root/'sns.db'
  with sqlite3.connect(self.db)as db:
   db.execute('CREATE TABLE SnsTimeLine(tid,content)')
   db.execute('INSERT INTO SnsTimeLine VALUES(?,?)',('123','''<TimelineObject><id>123</id><ContentObject><finderFeed><objectId>456</objectId><mediaList><media><coverUrl>http://wxapp.tc.qq.com/cover/0</coverUrl></media></mediaList></finderFeed><mediaList><media><id>789</id><type>2</type><url md5="source-md5">http://szmmsns.qpic.cn/photo/0</url></media></mediaList></ContentObject></TimelineObject>'''))
 def tearDown(self):self.temp.cleanup()
 def cached(self,url,body):
  (self.cache/(hashlib.md5((url+'|').encode()).hexdigest()+'.bin')).write_bytes(body)
 def test_cover_and_regular_photo_keep_their_distinct_parsed_slots(self):
  self.cached('http://wxapp.tc.qq.com/cover/0',b'cover');self.cached('https://szmmsns.qpic.cn/photo/0',b'photo')
  cover,body=fetch.frame(self.db,'123',1,cache_root=self.cache,thumbnail=True,source_media_id='456',source_kind='wechat_channels')
  self.assertEqual((cover['slot'],cover['source_media_id'],cover['source_md5'],body),(1,'456',None,b'cover'))
  photo,body=fetch.frame(self.db,'123',2,cache_root=self.cache,source_media_id='789',source_kind='wechat_channels')
  self.assertEqual((photo['slot'],photo['source_media_id'],photo['source_md5'],body),(2,'789','source-md5',b'photo'))
 def test_legacy_channels_flag_does_not_change_regular_slot_fetch(self):
  with sqlite3.connect(self.db) as db:
   db.execute('INSERT INTO SnsTimeLine VALUES(?,?)',('125','<TimelineObject><id>125</id><ContentObject><type>54</type><mediaList><media><id>890</id><url>http://szmmsns.qpic.cn/channels/0</url></media></mediaList></ContentObject></TimelineObject>'))
  self.cached('https://szmmsns.qpic.cn/channels/0',b'regular image')
  header,body=fetch.frame(self.db,'125',1,cache_root=self.cache,source_media_id='890',source_kind='wechat_channels')
  self.assertEqual((header['source_media_id'],body),('890',b'regular image'))
 def test_type_54_album_keeps_photos_and_identifies_nested_motion(self):
  still='http://szmmsns.qpic.cn/photo/0'
  movie='http://szzjwxsns.video.qq.com/movie'
  xml=f'<TimelineObject><id>126</id><contentDesc>&#2;</contentDesc><ContentObject><type>54</type><mediaList><media><id>still</id><type>2</type><url>{still}</url><LivePhoto><liveMedia><id>motion</id><type>6</type><url>{movie}</url></liveMedia></LivePhoto></media></mediaList></ContentObject></TimelineObject>'
  with sqlite3.connect(self.db) as db:
   db.execute('INSERT INTO SnsTimeLine VALUES(?,?)',('126',xml.encode() + b'\0'))
  _,obj=fetch.post_xml(xml)
  [photo]=moments.moments_media(obj)
  [motion]=fetch.live_assets(obj)
  self.assertEqual(photo['media_type'],'image')
  self.assertNotIn('source_kind',photo)
  self.assertEqual(photo['live_motion_url'],movie)
  self.assertEqual((motion['role'],motion['parent_media_id']),('live-media-1','still'))
  self.cached(movie,b'original motion')
  dest=self.root/'movie.mp4'
  header,_=fetch.frame(self.db,'126',1,cache_root=self.cache,destination=dest,source_media_id='motion',live_motion=True)
  self.assertEqual(header['raw_xml_sha256'],hashlib.sha256(xml.encode()).hexdigest())
  self.assertEqual((header['parent_media_id'],header['source_media_id'],dest.read_bytes()),('still','motion',b'original motion'))

 def test_native_video_poster_uses_its_source_thumbnail_cdn(self):
  with sqlite3.connect(self.db)as db:db.execute('INSERT INTO SnsTimeLine VALUES(?,?)',('124','<TimelineObject><id>124</id><ContentObject><mediaList><media><id>987</id><type>15</type><url>http://snsvideo.c2c.wechat.com/video</url><thumb>http://vweixinthumb.tc.qq.com/poster/0</thumb></media></mediaList></ContentObject></TimelineObject>'))
  self.cached('http://vweixinthumb.tc.qq.com/poster/0',b'video poster')
  header,body=fetch.frame(self.db,'124',1,cache_root=self.cache,thumbnail=True,source_media_id='987')
  self.assertEqual((header['source_media_id'],body),('987',b'video poster'))
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


 def test_changed_source_id_cannot_be_replaced_by_another_image(self):
  with self.assertRaises((ValueError,IndexError)):fetch.frame(self.db,'123',2,cache_root=self.cache,source_media_id='wrong')
 def test_streaming_mp4_preserves_the_tail_and_rejects_truncation(self):
  body=struct.pack('>I',24)+b'ftypisom'+bytes(307200);header=json.dumps(dict(size=len(body),seed='')).encode();frame=struct.pack('>I',len(header))+header+body
  command=['node',str(ROOT/'tools/sns-decrypt/decrypt-video.cjs'),str(self.root/'unused-wasm')]
  good=subprocess.run(command,input=frame,capture_output=True,timeout=10)
  self.assertEqual((good.returncode,good.stdout,good.stderr),(0,body,b''))
  short=subprocess.run(command,input=frame[:-1],capture_output=True,timeout=10)
  self.assertNotEqual(short.returncode,0);self.assertEqual(json.loads(short.stderr)['error_code'],'incomplete frame')
if __name__=='__main__':unittest.main()

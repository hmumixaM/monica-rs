"""Source identity and streaming contracts, with no network or real source data."""
import hashlib,importlib.util,json,sqlite3,struct,subprocess,tempfile,unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('frame',ROOT/'tools/sns-decrypt/fetch_frame.py');fetch=importlib.util.module_from_spec(spec);spec.loader.exec_module(fetch)
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
  photo,body=fetch.frame(self.db,'123',2,cache_root=self.cache,source_media_id='789')
  self.assertEqual((photo['slot'],photo['source_media_id'],photo['source_md5'],body),(2,'789','source-md5',b'photo'))
 def test_native_video_poster_uses_its_source_thumbnail_cdn(self):
  with sqlite3.connect(self.db)as db:db.execute('INSERT INTO SnsTimeLine VALUES(?,?)',('124','<TimelineObject><id>124</id><ContentObject><mediaList><media><id>987</id><type>15</type><url>http://snsvideo.c2c.wechat.com/video</url><thumb>http://vweixinthumb.tc.qq.com/poster/0</thumb></media></mediaList></ContentObject></TimelineObject>'))
  self.cached('http://vweixinthumb.tc.qq.com/poster/0',b'video poster')
  header,body=fetch.frame(self.db,'124',1,cache_root=self.cache,thumbnail=True,source_media_id='987')
  self.assertEqual((header['source_media_id'],body),('987',b'video poster'))
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

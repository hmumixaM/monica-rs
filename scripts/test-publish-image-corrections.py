import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('publisher', Path(__file__).with_name('publish-image-corrections.py'))
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


def png(color):
    # Tiny valid 3x2 fixtures keep this publisher check runnable on NAS Python.
    return base64.b64decode({
        'blue':'iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAIAAAASFvFNAAAAFElEQVR4nGNkYPjPAAZMEIqBgQEAESsBA37ueu4AAAAASUVORK5CYII=',
        'green':'iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAIAAAASFvFNAAAAEklEQVR4nGNkaGCAACYozcAAAAk8AITOVm/vAAAAAElFTkSuQmCC'
    }[color])


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name).resolve()
        bundle, media, raw = [base / n for n in ('bundle','media','raw')]
        for p in (bundle,media,raw):
            p.mkdir()
        (bundle / '.monica-image-correction-bundle').write_text('monica-image-correction-bundle-v1\n')
        (media / '.monica-rust-migration-media').write_text(publisher.helper.MARKER)
        for folder in ('objects','receipts','inputs','sources/plain','sources/raw'):
            (bundle / folder).mkdir(parents=True,exist_ok=True)
        original_raw = b'Synthetic retained frozen source bytes'
        (raw / 'msg').mkdir()
        (raw / 'msg/synthetic.dat').write_bytes(original_raw)
        source_sha = hashlib.sha256(original_raw).hexdigest()
        old_data = b'BM-invalid-compressed-fragment'
        old_sha = hashlib.sha256(old_data).hexdigest()
        self.old_cas = media / 'sha256' / old_sha[:2] / old_sha[2:4] / old_sha
        self.old_cas.parent.mkdir(parents=True)
        self.old_cas.write_bytes(old_data)
        old_asset = json.dumps(dict(sha256=old_sha,byte_length=len(old_data),media_type='image/bmp',object_path='legacy'))
        blue, green = png('blue'), png('green')
        blue_sha, green_sha = hashlib.sha256(blue).hexdigest(), hashlib.sha256(green).hexdigest()
        self.golden = bundle / 'sources/plain' / blue_sha
        self.golden.write_bytes(blue)
        (bundle / 'sources/raw' / blue_sha).write_bytes(blue)
        for sha, data in ((blue_sha,blue),(green_sha,green)):
            (bundle / 'objects' / sha).write_bytes(data)
        assets = lambda sha, data: dict(sha256=sha,byte_length=len(data),media_type='image/png',object_path='sha256/'+sha[:2]+'/'+sha[2:4]+'/'+sha,width=3,height=2)
        primary, shadow, index = [base / n for n in ('primary.sqlite','shadow.sqlite','index.sqlite')]
        with sqlite3.connect(index) as d:
            d.execute('CREATE TABLE files(source_key TEXT,kind TEXT,md5 TEXT,expected_bytes INT)')
            d.executemany('INSERT INTO files VALUES(?,?,?,?)', [('bmp','image','a'*32,len(original_raw)),('missing','image',hashlib.md5(blue).hexdigest(),len(blue))])
        index_hash = publisher.helper.checksum(index)
        account = dict(snapshot='synthetic',own_id='synthetic-account',input_sha256=index_hash)
        with sqlite3.connect(primary) as d:
            d.executescript('CREATE TABLE metadata(key TEXT,value TEXT); CREATE TABLE assets(source_key TEXT PRIMARY KEY,kind TEXT,md5 TEXT,source_path TEXT,raw_sha256 TEXT,asset TEXT,status TEXT);')
            d.executemany('INSERT INTO metadata VALUES(?,?)',account.items())
            d.executemany('INSERT INTO assets VALUES(?,?,?,?,?,?,?)', [('bmp','image','a'*32,'msg/synthetic.dat',source_sha,old_asset,'ready'),('missing','image',hashlib.md5(blue).hexdigest(),None,None,None,'source_missing')])
            primary_digest = publisher.digest_rows(d)
        shutil.copyfile(primary,shadow)
        with sqlite3.connect(shadow) as d:
            d.execute("UPDATE assets SET status='decode_failed' WHERE source_key='bmp'")
        records, packets = [], []
        for key, category, input_sha, old, asset, path in [
                ('bmp','bmp',source_sha,old_sha,assets(green_sha,green),'msg/synthetic.dat'),
                ('missing','source_missing',blue_sha,None,assets(blue_sha,blue),blue_sha)]:
            source_manifest = bundle / 'inputs' / (key+'.json')
            source_manifest.write_text(json.dumps(dict(metadata=account,rows=[[key,path,input_sha,old_asset if old else 'null']])))
            input_hash = publisher.helper.checksum(source_manifest)
            receipt = bundle / 'receipts' / (key+'.sqlite')
            with sqlite3.connect(receipt) as d:
                d.executescript('CREATE TABLE metadata(key TEXT,value TEXT); CREATE TABLE results(source_key TEXT,raw_sha256 TEXT,old_asset_sha256 TEXT,code TEXT,asset TEXT,frames INT,finished_at REAL);')
                d.executemany('INSERT INTO metadata VALUES(?,?)',dict(source_manifest_sha256=input_hash,codec_commit='a'*40,binary_sha256='b'*64,index_metadata_json=json.dumps(account)).items())
                d.execute('INSERT INTO results VALUES(?,?,?,?,?,?,?)',(key,input_sha,old,'ready',json.dumps(asset),1,0))
            packets.append(dict(path='receipts/'+key+'.sqlite',sha256=publisher.helper.checksum(receipt),input='inputs/'+key+'.json',input_sha256=input_hash))
            records.append(dict(key=key,category=category,input_sha256=input_sha,source_raw_sha256=input_sha,old_sha256=old,asset=asset,frames=1))
        manifest = dict(version=1,index_sha256=index_hash,primary_digest=primary_digest,account_metadata=account,codec_commit='a'*40,codec_sha256='b'*64,receipts=packets,records=records)
        (bundle / 'manifest.json').write_text(json.dumps(manifest))
        copy_lock, recovery_lock = base / 'copy.lock',base / 'recovery.lock'
        copy_lock.touch();recovery_lock.touch()
        self.args = argparse.Namespace(bundle=bundle,media=media,raw=raw,primary=primary,shadow=shadow,index=index,copy_lock=copy_lock,recovery_lock=recovery_lock,manifest_sha256=publisher.helper.checksum(bundle/'manifest.json'),index_sha256=index_hash,expected_records=2,owner_uid=os.getuid(),owner_gid=os.getgid())
        self.primary_before = primary.read_bytes()
        self.shadow_before = shadow.read_bytes()

    def test_publish_preserves_primary_and_old_bytes_and_resumes(self):
        result = publisher.publish(self.args)
        self.assertEqual(result['shadow_counts'], {'ready':2})
        self.assertEqual(self.args.primary.read_bytes(), self.primary_before)
        self.assertEqual(self.old_cas.read_bytes(), b'BM-invalid-compressed-fragment')
        self.assertEqual(publisher.publish(self.args)['shadow_counts'], {'ready':2})
        with sqlite3.connect(self.args.shadow) as d:
            self.assertEqual(d.execute('SELECT count(*) FROM image_correction_receipts').fetchone()[0],2)

    def test_corrupt_golden_source_refuses_before_any_publication(self):
        self.golden.write_bytes(b'wrong bytes')
        with self.assertRaises(Exception):
            publisher.publish(self.args)
        self.assertEqual(self.args.primary.read_bytes(),self.primary_before)
        self.assertEqual(self.args.shadow.read_bytes(),self.shadow_before)
        self.assertEqual(len(list(self.args.media.glob('sha256/*/*/*'))),1)

    def test_same_expected_raw_hash_does_not_skip_a_different_damaged_path(self):
        (self.args.raw / 'msg/damaged.dat').write_bytes(b'damaged second source copy')
        with sqlite3.connect(self.args.primary) as d:
            row = d.execute("SELECT * FROM assets WHERE source_key='bmp'").fetchone()
        alias = ('bmp-alias',) + row[1:3] + ('msg/damaged.dat',) + row[4:]
        for path, status in ((self.args.primary,'ready'),(self.args.shadow,'decode_failed')):
            with sqlite3.connect(path) as d:
                d.execute('INSERT INTO assets VALUES(?,?,?,?,?,?,?)',alias[:6]+(status,))
        with sqlite3.connect(self.args.index) as d:
            d.execute('INSERT INTO files SELECT ?,kind,md5,expected_bytes FROM files WHERE source_key=?',('bmp-alias','bmp'))
        self.args.index_sha256 = publisher.helper.checksum(self.args.index)
        for path in (self.args.primary,self.args.shadow):
            with sqlite3.connect(path) as d:
                d.execute("UPDATE metadata SET value=? WHERE key='input_sha256'",(self.args.index_sha256,))
        manifest_path = self.args.bundle / 'manifest.json'
        manifest = json.loads(manifest_path.read_text())
        manifest['index_sha256'] = self.args.index_sha256
        manifest['account_metadata']['input_sha256'] = self.args.index_sha256
        original = manifest['records'][0]
        manifest['records'].append(dict(original,key='bmp-alias'))
        for packet in manifest['receipts']:
            input_path = self.args.bundle / packet['input']
            data = json.loads(input_path.read_text())
            data['metadata'] = manifest['account_metadata']
            if packet['path']=='receipts/bmp.sqlite':
                data['rows'].append(['bmp-alias','msg/damaged.dat',row[4],row[5]])
            input_path.write_text(json.dumps(data))
            packet['input_sha256'] = publisher.helper.checksum(input_path)
            receipt_path = self.args.bundle / packet['path']
            with sqlite3.connect(receipt_path) as d:
                d.execute("UPDATE metadata SET value=? WHERE key='source_manifest_sha256'",(packet['input_sha256'],))
                d.execute("UPDATE metadata SET value=? WHERE key='index_metadata_json'",(json.dumps(manifest['account_metadata']),))
                if packet['path']=='receipts/bmp.sqlite':
                    d.execute("INSERT INTO results SELECT ?,raw_sha256,old_asset_sha256,code,asset,frames,finished_at FROM results WHERE source_key='bmp'",('bmp-alias',))
            packet['sha256'] = publisher.helper.checksum(receipt_path)
        with sqlite3.connect(self.args.primary) as d:
            manifest['primary_digest'] = publisher.digest_rows(d)
        manifest_path.write_text(json.dumps(manifest))
        self.args.manifest_sha256 = publisher.helper.checksum(manifest_path)
        self.args.expected_records = 3
        primary_before,shadow_before = self.args.primary.read_bytes(),self.args.shadow.read_bytes()
        with self.assertRaises(ValueError):
            publisher.publish(self.args)
        self.assertEqual(self.args.primary.read_bytes(),primary_before)
        self.assertEqual(self.args.shadow.read_bytes(),shadow_before)
        self.assertEqual(len(list(self.args.media.glob('sha256/*/*/*'))),1)


if __name__ == '__main__':
    unittest.main()

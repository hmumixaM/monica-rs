"""Recovered ready assets must remain consumable without inventing source paths."""
from contextlib import closing
import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('linker',Path(__file__).with_name('legacy-wechat-file-links.py'))
linker=importlib.util.module_from_spec(spec);spec.loader.exec_module(linker)

class RecoveryLinkContract(unittest.TestCase):
    def test_real_restoration_ledger_builds_indexed_names_and_content_keys(self):
        test_spec=importlib.util.spec_from_file_location('restore_test',Path(__file__).with_name('test-restore-verified-wechat-sources.py'))
        tests=importlib.util.module_from_spec(test_spec);test_spec.loader.exec_module(tests)
        fixture=tests.RestoreSourceTests()
        fixture.setUp()
        try:
            result=tests.restore.restore(fixture.args,lambda:None)
            self.assertEqual(result['recovered'],1)
            with closing(sqlite3.connect(fixture.args.index))as index:
                index.execute('ATTACH DATABASE ? AS copied',(str(fixture.args.state/'wechat-file-assets.sqlite'),))
                old=linker.EXPECTED;linker.EXPECTED=fixture.args.index_sha256
                try:linker.prepare_media_keys(index,fixture.args.state,'snapshot')
                finally:linker.EXPECTED=old
                self.assertEqual(index.execute('SELECT count(*) FROM media_keys.keys').fetchone()[0],1)
                self.assertEqual(index.execute("SELECT source_path FROM copied.assets WHERE source_key='missing'").fetchone()[0],None)
                self.assertEqual(index.execute('SELECT file_name FROM media_keys.names').fetchone()[0],'synthetic.txt')
                index.execute('DELETE FROM copied.verified_source_recoveries')
                kind,digest,raw,asset=index.execute("SELECT kind,md5,raw_sha256,asset FROM copied.assets WHERE source_key='missing'").fetchone()
                with self.assertRaises(ValueError):linker.proven_source_paths(index,{'verified_source_recoveries'},'missing',kind,digest,['msg/synthetic.txt'],None,raw,json.loads(asset))
        finally:fixture.doCleanups()

    def test_corrected_image_requires_matching_receipt_and_preserves_ordinary_checks(self):
        with tempfile.TemporaryDirectory()as directory,closing(sqlite3.connect(':memory:'))as index:
            index.execute("ATTACH DATABASE ':memory:' AS copied")
            index.execute('CREATE TABLE copied.image_correction_receipts(source_key,category,original_row,asset,source_raw_sha256,input_sha256,codec_commit,manifest_sha256,frames)')
            asset={'sha256':'a'*64,'byte_length':19,'media_type':'image/jpeg'}
            original=['image-key','image','b'*32,None,None,None,'source_missing']
            index.execute('INSERT INTO copied.image_correction_receipts VALUES(?,?,?,?,?,?,?,?,?)',('image-key','source_missing',json.dumps(original),json.dumps(asset),'c'*64,'d'*64,'e'*40,'f'*64,1))
            args=(index,{'image_correction_receipts'},'image-key','image','b'*32,['msg/original.dat'],None,'c'*64,asset)
            self.assertEqual(linker.proven_source_paths(*args),['msg/original.dat'])
            index.execute("UPDATE copied.image_correction_receipts SET asset='{}'")
            with self.assertRaises(ValueError):linker.proven_source_paths(*args)
            self.assertEqual(linker.proven_source_paths(index,set(),'legacy','image','b'*32,['msg/actual.dat'],'msg/actual.dat','c'*64,asset),['msg/actual.dat'])
            with self.assertRaises(ValueError):linker.proven_source_paths(index,set(),'legacy','image','b'*32,['msg/actual.dat'],'msg/other.dat','c'*64,asset)

    def test_duplicate_presentation_uses_only_unique_frozen_row_evidence(self):
        source=dict(local_id=114,server_id=2842852990818986645,create_time=1636266057,local_type=3)
        candidates=[('legacy-presentation',{'id':'old-presentation'},None),('frozen-message',source,source)]
        self.assertEqual(linker.message_with_frozen_row(candidates,114,2842852990818986645,1636266057,'3'),'frozen-message')
        with self.assertRaises(ValueError):linker.message_with_frozen_row(candidates,115,2842852990818986645,1636266057,'3')
        with self.assertRaises(ValueError):linker.message_with_frozen_row([*candidates,('another-proven-message',source,None)],114,2842852990818986645,1636266057,'3')
        with self.assertRaises(ValueError):linker.message_with_frozen_row([('fractional',{**source,'local_id':114.5},None)],114,2842852990818986645,1636266057,'3')

if __name__=='__main__':unittest.main()

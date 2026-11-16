import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from PIL import Image

spec = importlib.util.spec_from_file_location('correction', Path(__file__).with_name('redecode-known-image-snapshot.py'))
correction = importlib.util.module_from_spec(spec)
spec.loader.exec_module(correction)


@unittest.skipUnless(os.environ.get('MONICA_REVIEWED_CODEC'), 'provide the built reviewed codec for this integration check')
class CorrectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name).resolve()
        source, target = base / 'source', base / 'corrections'
        source.mkdir()
        target.mkdir()
        target.chmod(0o700)
        (target / '.monica-rust-migration-media').write_text(correction.helper.MARKER)
        image = source / 'synthetic.dat'
        Image.new('RGB', (3, 2), 'blue').save(image, format='PNG')
        self.original = image.read_bytes()
        manifest = base / 'input.json'
        manifest.write_text(json.dumps(dict(metadata={'input_sha256': 'index-hash', 'codec_commit': 'source-metadata'},
            rows=[['synthetic-key', image.name, hashlib.sha256(self.original).hexdigest(),
                   json.dumps({'sha256': 'a' * 64, 'media_type': 'image/bmp'})]])))
        codec = Path(os.environ['MONICA_REVIEWED_CODEC']).resolve()
        self.args = argparse.Namespace(source=source, target=target, input=manifest, codec=codec,
            codec_sha256=correction.helper.checksum(codec),
            codec_commit='e437f63b31603faa9801bbb6d8a03185d0579770',
            codec_path='/opt/homebrew/bin:/usr/bin:/bin', journal=base / 'receipts.sqlite', batch_size=2)

    def test_real_codec_pixels_namespaced_metadata_and_resume(self):
        result = correction.run(self.args, {'image_key': '', 'image_xor': ''})
        self.assertEqual(result['counts'], {'ready': 1})
        self.assertEqual((self.args.source / 'synthetic.dat').read_bytes(), self.original)
        correction.run(self.args, {'image_key': '', 'image_xor': ''})
        with sqlite3.connect(self.args.journal) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM results').fetchone()[0], 1)
            metadata = dict(db.execute('SELECT key,value FROM metadata'))
        self.assertEqual(json.loads(metadata['index_metadata_json'])['codec_commit'], 'source-metadata')

    def test_journal_inside_original_source_refuses_without_writing(self):
        self.args.journal = self.args.source / 'original.sqlite'
        with self.assertRaises(ValueError):
            correction.run(self.args, {'image_key': '', 'image_xor': ''})
        self.assertFalse(self.args.journal.exists())

    def test_resume_refuses_another_empty_cas_root(self):
        correction.run(self.args, {'image_key': '', 'image_xor': ''})
        other = self.args.target.with_name('other-corrections')
        other.mkdir(mode=0o700)
        (other / '.monica-rust-migration-media').write_text(correction.helper.MARKER)
        self.args.target = other
        with self.assertRaises(ValueError):
            correction.run(self.args, {'image_key': '', 'image_xor': ''})

    def test_resume_refuses_missing_previously_verified_object(self):
        correction.run(self.args, {'image_key': '', 'image_xor': ''})
        with sqlite3.connect(self.args.journal) as db:
            asset = json.loads(db.execute('SELECT asset FROM results').fetchone()[0])
        (self.args.target / asset['object_path']).unlink()
        with self.assertRaises(Exception):
            correction.run(self.args, {'image_key': '', 'image_xor': ''})

    def test_hardlinked_original_is_read_without_changing_either_name(self):
        source = self.args.source / 'synthetic.dat'
        alias = self.args.source / 'same-source.dat'
        os.link(source, alias)
        self.assertEqual(correction.run(self.args, {'image_key': '', 'image_xor': ''})['counts'], {'ready': 1})
        self.assertEqual(source.read_bytes(), self.original)
        self.assertEqual(alias.read_bytes(), self.original)
        self.assertEqual(source.stat().st_nlink, 2)

    def test_previously_missing_image_has_no_invented_previous_asset_hash(self):
        manifest = json.loads(self.args.input.read_text())
        manifest['rows'][0][3] = 'null'
        self.args.input.write_text(json.dumps(manifest))
        self.assertEqual(correction.run(self.args, {'image_key': '', 'image_xor': ''})['counts'], {'ready': 1})
        with sqlite3.connect(self.args.journal) as db:
            self.assertIsNone(db.execute('SELECT old_asset_sha256 FROM results').fetchone()[0])


if __name__ == '__main__':
    unittest.main()

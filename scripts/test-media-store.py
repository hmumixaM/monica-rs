"""Normal CAS publication: durability, replay, source immutability and conflicts."""
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest

from monica_imports.files import atomic_json
from monica_imports.media_store import MARKER, MediaStore


class MediaStoreContract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.media = self.root/'media'
        self.media.mkdir()
        (self.media/'.monica-rust-migration-media').write_text(MARKER)
        self.source = self.root/'source'
        self.source.write_bytes(b'Original source media bytes')
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        self.length = self.source.stat().st_size
        self.store = MediaStore(self.media)

    def test_publish_replay_keeps_source_and_existing_inode(self):
        path = self.store.publish(self.source, self.digest, self.length)
        target = self.media/path
        before = target.stat().st_ino
        self.assertEqual(self.store.publish(self.source, self.digest, self.length), path)
        self.assertEqual(target.stat().st_ino, before)
        self.assertEqual(target.read_bytes(), self.source.read_bytes())
        self.assertEqual(target.stat().st_nlink, 1)
        self.assertFalse(list(target.parent.glob('*.partial')))

    def test_bad_input_and_existing_corruption_are_never_overwritten(self):
        with self.assertRaises(ValueError):
            self.store.publish(self.source, 'f'*64, self.length)
        path = self.store.publish(self.source, self.digest, self.length)
        target = self.media/path
        target.write_bytes(b'keep corrupt bytes for investigation')
        with self.assertRaisesRegex(ValueError, 'media_content_conflict'):
            self.store.publish(self.source, self.digest, self.length)
        self.assertEqual(target.read_bytes(), b'keep corrupt bytes for investigation')
        self.assertEqual(self.source.read_bytes(), b'Original source media bytes')

    def test_interrupted_link_finishes_only_its_owned_temporary(self):
        path = self.store.publish(self.source, self.digest, self.length)
        target = self.media/path
        alias = target.parent/('.media-publish-'+'a'*32+'.partial')
        os.link(target, alias)
        self.store.publish(self.source, self.digest, self.length)
        self.assertFalse(alias.exists())
        unrelated = self.root/'unrelated-link'
        os.link(target, unrelated)
        with self.assertRaisesRegex(ValueError, 'unowned_hardlinks'):
            self.store.publish(self.source, self.digest, self.length)
        self.assertTrue(unrelated.exists())
        self.assertEqual(target.read_bytes(), self.source.read_bytes())

    def test_receipt_replacement_never_follows_a_symlink(self):
        path = self.root/'receipt.json'
        atomic_json(path, {'complete': False})
        atomic_json(path, {'complete': True})
        self.assertEqual(json.loads(path.read_text()), {'complete': True})
        path.unlink()
        path.symlink_to(self.source)
        with self.assertRaises(ValueError):
            atomic_json(path, {})
        self.assertEqual(self.source.read_bytes(), b'Original source media bytes')


if __name__ == '__main__':
    unittest.main()

#!/usr/bin/env python3
"""Run the actual bounded media CLI without server runtime/database configuration."""
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
import zlib

ROOT = Path(__file__).resolve().parents[1]
SERVER = Path(os.environ.get('MONICA_TEST_SERVER', ROOT / 'target/debug/monica-server')).resolve()


class MediaHelper(unittest.TestCase):
    def test_probe_and_preview_do_not_need_server_runtime_configuration(self):
        # This is irrelevant to a synchronous decoder. Parsing it would start
        # server workers before the decoder installs its process resource limits.
        env = {**os.environ, 'TOKIO_WORKER_THREADS': 'server-runtime-not-applicable',
               'MONICA_DATABASE_URL': 'not-a-real-database'}
        with tempfile.TemporaryDirectory(prefix='monica-media-cli-') as temporary:
            root = Path(temporary)

            def helper(*args):
                result = subprocess.run([str(SERVER), *map(str, args)], env=env,
                                        cwd=root, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                return json.loads(result.stdout)

            text = root / 'synthetic.txt'
            text.write_text('Synthetic media body\n' * 5000)
            self.assertEqual(helper('--monica-media-probe', text, text.name)['media_type'], 'text/plain')

            def chunk(kind, data):
                return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))

            png = root / 'synthetic.png'
            png.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!IIBBBBB', 16, 8, 8, 2, 0, 0, 0))
                            + chunk(b'IDAT', zlib.compress((b'\0' + b'\x40\x80\xc0' * 16) * 8)) + chunk(b'IEND', b''))
            info = helper('--monica-media-probe', png, png.name)
            self.assertEqual((info['media_type'], info['width'], info['height']), ('image/png', 16, 8))
            preview = root / 'preview.webp'
            self.assertEqual(helper('--monica-media-preview', png, preview, 'null'), {})
            info = helper('--monica-media-probe', preview, preview.name)
            self.assertEqual(info['media_type'], 'image/webp')
            self.assertTrue(0 < info['width'] <= 480 and 0 < info['height'] <= 480)


if __name__ == '__main__':
    unittest.main()

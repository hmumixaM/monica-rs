#!/usr/bin/env python3
"""Offline parsing and execution-boundary regressions, with no source accounts."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import unittest

SCRIPT = Path(__file__).with_name('legacy-wechat-emoji.py')
spec = importlib.util.spec_from_file_location('emoji', SCRIPT)
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class Emoji(unittest.TestCase):
    def test_only_structured_emoji_md5_is_a_shared_identity(self):
        key = 'a' * 32
        self.assertEqual(worker.references(key, '<msg><img md5="' + key + '"/></msg>'), [])
        self.assertEqual(worker.references({'chatlog_original': {'body': 'wxid:\n<msg><emoji md5="' + key.upper() + '" cdnurl="https://example.invalid/private"/></msg>'}}, '<emoji md5="' + key + '"/>'), [key])
        self.assertEqual(worker.references('<!DOCTYPE msg [<!ENTITY x "' + key + '">]><msg><emoji md5="&x;"/></msg>'), [])
        self.assertEqual(worker.references('<emoji md5="' + '0' * 32 + '"/>'), [])

    def test_execution_requires_explicit_run_without_importing_database_driver(self):
        result = subprocess.run([sys.executable, str(SCRIPT)], env={'PATH': os.environ['PATH']}, capture_output=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn(b'--run', result.stdout)


if __name__ == '__main__':
    unittest.main()

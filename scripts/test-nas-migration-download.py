#!/usr/bin/env python3
"""Verify download receipts remain bound to Chatlog after other copies run."""
from contextlib import redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('download',Path(__file__).with_name('nas-migration-download.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class DownloadContract(unittest.TestCase):
    def test_completed_chatlog_receipt_survives_another_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            (root/'copy-status.json').write_text(json.dumps({'source':'media','state':'running'}))
            (root/'copy-status-chatlog-db.json').write_text(json.dumps({'source':'chatlog-db','state':'complete'}))
            with sqlite3.connect(root/'chatlog-db.sqlite') as db:
                db.execute('CREATE TABLE files(path,sha256,bytes,mode)')
                db.execute("INSERT INTO files VALUES('message/a.db','synthetic-checksum',12,'sqlite')")
            output=io.StringIO();namespace={}
            with redirect_stdout(output):exec(m.manifest_program(root),namespace)
            namespace['db'].close()
            manifest=json.loads(output.getvalue())
            self.assertEqual(manifest['status']['source'],'chatlog-db')
            self.assertEqual(manifest['files'],[{'path':'message/a.db','sha256':'synthetic-checksum','bytes':12,'mode':'sqlite'}])
            (root/'copy-status-chatlog-db.json').write_text(json.dumps({'source':'chatlog-db','state':'paused'}))
            with self.assertRaisesRegex(RuntimeError,'must complete'):exec(m.manifest_program(root),{})

    def test_privileged_read_keeps_credentials_out_of_command(self):
        command=m.privileged_command('synthetic-host','print("synthetic receipt")')
        self.assertEqual(command[-2],'synthetic-host')
        self.assertTrue(command[-1].startswith("sudo -S -p '' /usr/bin/python3"))
        self.assertIn('print(',command[-1])

if __name__=='__main__':unittest.main()

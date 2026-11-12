"""Exercise the real Objective-C reader against a live WAL SQLite fixture."""
import ctypes
import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


@unittest.skipUnless(sys.platform == 'darwin', 'macOS native reader')
class NativeMessagesTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.folder = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.folder.cleanup)
        output = Path(cls.folder.name) / 'messages.dylib'
        source = Path(__file__).resolve().parents[1] / 'src-tauri/native/messages.m'
        subprocess.run(['xcrun', 'clang', '-shared', '-fobjc-arc', '-fmodules',
                        '-framework', 'Foundation', '-lsqlite3', str(source), '-o', str(output)], check=True, capture_output=True)
        cls.library = ctypes.CDLL(str(output))
        cls.read = cls.library.monica_messages_read_path
        cls.read.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
        cls.read.restype = ctypes.c_void_p
        cls.free = ctypes.CDLL(None).free
        cls.free.argtypes = [ctypes.c_void_p]

    def test_wal_pagination_edits_retractions_and_group_senders(self):
        path = Path(self.folder.name) / 'chat.db'
        with sqlite3.connect(path) as db:
            db.executescript('''PRAGMA journal_mode=WAL;
                CREATE TABLE message(guid TEXT,text TEXT,attributedBody BLOB,date INTEGER,is_from_me INTEGER,handle_id INTEGER,date_edited INTEGER,date_retracted INTEGER);
                CREATE TABLE handle(id TEXT);
                CREATE TABLE chat_handle_join(chat_id INTEGER,handle_id INTEGER);
                CREATE TABLE chat_message_join(chat_id INTEGER,message_id INTEGER);
                CREATE TABLE attachment(guid TEXT,filename TEXT,mime_type TEXT,transfer_name TEXT);
                CREATE TABLE message_attachment_join(message_id INTEGER,attachment_id INTEGER);
                INSERT INTO handle VALUES('+15550000001'),('friend@example.org');
                INSERT INTO chat_handle_join VALUES(1,1),(1,2);
            ''')
            db.executemany('INSERT INTO message VALUES(?,?,NULL,800000000000000000,0,1,0,0)',
                           [(f'm{i}', f'body {i}') for i in range(1, 251)])
            db.executemany('INSERT INTO chat_message_join VALUES(1,?)', [(i,) for i in range(1,251)])
            db.commit()
            cursor = {}
            seen = {}
            def poll():
                nonlocal cursor
                raw = self.read(str(path).encode(), json.dumps(cursor).encode())
                try: result = json.loads(ctypes.string_at(raw))
                finally: self.free(raw)
                self.assertNotIn('error', result)
                cursor = result['cursor']
                for row in result['records']: seen[row['external_id']] = row
                return result['records']
            first = poll()
            self.assertEqual(len(first),100)
            self.assertEqual(first[0]['external_id'],'m250')
            self.assertIs(first[0]['deleted'], False)
            self.assertEqual(len(first[0]['participants']),2)
            self.assertEqual(first[0]['participants'][0]['role'],'sender')
            for _ in range(25): poll()
            self.assertEqual(len(seen),250)
            db.execute("UPDATE message SET text='edited',date_edited=900000000000000000 WHERE rowid<=150")
            db.execute("UPDATE message SET date_retracted=900000000000000001 WHERE rowid=250")
            db.execute("INSERT INTO message VALUES('m251','new WAL',NULL,800000000000000001,1,0,0,0)")
            db.execute('INSERT INTO chat_message_join VALUES(1,251)')
            db.commit()
            for _ in range(18): poll()
            self.assertTrue(all(seen[f'm{i}']['body']=='edited' for i in range(1,151)))
            self.assertTrue(seen['m250']['deleted'])
            self.assertEqual(seen['m251']['direction'],'sent')
            self.assertTrue(next(p for p in seen['m251']['participants'] if p['role']=='sender')['is_self'])


if __name__ == '__main__': unittest.main()

#!/usr/bin/env python3
"""Synthetic control, backpressure and process cleanup checks; no collectors/accounts."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SPEC = importlib.util.spec_from_file_location('collector_supervisor', Path(__file__).resolve().parents[1] / 'infra/collector-supervisor.py')
supervisor = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(supervisor)
TOKEN = 'synthetic-supervisor-control-token-32bytes'


class Controls(unittest.TestCase):
    def setUp(self):
        self.runtime = {'state': 'running'}
        self.rows = [{'provider': p, 'enabled': True, 'paused': False} for p in ['linkedin', 'linkedin_messages']]
        self.mode = 'ok'
        self.requests = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                owner.requests.append((self.path, self.headers.get('Authorization')))
                if owner.mode == 'redirect':
                    self.send_response(302)
                    self.send_header('Location', owner.base + '/must-not-receive-token')
                    self.end_headers()
                    return
                self.send_response(503 if owner.mode == 'unavailable' else 200)
                self.end_headers()
                if owner.mode == 'oversized':
                    self.wfile.write(b'x' * 65537)
                elif owner.mode == 'invalid':
                    self.wfile.write(b'{')
                else:
                    self.wfile.write(json.dumps(owner.runtime if self.path.endswith('/status') else owner.rows).encode())

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.base = 'http://127.0.0.1:' + str(self.server.server_port)
        self.worker = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.worker.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(timeout=5)

    def allowed(self, lane='messages'):
        return supervisor.controls(self.base, TOKEN, 'linkedin', lane)

    def test_pause_missing_lane_and_control_failures_stop_admission(self):
        self.assertTrue(self.allowed())
        self.assertTrue(all(auth == 'Bearer ' + TOKEN for _, auth in self.requests))
        for state in ['paused', 'draining', 'stopping']:
            self.runtime['state'] = state
            self.assertFalse(self.allowed())
        self.runtime['state'] = 'running'
        self.rows[1]['paused'] = True
        self.assertFalse(self.allowed())
        self.assertTrue(self.allowed('profiles'))
        self.rows.pop()
        self.assertFalse(self.allowed())
        self.rows[0]['enabled'] = False
        self.assertFalse(self.allowed('profiles'))
        for mode in ['unavailable', 'invalid', 'oversized']:
            self.mode = mode
            self.assertFalse(self.allowed(), mode)

    def test_redirect_never_receives_control_token(self):
        self.mode = 'redirect'
        self.assertFalse(self.allowed())
        self.assertEqual([path for path, _ in self.requests], ['/internal/runtime/status'])


class FilesAndProcesses(unittest.TestCase):
    def test_backpressure_counts_only_complete_ready_pairs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for i in range(8):
                (root / f'{i}.ready').touch()
            self.assertFalse(supervisor.pending(root))
            for i in range(7):
                (root / f'{i}.jsonl').touch()
            self.assertFalse(supervisor.pending(root))
            (root / '7.jsonl').touch()
            self.assertTrue(supervisor.pending(root))

    @unittest.skipUnless(os.name == 'posix', 'managed collectors require POSIX process groups')
    def test_cancel_stops_stubborn_child_and_browser_descendant(self):
        with tempfile.TemporaryDirectory() as temporary:
            ready = Path(temporary) / 'browser.pid'
            browser = 'import os,signal,time;from pathlib import Path;signal.signal(signal.SIGTERM,signal.SIG_IGN);Path(' + repr(str(ready)) + ').write_text(str(os.getpid()));time.sleep(60)'
            parent = 'import signal,subprocess,sys,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);subprocess.Popen([sys.executable,"-c",' + repr(browser) + ']);time.sleep(60)'
            child = subprocess.Popen([sys.executable, '-c', parent], start_new_session=True)
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(.02)
                self.assertTrue(ready.exists(), 'synthetic browser did not start')
                browser_pid = int(ready.read_text())
                supervisor.terminate(child)
                self.assertIsNotNone(child.returncode)
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    state = subprocess.run(['ps', '-o', 'stat=', '-p', str(browser_pid)], capture_output=True, text=True)
                    if state.returncode or state.stdout.lstrip().startswith('Z'):
                        break
                    time.sleep(.02)
                else:
                    self.fail('browser descendant is still running after cancellation')
            finally:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()

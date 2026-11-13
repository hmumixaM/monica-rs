#!/usr/bin/env python3
"""Measure actual HTTP failures without treating them as fast successful reads."""
import importlib.util
import json
from pathlib import Path
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SPEC = importlib.util.spec_from_file_location('benchmark_backends', Path(__file__).with_name('benchmark-backends.py'))
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


class ReadResults(unittest.TestCase):
    def test_page_and_mixed_load_keep_capacity_failures_and_success_latencies(self):
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(503 if self.path == '/busy' else 200)
                self.end_headers()
                self.wfile.write(b'{' if self.path == '/broken' else json.dumps({'ok': self.path == '/ok'}).encode())

            def log_message(self, *_args):
                pass

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            client = benchmark.Client('http://127.0.0.1:' + str(server.server_port))
            paths = {'ok': '/ok', 'busy': '/busy', 'broken': '/broken'}
            done = threading.Event()
            done.set()
            results = [benchmark.page_load(client, paths, 2, 2),
                       benchmark.mixed_pages(client, paths, 2, 2, done)]
            for result in results:
                self.assertEqual((result['requests'], result['failures']), (6, 4))
                self.assertAlmostEqual(result['error_rate'], 4 / 6)
                ok, busy, broken = [result['by_route'][name] for name in paths]
                self.assertEqual((ok['samples'], ok['failures']), (2, 0))
                self.assertGreaterEqual(ok['p95_ms'], 0)
                self.assertEqual(busy['errors_by_status'], {'503': 2})
                self.assertEqual((busy['samples'], busy['failures']), (0, 2))
                self.assertIsNone(busy['p95_ms'])
                self.assertEqual(busy['failure_latency']['samples'], 2)
                self.assertEqual((broken['samples'], broken['failures']), (0, 2))
                self.assertIsNone(broken['p99_ms'])
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)


if __name__ == '__main__':
    unittest.main()

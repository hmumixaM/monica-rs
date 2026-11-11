#!/usr/bin/env python3
"""Exercise actual HTTP readiness and cooperative exit against the isolated cluster."""
import signal
import socket
import subprocess
import time
import urllib.request

with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
process = subprocess.Popen(['target/debug/monica-server', 'serve', '--listen', f'127.0.0.1:{port}'])
try:
    for attempt in range(100):
        if process.poll() is not None:
            raise RuntimeError('server exited before readiness')
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{port}/api/v1/ready', timeout=1) as response:
                assert response.status == 200
            break
        except OSError:
            time.sleep(0.1)
    else:
        raise RuntimeError('server readiness deadline exceeded')
    with urllib.request.urlopen(f'http://127.0.0.1:{port}/health', timeout=1) as response:
        assert response.status == 200
        assert b'"status":"ok"' in response.read()
finally:
    if process.poll() is None:
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise RuntimeError('cooperative shutdown failed')
assert process.returncode == 0
print('100-record CLI ingestion/projection and HTTP lifecycle smoke passed')

#!/usr/bin/env python3
"""Explicit Compose collector with fail-closed runtime controls and durable IPC."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import urllib.request
import urllib.error

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(req.full_url, code, 'control redirect rejected', headers, fp)

CONTROL = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
stopping = False

def stop(_signal, _frame):
    global stopping
    stopping = True

def controls(base, token, provider, lane):
    def get(endpoint):
        request = urllib.request.Request(base + endpoint, headers={'Authorization': 'Bearer ' + token})
        with CONTROL.open(request, timeout=2) as response:
            raw = response.read(65537)
            if len(raw) > 65536:
                raise ValueError('oversized control response')
            return json.loads(raw)
    try:
        runtime = get('/internal/runtime/status')
        rows = {row['provider']: row for row in get('/internal/adapter-state')}
        required = [provider] + (['linkedin_messages'] if lane == 'messages' else [])
        return runtime['state'] == 'running' and all(rows[p]['enabled'] and not rows[p]['paused'] for p in required)
    except (OSError, ValueError, KeyError, TypeError):
        return False

def terminate(child):
    # Also kill surviving browser descendants after their direct parent exits.
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    deadline = time.monotonic() + 3
    while child.poll() is None and time.monotonic() < deadline:
        time.sleep(.05)
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    child.wait()

def pending(spool):
    count = 0
    for entry in spool.glob('*.ready'):
        if entry.with_suffix('.jsonl').exists():
            count += 1
            if count >= 8:
                return True
    return False

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--provider', choices=['instagram', 'linkedin', 'wechat'], required=True)
    parser.add_argument('--lane', choices=['profiles', 'messages'], default='profiles')
    args = parser.parse_args()
    if args.lane == 'messages' and args.provider != 'linkedin':
        parser.error('messages lane requires LinkedIn')
    token = os.environ.get('MONICA_CONTROL_TOKEN', '')
    if len(token) < 32:
        parser.error('set the same independent MONICA_CONTROL_TOKEN as the Rust server')
    base = os.environ.get('MONICA_CONTROL_URL', 'http://server:17494').rstrip('/')
    # The fixed internal Compose endpoint never follows external redirects.
    if base != 'http://server:17494':
        parser.error('control endpoint must be the internal Compose Rust service')
    root = Path('/data/collectors'); root.mkdir(parents=True, exist_ok=True)
    key = 'linkedin_messages' if args.lane == 'messages' else args.provider
    spool = root / 'spool' / key; spool.mkdir(parents=True, exist_ok=True)
    command = [sys.executable, '/opt/monica/collector-bridge.py', '--collector', '/opt/monica/collectors/collector.py', '--provider', args.provider, '--lane', args.lane, '--data-root', str(root), '--spool-dir', str(spool)]
    if args.provider == 'wechat':
        account = os.environ.get('MONICA_CHATLOG_ACCOUNT', '')
        if not account:
            parser.error('set the explicit independent MONICA_CHATLOG_ACCOUNT')
        command += ['--snapshot-root', '/data/chatlog-snapshots', '--account', account]
        own = os.environ.get('MONICA_CHATLOG_SELF_ID', '')
        if own:
            command += ['--self-id', own]
    else:
        command += ['--collect']
    # The collection child has no backend control credential.
    environment = {key: value for key, value in os.environ.items() if not key.startswith('MONICA_CONTROL_')}
    environment['MONICA_COLLECTOR_OWNED_GROUP'] = '1'
    for signum in (signal.SIGTERM, signal.SIGINT): signal.signal(signum, stop)
    next_cycle = 0.
    while not stopping:
        if time.monotonic() < next_cycle or pending(spool) or not controls(base, token, args.provider, args.lane):
            time.sleep(1)
            continue
        child = subprocess.Popen(command, env=environment, start_new_session=True)
        deadline = time.monotonic() + 900
        try:
            while child.poll() is None and not stopping and time.monotonic() < deadline:
                if not controls(base, token, args.provider, args.lane):
                    break
                time.sleep(1)
        finally:
            terminate(child)
        next_cycle = time.monotonic() + (2 if args.provider == 'wechat' else 60)

if __name__ == '__main__':
    os.umask(0o077)
    main()

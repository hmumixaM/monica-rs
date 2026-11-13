#!/usr/bin/env python3
"""Check the actual Docker context keeps collector code and excludes private state."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BASE = Path('apps/desktop/collectors')
pins = json.loads((ROOT / BASE / 'VENDORED_SOURCES.json').read_text())['sources']
files = ['collector.py', 'linkedin_messages.py', 'publishing.py', 'run_linkedin.py', 'README.md',
         'pyproject.toml', 'uv.lock', 'VENDORED_SOURCES.json', 'vendor/linkedin_cli_package/SOURCE.json']
files = sorted({BASE / name for name in files + [entry['file'] for entry in pins]})
private = [BASE / '.venv/private-marker', BASE / 'collector-data/session.json', BASE / 'cookies.json',
           Path('.env'), Path('.git/config'), Path('.local-validation/private-marker'),
           Path('apps/native-desktop/runtime/private-marker')]
with tempfile.TemporaryDirectory(prefix='monica-docker-context-') as temporary:
    root = Path(temporary)
    context = root / 'input'
    output = root / 'output'
    context.mkdir()
    shutil.copyfile(ROOT / '.dockerignore', context / '.dockerignore')
    (context / 'Dockerfile').write_text('FROM scratch\nCOPY . /context\n')
    for name in files:
        target = context / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / name, target)
    for name in private:
        target = context / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text('synthetic private state; must not be copied')
    subprocess.run(['docker', 'build', '--output', 'type=local,dest=' + str(output), str(context)], check=True)
    for name in files:
        actual = output / 'context' / name
        assert actual.is_file(), 'Required collector source missing: ' + str(name)
        assert hashlib.sha256(actual.read_bytes()).digest() == hashlib.sha256((ROOT / name).read_bytes()).digest()
    for name in private:
        assert not (output / 'context' / name).exists(), 'Private state entered Docker context: ' + str(name)
print(f'Docker context passed: {len(files)} declared source files retained; {len(private)} private-state fixtures excluded.')

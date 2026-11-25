#!/usr/bin/env python3
"""Verify the actual Mac test bundle and run isolated owned-process lifecycle tests."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

def linked_libraries(path):
    with tempfile.TemporaryDirectory(prefix='monica-macho-audit-') as directory:
        alias=Path(directory)/'binary'
        alias.symlink_to(path.resolve())
        return subprocess.check_output(['otool','-L',str(alias)],text=True)

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('app',type=Path)
p.add_argument('--expected-commit')
p.add_argument('--require-clean',action='store_true')
a=p.parse_args()
if sys.platform!='darwin':p.error('macOS is required')
app=a.app.resolve();runtime=app/'Contents/Resources/runtime'
manifest=json.loads((runtime/'manifest.json').read_text())
if a.expected_commit and manifest['source_commit']!=a.expected_commit:p.error('Bundled source commit does not match the requested commit')
if a.require_clean and manifest.get('source_dirty',True):p.error('Runtime was built from an uncommitted or unknown tree')
server=runtime/'monica-server'
if hashlib.sha256(server.read_bytes()).hexdigest()!=manifest['server_sha256']:p.error('Bundled server hash differs from runtime manifest')
chatlog=runtime/'tools/bin/chatlog-beta'
if hashlib.sha256(chatlog.read_bytes()).hexdigest()!=manifest['chatlog_sha256']:p.error('Bundled Chatlog hash differs from runtime manifest')
chatlog_pin=json.loads((ROOT/'apps/desktop/patches/chatlog/beta.json').read_text())
chatlog_source=json.loads((runtime/'licenses/Chatlog-source.json').read_text())
if manifest.get('chatlog_source_commit')!=chatlog_pin['commit'] or any(chatlog_source.get(key)!=value for key,value in chatlog_pin.items()):
    p.error('Bundled Chatlog Beta source differs from the pin')
chatlog_media=runtime/'tools/bin/chatlog-beta-media'
if hashlib.sha256(chatlog_media.read_bytes()).hexdigest()!=manifest['chatlog_media_sha256']:p.error('Bundled Chatlog media hash differs from runtime manifest')
subprocess.run([str(chatlog_media),'--help'],check=True,stdout=subprocess.DEVNULL)
bridge=runtime/'tools/bin/monica-whatsapp'
if hashlib.sha256(bridge.read_bytes()).hexdigest()!=manifest['whatsapp_sha256']:p.error('Bundled WhatsApp bridge hash differs from runtime manifest')
for path in [runtime/'web/index.html',runtime/'tools/bin/ffprobe',runtime/'tools/bin/ffmpeg',runtime/'tools/bin/validate',runtime/'moments/run-worker',bridge,chatlog,runtime/'collectors/run-bridge',runtime/'collectors/python/bin/python3.12',runtime/'licenses/Chatlog-Cargo.lock',runtime/'licenses/WhatsApp-PROVENANCE.md',runtime/'licenses/GeoNames.md',runtime/'licenses/GeoNames-PROVENANCE.json']:
    if not path.is_file():p.error(f'Missing required runtime asset: {path.relative_to(runtime)}')
subprocess.run(['codesign','--verify','--deep','--strict',str(app)],check=True)
for name in manifest['native_files']:
    binary=runtime/name
    links=linked_libraries(binary)
    if '/opt/homebrew/' in links or '/usr/local/' in links:p.error(f'Nonportable dependency in {name}')
subprocess.run([sys.executable,str(ROOT/'scripts/macos-collectors-smoke.py'),str(runtime/'collectors')],check=True)
subprocess.run([str(runtime/'collectors/run-bridge'),'--help'],check=True,stdout=subprocess.DEVNULL)
subprocess.run([sys.executable,str(ROOT/'scripts/macos-chatlog-smoke.py'),str(chatlog)],check=True)
subprocess.run([str(runtime/'postgres/bin/postgres'),'--version'],check=True)
subprocess.run([str(runtime/'tools/bin/ffprobe'),'-version'],check=True,stdout=subprocess.DEVNULL)
env={**os.environ,'MONICA_NATIVE_SERVER':str(server),'MONICA_NATIVE_PG_BIN':str(runtime/'postgres/bin'),'MONICA_NATIVE_WEB':str(runtime/'web')}
subprocess.run(['cargo','test','--manifest-path',str(ROOT/'apps/native-desktop/src-tauri/Cargo.toml'),'--no-default-features','--test','native_runtime','--','--ignored','--nocapture'],env=env,check=True,cwd=ROOT)
# A smoke test must not modify a sealed package while exercising it.
subprocess.run(['codesign','--verify','--deep','--strict',str(app)],check=True)
size=sum(path.stat().st_size for path in app.rglob('*') if path.is_file() and not path.is_symlink())
print(json.dumps({'app':str(app),'bytes':size,'server_sha256':manifest['server_sha256'],'source_commit':manifest['source_commit'],'source_dirty':manifest.get('source_dirty'),'verified':'codesign, relocation, ffprobe, fresh PG16, static Web/auth, suspend/resume, pause, crash recovery, ports, clean exit'},indent=2))

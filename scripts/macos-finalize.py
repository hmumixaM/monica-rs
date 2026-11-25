#!/usr/bin/env python3
"""Seal the final ad-hoc test bundle after Tauri has signed embedded executables."""
import argparse, hashlib, json, subprocess, sys
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('app',type=Path)
a=p.parse_args()
if sys.platform!='darwin':p.error('macOS is required')
app=a.app.resolve();runtime=app/'Contents/Resources/runtime';path=runtime/'manifest.json'
manifest=json.loads(path.read_text())
# Signing changes Mach-O bytes. Record the delivered binary, preserving its
# pre-bundle digest as build provenance; then seal the outer resource envelope.
for key,relative in [('server_sha256','monica-server'),('whatsapp_sha256','tools/bin/monica-whatsapp'),('chatlog_sha256','tools/bin/chatlog-beta'),('chatlog_media_sha256','tools/bin/chatlog-beta-media')]:
    manifest.setdefault('build_'+key,manifest[key])
    with (runtime/relative).open('rb') as binary:
        digest=hashlib.sha256()
        for block in iter(lambda:binary.read(1024*1024),b''):digest.update(block)
    manifest[key]=digest.hexdigest()
manifest['bundle_signing']='ad-hoc; local testing only; not notarized'
path.write_text(json.dumps(manifest,indent=2)+'\n')
subprocess.run(['codesign','--force','--sign','-',str(app)],check=True)
subprocess.run(['codesign','--verify','--deep','--strict',str(app)],check=True)
print(json.dumps({'app':str(app),'server_sha256':manifest['server_sha256'],'source_commit':manifest['source_commit']},indent=2))

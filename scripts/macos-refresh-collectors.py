#!/usr/bin/env python3
"""Refresh only explicit collector code in an already locked native runtime."""
import argparse,hashlib,json,shutil
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('runtime',type=Path);a=p.parse_args()
root=a.runtime.resolve();source=ROOT/'apps/desktop/collectors';manifest=json.loads((root/'manifest.json').read_text())
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
if sha(source/'uv.lock')!=manifest['collector_lock_sha256']:p.error('Lock changed: rebuild the full runtime before refreshing code')
pins=json.loads((source/'VENDORED_SOURCES.json').read_text())['sources']
files=['collector.py','linkedin_messages.py','publishing.py','run_linkedin.py','README.md','pyproject.toml','uv.lock','VENDORED_SOURCES.json','vendor/linkedin_cli_package/SOURCE.json']+[e['file'] for e in pins]
for entry in pins:
 if sha(source/entry['file'])!=entry.get('bundled_sha256',entry['sha256']):raise SystemExit('Vendored source hash changed: '+entry['file'])
for name in files:
 relative=Path(name)
 if relative.is_absolute() or '..' in relative.parts:raise SystemExit('Unsafe manifest path')
 original=source/relative
 if original.is_symlink() or not original.is_file():raise SystemExit('Source must be a regular explicit file: '+name)
 target=root/'app'/relative;target.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(original,target)
for module in ['collector-bridge.py','collector-chatlog.py','chatlog_moments.py','collector_formats.py']:
 shutil.copy2(ROOT/'scripts'/module,root/module)
manifest['collector_files']={name:sha(root/'app'/name) for name in sorted(files)}
manifest['bridge_sha256']=sha(root/'collector-bridge.py')
manifest['bridge_modules']={name:sha(root/name) for name in ['collector-bridge.py','collector-chatlog.py','chatlog_moments.py','collector_formats.py']}
(root/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print(json.dumps({'runtime':str(root),'refreshed_explicit_files':len(files),'bridge_sha256':manifest['bridge_sha256']}))

# Keep an already locked dependency runtime usable after a source refresh.
for name in ['run-bridge','run-collector']:
 wrapper=root/name
 text=wrapper.read_text().replace('export PYTHONNOUSERSITE=1\n','export PYTHONNOUSERSITE=1\nexport PYTHONDONTWRITEBYTECODE=1\n')
 text=text.replace('exec "$HERE/python/bin/python3.12" "$HERE/','exec "$HERE/python/bin/python3.12" -B "$HERE/')
 # Refresh can be repeated without accumulating environment lines.
 text=text.replace('export PYTHONDONTWRITEBYTECODE=1\nexport PYTHONDONTWRITEBYTECODE=1\n','export PYTHONDONTWRITEBYTECODE=1\n')
 wrapper.write_text(text)

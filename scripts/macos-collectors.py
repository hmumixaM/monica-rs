#!/usr/bin/env python3
"""Build isolated, relocatable Python/Chromium collectors from the pinned lock."""
import argparse, hashlib, json, os, shutil, subprocess, sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
def run(*args,**kwargs):return subprocess.run([str(a) for a in args],check=True,**kwargs)
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--output',type=Path,required=True)
p.add_argument('--python-prefix',type=Path)
a=p.parse_args()
if sys.platform!='darwin':p.error('macOS runtime build only')
if a.output.exists():p.error('Output must be absent')
python_prefix=a.python_prefix
if python_prefix is None:
    executable=Path(subprocess.check_output(['uv','python','find','--managed-python','3.12.11'],text=True).strip())
    python_prefix=executable.resolve().parents[1]
if not (python_prefix/'bin/python3.12').is_file():p.error('Explicit standalone Python3.12 prefix required')
output=a.output.resolve();output.mkdir(parents=True)
app=output/'app';source=ROOT/'apps/desktop/collectors'
pins=json.loads((source/'VENDORED_SOURCES.json').read_text())['sources']
# Deny by default: never package an accidentally placed session, .env or archive.
files=['collector.py','linkedin_messages.py','publishing.py','run_linkedin.py','README.md','pyproject.toml','uv.lock','VENDORED_SOURCES.json','vendor/linkedin_cli_package/SOURCE.json']+[entry['file'] for entry in pins]
app.mkdir()
for name in files:
    relative=Path(name)
    if relative.is_absolute() or '..' in relative.parts:raise RuntimeError('Unsafe collector manifest path')
    original=source/relative
    if original.is_symlink() or not original.is_file():raise RuntimeError('Collector source must be a regular explicit file: '+name)
    destination=app/relative;destination.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(original,destination)
for entry in pins:
    if sha(app/entry['file'])!=entry.get('bundled_sha256',entry['sha256']):raise RuntimeError('Vendored collector hash differs: '+entry['file'])
# The managed interpreter is a distribution, not a user environment. Exclude its
# entire site-packages and reinstall solely from the tracked collector lock.
shutil.copytree(python_prefix,output/'python',symlinks=True,ignore=shutil.ignore_patterns('site-packages','__pycache__','*.pyc'))
(output/'python/lib/python3.12/site-packages').mkdir()
python=output/'python/bin/python3.12'
requirements=output/'requirements.txt'
run('uv','export','--frozen','--no-dev','--no-emit-project','--project',app,'--output-file',requirements,stdout=subprocess.DEVNULL)
run('uv','pip','install','--python',python,'--break-system-packages','--no-deps','-r',requirements,cwd=app)
for module in ['collector-bridge.py','collector-chatlog.py','chatlog_moments.py']:
    shutil.copy2(ROOT/'scripts'/module,output/module)
(output/'infra').mkdir()
# Source pin is committed; never read an existing personal runtime checkout.
publisher=subprocess.check_output(['git','-C','/Users/maxhu/play/monica-multisource','show','1a6e40e20c6ac12aca434b61244059d067847473:infra/snapshot_publish.py'])
(output/'infra/snapshot_publish.py').write_bytes(publisher)
env={**os.environ,'PLAYWRIGHT_BROWSERS_PATH':str(output/'browsers'),'PYTHONNOUSERSITE':'1'}
run(python,'-m','playwright','install','chromium',env=env)
for name,entry,extra in [('run-bridge','collector-bridge.py','--collector "$HERE/app/collector.py"'),('run-collector','app/collector.py','')]:
    path=output/name
    path.write_text('#!/bin/sh\nset -eu\nHERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\nexport PLAYWRIGHT_BROWSERS_PATH="$HERE/browsers"\nexport MONICA_SNAPSHOT_PUBLISHER="$HERE/infra/snapshot_publish.py"\nexport PYTHONNOUSERSITE=1\nexport PATH="$HERE/python/bin:/usr/bin:/bin"\nexec "$HERE/python/bin/python3.12" "$HERE/'+entry+'" '+extra+' "$@"\n')
    path.chmod(0o755)
# Preserve all distribution notices and record versions without executing collectors.
notices=output/'licenses';notices.mkdir()
shutil.copy2(output/'python/lib/python3.12/LICENSE.txt',notices/'Python-LICENSE.txt')
script="""import importlib.metadata,json
print(json.dumps([{'name':d.metadata['Name'],'version':d.version,'files':[str(d.locate_file(p)) for p in (d.files or []) if any(k in p.name.lower() for k in ['license','copying','notice','copyright'])]} for d in importlib.metadata.distributions()]))"""
packages=json.loads(subprocess.check_output([str(python),'-I','-c',script],text=True))
for package in packages:
    destination=notices/(package['name']+'-'+package['version']);destination.mkdir()
    for filename in package.pop('files'):
        file=Path(filename)
        if file.is_file():shutil.copy2(file,destination/file.name)
manifest={'schema':1,'python':subprocess.check_output([str(python),'--version'],text=True).strip(),'python_executable_sha256':sha(python.resolve()),'collector_lock_sha256':sha(app/'uv.lock'),'collector_source_commit':'1a6e40e20c6ac12aca434b61244059d067847473','publisher_sha256':sha(output/'infra/snapshot_publish.py'),'collector_files':{name:sha(app/name) for name in sorted(files)},'bridge_sha256':sha(output/'collector-bridge.py'),'bridge_modules':{name:sha(output/name) for name in ['collector-bridge.py','collector-chatlog.py','chatlog_moments.py']},'packages':packages,'browsers':json.loads((output/'python/lib/python3.12/site-packages/playwright/driver/package/browsers.json').read_text())}
(output/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
run(python,'-I','-c','import instaloader,playwright,pydantic,linkedin_scraper,linkedin_cli; print("Pinned collector imports passed")',env=env)
run(output/'run-bridge','--help',stdout=subprocess.DEVNULL)
print(json.dumps({'output':str(output),'python':manifest['python'],'packages':len(packages),'verified_vendored_files':len(pins)},indent=2))

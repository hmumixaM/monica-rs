#!/usr/bin/env python3
"""Build an isolated relocatable runtime from explicit binaries, never user data.
Run after building apps/web and monica-server. This does not install or launch the app.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

def linked_libraries(path):
    with tempfile.TemporaryDirectory(prefix='monica-macho-audit-') as directory:
        alias=Path(directory)/'binary'
        alias.symlink_to(path.resolve())
        return subprocess.check_output(['otool','-L',str(alias)],text=True)

ROOT = Path(__file__).resolve().parents[1]
DEFAULT = ROOT / 'apps/native-desktop/runtime'

def run(*args):
    return subprocess.check_output([str(a) for a in args], text=True)

def digest(path):
    h=hashlib.sha256()
    with path.open('rb') as f:
        for data in iter(lambda:f.read(1024*1024), b''): h.update(data)
    return h.hexdigest()

def macho(path):
    if not path.is_file() or path.is_symlink(): return False
    with path.open('rb') as f: return f.read(4) in (b'\xcf\xfa\xed\xfe',b'\xca\xfe\xba\xbe',b'\xbe\xba\xfe\xca')

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--server',type=Path,default=ROOT/'target/release/monica-server')
    p.add_argument('--postgres-prefix',type=Path,default=Path('/opt/homebrew/opt/postgresql@16'))
    p.add_argument('--collectors',type=Path,default=ROOT/'.local-validation/collector-runtime')
    p.add_argument('--chatlog-runtime',type=Path,default=ROOT/'.local-validation/chatlog-runtime')
    p.add_argument('--whatsapp-binary',type=Path,default=ROOT/'target/release/monica-whatsapp')
    p.add_argument('--whatsapp-licenses',type=Path,default=ROOT/'services/whatsapp/licenses')
    p.add_argument('--ffprobe',type=Path,default=Path('/opt/homebrew/bin/ffprobe'))
    p.add_argument('--web',type=Path,default=ROOT/'apps/web/dist')
    p.add_argument('--output',type=Path,default=DEFAULT)
    a=p.parse_args()
    if sys.platform!='darwin': p.error('This packaging audit requires macOS otool/codesign')
    if not a.server.is_file() or not (a.web/'index.html').is_file(): p.error('Build Rust server and Web before packaging')
    if not a.ffprobe.is_file(): p.error('Build/install ffprobe before packaging; video inspection is required')
    if not a.whatsapp_binary.is_file(): p.error('Build the explicit WhatsApp Go bridge before packaging')
    if not a.whatsapp_licenses.is_dir(): p.error('Generate the WhatsApp dependency license inventory before packaging')
    if not (a.collectors/'manifest.json').is_file() or not (a.chatlog_runtime/'source.json').is_file(): p.error('Build pinned collector and Chatlog runtimes before packaging')
    collector_manifest=json.loads((a.collectors/'manifest.json').read_text())
    if digest(ROOT/'apps/desktop/collectors/uv.lock')!=collector_manifest['collector_lock_sha256']: p.error('Collector dependency lock is stale; rebuild its runtime')
    if not collector_manifest.get('collector_files'): p.error('Collector source inventory is missing; run macos-refresh-collectors.py')
    for name,expected in collector_manifest['collector_files'].items():
        if digest(a.collectors/'app'/name)!=expected or digest(ROOT/'apps/desktop/collectors'/name)!=expected: p.error('Collector source is stale; run macos-refresh-collectors.py: '+name)
    for name in ['collector-bridge.py','collector-chatlog.py','chatlog_moments.py']:
        expected=collector_manifest.get('bridge_modules',{}).get(name)
        if not expected or digest(a.collectors/name)!=expected or digest(ROOT/'scripts'/name)!=expected: p.error('Collector module is stale; run macos-refresh-collectors.py: '+name)
    if a.output.exists(): p.error('Output must be absent. Keep prior artifacts and select a fresh --output directory.')
    a.output.mkdir(parents=True)
    pg=a.output/'postgres';(pg/'bin').mkdir(parents=True);(pg/'lib').mkdir()
    for name in ['postgres','initdb','pg_ctl','pg_isready','createdb','psql','pg_dump','pg_restore']:
        shutil.copy2(a.postgres_prefix/'bin'/name,pg/'bin'/name)
    shutil.copytree(a.postgres_prefix/'share',pg/'share',symlinks=False)
    shutil.copytree(a.postgres_prefix/'lib',pg/'lib',symlinks=False,dirs_exist_ok=True)
    pending=[f for f in pg.rglob('*') if macho(f)]
    origins={f:(a.postgres_prefix/f.relative_to(pg)).resolve() for f in pending}
    probe=a.output/'tools/bin/ffprobe';probe.parent.mkdir(parents=True)
    shutil.copy2(a.ffprobe,probe);pending.append(probe);origins[probe]=a.ffprobe.resolve()
    bridge=a.output/'tools/bin/monica-whatsapp'
    shutil.copy2(a.whatsapp_binary,bridge);pending.append(bridge);origins[bridge]=a.whatsapp_binary.resolve()
    chatlog=a.output/'tools/bin/chatlog-alpha'
    shutil.copy2(a.chatlog_runtime/'chatlog-alpha',chatlog);pending.append(chatlog);origins[chatlog]=(a.chatlog_runtime/'chatlog-alpha').resolve()
    seen=set()
    while pending:
        f=pending.pop()
        if f in seen: continue
        seen.add(f)
        for line in linked_libraries(f).splitlines()[1:]:
            dependency=line.strip().split(' (',1)[0]
            if dependency.startswith(('/System/','/usr/lib/')): continue
            if dependency.startswith('@loader_path/'):
                source=(origins[f].parent/dependency.removeprefix('@loader_path/')).resolve()
            elif dependency.startswith('@rpath/'):
                source=(origins[f].parent/dependency.removeprefix('@rpath/')).resolve()
            elif dependency.startswith('/'):
                source=Path(dependency).resolve()
            else: raise RuntimeError(f'Unrecognized dependency {dependency} in {f.name}')
            if source==origins[f]: continue
            if not source.is_file(): raise RuntimeError(f'Unresolved dependency {dependency} in {f.name}')
            destination=pg/'lib'/source.name
            if not destination.exists():
                shutil.copy2(source,destination);origins[destination]=source;pending.append(destination)
            relative=os.path.relpath(destination,f.parent)
            subprocess.run(['install_name_tool','-change',dependency,'@loader_path/'+relative,str(f)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        if f.suffix in ('.so','.dylib'):
            subprocess.run(['install_name_tool','-id','@rpath/'+f.name,str(f)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        subprocess.run(['codesign','--force','--sign','-',str(f)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
    shutil.copy2(a.server,a.output/'monica-server')
    shutil.copytree(a.web,a.output/'web')
    shutil.copytree(a.collectors,a.output/'collectors',symlinks=True,ignore=shutil.ignore_patterns('__pycache__','*.pyc'))
    notices=a.output/'licenses';notices.mkdir()
    geo=ROOT/'crates/monica-server/src/pages/geonames'
    shutil.copy2(geo/'README.md',notices/'GeoNames.md')
    shutil.copy2(geo/'PROVENANCE.json',notices/'GeoNames-PROVENANCE.json')
    shutil.copytree(a.chatlog_runtime/'licenses',notices/'Chatlog-dependencies')
    for name in ['source.json','desktop.patch','go.mod','go.sum']:
        shutil.copy2(a.chatlog_runtime/name,notices/('Chatlog-'+name))
    shutil.copytree(a.whatsapp_licenses,notices/'WhatsApp-dependencies')
    for name in ['PROVENANCE.md','go.mod','go.sum']:
        shutil.copy2(ROOT/'services/whatsapp'/name,notices/('WhatsApp-'+name))
    package_roots=set()
    for origin in origins.values():
        for parent in origin.parents:
            if (parent/'INSTALL_RECEIPT.json').is_file():
                package_roots.add(parent)
                break
    for package in sorted(package_roots):
        destination=notices/(package.parent.name+'-'+package.name)
        destination.mkdir()
        for pattern in ('LICENSE*','COPYING*','COPYRIGHT*','NOTICE*'):
            for source in package.glob(pattern):
                if source.is_file(): shutil.copy2(source,destination/source.name)
    audited=[]
    for f in a.output.rglob('*'):
        if macho(f):
            linked=linked_libraries(f)
            if '/opt/homebrew/' in linked or '/usr/local/' in linked: raise RuntimeError(f'Nonportable native dependency: {f.relative_to(a.output)}')
            audited.append(str(f.relative_to(a.output)))
    for f in a.output.rglob('*'):
        if f.is_file() and not f.is_symlink(): f.chmod(f.stat().st_mode | 0o200)
    manifest={'schema':1,'platform':'darwin/arm64','minimum_macos':'26.0','server_sha256':digest(a.server),'postgres':run(pg/'bin/postgres','--version').strip(),'ffprobe':run(probe,'-version').splitlines()[0],'whatsapp_sha256':digest(bridge),'chatlog_sha256':digest(chatlog),'collectors':json.loads((a.collectors/'manifest.json').read_text()),'native_files':audited,'adapters_enabled':False,'ai_enabled':False,'source_commit':run('git','-C',ROOT,'rev-parse','HEAD').strip(),'source_dirty':bool(run('git','-C',ROOT,'status','--porcelain').strip()),'license_packages':[p.parent.name+'-'+p.name for p in sorted(package_roots)]}
    (a.output/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(json.dumps({'output':str(a.output),'native_files':len(audited),'postgres':manifest['postgres']}))

if __name__=='__main__': main()

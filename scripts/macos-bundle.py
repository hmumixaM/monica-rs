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
    p.add_argument('--web',type=Path,default=ROOT/'apps/web/dist')
    p.add_argument('--output',type=Path,default=DEFAULT)
    a=p.parse_args()
    if sys.platform!='darwin': p.error('This packaging audit requires macOS otool/codesign')
    if not a.server.is_file() or not (a.web/'index.html').is_file(): p.error('Build Rust server and Web before packaging')
    if a.output.exists(): p.error('Output must be absent. Keep prior artifacts and select a fresh --output directory.')
    a.output.mkdir(parents=True)
    pg=a.output/'postgres';(pg/'bin').mkdir(parents=True);(pg/'lib').mkdir()
    for name in ['postgres','initdb','pg_ctl','pg_isready','createdb','psql','pg_dump','pg_restore']:
        shutil.copy2(a.postgres_prefix/'bin'/name,pg/'bin'/name)
    shutil.copytree(a.postgres_prefix/'share',pg/'share',symlinks=False)
    shutil.copytree(a.postgres_prefix/'lib',pg/'lib',symlinks=False,dirs_exist_ok=True)
    pending=[f for f in pg.rglob('*') if macho(f)]
    origins={f:(a.postgres_prefix/f.relative_to(pg)).resolve() for f in pending}
    seen=set()
    while pending:
        f=pending.pop()
        if f in seen: continue
        seen.add(f)
        for line in run('otool','-L',f).splitlines()[1:]:
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
    audited=[]
    for f in a.output.rglob('*'):
        if macho(f):
            linked=run('otool','-L',f)
            if '/opt/homebrew/' in linked or '/usr/local/' in linked: raise RuntimeError(f'Nonportable native dependency: {f.relative_to(a.output)}')
            audited.append(str(f.relative_to(a.output)))
    for f in a.output.rglob('*'):
        if f.is_file() and not f.is_symlink(): f.chmod(f.stat().st_mode | 0o200)
    manifest={'schema':1,'platform':'darwin/arm64','minimum_macos':'26.0','server_sha256':digest(a.server),'postgres':run(pg/'bin/postgres','--version').strip(),'native_files':audited,'adapters_enabled':False,'ai_enabled':False,'source_commit':run('git','-C',ROOT,'rev-parse','HEAD').strip()}
    (a.output/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(json.dumps({'output':str(a.output),'native_files':len(audited),'postgres':manifest['postgres']}))

if __name__=='__main__': main()

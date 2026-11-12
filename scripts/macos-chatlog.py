#!/usr/bin/env python3
"""Build a pinned Chatlog tree in a fresh checkout; retain upstream/dependency notices."""
import argparse,hashlib,json,os,shutil,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1];patches=ROOT/'apps/desktop/patches/chatlog'
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
if a.output.exists():p.error('Output must be absent')
output=a.output.resolve();output.mkdir(parents=True);source=json.loads((patches/'source.json').read_text())
if hashlib.sha256((patches/source['patch']).read_bytes()).hexdigest()!=source['patch_sha256']:raise SystemExit('Pinned Chatlog patch digest differs')
def run(*args,**kw):return subprocess.run([str(a) for a in args],check=True,**kw)
with tempfile.TemporaryDirectory(prefix='monica-rust-chatlog-build-') as temporary:
    checkout=Path(temporary)/'source'
    run('git','clone','--quiet','--no-checkout',source['repository'],checkout)
    run('git','-C',checkout,'checkout','--quiet','--detach',source['commit'])
    run('git','-C',checkout,'apply','--check',patches/source['patch'])
    run('git','-C',checkout,'apply',patches/source['patch'])
    env={**os.environ,'CGO_ENABLED':'1','GOOS':'darwin','GOARCH':'arm64'}
    run('go','build','-trimpath','-o',output/'chatlog-alpha','.',cwd=checkout,env=env)
    notices=output/'licenses';notices.mkdir()
    goroot=Path(subprocess.check_output(['go','env','GOROOT'],cwd=checkout,text=True).strip())
    go_license=next((p/'LICENSE' for p in [goroot,*goroot.parents] if (p/'LICENSE').is_file()),None)
    if go_license is None:raise RuntimeError('Missing Go license')
    shutil.copy2(go_license,notices/'Go-LICENSE.txt')
    for path in checkout.iterdir():
        if path.is_file() and path.name.lower().startswith(('license','notice','copying','copyright')):shutil.copy2(path,notices/path.name)
    for name in ['go.mod','go.sum']:shutil.copy2(checkout/name,output/name)
    raw=subprocess.check_output(['go','list','-m','-json','all'],cwd=checkout,text=True);decoder=json.JSONDecoder();index=0;inventory=[]
    while index<len(raw):
        while index<len(raw) and raw[index].isspace():index+=1
        if index==len(raw):break
        module,index=decoder.raw_decode(raw,index)
        if module.get('Main'):continue
        if not module.get('Dir'):
            module.update(json.loads(subprocess.check_output(['go','mod','download','-json',module['Path']+'@'+module['Version']],cwd=checkout,text=True)))
        folder=Path(module['Dir']);matches=[f for f in folder.iterdir() if f.is_file() and f.name.lower().startswith(('license','notice','copying','copyright'))]
        if not matches:raise RuntimeError('Missing dependency notice: '+module['Path'])
        destination=notices/(module['Path'].replace('/','_')+'@'+module['Version']);destination.mkdir()
        for path in matches:shutil.copy2(path,destination/path.name)
        inventory.append({'module':module['Path'],'version':module['Version']})
    shutil.copy2(patches/source['patch'],output/source['patch'])
    source.update({'go':subprocess.check_output(['go','version'],cwd=checkout,text=True).strip(),'dependencies':inventory,'binary_sha256':hashlib.sha256((output/'chatlog-alpha').read_bytes()).hexdigest()})
    (output/'source.json').write_text(json.dumps(source,indent=2)+'\n')
    run(output/'chatlog-alpha','--help',stdout=subprocess.DEVNULL)
print(json.dumps({'output':str(output),'commit':source['commit'],'dependencies':len(inventory)}))

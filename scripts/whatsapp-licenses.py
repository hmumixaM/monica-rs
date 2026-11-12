#!/usr/bin/env python3
"""Copy the pinned bridge's actual third-party notices into the macOS bundle input."""
import json,pathlib,shutil,subprocess
root=pathlib.Path(__file__).resolve().parents[1]
module=root/'services/whatsapp'
output=module/'licenses';output.mkdir(exist_ok=True)
raw=subprocess.check_output(['go','list','-m','-json','all'],cwd=module,text=True)
decoder=json.JSONDecoder();index=0;inventory=[]
while index<len(raw):
 while index<len(raw) and raw[index].isspace():index+=1
 if index==len(raw):break
 value,length=decoder.raw_decode(raw,index);index=length
 if value.get('Main'):continue
 directory=value.get('Dir')
 if not directory:
  info=json.loads(subprocess.check_output(['go','mod','download','-json',value['Path']+'@'+value['Version']],cwd=module,text=True));directory=info['Dir']
 directory=pathlib.Path(directory)
 matches=[p for p in directory.iterdir() if p.is_file() and p.name.lower().startswith(('license','copying','notice','copyright'))]
 if not matches:raise SystemExit('Missing third-party license: '+value['Path'])
 name=value['Path'].replace('/','_')+'@'+value['Version'];target=output/name;target.mkdir(exist_ok=True)
 for path in matches:shutil.copyfile(path,target/path.name)
 inventory.append({'module':value['Path'],'version':value['Version'],'files':[str((target/p.name).relative_to(output)) for p in matches]})
goroot=pathlib.Path(subprocess.check_output(['go','env','GOROOT'],cwd=module,text=True).strip())
go_license=next((p for p in [goroot/'LICENSE',goroot.parent/'LICENSE'] if p.is_file()),None)
if go_license is None:raise SystemExit('Go toolchain LICENSE file missing')
shutil.copyfile(go_license,output/'Go-LICENSE')
(output/'inventory.json').write_text(json.dumps({'source_commit':'1a6e40e20c6ac12aca434b61244059d067847473','go':subprocess.check_output(['go','version'],cwd=module,text=True).strip(),'modules':inventory},indent=2)+'\n')
print(f'Copied actual license files for {len(inventory)} pinned modules and Go into {output}')

#!/usr/bin/env python3
"""Audit frozen operation registration; deliberately not a claim of behavior parity."""
import json,re
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
VERBS={'get','post','put','patch','delete','head','options'}
def normalized(path):return re.sub(r'\{[^}]+\}','{}',path)
def route_calls(source):
    for match in re.finditer(r'\.route\s*\(',source):
        start=match.end();depth=1;quoted=False;escaped=False;i=start
        while i<len(source) and depth:
            c=source[i]
            if quoted:
                if escaped:escaped=False
                elif c=='\\':escaped=True
                elif c=='"':quoted=False
            elif c=='"':quoted=True
            elif c=='(':depth+=1
            elif c==')':depth-=1
            i+=1
        yield source[start:i-1],source[:start].count('\n')+1
registered={}
for file in (ROOT/'crates/monica-server/src').rglob('*.rs'):
    if 'test' in file.name:continue
    for call,line in route_calls(file.read_text()):
        match=re.match(r'\s*"([^\"]+)"\s*,',call)
        if match:path=match[1];handler=call[match.end():]
        elif re.match(r'\s*mail_oauth::CALLBACK\s*,',call):path='/api/v1/mail-oauth/callback';handler=call.split(',',1)[1]
        else:continue
        for method in re.findall(r'\b(get|post|put|patch|delete|head|options)\s*\(',handler):
            registered.setdefault((method.upper(),normalized(path)),[]).append({'file':str(file.relative_to(ROOT)),'line':line,'path':path})
operations={}
for baseline in ('crm','desktop','multisource'):
    for path,item in json.loads((ROOT/f'docs/contracts/{baseline}-openapi.json').read_text())['paths'].items():
        for method in item:
            if method not in VERBS:continue
            key=(method.upper(),normalized(path));entry=operations.setdefault(key,{'method':method.upper(),'path':path,'baselines':[]})
            entry['baselines'].append(baseline)
result=[]
for key,value in sorted(operations.items()):
    value['registration']=registered.get(key,[])
    value['status']='registered' if value['registration'] else 'missing'
    result.append(value)
output={'notice':'Static route registration audit only. Does not establish contract or behavior correctness. Read differential, integration and UI evidence separately.','operations':result,'missing':[x for x in result if x['status']=='missing']}
(ROOT/'docs/api-registration-audit.json').write_text(json.dumps(output,ensure_ascii=False,indent=2)+'\n')
print(json.dumps({'operations':len(result),'registered':sum(x['status']=='registered' for x in result),'missing':[x['method']+' '+x['path'] for x in result if x['status']=='missing']},indent=2))

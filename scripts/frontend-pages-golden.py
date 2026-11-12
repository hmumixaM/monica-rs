#!/usr/bin/env python3
"""Freeze pure page presentation outputs from the pinned CRM Python source."""
import argparse, ast, json, subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
REV='6d59cfef4b1a12f678d809a00e37d8332be70b06'
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--check',action='store_true');args=p.parse_args()
def load(path,names):
    source=subprocess.check_output(['git','-C','/Users/maxhu/play/monica','show',REV+':services/api/app/'+path],text=True)
    nodes=[node for node in ast.parse(source).body if isinstance(node,ast.Assign) or isinstance(node,ast.FunctionDef) and node.name in names]
    namespace={};exec(compile('from __future__ import annotations\nimport re\n'+ast.unparse(ast.Module(body=nodes,type_ignores=[])),path,'exec'),namespace);return namespace
facts=load('profile_facts.py',{'_text','aliases','group_names','headline','current_job','is_current','_negated','city','ordered_experience','compact'})
posts=load('post_text.py',{'display_text','excerpt'})
alias_cases=[['Alice',{'wechat.remark':'alice','wechat.nickname':'  A  ','wechat.alias':'B','monica.nickname':'a'},[['C','a'],['D',None]]],['Straße',{'wechat.remark':'STRASSE','wechat.nickname':'ﬃ','wechat.alias':'FFI','monica.nickname':'İ'},[['i̇','Σ'],['ς','σ']]],['',{'wechat.remark':1e-7,'wechat.nickname':False,'wechat.alias':42.0},[[str(i) for i in range(12)]]]]
group_cases=[['one｜ A\ntwo|a\nB\nthree | C','Alice',['A']],['g｜STRASSE\nx|ﬃ\ny|FFI\nz|New','Straße',[]]]
headline_cases=[[{},[]],[{'linkedin.headline':'Headline','monica.company':'Manual Co','monica.job':'Title'},[]],[{'linkedin.headline':'ignored'},[{'type':'employment','company':'Former','title':'Old','end_date':'2020'},{'type':'employment','company':'Current','title':'Engineer','end_date':'至今'}]]]
city_cases=[{}, {'linkedin.location':' Chicago, Illinois, United States '},{'monica.residence':'Paris','wechat.city':'Chicago'},{'wechat.province':'江苏','monica.city':'ignored'}]
experience=[{'type':'education','school':'School','degree':'BSc','start_date':'2010'},{'type':'employment','company':'Past','title':'Engineer','start_date':'2020','end_date':'2021'},{'type':'employment','company':'Current','title':'Lead','start_date':'2024','is_current':True},{'type':'employment','company':'Current','title':'Lead','start_date':'2024','is_current':True,'description':'duplicate'}]
result={'source_commit':REV,'aliases':[{'args':a,'expected':facts['aliases'](a[0],a[1],[v for row in a[2] for v in row])} for a in alias_cases],'group_names':[{'args':a,'expected':facts['group_names'](a[0],[a[1],*a[2]])} for a in group_cases],'headline':[{'args':a,'expected':facts['headline'](*a)} for a in headline_cases],'city':[{'args':a,'expected':facts['city'](a)} for a in city_cases],'ordered_experience':{'args':experience,'expected':facts['ordered_experience'](experience)},'excerpt':[{'args':[v,n],'expected':posts['excerpt'](v,n)} for v,n in [(None,12),(' Instagram story ',12),('complete',12),('hello  \nworld',12),('一个中文的很长动态内容',8),('hello beautiful world',10)]],'compact':{'args':{'null':None,'false':False,'zero':0,'empty':'','list':[],'object':{},'summary':{'count':0},'one':1},'expected':facts['compact']({'null':None,'false':False,'zero':0,'empty':'','list':[],'object':{},'summary':{'count':0},'one':1})}}
path=ROOT/'crates/monica-core/src/pages_presentation_golden.json';content=json.dumps(result,ensure_ascii=False,sort_keys=True,indent=2)+'\n'
if args.check:
    assert path.read_text()==content,'Pinned presentation golden changed'
else:path.write_text(content)
print('PASS: pinned aliases/group names/headline/city/experience/excerpt/compact presentation oracle')

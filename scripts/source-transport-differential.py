#!/usr/bin/env python3
"""Compare actual pinned pure mail/export functions against Rust, offline only."""
import ast,dataclasses,datetime,email.utils,hashlib,json,pathlib,re,subprocess,sys,types
from html.parser import HTMLParser
ROOT=pathlib.Path(__file__).resolve().parents[1]
REFERENCE='/Users/maxhu/play/monica-multisource'
COMMIT='1a6e40e20c6ac12aca434b61244059d067847473'
def source(path):return subprocess.check_output(['git','-C',REFERENCE,'show',COMMIT+':services/api/app/'+path],text=True)
def module(name,text):
 value=types.ModuleType(name);sys.modules[name]=value;exec(compile(text,name,'exec'),value.__dict__);return value
filtering=module('pinned_mail_filter',source('mail_filter.py'))
phone=module('pinned_whatsapp_export',source('whatsapp_export.py'))
tree=ast.parse(source('mail_sync.py'))
selected=ast.Module(body=[node for node in tree.body if isinstance(node,(ast.FunctionDef,ast.ClassDef)) and node.name in ('TextHTML','search_body','addresses','normalize')],type_ignores=[])
env=dict(hashlib=hashlib,json=json,re=re,datetime=datetime.datetime,UTC=datetime.timezone.utc,getaddresses=email.utils.getaddresses,parsedate_to_datetime=email.utils.parsedate_to_datetime,HTMLParser=HTMLParser)
exec(compile(selected,'pinned_mail_sync','exec'),env)
cases=[]
for headers,preview,labels in [({'subject':'Hi'},'',[]),({'Subject':'Invoice'},'',[]),({'subject':'Receipt','list-id':'x'},'',[]),({'subject':'Hello','list-unsubscribe':'x'},'',[]),({'subject':'Hi'},'',['CATEGORY_PROMOTIONS']),({'subject':'Sale','list-id':''},'',[]),({'subject':'Sale','precedence':'bulk'},'',[]),({'subject':'优惠码','list-id':'x'},'',[]),({'subject':'限时','list-id':'x'},'',[]),({'subject':'限时折扣','list-id':'x'},'',[])]:
 record=dict(headers=headers,preview=preview,labels=labels)
 for rule,reply in [(None,False),(None,True),('pending',True)]:
  cases.append((dict(op='classify',record=record,rule=rule,reply=reply),dataclasses.asdict(filtering.classify(headers,preview=preview,labels=labels,override=rule,has_reply=reply))))
for text,kind in [('hello\r\n> old\r\n--\r\nsignature','text/plain'),('<div>A  &amp; B</div><div>C<br>D</div><script>bad</script><p>&NotEqualTilde; &#128;','text/html'),('Hello\nOn Monday wrote: quoted\nprevious','text/plain'),('<p>Hi</p><p>Next <b>bold</b></p>','text/html'),('<title>Subject</title><div>Message &copy without semi</div>','text/html')]:
 record=dict(external_id='synthetic-mail',headers={'from':'Alice <alice@example.invalid>','to':'Owner <owner@example.invalid>','message-id':'<synthetic>','subject':'Hello','date':'Tue, 02 Jan 2024 09:30:00 -0600'},body=text,body_type=kind,attachments=[{'filename':'虚构.txt','blob':'a'*64}])
 owners={'owner@example.invalid'}
 cases.append((dict(op='normalize',record=record,owners=sorted(owners),category='personal',reason='user_rule'),env['normalize'](record,owners,'personal','user_rule')))
 cases.append((dict(op='fingerprint',record=record),hashlib.sha256(json.dumps({'headers':record.get('headers',{}),'preview':record.get('preview','')},sort_keys=True).encode()).hexdigest()))
options=dict(conversation='Synthetic chat',owner='Alice',timezone='America/Chicago',date_order='mdy',conversation_kind='group',participants=[])
for text in ['[1/2/24, 9:30 AM] Alice: hello\ncontinued\n[1/2/24, 9:31 AM] Bob: <attached: photo.jpg>\n[1/2/24, 9:31 AM] Bob: <attached: photo.jpg>','3/10/24, 2:30 AM - Alice: spring gap','11/3/24, 1:30 AM - Alice: fall overlap','2024-01-02, 14:20:01 - +1234567890: phone (file attached)','1/2/24, 9:00 - encryption event']:
 cases.append((dict(op='phone',text=text,options=options),list(phone.parse_export(text.splitlines(),**options))))
subprocess.run(['cargo','build','-q','-p','monica-core','--example','source_contract'],cwd=ROOT,check=True)
result=subprocess.run([str(ROOT/'target/debug/examples/source_contract')],input=''.join(json.dumps(case,ensure_ascii=False)+'\n' for case,_ in cases),text=True,capture_output=True,check=True)
actual=[json.loads(line) for line in result.stdout.splitlines()]
failures=[]
for i,((case,expected),observed) in enumerate(zip(cases,actual)):
 if expected!=observed:failures.append(dict(index=i,input=case,expected=expected,observed=observed))
if len(actual)!=len(cases):raise SystemExit('probe response count mismatch')
if failures:print(json.dumps(failures,ensure_ascii=False,indent=2));raise SystemExit(1)
print(json.dumps({'reference':COMMIT,'observations':len(cases),'result':'PASS','network_calls':0},ensure_ascii=False))

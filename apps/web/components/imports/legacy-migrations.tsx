import {useState} from 'react';
import {useQuery} from '@tanstack/react-query';
import {fetchJson} from '../../lib/queries';
import {Section,Failure,formatDateTime,formatNumber} from '../ui/primitives';
import {Button} from '../ui/button';
import {Dialog} from '../ui/dialog';
import styles from './imports.module.css';

type Table={store:string;table_name:string;mode:string;has_history?:boolean;expected:number;processed:number;status:string};
type Migration={id:string;status:string;created_at:string;manifest_hash:string;tables:Table[]};
type Chatlog={id:string;status:string;records:number;inserted:number;unchanged:number;changed:number;files:number;issues:Record<string,number>};
type Copy={source?:string;state:string;phase:string;files_verified:number;bytes_verified:number;bytes_processed_this_pass?:number;heartbeat_at:number;error?:string};
const names:Record<string,string>={crm:'人物与资料',wechat:'微信',instagram:'Instagram',linkedin:'LinkedIn'};
const states:Record<string,string>={importing:'正在转换数据库',database_ready:'数据库已准备，等待文件与部署验收',complete:'迁移与验收已完成',failed:'迁移需要处理'};

export function LegacyMigrations({compact=false}:{compact?:boolean}) {
 const query=useQuery({queryKey:['legacy-migrations'],queryFn:()=>fetchJson<{records:Migration[];chatlog:Chatlog[];copies:Copy[]}>('/api/v1/imports/migrations'),refetchInterval:()=>document.visibilityState==='visible'?10000:30000});
 const [selected,setSelected]=useState<Migration|null>(null);
 const [table,setTable]=useState<Table|null>(null);
 const [archiveSnapshot,setArchiveSnapshot]=useState<string|null>(null);
 const [issueSnapshot,setIssueSnapshot]=useState<string|null>(null);
 const [issueAfter,setIssueAfter]=useState<string|null>(null);
 const issues=useQuery({queryKey:['legacy-chatlog-issues',issueSnapshot,issueAfter],queryFn:()=>fetchJson<{records:Record<string,unknown>[];next:string|null}>(`/api/v1/imports/chatlog/${issueSnapshot}/issues?`+new URLSearchParams({limit:'25',...(issueAfter?{after:issueAfter}:{})})),enabled:!!issueSnapshot});
 const [after,setAfter]=useState<string|null>(null);
 const history=useQuery({queryKey:['legacy-history',selected?.id,table?.store,table?.table_name,after],queryFn:()=>fetchJson<{records:{key:string;original:Record<string,unknown>}[];next:string|null}>(`/api/v1/imports/migrations/${selected!.id}/history?`+new URLSearchParams({store:table!.store,table:table!.table_name,...(after?{after}:{}),limit:'25'})),enabled:!!selected&&!!table});
 if(query.isError)return <Failure onRetry={()=>void query.refetch()}>无法读取旧版迁移状态。</Failure>;
 if(!query.data?.records.length&&!query.data?.chatlog?.length&&!query.data?.copies?.length)return null;
 return <Section title="旧版数据迁移" className={styles.panel}>
  <p>本次复制保留独立的数据副本。历史采集记录与本次迁移进度分别保存。</p>
  {query.data?.records.map(m=><div key={m.id}><p><strong>{states[m.status]||m.status}</strong> · {formatDateTime(m.created_at,true)}</p>
   {!compact&&<ul>{Object.entries(names).map(([store,label])=>{const tables=m.tables.filter(t=>t.store===store&&t.mode!=='rebuild');return <li key={store}>{label}：已处理 {formatNumber(tables.reduce((n,t)=>n+t.processed,0))} / {formatNumber(tables.reduce((n,t)=>n+t.expected,0))} 条记录</li>;})}</ul>}
   <Button size="sm" variant="ghost" onClick={()=>{setSelected(m);setTable(null);setAfter(null);}}>查看迁移校验</Button></div>)}
  {query.data?.chatlog?.map(snapshot=><p key={snapshot.id}><strong>Chatlog 快照 · {snapshot.status==='importing'?'正在对账':snapshot.status==='verified'?'内容校验通过':'已转换，等待校验'}</strong>：{formatNumber(snapshot.files)} 个数据库，已检查 {formatNumber(snapshot.records)} 条；新增 {formatNumber(snapshot.inserted)}，未变化 {formatNumber(snapshot.unchanged)}，保留修订 {formatNumber(snapshot.changed)}。{(snapshot.issues?.source_missing||0)>0&&<> 源数据缺失 {formatNumber(snapshot.issues.source_missing)} 项。</>}{(snapshot.issues?.pending||0)>0&&<> 待处理 {formatNumber(snapshot.issues.pending)} 项。</>}<Button size="sm" variant="ghost" onClick={()=>{setIssueSnapshot(snapshot.id);setIssueAfter(null);}}>查看差异诊断</Button><Button size="sm" variant="ghost" onClick={()=>setArchiveSnapshot(snapshot.id)}>表覆盖与历史档案</Button></p>)}
  {query.data?.copies?.map(copy=><p key={copy.source||copy.phase} role={copy.state==='paused'?'status':undefined}><strong>{({'chatlog-db':'Chatlog 数据库',media:'媒体',raw:'原始档案',staging:'暂存档案',exports:'导出档案',backups:'历史备份','sns-cache':'朋友圈媒体'} as Record<string,string>)[copy.source||'chatlog-db']||copy.source} · {copy.state==='complete'?'字节副本完成':copy.state==='paused'?'复制已暂停':'复制与校验中'}</strong>：{formatNumber(copy.files_verified)} 个文件，{formatNumber(Math.ceil((copy.bytes_verified||0)/1024/1024))} MiB 已校验{copy.state!=='complete'&&copy.bytes_processed_this_pass!==undefined&&`，本轮已读取 ${formatNumber(Math.ceil(copy.bytes_processed_this_pass/1024/1024))} MiB`}。最近推进：{formatDateTime(new Date(copy.heartbeat_at*1000).toISOString(),true)}{copy.error&&` · ${copy.error}`}</p>)}
  {archiveSnapshot&&<ChatlogArchive id={archiveSnapshot} onClose={()=>setArchiveSnapshot(null)}/>}
  {issueSnapshot&&<Dialog title="Chatlog 差异诊断" size="wide" onClose={()=>setIssueSnapshot(null)}><p>“源数据缺失”表示冻结的原始副本缺少相应引用。原文仍会保留；系统不会猜测发送者或撤回目标。</p>{issues.isError?<Failure onRetry={()=>void issues.refetch()}>无法读取差异。</Failure>:issues.isPending?<p role="status">正在读取…</p>:<><pre style={{whiteSpace:'pre-wrap',overflowWrap:'anywhere',maxHeight:'50vh',overflow:'auto'}}>{JSON.stringify(issues.data?.records,null,2)}</pre>{!issues.data?.records.length&&<p>本批次没有记录差异。</p>}<Button size="sm" variant="ghost" disabled={!issueAfter} onClick={()=>setIssueAfter(null)}>首批记录</Button><Button size="sm" variant="ghost" disabled={(issues.data?.records.length||0)<25} onClick={()=>setIssueAfter(issues.data?.next||null)}>后续记录</Button></>}</Dialog>}
  {selected&&<Dialog title="迁移校验与历史档案" size="wide" onClose={()=>setSelected(null)}>
   <p>{states[selected.status]||selected.status}</p><p>快照清单校验值：<code style={{overflowWrap:'anywhere'}}>{selected.manifest_hash}</code></p>
   <ul>{selected.tables.map(t=><li key={t.store+':'+t.table_name}><code>{t.store}.{t.table_name}</code> · {formatNumber(t.processed)} / {formatNumber(t.expected)} · {t.status==='complete'?'该项已提交':'尚未完成'}{(t.mode==='history'||t.has_history)&&<Button size="sm" variant="ghost" onClick={()=>{setTable(t);setAfter(null);}}>读取历史档案</Button>}</li>)}</ul>
   {table&&<section aria-label="历史档案"><h3>{table.table_name}</h3>{history.isError?<Failure onRetry={()=>void history.refetch()}>无法读取历史档案。</Failure>:history.isPending?<p role="status">正在读取…</p>:<><pre style={{whiteSpace:'pre-wrap',overflowWrap:'anywhere',maxHeight:'50vh',overflow:'auto'}}>{JSON.stringify(history.data?.records,null,2)}</pre>{history.data?.records.filter(r=>typeof r.original.raw_manifest_object_id==='string').map(r=><p key={r.key}><a href={'/api/v1/imports/legacy/content/'+encodeURIComponent(String(r.original.raw_manifest_object_id))} download>下载该批次原文</a></p>)}<Button size="sm" variant="ghost" disabled={!after} onClick={()=>setAfter(null)}>首批记录</Button><Button size="sm" variant="ghost" disabled={(history.data?.records.length||0)<25} onClick={()=>setAfter(history.data?.next||null)}>后续记录</Button></>}</section>}
  </Dialog>}
 </Section>;
}


type CatalogTable={file_name:string;table_name:string;mode:string;expected:number;processed:number;status:string;reason:string};
function ChatlogArchive({id,onClose}:{id:string;onClose:()=>void}) {
 const [cursor,setCursor]=useState<{after_file:string;after_table:string}|null>(null);
 const [selected,setSelected]=useState<CatalogTable|null>(null);
 const [after,setAfter]=useState<string|null>(null);
 const catalog=useQuery({queryKey:['chatlog-tables',id,cursor],queryFn:()=>fetchJson<{records:CatalogTable[];next:{after_file:string;after_table:string}|null}>(`/api/v1/imports/chatlog/${id}/tables?`+new URLSearchParams(cursor||{}))});
 const history=useQuery({queryKey:['chatlog-history',id,selected?.file_name,selected?.table_name,after],queryFn:()=>fetchJson<{records:{key:string;original:unknown}[];next:string|null}>(`/api/v1/imports/chatlog/${id}/history?`+new URLSearchParams({file:selected!.file_name,table:selected!.table_name,...(after?{after}:{})})),enabled:!!selected});
 return <Dialog title="Chatlog 表覆盖与历史档案" size="wide" onClose={onClose}>
  <p>业务表通过聊天和动态页面读取。历史表在此分页保留原始记录；运行状态与授权缓存只保留离线快照，不安装为可执行任务或登录凭据。</p>
  {catalog.isError?<Failure onRetry={()=>void catalog.refetch()}>无法读取表覆盖。</Failure>:catalog.isPending?<p role="status">正在读取…</p>:<><ul>{catalog.data?.records.map(t=><li key={t.file_name+':'+t.table_name}><code style={{overflowWrap:'anywhere'}}>{t.file_name} · {t.table_name}</code> · {({business:'业务转换',history:'历史归档',derived:'派生或运行状态'} as Record<string,string>)[t.mode]||t.mode} · {formatNumber(t.processed)} / {formatNumber(t.expected)} · {t.status==='complete'?'该项已提交':'尚未完成'}{t.mode==='history'&&<Button size="sm" variant="ghost" onClick={()=>{setSelected(t);setAfter(null);}}>读取记录</Button>}</li>)}</ul>{!catalog.data?.records.length&&<p>表覆盖尚未完成登记。</p>}<Button size="sm" variant="ghost" disabled={!cursor} onClick={()=>setCursor(null)}>首批表</Button><Button size="sm" variant="ghost" disabled={(catalog.data?.records.length||0)<100} onClick={()=>setCursor(catalog.data?.next||null)}>后续表</Button></>}
  {selected&&<section aria-label="Chatlog 历史记录"><h3>{selected.table_name}</h3>{history.isError?<Failure onRetry={()=>void history.refetch()}>无法读取历史记录。</Failure>:history.isPending?<p role="status">正在读取…</p>:<><pre style={{whiteSpace:'pre-wrap',overflowWrap:'anywhere',maxHeight:'50vh',overflow:'auto'}}>{JSON.stringify(history.data?.records,null,2)}</pre><Button size="sm" variant="ghost" disabled={!after} onClick={()=>setAfter(null)}>首批记录</Button><Button size="sm" variant="ghost" disabled={(history.data?.records.length||0)<25} onClick={()=>setAfter(history.data?.next||null)}>后续记录</Button></>}</section>}
 </Dialog>;
}

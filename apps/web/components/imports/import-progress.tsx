import {useState} from 'react';
import {useQuery} from '@tanstack/react-query';
import {fetchJson} from '../../lib/queries';
import {Button} from '../ui/button';
import {Field,Input,Select} from '../ui/field';
import {Dialog} from '../ui/dialog';
import {EmptyState,Failure,Loading,Section,formatDateTime,formatNumber,providerLabel} from '../ui/primitives';
import Link from '../../src/router/link';
import styles from './imports.module.css';
type Connection={id:string;provider:string;display_name:string;state:string;enabled:boolean;records:number;error:string|null;last_success_at:string|null;next_run_at:string|null};
type Adapter={provider:string;enabled:boolean;paused:boolean;run?:{status:string;heartbeat_at:string;progress:{phase?:string;processed?:number;total?:number|null}}};
type Status={collection:Run[];connections:Connection[];adapters:Adapter[];stages:Record<string,number>;analysis_execution_enabled?:boolean;week_analysis?:{job_id:string;kind:string;subject:string;status:string;sources:number|null;processed:number;phase:string}[];as_of:string};
type Run={kind:string;phase:string|null;heartbeat_at:string|null;next_run_at:string|null;id:string;provider:string;connection_id:string|null;stream:string;state:string;received_at:string;records:number;processed:number;inserted:number;changed:number;unchanged:number;failures:number;batch_id:string|null};
type Daily={date:string;provider:string;connection_id:string|null;stream:string;records:number;processed:number;inserted:number;changed:number;unchanged:number;failures:number};
const states:Record<string,string>={received:'已接收',running:'处理中',complete:'已可查询',saved:'已保存',failed:'失败',connected:'已连接',paused:'已暂停',error:'需要处理',syncing:'同步中',needs_auth:'需要授权',disconnected:'已断开',queued:'等待处理',cancelled:'已取消',ready:'本轮采集完成',needs_login:'需要登录',stopped:'已停止',waiting:'等待下一轮',collecting:'正在采集',publishing:'正在保存'};
const stageGroups = [
 {title:'保存与查询', description:'文字保存后，还需处理为页面和搜索能读取的内容。', items:[
  ['import_pending','尚未完成的导入','批次','一个批次可包含多条记录；失败的批次也计入。'],
  ['connection_records_pending','已保存、待处理','条记录','来源记录已接收，正在转换为人物、消息或动态。'],
  ['projection_pending','待更新的搜索与页面','项任务','处理完成后，相关内容才能在列表和搜索中出现。']]},
 {title:'补齐照片与人物资料', description:'这些步骤与文字查询分别推进。', items:[
  ['media_pending','等待下载或归档','个媒体引用','同一张照片可能被多处引用；这不是独立文件数。'],
  ['memory_records_pending','待补齐的分析素材','条引用','把已存正文、资料和图片识别结果整理给人物 Memory。'],
  ['memory_scopes_pending','待确认的人物归属','组记录','让聊天和来源内容归到正确的人物。'],
  ['observation_records_pending','待核对的发现时间','条动态','区分实际发生日期与首次发现日期，避免把旧事当新事。']]},
 {title:'AI 分析', description:'生成新的摘要、人物近况和周报；已保存结果可单独查看。', items:[
  ['analysis_pending','排队或执行中的分析','项任务','这是任务数量，不是消息或照片数量。'],
  ['week_refresh_pending','等待重新安排的周报','个范围','资料更新后，等待素材就绪再重新安排。']]},
];
const phases:Record<string,string>={convert:'数据库转换',database_ready:'数据库已准备',snapshot:'正在获取备份',media_copy:'媒体复制',verify:'完整性校验',followees:'关注账号',profiles:'人物资料',posts:'帖子',stories:'Story',highlights:'Highlight',connections:'联系人',profiles_posts:'资料与动态',messages:'消息',browser_login:'登录验证',idle:'等待下一轮',collecting:'采集中',sync:'来源同步',native_read:'本机读取'};
const stamp=(v:string|null)=>v?formatDateTime(v,true):'尚无记录';
export function ImportProgress({compact=false}:{compact?:boolean}){
 const status=useQuery({queryKey:['import-status'],queryFn:()=>fetchJson<Status>('/api/v1/imports/status'),refetchInterval:()=>document.visibilityState==='visible'?5000:30000});
 const [provider,setProvider]=useState('');const [connection,setConnection]=useState('');const [days,setDays]=useState('7');const [timezone,setTimezone]=useState(Intl.DateTimeFormat().resolvedOptions().timeZone||'America/Chicago');
 const [cursor,setCursor]=useState<{before:string;before_id:string}|null>(null);const [selected,setSelected]=useState<string|null>(null);
 const params=new URLSearchParams({...(provider?{provider}:{}),...(connection?{connection_id:connection}:{}),days,timezone});
 const daily=useQuery({queryKey:['import-daily',params.toString()],queryFn:()=>fetchJson<{records:Daily[]}>('/api/v1/imports/daily?'+params),enabled:!compact,refetchInterval:30000});
 const runs=useQuery({queryKey:['import-runs',params.toString(),cursor],queryFn:()=>fetchJson<{records:Run[];next:{before:string;before_id:string}|null}>('/api/v1/imports/runs?'+params+(cursor?'&'+new URLSearchParams(cursor):'')),enabled:!compact,refetchInterval:10000});
 const detail=useQuery({queryKey:['import-run',selected],queryFn:()=>fetchJson<{run:Run;events:{state:string;phase:string;created_at:string}[];note:string}>('/api/v1/imports/runs/'+encodeURIComponent(selected!)),enabled:!!selected});
 const connections=status.data?.connections.filter(c=>(!provider||c.provider===provider)&&(!connection||c.id===connection))||[];
 return <><Section title="同步与导入进度" className={styles.panel} action={compact?<Link href="/imports">查看每日进度与诊断</Link>:<Button size="sm" variant="ghost" onClick={()=>void status.refetch()}>更新状态</Button>}>
 {status.isError?<Failure onRetry={()=>void status.refetch()}>无法读取进度。</Failure>:null}
  {!status.data&&!status.isError&&<Loading>正在读取导入状态…</Loading>}
 {status.data&&compact?<p>尚未完成 {formatNumber(status.data.stages.import_pending)} 个批次 · 等待归档 {formatNumber(status.data.stages.media_pending)} 个媒体引用 · {status.data.connections.filter(c=>c.state==='error'||c.state==='needs_auth').length} 个来源需要处理</p>:status.data&&<>
 <p className={styles.note}>以下是全站待处理数量，各阶段单位不同，不能相加为总进度。更新于 {stamp(status.data.as_of)}。</p>
 <ol className={styles.pipeline}>{stageGroups.map(group=><li key={group.title}><h3>{group.title}</h3><p>{group.description}</p><dl>{group.items.filter(([key])=>key in status.data.stages).map(([key,label,unit,help])=><div key={key}><dt>{label}<small>{help}</small></dt><dd><strong>{formatNumber(status.data.stages[key])}</strong><span>{unit}</span></dd></div>)}</dl></li>)}</ol>
 {status.data.analysis_execution_enabled===false&&<p className={styles.notice}>新 AI 分析已暂停。队列会等待恢复；已有摘要、OCR 和周报仍可查看。</p>}
 {!!status.data.week_analysis?.length&&<details className={styles.explanation}><summary>查看正在处理的周报（最多 20 项）</summary><ul className={styles.sources}>{status.data.week_analysis.map(w=><li key={w.job_id}><strong>{w.kind==='ai.digest'?'每周回顾':'人物每周近况'} · {w.subject.split(':').at(-1)}</strong><p>{w.phase==='writing'?'素材已读完，等待生成最终内容':w.sources==null?'等待准备完整素材':`已读 ${formatNumber(w.processed)} / ${formatNumber(w.sources)} 条素材`}</p>{w.sources!=null&&w.sources>0&&<progress aria-label="周报素材处理进度" value={w.processed} max={w.sources}/>}</li>)}</ul></details>}
 </>}

 {!compact&&<><h3 className={styles.filterTitle}>按来源查看记录</h3><p className={styles.note}>筛选下方的来源、每日进度和运行记录。上方待处理数量始终显示全站。</p><div className={styles.filters}><Field label="来源"><Select value={provider} onChange={e=>{setProvider(e.target.value);setConnection('');setCursor(null);}}><option value="">全部来源</option>{['wechat','instagram','linkedin','whatsapp','apple_messages','apple_contacts','mail','monica'].map(p=><option key={p} value={p}>{providerLabel(p)}</option>)}</Select></Field><Field label="账号"><Select value={connection} onChange={e=>{setConnection(e.target.value);setCursor(null);}}><option value="">全部账号</option>{status.data?.connections.filter(c=>!provider||c.provider===provider).map(c=><option key={c.id} value={c.id}>{c.display_name}</option>)}</Select></Field><Field label="日期范围"><Select value={days} onChange={e=>setDays(e.target.value)}><option value="1">今天</option><option value="7">最近七天</option><option value="30">最近三十天</option><option value="90">最近九十天</option></Select></Field><Field label="时区"><Input value={timezone} onChange={e=>setTimezone(e.target.value)}/></Field></div></>}
 <ul className={styles.sources}>{!compact&&connections.map(c=><li key={c.id}><div className={styles.sourceHead}><strong>{providerLabel(c.provider)} · {c.display_name}</strong><span>{states[c.state]||c.state}</span></div><p>已接收 {formatNumber(c.records)} 条记录 · 最近成功 {stamp(c.last_success_at)}</p>{c.enabled&&<p>下次执行 {stamp(c.next_run_at)}</p>}{c.error&&<p role="alert">{c.error}</p>}</li>)}</ul>
 {!compact&&status.data?.collection?.filter(r=>!provider||r.provider===provider).map(r=><p key={r.id}>{providerLabel(r.provider)} · {states[r.state]||r.state} · {phases[r.phase||'']||r.phase||'等待'}{r.next_run_at?` · 下次执行 ${stamp(r.next_run_at)}`:''}{r.state==='collecting'&&r.heartbeat_at&&Date.now()-Date.parse(r.heartbeat_at)>120000?' · 心跳过期，等待恢复':''}</p>)}
 {!compact&&status.data?.adapters.filter(a=>!provider||a.provider===provider||a.provider===provider+'_messages').map(a=><p key={a.provider}>{providerLabel(a.provider)}：{!a.enabled?'未启用':a.paused?'已暂停':a.run?states[a.run.progress.phase||a.run.status]||a.run.status:'等待开始'}{a.run?.progress.processed!==undefined?` · 已处理 ${a.run.progress.processed}${a.run.progress.total!=null?' / '+a.run.progress.total:' · 总量未知'}`:''}{a.run?.status==='running'&&Date.now()-Date.parse(a.run.heartbeat_at)>120000?' · 心跳过期，等待恢复':''}</p>)}
 </Section>{!compact&&<><Section title="每日进度" className={styles.panel}><p className={styles.note}>按接收日期和所选时区汇总。“已处理”表示完成内容转换，不代表照片已下载或 AI 已分析。没有遥测的日期显示“无记录”，不等于没有采集。</p>{daily.isError?<Failure onRetry={()=>void daily.refetch()}>无法读取每日进度，请检查时区。</Failure>:daily.data?.records.length?<ul className={styles.batches}>{daily.data.records.map((d,i)=><li key={i}><div><strong>{d.date} · {providerLabel(d.provider)} · {status.data?.connections.find(c=>c.id===d.connection_id)?.display_name||'本地归档'} · {phases[d.stream]||d.stream}</strong><small>{formatNumber(d.inserted)} 新增 · {formatNumber(d.changed)} 更新 · {formatNumber(d.unchanged)} 未变化 · {formatNumber(d.failures)} 失败</small><small>已保存 {formatNumber(d.records)} 条 · 已处理 {formatNumber(d.processed)} 条</small></div></li>)}</ul>:<EmptyState title="此日期范围无记录">采集器没有上报可用的每日统计；可查看下方运行记录。</EmptyState>}</Section>
 <Section title="运行记录与诊断" className={styles.panel}>{runs.isError?<Failure onRetry={()=>void runs.refetch()}>无法读取运行记录。</Failure>:runs.data?.records.length?<ul className={styles.batches}>{runs.data.records.map(r=><li key={r.id}><div><strong>{providerLabel(r.provider)} · {phases[r.stream]||r.stream}</strong><small>{stamp(r.received_at)} · {r.kind==='collection'?phases[r.phase||'']||r.phase||'采集':`已保存 ${formatNumber(r.records)} · 已处理 ${formatNumber(r.processed)}`}</small></div><span>{states[r.state]||r.state}</span><Button size="sm" variant="ghost" onClick={()=>setSelected(r.id)}>查看诊断</Button></li>)}</ul>:<EmptyState title="尚无运行记录"/>}<div className={styles.filters}><Button size="sm" variant="ghost" disabled={!cursor} onClick={()=>setCursor(null)}>最新记录</Button><Button size="sm" variant="ghost" disabled={(runs.data?.records.length||0)<40} onClick={()=>setCursor(runs.data?.next||null)}>更早记录</Button></div></Section></>}
 {selected&&<Dialog title="运行详情" onClose={()=>setSelected(null)} size="wide">{detail.isError?<Failure>无法读取运行详情。</Failure>:detail.data&&<><p>{providerLabel(detail.data.run.provider)} · {detail.data.run.stream} · {states[detail.data.run.state]||detail.data.run.state}</p><p>新增 {detail.data.run.inserted} · 更新 {detail.data.run.changed} · 未变化 {detail.data.run.unchanged}</p>{detail.data.events.map((e,i)=><p key={i}>{stamp(e.created_at)} · {e.phase} · {states[e.state]||e.state}</p>)}<p>{detail.data.note}</p>{detail.data.run.batch_id&&<p>关联批次：{detail.data.run.batch_id}</p>}<Button size="sm" onClick={()=>{const blob=new Blob([JSON.stringify(detail.data,null,2)],{type:'application/json'});const url=URL.createObjectURL(blob);const a=document.createElement('a');a.href=url;a.download='monica-import-diagnostics.json';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}}>导出诊断</Button></>}</Dialog>}</>;
}

"use client";
import {Page, PageHeader} from "../ui/primitives";
import {Button} from "../ui/button";
import {Input,Select} from "../ui/field";

import { useEffect, useState, useRef } from 'react';
import { Mail, MessageCircle, ContactRound, Smartphone, Camera, BriefcaseBusiness, MessagesSquare } from 'lucide-react';
import Link from '../../src/router/link';
import { apiFetch } from '../../lib/api';
import {ImportProgress} from "../imports/import-progress";
import styles from './sources.module.css';
import { PersonPicker } from '../ui/person-picker';
import { VirtualList } from '../ui/virtual-list';

type Connection = { provider: string; records?: number; earliest_at?: string; latest_at?: string; sync_pending?: boolean; pending_records?: number; progress?: string; coverage?: string; id: string; display_name: string; external_account: string; state: string; enabled: boolean; error: string | null; last_success_at: string | null };
type SourceFile = { blob?: string; filename?: string; size?: number };
type Evidence = { id: string; connection_id: string; external_id: string; title: string; body: string; raw_body?: string; events?: { kind: string; title: string; date_text?: string; organization?: string; location?: string; reference?: string; quote: string }[]; copies?: { id: string; revision: number; account_name: string }[]; category: string; occurred_at: string | null; account_name: string; provider: string; revision: number; metadata: { original?: SourceFile; attachments?: SourceFile[]; participants?: { id: string; role: string }[] } };
async function request(path: string, body?: unknown) {
  const response = await apiFetch(path, body ? { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {});
  if (!response.ok) { const error = await response.json(); throw new Error(error.detail || `请求失败 (${response.status})`); }
  return response.json();
}

const providers = [
 {id:'mail',name:'邮件',detail:'Gmail、Outlook 与 IMAP。每个邮箱独立管理收录范围。',icon:Mail},
 {id:'wechat',name:'微信',detail:'联系人、聊天与朋友圈。连接本机 Chatlog 归档。',icon:MessagesSquare},
 {id:'whatsapp',name:'WhatsApp',detail:'连接设备同步新消息，也可导入手机历史。',icon:MessageCircle},
 {id:'apple_contacts',name:'Contacts',detail:'Mac 通讯录中的姓名、电话与邮箱。',icon:ContactRound},
 {id:'apple_messages',name:'Messages',detail:'这台 Mac 已保存的短信与 iMessage。',icon:Smartphone},
 {id:'instagram',name:'Instagram',detail:'已授权归档的个人资料、动态与媒体。',icon:Camera},
 {id:'linkedin',name:'LinkedIn',detail:'已授权归档的经历、资料与动态。',icon:BriefcaseBusiness},
];
const stateNames:Record<string,string>={legacy:'已导入归档',connected:'已连接',connecting:'正在连接',paused:'已暂停',disconnected:'已断开',error:'需要处理',needs_auth:'需要授权',syncing:'同步中'};
export function SourcesView() {
  const [capabilities, setCapabilities] = useState<Record<string, { collection_available: boolean; unavailable_reason?: string }>>({});
  useEffect(() => { void request('/api/v1/sources').then(rows => setCapabilities(Object.fromEntries(rows.map((row: {id: string; collection_available: boolean}) => [row.id, row])))).catch(e => setError(String(e))); }, []);
  const [provider,setProvider]=useState('mail');
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [connections, setConnections] = useState<Connection[]>([]);
  const [records, setRecords] = useState<Evidence[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [filter, setFilter] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [saving, setSaving] = useState(false);
  const [method, setMethod] = useState('imap');
  const [redirect, setRedirect] = useState('');
  const refreshVersion=useRef(0);
  const refresh = async () => { const version=++refreshVersion.current; const [rows,progress]=await Promise.all([request('/api/v1/source-connections'),request('/api/v1/source-connections/progress')]); if(version!==refreshVersion.current)return; const byId=new Map(progress.map((p:Connection)=>[p.id,p]));setConnections(rows.map((row:Connection)=>({...row,...(byId.get(row.id) as Partial<Connection>||{})}))); };
  useEffect(() => { void refresh().catch(e => setError(String(e))); const timer=setInterval(()=>void refresh().catch(e=>setError(String(e))),10000);return()=>clearInterval(timer); }, []);
  useEffect(() => {
    const result = new URLSearchParams(location.search).get('mail_authorization');
    if (result === 'connected') setNotice('邮箱已授权，后台正在开始同步。');
    if (result === 'cancelled') setNotice('邮箱授权已取消。');
    if (result === 'failed') setError('邮箱授权未完成。请检查 Client ID、回调地址和账号权限后重试。');
  }, []);
  useEffect(() => {
    if (method === 'imap') return;
    void request('/api/v1/mail-oauth/config').then(value => setRedirect(value.redirect_uri)).catch(e => setError(String(e)));
  }, [method]);
  useEffect(() => {
    let active = true;
    void request('/api/v1/evidence?limit=40&include_filtered=true&provider=mail' + (filter ? '&category=' + filter : '')).then(rows => { if (active) { setRecords(rows); setHasMore(rows.length === 40); } }).catch(e => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, [filter]);
  async function action(id: string, action: string) {
    try { await request(`/api/v1/source-connections/${id}/action`, { action }); await refresh(); setError(''); } catch (e) { setError(String(e)); }
  }
  async function rule(row: Evidence, scope: string, category: string) {
    const target = scope === 'message' ? row.external_id : row.metadata.participants?.find(p => p.role === 'sender')?.id;
    if (!target) return;
    try {
      const result = await request(`/api/v1/source-connections/${row.connection_id}/mail-rules`, { scope, target, category });
      setNotice(`已保存规则，正在重新处理 ${result.queued} 封相关邮件。`); setError('');
    } catch (e) { setError(String(e)); }
  }
  return <Page width="wide" className={styles.page}>
    <PageHeader title="来源" description="把不同账号的往来汇入同一份关系档案。每个来源单独授权、同步和暂停。" />
    <ImportProgress compact />
    <div className={styles.catalogue} aria-label="来源类型">{providers.map(source=><article key={source.id} className={`${styles.sourceCard} ${provider===source.id?styles.selected:''}`}>
      <div className={styles.cardHeading}><source.icon size={23} aria-hidden="true"/><h2>{source.name}</h2><span>{connections.filter(c=>c.provider===source.id).length} 个账号</span></div><p>{source.detail}</p>
      {source.id==='mail'?<div className={styles.row}>{[['gmail','Gmail'],['outlook','Outlook'],['imap','IMAP']].map(([value,label])=><Button key={value} onClick={()=>{setProvider('mail');setMethod(value);document.getElementById('source-details')?.scrollIntoView({block:'start'});}}>添加 {label}</Button>)}</div>:null}
      <Button className={styles.chooseSource} aria-pressed={provider===source.id} onClick={()=>setProvider(source.id)}>查看{source.name}来源</Button>
    </article>)}</div>
    <div id="source-details" className={styles.detailHeading}><h2>{providers.find(s=>s.id===provider)?.name}账号</h2><p>状态来自后台同步记录。历史尚未补齐时，只代表当前已收录的范围。</p></div>
    {error && <p role="alert">{error}</p>}{notice && <p role="status">{notice}</p>}
    <section aria-label="已连接账号"><div className={styles.row}><h2>已连接账号</h2><Button onClick={() => void refresh().catch(e => setError(String(e)))}>更新状态</Button></div>
      {connections.filter(c=>c.provider===provider).length === 0 && <p>还没有连接此来源。</p>}
      {connections.filter(c=>c.provider===provider).map(c => <article key={c.id}><h3>{c.display_name}</h3><p>{c.external_account} · {stateNames[c.state]||c.state}</p>
        <p>{c.records===undefined?'收录范围待同步':`已收录 ${c.records} 条`}{c.sync_pending?' · 等待后台继续同步':''}{c.pending_records?` · ${c.pending_records} 条待投影`:''}</p>
        {(c.earliest_at||c.latest_at)&&<p>已收录范围：{c.earliest_at?new Date(c.earliest_at).toLocaleDateString():'未知'} — {c.latest_at?new Date(c.latest_at).toLocaleDateString():'未知'}</p>}
        {c.progress&&<p>{c.progress}</p>}{c.coverage&&<p>{c.coverage}</p>}
        {c.last_success_at && <small>上次同步：{new Date(c.last_success_at).toLocaleString()}</small>}{c.error && <p>{c.error}</p>}
        <div className={styles.row}><Button disabled={capabilities[c.provider]?.collection_available === false} onClick={() => void action(c.id, 'refresh')}>立即同步</Button><Button disabled={!c.enabled && capabilities[c.provider]?.collection_available === false} onClick={() => void action(c.id, c.enabled ? 'pause' : 'resume')}>{c.enabled ? '暂停' : '继续'}</Button>
          <Button onClick={() => void action(c.id, 'disconnect')}>断开连接</Button></div><details onToggle={event => { const open = event.currentTarget.open; setExpanded(old => ({...old,[c.id]:open})); }}><summary>管理人物关联</summary>{expanded[c.id] && <ConnectionPeople connection={c.id} />}</details>{c.provider==='mail'&&<MailRules connection={c.id}/>}</article>)}
    </section>
    {provider==='mail'&&<><section><h2>添加邮箱</h2>
      <label>连接方式 <Select value={method} onChange={e => setMethod(e.target.value)}><option value="imap">IMAP／应用专用密码</option><option value="gmail">Google 授权</option><option value="outlook">Microsoft 授权</option></Select></label>
      {method === 'imap' ? <><p>Gmail 可使用应用专用密码。其他邮箱需要开启 IMAP over TLS。</p>
      <form onSubmit={async event => {
        event.preventDefault(); const form = event.currentTarget; const data = new FormData(form); setSaving(true);
        try {
          await request('/api/v1/source-connections', { provider: 'mail', connection_method: 'imap', external_account: data.get('email'), display_name: data.get('name'), host: data.get('host'), password: data.get('password'), folders: String(data.get('folders')).split(',').map(v => v.trim()).filter(Boolean) });
          form.reset(); await refresh(); setNotice('邮箱已添加，后台将开始只读同步。'); setError('');
        } catch (e) { setError(String(e)); } finally { setSaving(false); }
      }}>
        <label>名称<Input name="name" placeholder="个人邮箱" required /></label>
        <label>邮箱地址<Input name="email" type="email" autoComplete="username" required /></label>
        <label>IMAP 服务器<Input name="host" defaultValue="imap.gmail.com" required /></label>
        <label>应用专用密码<Input name="password" type="password" autoComplete="new-password" required /></label>
        <label>文件夹（逗号分隔）<Input name="folders" defaultValue="INBOX" required /></label>
        <Button type="submit" disabled={saving}>{saving ? '正在保存…' : '连接邮箱'}</Button>
      </form></> : <form onSubmit={async event => {
        event.preventDefault(); const data = new FormData(event.currentTarget); setSaving(true);
        try {
          const value = await request('/api/v1/mail-oauth/start', { provider: method, client_id: data.get('client_id'), ...(data.get('client_secret') ? { client_secret: data.get('client_secret') } : {}) });
          location.assign(value.authorization_url);
        } catch (e) { setError(String(e)); setSaving(false); }
      }}>
        <p>{method === 'gmail' ? '使用 Google Cloud 的桌面应用凭据。' : '使用 Microsoft Entra 的公共客户端应用，并登记下方回调地址。'}授权只读取邮件。</p>
        <label>Client ID<Input name="client_id" required autoComplete="off" /></label>
        {method === 'gmail' && <label>桌面应用 Client Secret（凭据文件中如有）<Input name="client_secret" type="password" autoComplete="off" /></label>}
        <label>回调地址<Input value={redirect} readOnly /></label>
        <Button type="submit" disabled={saving || !redirect}>{saving ? '正在打开授权…' : '前往账号授权'}</Button>
      </form>}
    </section>
    <section><div className={styles.row}><h2>邮件收录</h2><label>显示 <Select value={filter} onChange={e => setFilter(e.target.value)}><option value="">所有类别</option><option value="personal">人际交流</option><option value="transactional">实用事务</option><option value="marketing">已过滤</option><option value="pending">待分类</option></Select></label></div>
      <VirtualList items={records.map(row => ({key:row.id,content:<article><h3><Link href={'/evidence/' + row.id}>{row.title || '无标题'}</Link></h3><p>{row.account_name} · {row.category} · {row.occurred_at ? new Date(row.occurred_at).toLocaleString() : '时间未知'}</p>
        <div className={styles.row}><Button onClick={() => void rule(row, 'message', 'personal')}>包含此邮件</Button><Button onClick={() => void rule(row, 'sender', 'personal')}>始终包含此发件人</Button><Button onClick={() => void rule(row, 'sender', 'marketing')}>始终忽略此发件人</Button></div>
      </article>}))} />
      {hasMore && <Button onClick={async () => {
        const last = records.at(-1)!;
        try { const more = await request('/api/v1/evidence?' + new URLSearchParams({ limit: '40', include_filtered: 'true', provider: 'mail', ...(last.occurred_at ? { before: last.occurred_at } : {}), before_id: last.id, ...(filter ? { category: filter } : {}) })); setRecords(old => [...old, ...more]); setHasMore(more.length === 40); } catch (e) { setError(String(e)); }
      }}>更早的邮件</Button>}
    </section>
    </>}
    {provider==='whatsapp'&&<><WhatsAppConnect onConnected={refresh}/><WhatsAppExport /></>}
    {['apple_contacts','apple_messages'].includes(provider)&&<section>{capabilities[provider]?.collection_available ? <><h2>在 Mac 上连接</h2><p>打开 Monica Rust 菜单栏的设置窗口，在「本机来源」中启用 {provider==='apple_contacts'?'Contacts':'Messages'}。系统授权在本机完成。</p><p>默认关闭。暂停来源会保留已经收录的证据。</p></> : <><h2>本机采集不可用</h2><p>{capabilities[provider]?.unavailable_reason || '正在确认此部署的本机来源能力。'}</p></>}</section>}
    {['wechat','instagram','linkedin'].includes(provider)&&<section><h2>连接与导入</h2><p>使用已授权的本地归档或采集适配器，进度和记录可在导入页面查看。</p><Link href="/imports">打开导入与同步</Link></section>}
  </Page>;
}

function MailRules({connection}:{connection:string}) {
 const [rules,setRules]=useState<{scope:string;target:string;category:string}[]>([]);const [error,setError]=useState('');
 return <details onToggle={event=>{if(event.currentTarget.open)void request(`/api/v1/source-connections/${connection}/mail-rules`).then(setRules).catch(e=>setError(String(e)))}}><summary>邮件收录规则</summary>
 <p>在下方「邮件收录」中选择单封邮件或发件人，调整包含与忽略规则。</p>{error&&<p role="alert">{error}</p>}{rules.length?rules.map((rule,index)=><p key={index}>{({message:'单封邮件',thread:'邮件会话',sender:'发件人'} as Record<string,string>)[rule.scope]||rule.scope}：{rule.target} · {({personal:'人际交流',transactional:'实用事务',marketing:'已过滤',pending:'待分类'} as Record<string,string>)[rule.category]||rule.category}</p>):<p>尚未设置自定义规则。</p>}</details>;
}

function WhatsAppConnect({onConnected}:{onConnected:()=>Promise<void>}) {
 const [connection,setConnection]=useState('');const [state,setState]=useState<{state?:string;qr_data?:string;error?:string}>({});const [error,setError]=useState('');const [busy,setBusy]=useState(false);
 useEffect(()=>{if(!connection)return;let active=true;const read=()=>request(`/api/v1/source-connections/${connection}/whatsapp/status`).then(value=>{if(active)setState(value)}).catch(e=>{if(active)setError(String(e))});void read();const timer=setInterval(read,2500);return()=>{active=false;clearInterval(timer)}},[connection]);
 return <section><h2>连接 WhatsApp 设备</h2><p>显式连接后才启动设备同步。用手机 WhatsApp 的「已连接的设备」扫描二维码。</p><form onSubmit={async event=>{event.preventDefault();const data=new FormData(event.currentTarget);setBusy(true);setError('');try{const value=await request('/api/v1/whatsapp/connect',{external_account:data.get('account'),display_name:data.get('name')});setConnection(value.id||value.connection_id);await onConnected();}catch(e){setError(String(e))}finally{setBusy(false)}}}><label>账号标识<Input name="account" placeholder="个人 WhatsApp" required/></label><label>显示名称<Input name="name" placeholder="我的 WhatsApp" required/></label><Button type="submit" disabled={busy}>{busy?'正在连接…':'连接设备'}</Button></form>
 {state.state&&<p role="status">{state.state==='needs_bridge'?'设备同步组件尚未就绪。仍可使用下方手机历史导入。':state.state}</p>}{state.qr_data&&<img className={styles.qr} src={state.qr_data} alt="WhatsApp 连接二维码"/>}{(error||state.error)&&<p role="alert">{error||state.error}</p>}</section>;
}

export function EvidenceView({ id, revision }: { id: string; revision?: string }) {
  const [original, setOriginal] = useState(false);
  const [record, setRecord] = useState<Evidence | null>(null); const [error, setError] = useState('');
  useEffect(() => { void request('/api/v1/evidence/' + id + (revision ? '?revision=' + encodeURIComponent(revision) : '')).then(setRecord).catch(e => setError(String(e))); }, [id, revision]);
  return <Page width="wide" className={styles.page}><Link href="/sources">返回来源</Link>{error && <p role="alert">{error}</p>}{record ? <>
    <PageHeader title={record.title || '原始证据'} /><p>{record.provider} · {record.account_name} · 版本 {record.revision}</p><p>{record.occurred_at ? new Date(record.occurred_at).toLocaleString() : '时间未知'}</p>
    <p>{record.metadata.participants?.map(p => `${p.role}: ${p.id}`).join(' · ')}</p><div className={styles.row}><Button onClick={() => setOriginal(!original)}>{original ? '查看检索正文' : '查看完整原文'}</Button></div>
    <pre className={styles.body}>{(original ? record.raw_body : record.body) || '此记录尚未收录完整正文。'}</pre>
    {[...(record.metadata.original ? [record.metadata.original] : []), ...(record.metadata.attachments || [])].map((file, index) => file.blob ? <p key={file.blob + index}><a href={`/api/v1/evidence/${record.id}/files/${file.blob}?revision=${record.revision}`}>{file.filename || '附件'}{file.size !== undefined ? ` (${Math.ceil(file.size / 1024)} KB)` : ''}</a></p> : <p key={index}>{file.filename || '附件'} · 尚未下载</p>)}
    {record.events && record.events.length > 0 && <section><h2>事务与事件候选</h2>{record.events.map((event, index) => <article key={index}><h3>{event.title}</h3><p>{[event.date_text, event.organization, event.location, event.reference].filter(Boolean).join(' · ')}</p><blockquote>{event.quote}</blockquote><small>来自此版本邮件的候选信息</small></article>)}</section>}
    {record.copies && record.copies.length > 1 && <p>此邮件的账号副本：{record.copies.map(copy => <Link key={copy.id} href={`/evidence/${copy.id}?revision=${copy.revision}`}>{copy.account_name} </Link>)}</p>}
  </> : !error && <p role="status">正在读取…</p>}</Page>;
}

type SourcePerson = { external_id: string; display_name: string; is_self: boolean; profile_id: string | null; profile_name: string | null; blocked: boolean | null; candidates: { id: string; name: string }[] };
function ConnectionPeople({ connection }: { connection: string }) {
  const [people, setPeople] = useState<SourcePerson[]>([]);
  const [query, setQuery] = useState('');
  const [editing, setEditing] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [more, setMore] = useState(false);
  async function load(append = false) {
    const after = append ? people.at(-1)?.external_id || '' : '';
    const rows = await request(`/api/v1/source-connections/${connection}/people?` + new URLSearchParams({ q: query, after }));
    setPeople(old => append ? [...old, ...rows] : rows); setMore(rows.length === 40);
  }
  useEffect(() => { let active = true; const timer = setTimeout(() => {
    void request(`/api/v1/source-connections/${connection}/people?` + new URLSearchParams({ q: query })).then(rows => {
      if (active) { setPeople(rows); setMore(rows.length === 40); }
    }).catch(e => { if (active) setError(String(e)); });
  }, 200); return () => { active = false; clearTimeout(timer); }; }, [connection, query]);
  async function decide(external_id: string, action: string, profile_id?: string) {
    try { await request(`/api/v1/source-connections/${connection}/identity`, { external_id, action, profile_id }); setEditing(null); await load(); setError(''); }
    catch (e) { setError(String(e)); }
  }
  return <div><Button onClick={() => void load().catch(e => setError(String(e)))}>刷新人物</Button><Input aria-label="搜索来源人物" placeholder="搜索姓名或账号" value={query} onChange={e => setQuery(e.target.value)} />
    {error && <p role="alert">{error}</p>}
    {people.map(person => <article key={person.external_id}><strong>{person.display_name}</strong><p>{person.external_id}{person.is_self ? ' · 我' : ''}</p>
      <p>{person.blocked ? '已解除，不自动关联' : person.profile_name ? `已关联：${person.profile_name}` : '自动匹配；有冲突时等待确认'}</p>
      <div className={styles.row}><Button onClick={() => setEditing(person.external_id)}>选择人物</Button><Button onClick={() => void decide(person.external_id, 'unlink')}>解除关联</Button><Button onClick={() => void decide(person.external_id, 'automatic')}>恢复自动匹配</Button></div>
      {!person.profile_id && !person.blocked && person.candidates.map(candidate => <Button key={candidate.id} onClick={() => void decide(person.external_id, 'link', candidate.id)}>关联到 {candidate.name}</Button>)}
      {editing === person.external_id && <PersonPicker onPick={person => void decide(editing, 'link', person.id)} />}
    </article>)}
    {more && <Button onClick={() => void load(true).catch(e => setError(String(e)))}>更多人物</Button>}
    {!people.length && <p>还没有收录匹配的来源人物。</p>}
  </div>;
}

function WhatsAppExport() {
  const [message, setMessage] = useState(''); const [busy, setBusy] = useState(false);
  return <section><h2>补充 WhatsApp 手机历史</h2><p>选择手机导出的聊天 TXT 或含附件的 ZIP。导出姓名只在该会话内识别，导入后可在账号下关联到人物。</p>
    <form onSubmit={async event => {
      event.preventDefault(); const form = event.currentTarget; setBusy(true); setMessage('正在读取导出文件…');
      try { const response = await apiFetch('/api/v1/whatsapp/phone-export', { method: 'POST', body: new FormData(form) }); const result = await response.json();
        if (!response.ok) throw new Error(result.detail || '导入失败'); setMessage(`已读取 ${result.records_read} 条记录，后台正在更新人物时间线。可刷新上方账号列表查看。`);
      } catch (e) { setMessage(String(e)); } finally { setBusy(false); }
    }}>
      <label>导出文件<Input name="file" type="file" accept=".txt,.zip" required /></label>
      <label>会话名称<Input name="conversation" placeholder="相同会话请使用相同名称" required /></label>
      <label>会话类型<Select name="conversation_kind"><option value="group">群聊 / 不确定</option><option value="direct">我和另一人的私聊</option></Select></label>
      <label>我在导出文件中的姓名<Input name="owner" required /></label>
      <label>日期顺序<Select name="date_order"><option value="mdy">月 / 日 / 年</option><option value="dmy">日 / 月 / 年</option></Select></label>
      <label>导出时间所在时区<Input name="timezone" defaultValue={Intl.DateTimeFormat().resolvedOptions().timeZone || 'America/Chicago'} required /></label>
      <Button type="submit" disabled={busy}>{busy ? '正在导入…' : '导入历史'}</Button>
    </form>{message && <p role="status">{message}</p>}
  </section>;
}

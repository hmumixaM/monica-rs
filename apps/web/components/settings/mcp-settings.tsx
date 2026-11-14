import {useState} from 'react';
import {useQuery} from '@tanstack/react-query';
import {fetchJson} from '../../lib/queries';
import {Button} from '../ui/button';
import {Field,Input,Checkbox} from '../ui/field';
import {Failure,Section,formatDateTime} from '../ui/primitives';
import styles from './settings.module.css';
type Settings={enabled:boolean;public_url:string|null};
type Key={id:string;name:string;last_used_at:string|null;revoked_at:string|null};
async function send(url:string,body:unknown,method='POST') { const {apiFetch}=await import('../../lib/api'); const response=await apiFetch(url,{method,headers:{'Content-Type':'application/json'},body:JSON.stringify(body)}); const value=await response.json(); if(!response.ok)throw new Error(value.detail||'操作未完成，请重试。'); return value; }
export function McpSettings(){
 const settings=useQuery({queryKey:['mcp-settings'],queryFn:()=>fetchJson<Settings>('/api/v1/mcp/settings')});
 const keys=useQuery({queryKey:['mcp-keys'],queryFn:()=>fetchJson<Key[]>('/api/v1/mcp/keys')});
 const [error,setError]=useState('');const [secret,setSecret]=useState('');const [notice,setNotice]=useState('');const [busy,setBusy]=useState(false);
 async function perform(action:()=>Promise<unknown>){setBusy(true);setError('');setNotice('');try{await action();await Promise.all([settings.refetch(),keys.refetch()]);}catch(e){setError(String(e));}finally{setBusy(false);}}
 const address=settings.data?.public_url||location.origin+'/mcp';
 return <Section title="AI 访问 / MCP" className={styles.panel}>
  <p>让 AI 查询消息、人物和导入状态，并管理人物资料。每个客户端可以使用独立的 access key。</p>
  {error&&<Failure>{error}</Failure>}{notice&&<p role="status">{notice}</p>}
  {settings.isError||keys.isError?<Failure onRetry={()=>void Promise.all([settings.refetch(),keys.refetch()])}>无法读取 MCP 设置。</Failure>:null}
  {settings.data&&<form key={JSON.stringify(settings.data)} className={styles.setup} onSubmit={e=>{e.preventDefault();const f=new FormData(e.currentTarget);void perform(async()=>{await send('/api/v1/mcp/settings',{enabled:f.get('enabled')==='on',public_url:String(f.get('public_url')||'')},'PATCH');setNotice('MCP 设置已保存。');});}}>
   <Checkbox name="enabled" defaultChecked={settings.data.enabled}>启用 MCP</Checkbox>
   <Field label="远程 MCP 地址" hint="使用已有 HTTPS 反向代理时填写，例如 https://monica.example.com/mcp。留空使用本机地址。"><Input name="public_url" type="url" defaultValue={settings.data.public_url||''} placeholder="https://…/mcp"/></Field>
   <Button type="submit" size="sm" disabled={busy}>保存 MCP 设置</Button>
  </form>}
  <p>当前连接地址：<code>{address}</code></p>
  <form className={styles.setup} onSubmit={e=>{e.preventDefault();const form=e.currentTarget;const f=new FormData(form);void perform(async()=>{const value=await send('/api/v1/mcp/keys',{name:f.get('name'),key:f.get('key')}) as {key:string};setSecret(value.key);form.reset();});}}>
   <Field label="客户端名称"><Input name="name" required maxLength={100} placeholder="我的 AI 助手"/></Field>
   <Field label="Access key" hint="留空自动生成，也可填写 32–512 字节的独立密钥。"><Input name="key" type="password" autoComplete="new-password" maxLength={512}/></Field>
   <Button type="submit" size="sm" disabled={busy}>创建 access key</Button>
  </form>
  {secret&&<div className={styles.recovery}><strong>请保存新密钥，仅本次显示。</strong><code className={styles.secret}>{secret}</code><div className={styles.actions}>
   <Button size="sm" onClick={()=>void perform(async()=>{await navigator.clipboard.writeText(JSON.stringify({mcpServers:{monica:{url:address,headers:{Authorization:`Bearer ${secret}`}}}},null,2));setNotice('客户端配置已复制。');})}>复制客户端配置</Button>
   <Button size="sm" variant="ghost" onClick={()=>setSecret('')}>已保存，隐藏密钥</Button></div></div>}
  <ul className={styles.list}>{keys.data?.map(key=><li key={key.id} className={styles.split}><div className={styles.text}><strong>{key.name}</strong><small>{key.revoked_at?'已撤销':key.last_used_at?`最近使用：${formatDateTime(key.last_used_at,true)}`:'尚未使用'}</small></div>{!key.revoked_at&&<div className={styles.actions}><Button size="sm" variant="ghost" disabled={busy} onClick={()=>{if(window.confirm(`替换“${key.name}”的密钥？旧密钥将立即失效，客户端需要配置新密钥。`))void perform(async()=>{const value=await send(`/api/v1/mcp/keys/${key.id}/rotate`,{});setSecret(value.key);setNotice('密钥已替换，旧密钥立即失效。');});}}>替换</Button><Button size="sm" variant="ghost" disabled={busy} onClick={()=>{if(window.confirm(`撤销“${key.name}”的密钥？使用它的客户端将立即断开。`))void perform(async()=>{await send(`/api/v1/mcp/keys/${key.id}/revoke`,{});setSecret('');setNotice('密钥已撤销。');});}}>撤销</Button></div>}</li>)}</ul>
 </Section>;
}
export function PasswordSettings(){
 const [error,setError]=useState('');const [busy,setBusy]=useState(false);
 return <Section title="账号与密码" className={styles.panel}><p>忘记密码时，在本机 Monica Rust 设置中选择“重置登录密码”，已有资料和来源配置会保留。</p>{error&&<Failure>{error}</Failure>}
 <form className={styles.setup} onSubmit={async e=>{e.preventDefault();const f=new FormData(e.currentTarget);if(f.get('password')!==f.get('confirm')){setError('两次新密码不一致。');return;}setBusy(true);setError('');try{await send('/api/v1/auth/password',{current_password:f.get('current'),password:f.get('password')});location.assign('/login');}catch(e){setError(String(e));}finally{setBusy(false);}}}>
 <Field label="当前密码"><Input name="current" type="password" autoComplete="current-password" required/></Field><Field label="新密码"><Input name="password" type="password" autoComplete="new-password" minLength={12} maxLength={512} required/></Field><Field label="确认新密码"><Input name="confirm" type="password" autoComplete="new-password" minLength={12} maxLength={512} required/></Field><Button type="submit" size="sm" disabled={busy}>修改密码并重新登录</Button></form></Section>;
}

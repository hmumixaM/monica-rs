"use client";
import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';
import { Link2 } from 'lucide-react';
import { apiFetch } from '../../lib/api';
import { fetchJson } from '../../lib/queries';
import { Button } from '../ui/button';
import { Checkbox, Field, Input, Select } from '../ui/field';
import { Failure, Section, SourceMark, formatNumber } from '../ui/primitives';
import styles from './source-evidence.module.css';

type Coverage = { sources: { connection_id: string; display_name: string; provider: string; state: string; records: number; earliest_at: string; latest_at: string }[];
  identifiers: { kind: string; value: string; is_self: boolean }[]; coverage: string };
export function SourceCoverage({ profileId }: { profileId: string }) {
  const query = useQuery({ queryKey: ['source-coverage', profileId], queryFn: () => fetchJson<Coverage>(`/api/v1/profiles/${profileId}/source-coverage`) });
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const [kind, setKind] = useState('email');
  const [saved, setSaved] = useState(false);
  return <Section title="来源与关联账号" icon={<Link2 size={16} aria-hidden="true" />} className={styles.coverage}>
    {query.isPending && <p className={styles.hint} role="status">正在读取来源信息…</p>}
    {query.data && !query.data.sources.length && <p className={styles.hint}>还没有关联的来源账号。</p>}
    {!!query.data?.sources.length && <ul className={styles.sources}>{query.data.sources.map(source => <li key={source.connection_id}>
      <SourceMark provider={source.provider} decorative /><div><strong>{source.display_name}</strong><span>{formatNumber(source.records)} 条记录 · {({connected:'已连接',syncing:'同步中',paused:'已暂停',needs_auth:'需要重新授权',disconnected:'已断开',error:'需要处理'} as Record<string,string>)[source.state]||source.state}</span>
      {(source.earliest_at || source.latest_at) && <small>{source.earliest_at && new Date(source.earliest_at).toLocaleDateString()} — {source.latest_at && new Date(source.latest_at).toLocaleDateString()}</small>}</div>
    </li>)}</ul>}
    {query.data?.coverage && <p className={styles.hint}>{query.data.coverage}</p>}
    {!!query.data?.identifiers.length && <ul className={styles.identifiers}>{query.data.identifiers.map(value => <li key={value.kind + value.value}><span>{value.value}</span>{value.is_self && <small>我</small>}</li>)}</ul>}
    <details className={styles.linkContact}><summary>关联邮箱或手机号</summary><p className={styles.hint}>添加已知联系方式，帮助匹配邮件、短信和通讯录中的同一个人。</p>
    <form className={styles.form} onSubmit={async event => {
      event.preventDefault(); const form=event.currentTarget; const data=new FormData(form); setSaving(true); setSaved(false); setError('');
      try {
        const response=await apiFetch(`/api/v1/profiles/${profileId}/identifiers`, {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({kind:data.get('kind'),value:data.get('value'),is_self:data.get('self')==='on'})});
        if (!response.ok) throw new Error(`关联未保存 (${response.status})`);
        await query.refetch(); form.reset(); setSaved(true);
      } catch(e) { setError(String(e)); } finally { setSaving(false); }
    }}>
      <div className={styles.contactFields}><Field label="类型"><Select name="kind" value={kind} onChange={e=>{setKind(e.target.value);setSaved(false);}}><option value="email">邮箱</option><option value="phone">国际格式手机号</option></Select></Field>
      <Field label={kind==='email'?'邮箱地址':'国际格式手机号'}><Input name="value" type={kind==='email'?'email':'tel'} autoComplete="off" spellCheck={false} placeholder={kind==='email'?'name@example.com…':'+1 415 555 0123…'} required /></Field></div>
      <Checkbox name="self">这是我自己的联系方式</Checkbox>
      <Button type="submit" disabled={saving}>{saving ? '正在关联…' : '确认关联'}</Button>
      <p className={styles.hint}>共享邮箱或电话号码会保留为候选，不自动合并人物。</p>
    </form>
    {saved&&<p role="status" className={styles.hint}>联系方式已关联。</p>}
    {error&&<Failure>{error}，请核对联系方式后重试。</Failure>}
    </details>
    {query.isError&&<Failure onRetry={()=>void query.refetch()}>来源信息暂时无法读取。</Failure>}
  </Section>;
}

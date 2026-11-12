"use client";
import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';
import { apiFetch } from '../../lib/api';
import { fetchJson } from '../../lib/queries';
import { Button } from '../ui/button';
import { Input } from '../ui/field';
import { Section } from '../ui/primitives';
import { EvidenceTimeline } from './evidence-timeline';

type Coverage = { sources: { connection_id: string; display_name: string; provider: string; state: string; records: number; earliest_at: string; latest_at: string }[];
  identifiers: { kind: string; value: string; is_self: boolean }[]; coverage: string };
export function SourceCoverage({ profileId }: { profileId: string }) {
  const query = useQuery({ queryKey: ['source-coverage', profileId], queryFn: () => fetchJson<Coverage>(`/api/v1/profiles/${profileId}/source-coverage`) });
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  return <><Section title="来源与关联账号">
    {query.data?.sources.map(source => <p key={source.connection_id}><strong>{source.display_name}</strong> · {source.records} 条 · {source.state}<br />
      {source.earliest_at && new Date(source.earliest_at).toLocaleDateString()} — {source.latest_at && new Date(source.latest_at).toLocaleDateString()}</p>)}
    <p>{query.data?.coverage}</p>
    {query.data?.identifiers.map(value => <p key={value.kind + value.value}>{value.value}{value.is_self ? '（我）' : ''}</p>)}
    <form onSubmit={async event => {
      event.preventDefault(); const form=event.currentTarget; const data=new FormData(form); setSaving(true);
      try {
        const response=await apiFetch(`/api/v1/profiles/${profileId}/identifiers`, {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({kind:data.get('kind'),value:data.get('value'),is_self:data.get('self')==='on'})});
        if (!response.ok) throw new Error(`关联未保存 (${response.status})`);
        await query.refetch(); form.reset(); setError('');
      } catch(e) { setError(String(e)); } finally { setSaving(false); }
    }}>
      <label>联系方式类型 <select name="kind"><option value="email">邮箱</option><option value="phone">国际格式手机号</option></select></label>
      <Input name="value" aria-label="关联邮箱或手机号" placeholder="name@example.com 或 +1…" required />
      <label><input type="checkbox" name="self" />这是我的联系方式</label>
      <Button type="submit" disabled={saving}>确认关联</Button>
      <p>共享邮箱或电话号码会保留为候选，不自动合并人物。</p>
    </form>
    {(error || query.isError) && <p role="alert">{error || '来源信息暂时无法读取。'}</p>}
  </Section><EvidenceTimeline profileId={profileId} /></>;
}

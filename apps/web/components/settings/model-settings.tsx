"use client";

import { useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchJson } from '../../lib/queries';
import { Section, Failure, Skeleton } from '../ui/primitives';
import { Input } from '../ui/field';
import { Button } from '../ui/button';
import styles from './settings.module.css';

type Settings = {
  enabled: boolean; qa_enabled: boolean; model: string; base_url: string | null; has_key: boolean;
  include_messages: boolean; include_private_notes: boolean; context_window: number; max_tokens: number;
};
export function ModelSettings() {
  const client = useQueryClient();
  const query = useQuery({ queryKey: ['ai', 'settings'], queryFn: () => fetchJson<Settings>('/api/v1/ai/settings') });
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const data = query.data;
  return <Section title="模型设置" className={styles.panel}>
    <p>配置兼容 OpenAI 的模型服务。默认关闭；保存后供这台部署的问答与后台任务共用。API Key 加密保存，已有密钥不会返回浏览器。</p>
    {query.isError ? <Failure onRetry={() => void query.refetch()}>无法读取模型设置。</Failure> : !data ? <Skeleton height={120} /> :
      <form key={query.dataUpdatedAt} onSubmit={async event => {
        event.preventDefault(); const form = event.currentTarget; const values = new FormData(form);
        setBusy(true); setMessage('');
        const key = String(values.get('api_key') || '');
        try {
          await fetchJson('/api/v1/ai/settings', { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({
            enabled: values.has('enabled'), qa_enabled: values.has('qa_enabled'),
            model: values.get('model'), base_url: values.get('base_url'),
            ...(key ? { api_key: key } : {}), clear_key: values.has('clear_key'),
            include_messages: values.has('include_messages'), include_private_notes: values.has('include_private_notes'),
            context_window: Number(values.get('context_window')), max_tokens: Number(values.get('max_tokens')),
          }) });
          form.reset(); await client.invalidateQueries({ queryKey: ['ai'] }); setMessage('模型设置已保存。');
        } catch (error) { setMessage(error instanceof Error ? error.message : '保存失败，请重试。'); }
        finally { setBusy(false); }
      }}>
        <p><label><input type="checkbox" name="enabled" defaultChecked={data.enabled} /> 启用模型服务</label></p>
        <p><label>服务地址<Input name="base_url" type="url" required defaultValue={data.base_url || ''} placeholder="https://你的模型服务/v1" /></label></p>
        <p><label>模型名称<Input name="model" required maxLength={255} defaultValue={data.model} /></label></p>
        <p><label>API Key<Input name="api_key" type="password" autoComplete="new-password" maxLength={8192} placeholder={data.has_key ? '已保存；留空保留现有密钥' : '填写此部署的 API Key'} /></label></p>
        {data.has_key && <p><label><input type="checkbox" name="clear_key" /> 删除已保存的密钥</label></p>}
        <p><label><input type="checkbox" name="qa_enabled" defaultChecked={data.qa_enabled} /> 启用浏览器问答</label></p>
        <p><label><input type="checkbox" name="include_messages" defaultChecked={data.include_messages} /> 允许后台人物分析读取已保存的聊天正文</label></p>
        <p><label><input type="checkbox" name="include_private_notes" defaultChecked={data.include_private_notes} /> 允许后台人物分析读取私人笔记</label></p>
        <p><label>上下文窗口<Input name="context_window" type="number" min={8192} max={2097152} required defaultValue={data.context_window} /></label></p>
        <p><label>回答输出上限<Input name="max_tokens" type="number" min={1} max={131072} required defaultValue={data.max_tokens} /></label></p>
        <Button type="submit" disabled={busy}>{busy ? '正在保存…' : '保存模型设置'}</Button>
      </form>}
    {message && <p role="status">{message}</p>}
  </Section>;
}

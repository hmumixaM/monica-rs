"use client";

import { useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchJson } from '../../lib/queries';
import { Section, Failure, Skeleton } from '../ui/primitives';
import { Checkbox, Field, Input } from '../ui/field';
import { Button } from '../ui/button';
import styles from './settings.module.css';

type Settings = {
  enabled: boolean; execution_enabled: boolean; qa_enabled: boolean; model: string; base_url: string | null; has_key: boolean;
  include_messages: boolean; include_private_notes: boolean; context_window: number; max_tokens: number;
  model_routes: Record<string,string>; qa_models?: string[]; vision_model: string;
};
export function ModelSettings() {
  const client = useQueryClient();
  const query = useQuery({ queryKey: ['ai', 'settings'], queryFn: () => fetchJson<Settings>('/api/v1/ai/settings') });
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const data = query.data;
  return <><Section title="模型设置" className={styles.panel}>
    <p>配置兼容 OpenAI 的模型服务。默认关闭；保存后供这台部署的问答与后台任务共用。API Key 加密保存，已有密钥不会返回浏览器。</p>
    {data?.execution_enabled===false&&<p role="status">此部署已暂停新模型调用。已保存的摘要、OCR 和周报仍可查看。恢复调用需要由部署配置明确开启。</p>}
    {query.isError ? <Failure onRetry={() => void query.refetch()}>无法读取模型设置。</Failure> : !data ? <Skeleton height={120} /> :
      <form className={styles.modelForm} key={query.dataUpdatedAt} onSubmit={async event => {
        event.preventDefault(); const form = event.currentTarget; const values = new FormData(form);
        setBusy(true); setMessage('');
        const key = String(values.get('api_key') || '');
        try {
          await fetchJson('/api/v1/ai/settings', { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({
            enabled: data.execution_enabled===false ? data.enabled : values.has('enabled'), qa_enabled: data.execution_enabled===false ? data.qa_enabled : values.has('qa_enabled'),
            model: values.get('model'), ...(values.get('base_url') ? {base_url: values.get('base_url')} : {}),
            model_routes: Object.fromEntries(['batch','person','writer','qa'].map(role=>[role,String(values.get(`model_${role}`)||'').trim()])),
            qa_models: String(values.get('qa_models') || '').split(/[,，\s]+/).filter(Boolean),
            vision_model: values.get('vision_model'),
            ...(key ? { api_key: key } : {}), clear_key: values.has('clear_key'),
            include_messages: values.has('include_messages'), include_private_notes: values.has('include_private_notes'),
            context_window: Number(values.get('context_window')), max_tokens: Number(values.get('max_tokens')),
          }) });
          form.reset(); await client.invalidateQueries({ queryKey: ['ai'] }); setMessage('模型设置已保存。');
        } catch (error) { setMessage(error instanceof Error ? error.message : '保存失败，请重试。'); }
        finally { setBusy(false); }
      }}>
        <fieldset className={styles.formGroup}><legend>模型连接</legend>
        <Checkbox name="enabled" defaultChecked={data.enabled} disabled={data.execution_enabled===false}>{data.execution_enabled===false ? '显示已有 AI 分析' : '启用模型服务'}</Checkbox>
        <Field label="服务地址"><Input name="base_url" type="url" autoComplete="off" required={data.execution_enabled!==false} defaultValue={data.base_url || ''} placeholder="https://你的模型服务/v1…" /></Field>
        <div className={styles.formGrid}><Field label="默认模型" hint="各项任务未单独指定时，使用此模型。"><Input name="model" required maxLength={255} defaultValue={data.model} autoComplete="off" /></Field>
        <Field label="API Key" hint="密钥加密保存。留空会保留当前密钥。"><Input name="api_key" type="password" autoComplete="new-password" maxLength={8192} placeholder={data.has_key ? '已保存密钥…' : '填写此部署的 API Key…'} /></Field></div>
        {data.has_key && <Checkbox name="clear_key">删除已保存的密钥</Checkbox>}
        </fieldset>
        <fieldset className={styles.formGroup}><legend>各功能使用的模型</legend>
        <div className={styles.formGrid}>{Object.entries({batch:['批量整理','处理素材与 Memory 分片'],person:['人物分析','人物 Memory 与每周近况'],writer:['写作','生成每周回顾'],qa:['问 Monica','留空时优先沿用写作模型']}).map(([role,[label,hint]])=><Field key={role} label={label} hint={hint}><Input name={`model_${role}`} maxLength={255} defaultValue={data.model_routes?.[role]||''} placeholder={data.model+'…'} autoComplete="off"/></Field>)}
        <Field label="识图与 OCR" hint="需要支持图片输入的模型。"><Input name="vision_model" required maxLength={255} defaultValue={data.vision_model} autoComplete="off"/></Field>
        <Field label="问 Monica 的其他可选模型" hint="用逗号分隔。提问时可以在输入框下面切换。"><Input name="qa_models" maxLength={2000} defaultValue={(data.qa_models ?? []).join(', ')} placeholder="例如 deepseek-v4-pro, gpt-6.2-sol…" autoComplete="off"/></Field></div>
        </fieldset>
        <fieldset className={styles.formGroup}><legend>允许读取的内容</legend>
        <Checkbox name="qa_enabled" defaultChecked={data.qa_enabled} disabled={data.execution_enabled===false}>启用浏览器问答</Checkbox>
        <Checkbox name="include_messages" defaultChecked={data.include_messages}>后台人物分析可读取已保存的聊天正文</Checkbox>
        <Checkbox name="include_private_notes" defaultChecked={data.include_private_notes}>后台人物分析可读取私人笔记</Checkbox>
        </fieldset>
        <details className={styles.advanced}><summary>高级：上下文与输出长度</summary><div className={styles.formGrid}>
        <Field label="上下文窗口（tokens）" hint="模型每次可容纳的输入与输出容量。"><Input name="context_window" type="number" min={8192} max={2097152} required defaultValue={data.context_window} /></Field>
        <Field label="回答输出上限（tokens）" hint="限制单次回答长度；不会截断保存的原始资料。"><Input name="max_tokens" type="number" min={1} max={131072} required defaultValue={data.max_tokens} /></Field>
        </div></details>
        <Button type="submit" disabled={busy}>{busy ? '正在保存…' : '保存模型设置'}</Button>
      </form>}
    {message && <p role="status">{message}</p>}
  </Section><WebSearchSettings /></>;
}

function WebSearchSettings() {
  const client = useQueryClient();
  const queryKey = ['ai', 'web-search'] as const;
  const path = '/api/v1/ai/web-search';
  const query = useQuery({ queryKey, queryFn: () => fetchJson<{ configured: boolean }>(path) });
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  async function save(body: { api_key: string } | { clear_key: true }) {
    setBusy(true);
    setMessage('');
    try {
      const settings = await fetchJson<{ configured: boolean }>(path, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      client.setQueryData(queryKey, settings);
      setMessage(settings.configured ? 'Exa 密钥已保存。' : 'Exa 密钥已删除。');
      return true;
    } catch (error) {
      setMessage(error instanceof Error ? error.message : '无法保存 Exa 设置。');
      return false;
    } finally {
      setBusy(false);
    }
  }
  return <Section title="公开网络搜索" className={styles.panel}>
    <p>配置 Exa 后，问 Monica 可以检索公开的人物资料、最新消息和地点信息。密钥保存在本部署的加密设置里。</p>
    {query.isError ? <Failure onRetry={() => void query.refetch()}>无法读取 Exa 设置。</Failure> : !query.data ? <Skeleton height={80} /> :
      <form className={styles.modelForm} onSubmit={async event => {
        event.preventDefault();
        const form = event.currentTarget;
        const apiKey = String(new FormData(form).get('exa_key') || '').trim();
        if (await save({ api_key: apiKey })) form.reset();
      }}>
        <Field label="Exa API Key" hint={query.data.configured ? '已配置。填写新密钥即可替换。' : '保存后启用公开网络搜索。'}>
          <Input name="exa_key" type="password" autoComplete="new-password" maxLength={1024} required disabled={busy} />
        </Field>
        <div className={styles.formGrid}>
          <Button type="submit" disabled={busy}>保存 Exa 密钥</Button>
          {query.data.configured && <Button type="button" variant="ghost" disabled={busy} onClick={() => void save({ clear_key: true })}>删除 Exa 密钥</Button>}
        </div>
      </form>}
    {message && <p role="status">{message}</p>}
  </Section>;
}

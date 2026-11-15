"use client";
import {EmptyState, Page, PageHeader} from "../ui/primitives";
import {Button} from "../ui/button";
import {Field,Select,Textarea} from "../ui/field";

import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { useSearchParams } from '../../src/router/navigation';
import { apiFetch } from '../../lib/api';
import { PersonPicker, type PickedPerson } from '../ui/person-picker';
import { Markdown } from '../ui/markdown';
import { exportConversation, importConversation, restore, run, snapshot, stop, subscribe } from '../../lib/agent/runtime';
import { activeConversation, createConversation, listConversations, saveConversation, selectConversation, type Conversation } from '../../lib/agent/sessions';
import {Download, Plus, Sparkles, Upload, X} from 'lucide-react';
import styles from './ask.module.css';

export function AskView() {
  const params = useSearchParams();
  const [id, setId] = useState('default');
  const [sessions, setSessions] = useState<Conversation[]>([]);
  const [ready, setReady] = useState(false);
  const [people, setPeople] = useState<PickedPerson[]>([]);
  const [question, setQuestion] = useState('');
  const [fileError, setFileError] = useState('');
  const fileInput = useRef<HTMLInputElement>(null);
  const state = useSyncExternalStore(subscribe, () => snapshot(id), () => snapshot(id));
  async function activate(session: Conversation) {
    setReady(false); setQuestion(''); setFileError('');
    await restore(session.id); await selectConversation(session.id);
    setId(session.id); setPeople(session.people); setReady(true);
  }
  useEffect(() => {
    let active = true;
    void Promise.all([listConversations(), activeConversation()]).then(async ([rows, selected]) => {
      const session = rows.find(c => c.id === selected) ?? rows[0];
      await restore(session.id);
      if (active) { setSessions(rows); setId(session.id); setPeople(session.people); setReady(true); }
    }).catch(e => { if (active) setFileError(String(e)); });
    return () => { active = false; };
  }, []);
  useEffect(() => {
    if (!ready) return;
    void listConversations().then(setSessions).catch(e => setFileError(String(e)));
  }, [state.running, ready]);
  async function choosePeople(next: PickedPerson[]) {
    setPeople(next);
    const rows = await listConversations();
    const session = rows.find(c => c.id === id)!;
    await saveConversation({ ...session, people: next, updatedAt: Date.now() });
  }
  const selectedProfile = params.get('profile');
  const appliedProfile = useRef<string | null>(null);
  useEffect(() => {
    if (!selectedProfile || !ready || appliedProfile.current === selectedProfile) return;
    let active = true;
    void apiFetch('/api/v1/profiles/' + encodeURIComponent(selectedProfile)).then(async r => {
      if (!r.ok) throw new Error('无法读取选定人物');
      const p = await r.json(); if (active) {
        appliedProfile.current = selectedProfile;
        await choosePeople([{ id: p.id, display_name: p.display_name, avatar_media_id: p.avatar_media_id ?? null }]);
      }
    }).catch(e => { if (active) setFileError(String(e)); });
    return () => { active = false; };
  }, [selectedProfile, ready, id]);
  return <Page width="wide" className={styles.page}>
    <PageHeader title="问 Monica" description="选择人物，查询聊天、资料与时间线。回答可以继续追问，并查看原始依据。" />
    <div className={styles.workspace}>
    <div className={styles.toolbar}>
      <Field label="当前会话" className={styles.session}><Select aria-label="选择会话" value={id} disabled={!ready || state.running} onChange={event => {
        const selected = sessions.find(c => c.id === event.target.value);
        if (selected) void activate(selected).catch(e => setFileError(String(e)));
      }}>{sessions.map(s => <option key={s.id} value={s.id}>{s.title}</option>)}</Select></Field>
      <Button disabled={!ready || state.running} onClick={async () => {
        try { const session = await createConversation(); setSessions(await listConversations()); await activate(session); }
        catch (e) { setFileError(String(e)); }
      }}><Plus size={15} aria-hidden="true" />新会话</Button>
      <div className={styles.transfer}><Button size="sm" variant="ghost" disabled={!ready} onClick={() => void exportConversation(id).catch(e => setFileError(String(e)))}><Download size={14} aria-hidden="true" />导出</Button>
      <Button size="sm" variant="ghost" disabled={!ready || state.running} onClick={() => fileInput.current?.click()}><Upload size={14} aria-hidden="true" />导入会话</Button>
      <input ref={fileInput} className={styles.file} type="file" aria-label="导入会话文件" accept="application/json" disabled={!ready || state.running} onChange={async event => {
        const file = event.target.files?.[0]; if (!file) return;
        try {
          const session = await createConversation(); await importConversation(session.id, file);
          const rows = await listConversations(); setSessions(rows); await activate(rows.find(s => s.id === session.id)!);
        } catch (error) { setFileError(String(error)); }
        event.target.value = '';
      }} /></div></div>{fileError && <p role="alert">{fileError}</p>}
    <section aria-label="选择人物" className={styles.context}><div className={styles.contextTitle}><strong>这次想了解谁？</strong><span>可选择多个人物，也可直接提问。</span></div><PersonPicker disabled={!ready || state.running} placeholder="搜索并添加人物…" onPick={person => { if (ready && !state.running) void choosePeople(people.some(p => p.id === person.id) ? people : [...people, person]).catch(e => setFileError(String(e))); }} />
      <div className={styles.people}>{people.map(p => <Button size="sm" variant="ghost" aria-label={`移除人物 ${p.display_name}`} key={p.id} disabled={!ready || state.running} onClick={() => void choosePeople(people.filter(x => x.id !== p.id)).catch(e => setFileError(String(e)))}>@{p.display_name}<X size={13} aria-hidden="true" /></Button>)}</div>
    </section>
    <section aria-label="聊天记录" className={styles.transcript}>
      {!state.messages.some(m => m.role === 'user' || m.role === 'assistant') && <EmptyState icon={<Sparkles size={24} aria-hidden="true" />} title="从一个问题开始" actions={<div className={styles.prompts}>{['我最近和谁联系得最多？', '有哪些我答应过、还没做的事？', '总结所选人物最近的近况'].map(text => <Button key={text} size="sm" variant="ghost" disabled={!ready} onClick={() => {setQuestion(text); document.getElementById('monica-question')?.focus();}}>{text}</Button>)}</div>}>回顾近况、找回聊天中的约定，或比较几个人物的经历。</EmptyState>}
      {state.messages.map((message, i) => {
      if (message.role !== 'user' && message.role !== 'assistant') return null;
      const content = typeof message.content === 'string' ? message.content : message.content.filter(p => p.type === 'text').map(p => p.text).join('\n');
      if (!content.trim()) return null;
      return <article key={i}><strong>{message.role === 'user' ? '你' : 'Monica'}</strong><Markdown>{content}</Markdown></article>;
    })}</section>
    {state.status && <p className={styles.status} role="status">{state.status}</p>}{state.error && <p role="alert">{state.error}</p>}
    <form className={styles.composer} onSubmit={event => { event.preventDefault(); if (!question.trim() || state.running) return; const value = question; setQuestion(''); void run(id, value, people.map(p => p.id)); }}>
      <Textarea id="monica-question" name="question" aria-label="问题" rows={3} value={question} onChange={event => setQuestion(event.target.value)} placeholder="输入问题，例如：我上次答应所选人物什么？" />
      <div className={styles.actions}><span>会话保存在当前浏览器和本地地址中。</span>{state.running ? <Button type="button" onClick={() => stop(id)}>停止</Button> : <Button variant="primary" type="submit" disabled={!ready || !question.trim()}>提问</Button>}</div>
    </form>
    </div>
  </Page>;
}

"use client";
import { Agent, type AgentMessage } from '@earendil-works/pi-agent-core';
import { streamSimple } from '@earendil-works/pi-ai/api/openai-completions';
import { del, get, set } from 'idb-keyval';
import { apiFetch } from '../api';
import type { PickedPerson } from '../../components/ui/person-picker';
import { compactContext, resumeContext } from './context';
import { listConversations, saveConversation } from './sessions';
import { TOOL_LABELS, tools } from './tools';

/** One conversation's transcript and progress, shared by every view of it. */
export type Snapshot = { messages: AgentMessage[]; running: boolean; status: string; error: string };
const snapshots = new Map<string, Snapshot>();
const listeners = new Set<() => void>();
const agents = new Map<string, Agent>();
const controllers = new Map<string, AbortController>();
const empty: Snapshot = { messages: [], running: false, status: '', error: '' };
const transcriptKey = (id: string) => 'monica:ask:' + id;

export const subscribe = (fn: () => void) => { listeners.add(fn); return () => { listeners.delete(fn); }; };
export const snapshot = (id: string) => snapshots.get(id) ?? empty;
function update(id: string, value: Partial<Snapshot>) {
  snapshots.set(id, { ...snapshot(id), ...value }); listeners.forEach(fn => fn());
}

export async function restore(id: string) {
  if (snapshots.has(id)) return;
  const saved = await get<AgentMessage[]>(transcriptKey(id));
  if (!snapshots.has(id)) update(id, { messages: saved ?? [] });
}
export function stop(id: string) { controllers.get(id)?.abort(); agents.get(id)?.abort(); }
export async function forget(id: string) { stop(id); snapshots.delete(id); await del(transcriptKey(id)); }

function systemPrompt(people: PickedPerson[]) {
  const today = new Intl.DateTimeFormat('zh-CN', { dateStyle: 'full' }).format(new Date());
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const focus = people.length
    ? '用户这次想了解的人：\n' + people.map(person => `- ${person.display_name}（profile_id: ${person.id}）`).join('\n') + '\n问题里的「TA」「他们」指这些人。'
    : '用户没有指定人物。问题提到某个人时先用 find_people 找到 TA；问题关于整体往来时用 relationship_overview。';
  return `你是 Monica，用户的私人关系助手。你可以查用户通讯录里的人物资料、聊天记录、朋友发的动态、邮件，以及 Monica 整理的人物概括和每周近况。
今天是${today}，时区 ${zone}。工具返回的时间都是这个时区的本地时间。

${focus}

怎么查：
- 先查再答。了解一个人从 get_person 开始；想知道聊天里说过什么，用 search_messages 按原文关键词搜，或用 read_messages 按时间读。
- 搜索是逐字匹配，一个词只能找到恰好这么写的消息。每个问题先想它在聊天里可能怎么说：同义词、口语、英文和缩写、相关的人名地名、事情的前因后果，用 | 一次搜完；第一轮的结果里出现了新的说法，就拿它再搜一轮。
- 搜到线索后，用 read_messages 读那几天前后的聊天，看清上下文再下结论；只凭一句命中的话容易看错。
- 结果带 earlier 说明更早还有。问题问的是「所有」「每次」「第一次」「这些年」时，用 before 一直翻到问题要求的时间范围读完，或者 earlier 为空。
- 问题涉及好几个人或好几件事时，逐个查，不要查完一个就作答。
- 确实查不到时，说明查过哪些说法、哪段时间。
- 聊天里 from 为「我」的是用户本人，其余是对方或群成员。

怎么答：
- 用中文，先给结论，再写支撑它的具体内容：谁、哪一天、说了什么。不复述查询过程。
- 提到具体的消息、动态或邮件时，用工具结果里的 link 写成 Markdown 链接，例如 [11 月 3 日的聊天](/messages?conversation=…)；提到人物时链接到 /profiles/人物ID。
- 只根据查到的内容回答。资料里没有的，直接说没有记录。只读了一部分就说明读到哪里，不要把一部分当成全部。`;
}

/** An assistant turn that neither says anything nor asks for a lookup. */
function wroteNothing(message?: AgentMessage) {
  if (!message || message.role !== 'assistant' || message.errorMessage) return false;
  return !message.content.some(part => part.type === 'toolCall' || (part.type === 'text' && part.text.trim()));
}

async function settings(signal: AbortSignal) {
  const response = await apiFetch('/api/v1/ai/browser-model', { signal });
  if (!response.ok) throw new Error(`无法读取模型设置 (${response.status})`);
  const value = await response.json();
  if (!value.enabled) throw new Error('问答还没有开启。请在「设置 → 模型」里启用模型服务和问 Monica。');
  return value as { model: string; context_window: number; max_tokens: number };
}

export async function run(id: string, question: string, people: PickedPerson[]) {
  if (snapshot(id).running) return;
  const controller = new AbortController(); controllers.set(id, controller);
  update(id, { running: true, error: '', status: '正在思考…' });
  try {
    const conversation = (await listConversations()).find(c => c.id === id);
    if (conversation) await saveConversation({ ...conversation, updatedAt: Date.now(),
      title: snapshot(id).messages.length ? conversation.title : question.trim().slice(0, 60) });
    await navigator.locks.request(transcriptKey(id), { ifAvailable: true }, async lock => {
      if (!lock) throw new Error('这个会话正在另一个标签页里回答');
      const model = await settings(controller.signal);
      let nudge = false;
      const agent = new Agent({ initialState: {
        model: { id: model.model, name: model.model, provider: 'monica', api: 'openai-completions',
          baseUrl: location.origin + '/api/v1/ai', reasoning: false, input: ['text'],
          contextWindow: model.context_window, maxTokens: model.max_tokens,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
          compat: { supportsDeveloperRole: false, supportsStore: false, maxTokensField: 'max_tokens' } },
        messages: resumeContext(snapshot(id).messages),
        systemPrompt: systemPrompt(people),
        tools: tools(),
      }, transformContext: async messages => {
        const context = compactContext(messages, model.context_window, model.max_tokens);
        return nudge ? [...context, { role: 'user', content: '继续：还需要查就接着查；查够了，就根据已经查到的内容写出回答。', timestamp: Date.now() }] : context;
      },
      streamFn: (target, context, options) => streamSimple(target as Parameters<typeof streamSimple>[0], context, {
        ...options, apiKey: 'local-session', fetch: apiFetch, maxRetries: 0,
      }) });
      agents.set(id, agent);
      const unsubscribe = agent.subscribe(async event => {
        if (event.type === 'tool_execution_start') update(id, { status: `正在${TOOL_LABELS[event.toolName] ?? event.toolName}…` });
        if (event.type === 'tool_execution_end') update(id, { status: '正在整理回答…' });
        if (event.type === 'message_update' || event.type === 'message_end') {
          update(id, { messages: event.type === 'message_update' ? [...agent.state.messages.filter(m => m !== event.message), event.message] : [...agent.state.messages] });
        }
        if (event.type === 'message_end') await set(transcriptKey(id), [...agent.state.messages]);
      });
      try {
        await agent.prompt(question);
        // After several lookups the model sometimes ends its turn having written nothing.
        // Asked to go on, it answers from what it has read; the request is not kept in the transcript.
        for (let attempt = 0; attempt < 2 && wroteNothing(agent.state.messages.at(-1)) && !controller.signal.aborted; attempt++) {
          agent.state.messages = agent.state.messages.slice(0, -1);
          update(id, { status: '正在整理回答…' });
          nudge = true;
          await agent.continue();
        }
        if (agent.state.errorMessage) throw new Error(agent.state.errorMessage);
        if (wroteNothing(agent.state.messages.at(-1)) && !controller.signal.aborted) throw new Error('模型这次没有写出回答，请再问一次。');
      } finally {
        unsubscribe(); agents.delete(id);
        const messages = [...agent.state.messages];
        update(id, { messages }); await set(transcriptKey(id), messages);
      }
    });
  } catch (error) {
    if (!controller.signal.aborted) update(id, { error: error instanceof Error ? error.message : String(error) });
  } finally { controllers.delete(id); update(id, { running: false, status: '' }); }
}

export async function exportConversation(id: string) {
  const conversation = (await listConversations()).find(c => c.id === id);
  const blob = new Blob([JSON.stringify({ version: 1, conversation, messages: snapshot(id).messages }, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a'); link.href = url; link.download = 'monica-conversation.json'; link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export async function importConversation(id: string, file: File) {
  const value = JSON.parse(await file.text());
  if (value.version !== 1 || !Array.isArray(value.messages)) throw new Error('这不是 Monica 导出的会话文件');
  await set(transcriptKey(id), value.messages);
  if (value.conversation) await saveConversation({ id, title: String(value.conversation.title ?? '导入的会话').slice(0, 60), updatedAt: Date.now(), people: value.conversation.people ?? [] });
  update(id, { messages: value.messages, error: '' });
}

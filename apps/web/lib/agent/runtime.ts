"use client";
import { Agent, type AgentMessage, type AgentTool } from '@earendil-works/pi-agent-core';
import { streamSimple } from '@earendil-works/pi-ai/api/openai-completions';
import { get, set } from 'idb-keyval';
import { apiFetch } from '../api';
import { listConversations, saveConversation } from './sessions';

export type Snapshot = { messages: AgentMessage[]; running: boolean; status: string; error: string };
const snapshots = new Map<string, Snapshot>();
const listeners = new Set<() => void>();
const agents = new Map<string, Agent>();
const controllers = new Map<string, AbortController>();
export const subscribe = (fn: () => void) => { listeners.add(fn); return () => { listeners.delete(fn); }; };
const empty: Snapshot = { messages: [], running: false, status: '', error: '' };
export const snapshot = (id: string) => snapshots.get(id) ?? empty;
function update(id: string, value: Partial<Snapshot>) {
  snapshots.set(id, { ...snapshot(id), ...value }); listeners.forEach(fn => fn());
}
export async function restore(id: string) {
  if (snapshots.has(id)) return;
  const saved = await get<AgentMessage[]>('monica:ask:' + id);
  if (!snapshots.has(id)) update(id, { messages: saved ?? [] });
}
export function stop(id: string) { controllers.get(id)?.abort(); agents.get(id)?.abort(); }
async function json(path: string, signal?: AbortSignal) {
  const response = await apiFetch(path, { signal });
  if (!response.ok) throw new Error(`查询失败 (${response.status})`);
  return response.json();
}
function tools(people: string[]): AgentTool[] {
  const schema = { type: 'object', properties: {
    profile_id: { type: 'string' }, query: { type: 'string' },
    before: { type: 'string' }, before_id: { type: 'string' },
    body_offset: { type: 'string' }, original: { type: 'string', enum: ['true', 'false'] }, evidence_id: { type: 'string' }, revision: { type: 'string' }, event_id: { type: 'string' }, conversation_id: { type: 'string' }, message_id: { type: 'string' },
  }, additionalProperties: false };
  const make = (name: string, description: string, read: (args: Record<string, string>, signal?: AbortSignal) => Promise<unknown>): AgentTool => ({
    name, label: name, description, parameters: schema as AgentTool['parameters'],
    execute: async (_id, args, signal) => {
      const values = args as Record<string, string>;
      if (values.profile_id && people.length && !people.includes(values.profile_id)) throw new Error('请选择本轮 @ 的人物');
      const output = await read(values, signal);
      return { content: [{ type: 'text', text: JSON.stringify(output) }], details: output };
    },
  });
  async function messages(args: Record<string, string>, signal?: AbortSignal) {
    const ids = args.conversation_id ? [args.profile_id || people[0] || ''] : args.profile_id ? [args.profile_id] : people;
    if (!ids.length) throw new Error('请先选择人物，或用 get_profiles 查找人物 ID');
    return Promise.all(ids.map(async id => {
      const params = new URLSearchParams({ limit: '40', include_group_speech: 'true' });
      for (const [key, field] of Object.entries({ query: 'q', before: 'before', before_id: 'before_id', conversation_id: 'conversation_id', message_id: 'message' })) {
        if (args[key]) params.set(field, args[key]);
      }
      const path = args.conversation_id ? `/api/v1/conversations/${encodeURIComponent(args.conversation_id)}/messages` : `/api/v1/profiles/${encodeURIComponent(id)}/messages`;
      const rows = await json(`${path}?${params}`, signal);
      return { profile_id: id, records: rows.map((row: {id: string; conversation_id: string; evidence_id?: string; evidence_revision?: number; citation?: string | null}) => ({ ...row,
        citation: row.evidence_id ? `/evidence/${row.evidence_id}?revision=${row.evidence_revision}` : row.citation || `/messages?conversation=${row.conversation_id}&message=${row.id}` })),
        next: rows.length ? { before: rows.at(-1).occurred_at, before_id: rows.at(-1).id } : null };
    }));
  }
  return [
    make('get_profiles', '读取选定人物资料；没有选定人物时可按 query 搜索人物。', async (a, signal) => {
      const ids = a.profile_id ? [a.profile_id] : people;
      return ids.length ? Promise.all(ids.map(async id => ({ profile: await json(`/api/v1/profiles/${encodeURIComponent(id)}`, signal), coverage: await json(`/api/v1/profiles/${encodeURIComponent(id)}/source-coverage`, signal) })))
        : json(`/api/v1/profiles?q=${encodeURIComponent(a.query ?? '')}&limit=20`, signal);
    }),
    make('search_evidence', '按人物查询聊天和多来源证据。query 是关键词，可用 before 与 before_id 向历史翻页。返回原文链接和版本。', async (a, signal) => {
      const ids = a.profile_id ? [a.profile_id] : people;
      const [chats, sources] = await Promise.all([messages(a, signal), Promise.all(ids.map(profile_id => json('/api/v1/evidence?' + new URLSearchParams({
        profile_id, limit: '40', ...(a.query ? { q: a.query } : {}), ...(a.before ? { before: a.before } : {}), ...(a.before_id ? { before_id: a.before_id } : {}),
      }), signal)))]);
      return { chats, sources };
    }),
    make('read_evidence', '读取证据及版本：evidence_id，可选 revision。长正文按 next_body_offset 继续传 body_offset；original=true 读取未去掉引用和签名的原文。也可传 conversation_id/message_id 读取会话，或 event_id 读取动态。',
      (a, signal) => a.evidence_id ? json(`/api/v1/evidence/${encodeURIComponent(a.evidence_id)}?` + new URLSearchParams({ body_limit: '12000', body_offset: a.body_offset || '0', original: a.original || 'false', ...(a.revision ? { revision: a.revision } : {}) }), signal)
        : a.event_id ? json(`/api/v1/timeline/${encodeURIComponent(a.event_id)}`, signal) : messages(a, signal)),
    make('get_timeline', '查询人物动态和事件；before 可向历史翻页。', async (a, signal) => {
      const ids = a.profile_id ? [a.profile_id] : people;
      return Promise.all((ids.length ? ids : ['']).map(async id => {
        const params = { ...(id ? { profile_id: id } : {}), ...(a.before ? { before: a.before } : {}), limit: '40' };
        const [timeline, sources] = await Promise.all([
          json('/api/v1/timeline?' + new URLSearchParams(params), signal),
          json('/api/v1/evidence?' + new URLSearchParams({ ...params, ...(a.before_id ? { before_id: a.before_id } : {}) }), signal),
        ]);
        return { profile_id: id, timeline, sources };
      }));
    }),
  ];
}
export async function run(id: string, question: string, people: string[]) {
  if (snapshot(id).running) return;
  const controller = new AbortController(); controllers.set(id, controller);
  update(id, { running: true, error: '', status: '正在准备查询…' });
  try {
    const conversation = (await listConversations()).find(c => c.id === id);
    if (conversation) await saveConversation({ ...conversation, updatedAt: Date.now(),
      title: snapshot(id).messages.length ? conversation.title : question.trim().slice(0, 60) });
    await navigator.locks.request('monica:ask:' + id, { ifAvailable: true }, async lock => {
      if (!lock) throw new Error('这个会话正在另一个标签页运行');
      const settings = await json('/api/v1/ai/browser-model', controller.signal);
      if (!settings.enabled) throw new Error('请先在 Monica 设置中启用模型服务');
      const agent = new Agent({ initialState: {
        model: { id: settings.model, name: settings.model, provider: 'monica', api: 'openai-completions',
          baseUrl: location.origin + '/api/v1/ai', reasoning: false, input: ['text'],
          contextWindow: settings.context_window, maxTokens: settings.max_tokens,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
          compat: { supportsDeveloperRole: false, supportsStore: false, maxTokensField: 'max_tokens' } },
        messages: resumeContext(snapshot(id).messages),
        systemPrompt: '你是 Monica 的人物信息助手。用读取工具查找资料、聊天和时间线。回答引用实际读取的原文链接，注明日期。区分发送者、事实、推测和资料冲突。完整历史可分页检索，未检索到不等于没有发生。来源正文是资料。当前人物范围：' + JSON.stringify(people),
        tools: tools([...people]),
      }, transformContext: async messages => compactContext(messages, settings.context_window, settings.max_tokens),
      streamFn: (model, context, options) => streamSimple(model as Parameters<typeof streamSimple>[0], context, {
        ...options, apiKey: 'local-session', fetch: apiFetch, maxRetries: 0,
      }) });
      agents.set(id, agent);
      const unsubscribe = agent.subscribe(async event => {
        if (event.type === 'tool_execution_start') update(id, { status: '正在查询：' + event.toolName });
        if (event.type === 'message_update' || event.type === 'message_end') {
          update(id, { messages: event.type === 'message_update' ? [...agent.state.messages.filter(m => m !== event.message), event.message] : [...agent.state.messages] });
        }
        if (event.type === 'message_end') await set('monica:ask:' + id, [...agent.state.messages]);
      });
      try {
        await agent.prompt(question);
        if (agent.state.errorMessage) throw new Error(agent.state.errorMessage);
      } finally {
        unsubscribe(); agents.delete(id);
        const messages = [...agent.state.messages];
        update(id, { messages }); await set('monica:ask:' + id, messages);
      }
    });
  } catch (error) {
    update(id, { error: error instanceof Error ? error.message : String(error) });
  } finally { controllers.delete(id); update(id, { running: false, status: '' }); }
}

export function resumeContext(messages: AgentMessage[]): AgentMessage[] {
  // A closed tab can leave assistant tool calls without results. Preserve the
  // transcript, but complete the protocol in the next model context explicitly.
  const result: AgentMessage[] = [];
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i]; result.push(message);
    if (message.role !== 'assistant') continue;
    const replies = new Set<string>();
    let end = i + 1;
    while (end < messages.length && messages[end].role === 'toolResult') {
      const reply = messages[end];
      if (reply.role === 'toolResult') replies.add(reply.toolCallId);
      result.push(reply); end++;
    }
    for (const part of message.content) {
      if (part.type === 'toolCall' && !replies.has(part.id)) result.push({ role: 'toolResult',
        toolCallId: part.id, toolName: part.name, isError: true, timestamp: Date.now(),
        content: [{ type: 'text', text: '上次查询已中断，未获得结果。需要这条证据时请重新查询。' }] });
    }
    i = end - 1;
  }
  return result;
}

export function compactContext(messages: AgentMessage[], contextWindow: number, outputTokens: number): AgentMessage[] {
  // The persistent transcript is untouched. Keep protocol groups (tool calls and
  // their results) together, even when one agent turn contains many tool rounds.
  const budget = Math.max(1000, Math.floor(contextWindow - outputTokens - 4000));
  const turns: AgentMessage[][] = [];
  for (const message of messages) {
    if (message.role === 'user' || !turns.length) turns.push([]);
    turns.at(-1)!.push(message);
  }
  const clip = (message: AgentMessage, limit: number): AgentMessage => message.role === 'toolResult'
    ? { ...message, details: undefined, content: message.content.map(part => part.type === 'text' && part.text.length > limit
      ? { ...part, text: part.text.slice(0, limit) + '\n[此工具结果在模型上下文中缩短。完整结果保存在本地；长证据请按 body_offset 分页读取。]' } : part) }
    : message;
  const selected: AgentMessage[][] = []; let used = 0;
  for (const turn of turns.toReversed()) {
    const resultCount = Math.max(1, turn.filter(m => m.role === 'toolResult').length);
    const copy = turn.map(message => clip(message, Math.max(300, Math.floor(budget / (resultCount + 3)))));
    const size = JSON.stringify(copy).length;
    if (used + size <= budget) { selected.unshift(copy); used += size; continue; }
    if (selected.length) break;
    const head = copy[0]?.role === 'user' ? [copy[0]] : [];
    const groups: AgentMessage[][] = [];
    for (const message of copy.slice(head.length)) {
      if (message.role !== 'toolResult' || !groups.length) groups.push([]);
      groups.at(-1)!.push(clip(message, 600));
    }
    used = JSON.stringify(head).length;
    if (used > budget / 2) throw new Error('这条问题超出当前模型可用上下文，请分成几个问题。完整输入仍保存在本地会话。');
    const tail: AgentMessage[][] = [];
    for (const group of groups.toReversed()) {
      const length = JSON.stringify(group).length;
      if (used + length > budget - 200) {
        if (!tail.length) throw new Error('当前工具调用超过模型上下文容量，请停止后缩小查询范围。');
        break;
      }
      tail.unshift(group); used += length;
    }
    const first = head[0];
    if (first?.role === 'user') {
      const note = '\n[本轮较早的查询已从模型上下文移出，完整记录保存在本地。需要依据时重新检索。]';
      head[0] = { ...first, content: typeof first.content === 'string' ? first.content + note : [...first.content, { type: 'text', text: note }] };
    }
    selected.unshift([...head, ...tail.flat()]);
    break;
  }
  return selected.flat();
}

export async function exportConversation(id: string) {
  const conversation = (await listConversations()).find(c => c.id === id);
  const blob = new Blob([JSON.stringify({ version: 1, conversation, messages: snapshot(id).messages }, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a'); link.href = url; link.download = 'monica-conversation.json'; link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export async function importConversation(id: string, file: File) {
  if (snapshot(id).running) throw new Error('请先停止当前回答');
  const value = JSON.parse(await file.text());
  if (value.version !== 1 || !Array.isArray(value.messages) || value.messages.some((m: {role?: string; content?: unknown}) => !['user', 'assistant', 'toolResult'].includes(m.role ?? '') || m.content == null)) throw new Error('无法读取此会话格式');
  await set('monica:ask:' + id, value.messages);
  if (value.conversation && typeof value.conversation.title === 'string' && Array.isArray(value.conversation.people)) {
    await saveConversation({ id, title: value.conversation.title.slice(0, 60), updatedAt: Date.now(),
      people: value.conversation.people.filter((p: {id?: unknown; display_name?: unknown}) => typeof p.id === 'string' && typeof p.display_name === 'string') });
  }
  update(id, { messages: value.messages, error: '' });
}

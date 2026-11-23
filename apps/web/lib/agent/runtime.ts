"use client";
import { Agent, type AgentMessage } from '@earendil-works/pi-agent-core';
import { normalizeContext } from '@earendil-works/pi-ai/utils/transcript';
import { del, get, set } from 'idb-keyval';
import { apiFetch } from '../api';
import type { PickedPerson } from '../../components/ui/person-picker';
import { cliTool } from './cli';
import { compactContext, contextBudget, estimateTokens, resumeContext } from './context';
import { streamModel } from './model-stream';
import { readJson } from './requests';
import { deleteConversation, listConversations, saveConversation } from './sessions';
import { TOOL_LABELS } from './tools';

/** One conversation's transcript and progress, shared by every view of it. */
export type Snapshot = { messages: AgentMessage[]; running: boolean; status: string; error: string; model?: string };
const snapshots = new Map<string, Snapshot>();
const listeners = new Set<() => void>();
const agents = new Map<string, Agent>();
const controllers = new Map<string, AbortController>();
let runningIds = new Set<string>();
const empty: Snapshot = { messages: [], running: false, status: '', error: '' };
const transcriptKey = (id: string) => 'monica:ask:' + id;

export const subscribe = (fn: () => void) => { listeners.add(fn); return () => { listeners.delete(fn); }; };
export const snapshot = (id: string) => snapshots.get(id) ?? empty;
export const runningConversations = (): ReadonlySet<string> => runningIds;
function update(id: string, value: Partial<Snapshot>) {
  if (value.running !== undefined && value.running !== runningIds.has(id)) {
    runningIds = new Set(runningIds);
    if (value.running) runningIds.add(id);
    else runningIds.delete(id);
  }
  snapshots.set(id, { ...snapshot(id), ...value }); listeners.forEach(fn => fn());
}

export async function restore(id: string) {
  if (snapshots.has(id)) return;
  const saved = await get<AgentMessage[]>(transcriptKey(id));
  if (!snapshots.has(id)) update(id, { messages: saved ?? [] });
}
export function stop(id: string) { controllers.get(id)?.abort(); agents.get(id)?.abort(); }
export async function forget(id: string) {
  if (runningIds.has(id)) throw new Error('这个会话还在回答。');
  await navigator.locks.request(transcriptKey(id), { ifAvailable: true }, async lock => {
    if (!lock) throw new Error('这个会话正在另一个标签页里回答。');
    await deleteConversation(id);
    await del(transcriptKey(id));
    snapshots.delete(id);
  });
}

function systemPrompt(people: PickedPerson[]) {
  const today = new Intl.DateTimeFormat('zh-CN', { dateStyle: 'full' }).format(new Date());
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const focus = people.length
    ? '用户这次想了解的人：\n' + people.map(person => `- ${person.display_name}（profile_id: ${person.id}）`).join('\n') + '\n问题里的「TA」「他们」指这些人。'
    : '用户没有指定人物。问题提到某个人时先用 directory 找到 TA。问题关于整体往来时用 relationship_overview。';
  return `你是 Monica，用户的私人关系助手。你可以查用户通讯录里的人物资料、聊天记录、朋友发的动态、邮件，以及 Monica 整理的人物概括和每周近况。
今天是${today}，时区 ${zone}。工具返回的时间都是这个时区的本地时间。

${focus}

怎么查：
- 通过 monica_cli 提交 JavaScript，脚本中用 cli("命令", 参数对象) 读取资料，最后 return 需要交给模型的结果。cli 同步返回，无需 await。例如 return cli("directory", {tags:["同事"], sort:"last_contact"}).rows.slice(0, 10)。
- 有标签、公司、学校和平台条件时先用 directory 筛选。涉及很多人时，用 batch 的第三个参数逐项提取与问题有关的数据。例如 const people = cli("directory", {tags:["同事"]}).rows; return cli("batch", people.map(p => ({command:"read_messages", args:{profile_id:p.profile_id}})), (result, i) => ({name:people[i].name, count:result.messages?.length, error:result.error}))。
- 先查再答。了解一个人从 get_person 开始；想知道聊天里说过什么，用 search_messages 按原文关键词搜，或用 read_messages 按时间读。
- 搜索是逐字匹配，一个词只能找到恰好这么写的消息。每个问题先想它在聊天里可能怎么说：同义词、口语、英文和缩写、相关的人名地名、事情的前因后果，用 | 一次搜完；第一轮的结果里出现了新的说法，就拿它再搜一轮。
- 搜到线索后，用 read_messages 读那几天前后的聊天，看清上下文再下结论；只凭一句命中的话容易看错。
- 结果带 earlier 说明更早还有。问题问的是「所有」「每次」「第一次」「这些年」时，用 before 一直翻到问题要求的时间范围读完，或者 earlier 为空。
- 查询返回 {error} 或 not_searched 时，那部分的结果未知。批量结果可能只有部分失败。保留已有线索，持续失败时可以缩小范围、换关键词或换命令。用已经核实的内容回答，并说明暂时查不到的资料。
- 确实查不到时，说明查过哪些说法、哪段时间。
- 聊天里 from 为「我」的是用户本人，其余是对方或群成员。

怎么答：
- 用中文，先给结论，再写支撑它的具体内容：谁、哪一天、说了什么。不复述查询过程。
- 提到具体的消息、动态或邮件时，用工具结果里的 link 写成 Markdown 链接，例如 [11 月 3 日的聊天](/messages?conversation=…)；提到人物时链接到 /profiles/人物ID。
- 只根据查到的内容回答。资料里没有的，直接说没有记录。只读了一部分就说明读到哪里，不要把一部分当成全部。`;
}

/** An assistant turn that neither says anything nor asks for a lookup. */
function wroteNothing(message?: AgentMessage) {
  if (!message || message.role !== 'assistant' || (message.stopReason !== 'stop' && message.stopReason !== 'length')) return false;
  return !message.content.some(part => part.type === 'toolCall' || (part.type === 'text' && part.text.trim()));
}

async function settings(signal: AbortSignal) {
  const value = await readJson<{ enabled: boolean; model: string; models?: string[]; context_window: number; max_tokens: number }>(
    '/api/v1/ai/browser-model', signal, '无法读取模型设置');
  if (!value.enabled) throw new Error('问答还没有开启。请在「设置 → 模型」里启用模型服务和问 Monica。');
  return value;
}

const MODEL_KEY = 'monica:ask:model';
const MAX_LOOKUP_ROUNDS = 50;
/** The models a question can be answered with, the configured one first, and the one chosen in this browser. */
export async function modelChoices(): Promise<{ models: string[]; chosen: string }> {
  const response = await apiFetch('/api/v1/ai/browser-model');
  if (!response.ok) return { models: [], chosen: '' };
  const value = await response.json();
  const models: string[] = value.enabled ? value.models ?? [value.model] : [];
  const saved = localStorage.getItem(MODEL_KEY) ?? '';
  return { models, chosen: models.includes(saved) ? saved : models[0] ?? '' };
}
export function chooseModel(model: string) { localStorage.setItem(MODEL_KEY, model); }

export async function run(id: string, question: string, people: PickedPerson[]) {
  if (snapshot(id).running) return;
  const controller = new AbortController(); controllers.set(id, controller);
  update(id, { running: true, error: '', status: '正在思考…', model: undefined });
  try {
    const chosen = localStorage.getItem(MODEL_KEY) ?? '';
    await navigator.locks.request(transcriptKey(id), { ifAvailable: true }, async lock => {
      if (!lock) throw new Error('这个会话正在另一个标签页里回答');
      const history = await get<AgentMessage[]>(transcriptKey(id)) ?? [];
      const conversation = (await listConversations()).find(c => c.id === id);
      if (!conversation) throw new Error('这个会话已被删除。');
      await saveConversation({ ...conversation, updatedAt: Date.now(),
        title: history.some(message => message.role === 'user') ? conversation.title : question.trim().slice(0, 60) });
      const messages: AgentMessage[] = [
        ...history.filter(message => message.role !== 'system'),
        { role: 'user', content: question, timestamp: Date.now() },
      ];
      await set(transcriptKey(id), messages);
      update(id, { messages });
      const model = await settings(controller.signal);
      // The server answers with the configured model unless this one is among its choices.
      const modelId = (model.models ?? []).includes(chosen) ? chosen : model.model;
      update(id, { model: modelId });
      let nudges = 0;
      let lengthContinuations = 0;
      let lookupRounds = 0;
      let instruction = '';
      let requestInstruction = '';
      let requestMessages: AgentMessage[] = [];
      let budget = contextBudget(model.context_window, model.max_tokens);
      function prepareContext(limit: number) {
        const context = compactContext(requestMessages, limit);
        if (!requestInstruction) return context;
        if (wroteNothing(context.at(-1))) context.pop();
        return [...context, { role: 'user' as const, content: requestInstruction, timestamp: Date.now() }];
      }
      const agent = new Agent({
        initialState: {
          model: { id: modelId, name: modelId, provider: 'monica', api: 'openai-completions',
            baseUrl: location.origin + '/api/v1/ai', reasoning: false, input: ['text'],
            contextWindow: model.context_window, maxTokens: model.max_tokens,
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
            compat: { supportsDeveloperRole: false, supportsStore: false, maxTokensField: 'max_tokens' } },
          messages,
          systemPrompt: systemPrompt(people),
          tools: [cliTool],
        },
        toolExecution: 'sequential',
        transformContext: async messages => {
          requestMessages = [...messages.filter(message => message.role === 'system'), ...resumeContext(messages)];
          requestInstruction = instruction;
          instruction = '';
          return prepareContext(budget);
        },
        streamFn: (target, context, options) => streamModel(target as Parameters<typeof streamModel>[0], context, {
          ...options, toolChoice: lookupRounds >= MAX_LOOKUP_ROUNDS ? 'none' : options?.toolChoice,
        }, {
          onRetry: (attempt, limit) => update(id, {
            messages: [...agent.state.messages], status: `模型连接中断，正在重试（${attempt}/${limit}）…`,
          }),
          compact: current => {
            const size = estimateTokens(current.messages.filter(message => message.role !== 'system'));
            const smaller = Math.min(budget, Math.max(1_000, Math.floor(size * 0.55)));
            const messages = prepareContext(smaller);
            if (estimateTokens(messages) >= estimateTokens(current.messages)) return;
            budget = smaller;
            update(id, { messages: [...agent.state.messages], status: '正在缩短上下文，保留本轮问题和查询结果…' });
            return normalizeContext({ messages });
          },
        }),
        beforeToolCall: async () => lookupRounds >= MAX_LOOKUP_ROUNDS
          ? { block: true, reason: '本轮查询已到上限。' }
          : undefined,
        finishTurn: ({ message, toolResults }, signal) => {
          if (signal?.aborted || message.stopReason === 'error' || message.stopReason === 'aborted') return;
          if (toolResults.length) {
            if (lookupRounds >= MAX_LOOKUP_ROUNDS) return { action: 'end' };
            if (++lookupRounds === MAX_LOOKUP_ROUNDS) {
              instruction = '本轮查询已到上限。根据已核实的资料回答，并说明还有哪些范围没查完。';
            }
            return;
          }
          if (wroteNothing(message) && nudges < 2) {
            nudges++;
            instruction = '继续：还需要查就接着查，查够了就根据已经查到的内容回答。';
            return { action: 'continue' };
          }
          if (message.stopReason === 'length' && !wroteNothing(message) && lengthContinuations < 2) {
            lengthContinuations++;
            instruction = '上一段回答达到输出上限。请从断开的地方继续，避免重复已经写过的内容。';
            return { action: 'continue' };
          }
        },
      });
      agents.set(id, agent);
      const unsubscribe = agent.subscribe(async event => {
        if (event.type === 'tool_execution_start') update(id, { status: `正在${TOOL_LABELS[event.toolName] ?? event.toolName}…` });
        if (event.type === 'tool_execution_end') update(id, { status: '正在整理回答…' });
        if (event.type === 'message_update') {
          update(id, { messages: [...agent.state.messages, event.message],
            status: event.assistantMessageEvent.type.startsWith('text_') ? '正在回答…' : '正在思考…' });
        }
        if (event.type === 'message_end') {
          const messages = [...agent.state.messages];
          update(id, { messages });
          await set(transcriptKey(id), messages);
        }
      });
      try {
        controller.signal.throwIfAborted();
        await agent.continue();
        if (agent.state.errorMessage) throw new Error(agent.state.errorMessage);
        const last = agent.state.messages.at(-1);
        if (wroteNothing(last)) throw new Error('模型这次没有生成回答。本轮进度已保存。');
        if (last?.role === 'assistant' && last.stopReason === 'length') throw new Error('回答达到长度上限。已生成的内容已保存。');
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

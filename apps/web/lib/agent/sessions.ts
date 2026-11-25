import { get, set } from 'idb-keyval';
import { apiFetch } from '../api';
import type { PickedPerson } from '../../components/ui/person-picker';

export type Conversation = { id: string; title: string; people: PickedPerson[]; updatedAt: string; running: boolean };
export type Part =
  | { type: 'text'; text: string }
  | { type: 'thinking'; thinking: string; redacted?: boolean }
  | { type: 'toolCall'; id: string; name: string; arguments: Record<string, unknown> };
export type Message =
  | { role: 'user'; content: string }
  | { role: 'assistant'; content: Part[]; stopReason?: string }
  | { role: 'toolResult'; toolName: string; toolCallId: string; isError: boolean; details?: unknown };
export type CliQueryDetails = { queries: string[]; omittedQueries: number };
export type Run = { id: string; status: string; progress: string; error: string; model: string; draft: Message | null };
export type Edit = {
  id: string;
  status: 'pending' | 'applied' | 'dismissed' | 'stale';
  preview: {
    title: string;
    people: { id: string; name: string }[];
    before: Record<string, unknown> | null;
    after: Record<string, unknown>;
  };
  result: { message?: string; link?: string } | null;
};
export type ChatPage = { chat: Conversation; rows: { seq: number; message: Message }[]; run: Run | null; edits?: Edit[]; more: boolean };

type Export = { version: number; conversation?: Pick<Conversation, 'title' | 'people'>; messages: Message[] };
const chats = '/api/v1/ask/chats';
const selected = 'monica:ask:server-active';
const modelKey = 'monica:ask:model';

export class AskError extends Error {
  constructor(message: string, readonly status: number) { super(message); }
}

async function request<T>(path: string, init?: RequestInit, allowRetry = false): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    init?.signal?.throwIfAborted();
    try {
      const response = await apiFetch(path, { ...init, cache: 'no-store', propagateCsrfFailure: true });
      if (response.ok) return response.status === 204 || response.status === 202 ? undefined as T : await response.json();
      const body = await response.json().catch(() => ({}));
      const detail = typeof body.detail === 'string' ? body.detail : `暂时无法连接会话（HTTP ${response.status}）`;
      throw new AskError(detail, response.status);
    } catch (error) {
      init?.signal?.throwIfAborted();
      const transient = error instanceof TypeError || (error instanceof AskError && (error.status >= 500 || [408, 429].includes(error.status)));
      if (!allowRetry || !transient || attempt >= 10) throw error;
      await new Promise(resolve => setTimeout(resolve, Math.min(1500, 250 * 2 ** attempt)));
    }
  }
}

function json(method: string, data?: unknown): RequestInit {
  return { method, ...(data === undefined ? {} : { headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(data) }) };
}

export const listConversations = (signal?: AbortSignal, retry = false) => request<Conversation[]>(chats, { signal }, retry);
export const createConversation = (id: string) => request<Conversation>(chats, json('POST', { id }), true);
export const editConversation = (id: string, people: PickedPerson[]) => request<Conversation>(`${chats}/${id}`, json('PATCH', { people }));
export const deleteConversation = (id: string) => request<void>(`${chats}/${id}`, json('DELETE'));
export const readConversation = (id: string, after = 0, signal?: AbortSignal) =>
  request<ChatPage>(`${chats}/${id}?after=${after}`, { signal });
export const decideEdit = (chat: string, edit: string, confirm: boolean) =>
  request<Edit>(`${chats}/${chat}/edits/${edit}`, json('POST', { confirm }), true);
export const stop = (id: string, run: string) => request<void>(`${chats}/${id}/runs/${run}`, json('DELETE'), true);
export const activeConversation = () => localStorage.getItem(selected) ?? '';
export const selectConversation = (id: string) => localStorage.setItem(selected, id);

export function run(id: string, question: string, people: PickedPerson[], requestId: string) {
  return request<void>(`${chats}/${id}/runs`, json('POST', {
    request_id: requestId, question, people, model: localStorage.getItem(modelKey) || undefined,
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
  }), true);
}

export async function modelChoices(): Promise<{ models: string[]; chosen: string }> {
  const value = await request<{ enabled: boolean; models?: string[]; model: string }>('/api/v1/ai/browser-model');
  const models = value.enabled ? value.models ?? [value.model] : [];
  const saved = localStorage.getItem(modelKey) ?? '';
  return { models, chosen: models.includes(saved) ? saved : models[0] ?? '' };
}
export const chooseModel = (model: string) => localStorage.setItem(modelKey, model);

export async function exportConversation(id: string) {
  const messages: Message[] = [];
  let cursor = 0;
  let page: ChatPage;
  do {
    page = await readConversation(id, cursor);
    messages.push(...page.rows.map(({ message }) => message));
    cursor = page.rows.at(-1)?.seq ?? cursor;
  } while (page.more);
  const data: Export = { version: 1, conversation: page.chat, messages };
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = 'monica-conversation.json';
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function importData(data: Export) {
  if (data?.version !== 1 || !Array.isArray(data.messages)) throw new Error('这不是 Monica 导出的会话文件。');
  return request<Conversation>(`${chats}/import`, json('POST', data));
}
export async function importConversation(file: File) {
  if (file.size > 24 * 1024 * 1024) throw new Error('会话文件超过 24 MB。');
  return importData(JSON.parse(await file.text()));
}

// Older transcripts were browser-local and were never keyed by account. Only
// transfer them on explicit request, and retain the source as a local copy.
export type LegacyChat = Export & { localId: string };
export async function legacyConversations(): Promise<LegacyChat[]> {
  const index = await get<(Pick<Conversation, 'title' | 'people'> & { id: string })[]>('monica:ask:sessions')
    ?? [{ id: 'default', title: '导入的会话', people: [] }];
  const me = await request<{ id: string }>('/api/v1/me');
  const imported = new Set(await get<string[]>(`monica:ask:imported:${me.id}`) ?? []);
  const chats = await Promise.all(index.filter(chat => !imported.has(chat.id)).map(async conversation => ({
    version: 1, conversation, localId: conversation.id,
    messages: await get<Message[]>(`monica:ask:${conversation.id}`) ?? [],
  })));
  return chats.filter(chat => chat.messages.length > 0);
}
export async function importLegacy(chat: LegacyChat) {
  const me = await request<{ id: string }>('/api/v1/me');
  const key = `monica:ask:imported:${me.id}`;
  const saved = await importData(chat);
  await set(key, [...await get<string[]>(key) ?? [], chat.localId]);
  return saved;
}

export const cliTitle = (value: unknown) =>
  (typeof value === 'string' && value.trim().replace(/\s+/g, ' ').slice(0, 60)) || '运行 JavaScript 查询';

export const TOOL_LABELS: Record<string, string> = {
  find_people: '查找人物', get_person: '读取人物资料', search_messages: '搜索聊天', read_messages: '读取聊天',
  get_posts: '读取动态', search_records: '搜索动态与记录', search_mail: '搜索邮件与来源记录', read_record: '读取原文', relationship_overview: '查看往来概况',
};

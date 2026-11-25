import { expect, test, type Page } from '@playwright/test';
import type { Edit } from '../lib/agent/sessions';

type Chat = { id: string; title: string; people: unknown[]; updatedAt: string; running: boolean };
type Message = Record<string, unknown>;
type Run = { id: string; status: string; progress: string; model: string; error: string; draft: Message | null };

async function server(page: Page) {
  const chats = new Map<string, { chat: Chat; messages: Message[]; run: Run | null; edits?: Edit[] }>();
  const unexpected: string[] = [];
  const submissions: string[] = [];
  const cancelled: string[] = [];
  const decisions: unknown[] = [];
  let dropStart = false;
  let editGate: Promise<void> | undefined;
  const submitted = new Set<string>();
  const profile = '00000000-0000-4000-8000-000000000042';

  await page.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    const method = route.request().method();
    const path = url.pathname;
    if (path === '/api/v1/me') return route.fulfill({ json: { id: 'synthetic', email: 'synthetic@example.invalid' } });
    if (path === '/api/v1/auth/csrf') return route.fulfill({ json: { csrf_token: 'synthetic' } });
    if (path === '/api/v1/ai/browser-model') return route.fulfill({
      json: { enabled: true, model: 'deepseek-v4-pro', models: ['deepseek-v4-pro', 'deepseek-v4-pro-medium', 'gemini-4-neon'] },
    });
    if (path === `/api/v1/profiles/${profile}`) return route.fulfill({ json: { id: profile, display_name: '合成联系人' } });
    if (path === '/api/v1/ask/chats' && method === 'GET') return route.fulfill({ json: [...chats.values()].map(row => row.chat).reverse() });
    const create = () => ({ id: crypto.randomUUID(), title: '新会话', people: [], updatedAt: new Date().toISOString(), running: false });
    if (path === '/api/v1/ask/chats' && method === 'POST') {
      const id = route.request().postDataJSON().id;
      const existing = chats.get(id);
      if (existing) return route.fulfill({ json: existing.chat });
      const chat = { ...create(), id };
      chats.set(chat.id, { chat, messages: [], run: null });
      return route.fulfill({ json: chat });
    }
    if (path === '/api/v1/ask/chats/import' && method === 'POST') {
      const body = route.request().postDataJSON();
      const chat: Chat = { ...create(), title: body.conversation.title, people: body.conversation.people };
      chats.set(chat.id, { chat, messages: body.messages, run: null });
      return route.fulfill({ json: chat });
    }

    const id = path.match(/^\/api\/v1\/ask\/chats\/([^/]+)/)?.[1] ?? '';
    if (!id) {
      if (/\/api\/v1\/(?:ai\/chat\/completions|directory|messages\/search)(?:\/|$)/.test(path)) unexpected.push(path);
      return route.fulfill({ json: [] });
    }
    const row = chats.get(id);
    if (!row) return route.fulfill({ status: 404, json: { detail: '会话不存在' } });
    if (path.endsWith('/runs')) {
      const body = route.request().postDataJSON();
      submissions.push(body.request_id);
      if (!submitted.has(body.request_id)) {
        submitted.add(body.request_id);
        row.chat.title = row.messages.length ? row.chat.title : body.question;
        row.chat.people = body.people;
        row.chat.running = true;
        row.run = { id: body.request_id, status: 'running', progress: '正在按 QD 标签查询…', model: body.model ?? 'deepseek-v4-pro', error: '', draft: null };
        row.messages.push({ role: 'user', content: body.question });
      }
      if (dropStart) { dropStart = false; return route.abort('connectionreset'); }
      return route.fulfill({ status: 202, body: '' });
    }
    if (path.includes('/runs/') && method === 'DELETE') {
      cancelled.push(id);
      row.chat.running = false;
      if (row.run) row.run.status = 'cancelled';
      return route.fulfill({ status: 204 });
    }
    if (path.includes('/edits/') && method === 'POST') {
      const edit = row.edits?.find(item => path.endsWith(`/${item.id}`));
      if (!edit) return route.fulfill({ status: 404 });
      const decision = route.request().postDataJSON();
      decisions.push(decision);
      edit.seq = Math.max(...(row.edits ?? []).map(item => item.seq)) + 1;
      edit.status = decision.confirm ? 'applied' : 'dismissed';
      return route.fulfill({ json: edit });
    }
    if (method === 'DELETE') { chats.delete(id); return route.fulfill({ status: 204 }); }
    if (method === 'PATCH') {
      await editGate;
      row.chat.people = route.request().postDataJSON().people;
      return route.fulfill({ json: row.chat });
    }
    const cursor = Number(url.searchParams.get('after') ?? '0');
    const editCursor = Number(url.searchParams.get('after_edit') ?? '0');
    const changes = (row.edits ?? []).filter(edit => edit.seq > editCursor).sort((a, b) => a.seq - b.seq);
    return route.fulfill({ json: { chat: row.chat, run: row.run, edits: changes.slice(0, 1), moreEdits: changes.length > 1, more: false,
      rows: row.messages.map((message, index) => ({ seq: index + 1, message })).filter(message => message.seq > cursor),
    } });
  });
  return { chats, unexpected, cancelled, submissions, decisions, profile,
    disconnectNextStart() { dropStart = true; },
    holdPeopleEdit() {
      let release!: () => void;
      editGate = new Promise(resolve => { release = resolve; });
      return release;
    },
  };
}

async function ask(page: Page, question: string) {
  await page.getByRole('textbox', { name: '问题', exact: true }).fill(question);
  await page.getByRole('button', { name: '提问', exact: true }).click();
}

function finish(entry: { chat: Chat; messages: Message[]; run: Run | null }, profile: string) {
  const script = 'return cli("directory", {tags: ["QD"]});';
  entry.messages.push(
    { role: 'assistant', content: [
      { type: 'thinking', thinking: '先核实标签名单。' },
      { type: 'toolCall', name: 'monica_cli', id: 'lookup', arguments: { title: '按 QD 标签查询联系人', script } },
    ] },
    { role: 'toolResult', toolName: 'monica_cli', toolCallId: 'lookup', isError: false,
      details: { queries: ['cli("directory", {"tags":["QD"]})'], omittedQueries: 0 } },
    { role: 'assistant', content: [{ type: 'text', text: `查到了[合成联系人](/profiles/${profile})。` }] },
  );
  entry.chat.running = false;
  entry.run = { ...entry.run!, status: 'done', progress: '' };
}

test('Ask restores server progress and expands the saved script, queries and model thinking', async ({ page }) => {
  const api = await server(page);
  api.disconnectNextStart();
  await page.goto('/ask');
  await page.getByLabel('回答用的模型').selectOption({ label: 'DeepSeek V4 Pro · Medium' });
  await ask(page, '所有 QD 标签的人');
  await expect(page.getByRole('button', { name: '停止回答' })).toBeVisible();
  await expect.poll(() => api.submissions.length).toBe(2);
  expect(new Set(api.submissions).size).toBe(1);
  const entry = [...api.chats.values()][0];
  expect(entry.run?.model).toBe('deepseek-v4-pro-medium');
  await page.goto('/contacts');
  finish(entry, api.profile);
  await page.goto('/ask');
  await expect(page.getByRole('link', { name: '合成联系人' })).toBeVisible();
  await expect(page.getByText('先核实标签名单。')).toHaveCount(0);
  await page.getByText('模型思考', { exact: true }).click();
  await expect(page.getByLabel('模型思考详情')).toContainText('先核实标签名单。');
  await page.getByText('查询记录（1 步）', { exact: true }).click();
  await page.getByText('按 QD 标签查询联系人', { exact: true }).click();
  await expect(page.getByLabel('JavaScript 脚本')).toHaveText('return cli("directory", {tags: ["QD"]});');
  await expect(page.getByLabel('查询命令与参数')).toHaveText('cli("directory", {"tags":["QD"]})');
  expect(api.unexpected).toEqual([]);
  expect(api.cancelled).toEqual([]);
  expect(entry.messages.filter(message => message.role === 'user')).toHaveLength(1);
});

test('independent chats can run at once and can be stopped or deleted independently', async ({ page }) => {
  const api = await server(page);
  await page.goto('/ask');
  await ask(page, '第一条问题');
  await expect(page.getByRole('button', { name: '停止回答' })).toBeVisible();
  await page.getByRole('button', { name: '新会话', exact: true }).click();
  await page.getByLabel('回答用的模型').selectOption('gemini-4-neon');
  await ask(page, '第二条问题');
  await expect(page.getByRole('button', { name: '停止回答' })).toBeVisible();
  expect([...api.chats.values()].map(row => row.run?.model)).toEqual(['deepseek-v4-pro', 'gemini-4-neon']);
  expect([...api.chats.values()].filter(row => row.chat.running)).toHaveLength(2);
  await page.getByRole('button', { name: '停止回答' }).click();
  await expect(page.getByRole('button', { name: '停止回答' })).toHaveCount(0);
  await page.getByRole('button', { name: '删除会话：第一条问题', exact: true }).click();
  await expect.poll(() => api.chats.size).toBe(1);
  await page.reload();
  await expect(page.getByText('第二条问题', { exact: true }).last()).toBeVisible();
  expect(api.cancelled).toHaveLength(1);
});

test('imported browser exports are retained by the server after reload', async ({ page }) => {
  const api = await server(page);
  await page.goto('/ask');
  await page.getByLabel('导入会话文件').setInputFiles({
    name: 'legacy.json', mimeType: 'application/json',
    buffer: Buffer.from(JSON.stringify({ version: 1, conversation: { title: '旧会话', people: [] },
      messages: [{ role: 'user', content: '之前的问题' }, { role: 'assistant', content: [{ type: 'text', text: '之前的回答' }] }],
    })),
  });
  await expect(page.getByText('之前的回答')).toBeVisible();
  await page.reload();
  await expect(page.getByText('之前的回答')).toBeVisible();
  expect(api.chats.size).toBe(1);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('button', { name: '会话操作' }).click();
  await page.getByRole('menuitem', { name: '删除当前会话' }).click();
  await expect.poll(() => api.chats.size).toBe(0);
  await page.reload();
  await expect(page.getByText('之前的回答')).toHaveCount(0);
});

test('the question uses the visible person selection while a save is pending', async ({ page }) => {
  const api = await server(page);
  await page.goto('/ask');
  await page.getByLabel('导入会话文件').setInputFiles({
    name: 'people.json', mimeType: 'application/json',
    buffer: Buffer.from(JSON.stringify({ version: 1, conversation: { title: '人物会话', people: [
      { id: api.profile, display_name: '合成联系人', avatar_media_id: null },
    ] }, messages: [] })),
  });
  const remove = page.getByRole('button', { name: '移除人物 合成联系人' });
  await expect(remove).toBeVisible();
  await page.getByLabel('问题', { exact: true }).fill('问一下近况');
  const release = api.holdPeopleEdit();
  await remove.click();
  await expect(remove).toHaveCount(0);
  const send = page.getByRole('button', { name: '提问', exact: true });
  await expect(send).toBeDisabled();
  release();
  await send.click();
  await expect(page.getByRole('button', { name: '停止回答' })).toBeVisible();
  expect([...api.chats.values()][0].chat.people).toEqual([]);
});

test('Ask previews a saved edit and sends an approval without the editable payload', async ({ page }) => {
  const api = await server(page);
  await page.goto('/ask');
  await ask(page, '把她的常驻地改成上海');
  await expect(page.getByRole('button', { name: '停止回答' })).toBeVisible();
  const row = [...api.chats.values()][0];
  const editId = crypto.randomUUID();
  const previousId = crypto.randomUUID();
  row.edits = [{
    id: previousId, seq: 1, status: 'applied', result: null,
    preview: { title: '添加已有标签', people: [], before: null, after: { 标签: '同学' } },
  }, {
    id: editId, seq: 2, status: 'pending', result: null,
    preview: {
      title: '补录联系人资料', people: [{ id: api.profile, name: '合成联系人' }],
      before: { 常驻地: '杭州' }, after: { 常驻地: '上海' },
    },
  }];
  for (const id of [previousId, editId]) row.messages.push({
    role: 'toolResult', toolName: 'propose_monica_edit', toolCallId: `edit-fixture-${id}`,
    isError: false, details: { editId: id },
  });
  row.chat.running = false;
  row.run = { ...row.run!, status: 'done', progress: '' };
  const card = page.getByRole('region', { name: '补录联系人资料' });
  const previous = page.getByRole('region', { name: '添加已有标签' });
  await expect(previous).toContainText('已应用');
  await expect(card).toContainText('杭州');
  await expect(card).toContainText('上海');
  await expect(card.getByRole('link', { name: '合成联系人' })).toBeVisible();
  expect(api.decisions).toEqual([]);
  await card.getByRole('button', { name: '确认写入' }).click();
  await expect(card).toContainText('已应用');
  expect(api.decisions).toEqual([{ confirm: true }]);
  await page.reload();
  await expect(previous).toContainText('已应用');
  await expect(card).toContainText('已应用');
  await expect(card.getByRole('button', { name: '确认写入' })).toHaveCount(0);
});

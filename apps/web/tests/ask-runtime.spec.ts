import { test, expect } from '@playwright/test';

function completion(delta: object, finish: 'stop' | 'tool_calls' | 'length' | null) {
  const chunk = (delta: object, finish_reason: string | null) => 'data: ' + JSON.stringify({
    id: 'synthetic-completion', object: 'chat.completion.chunk', created: 1, model: 'synthetic-model',
    choices: [{ index: 0, delta, finish_reason }],
  }) + '\n\n';
  return {
    contentType: 'text/event-stream',
    body: chunk(delta, null) + (finish ? chunk({}, finish) + 'data: [DONE]\n\n' : ''),
  };
}

function scriptCall(id: string, script: string) {
  return completion({ role: 'assistant', tool_calls: [
    { index: 0, id, type: 'function', function: { name: 'monica_cli', arguments: JSON.stringify({ script }) } },
  ] }, 'tool_calls');
}

function lookup(id: string, name: string, args: object) {
  return scriptCall(id, `return cli(${JSON.stringify(name)}, ${JSON.stringify(args)});`);
}

test('PI browser loop executes tools, persists IndexedDB sessions, and restores after reload', async ({page}) => {
 const errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));let modelCalls=0;let toolsRead=0;let messagesRead=0;let includedGroupSpeech=false;let submittedEvidence='';let systemPrompt='';
 const profile='00000000-0000-4000-8000-000000000042';
 const native='00000000-0000-4000-8000-000000000043';const legacy='00000000-0000-4000-8000-000000000044';
 await page.route('**/api/**',async route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/api/v1/me')return route.fulfill({json:{id:'synthetic',email:'synthetic@example.invalid'}});
  if(path==='/api/v1/auth/csrf')return route.fulfill({json:{csrf_token:'synthetic'}});
  if(path==='/api/v1/ai/browser-model')return route.fulfill({json:{enabled:true,model:'synthetic-model',context_window:32000,max_tokens:1000}});
  if(path==='/api/v1/profiles'){toolsRead++;return route.fulfill({json:[{id:profile,display_name:'合成联系人'}]});}
  if(path===`/api/v1/profiles/${profile}`)return route.fulfill({json:{id:profile,display_name:'合成联系人'}});
  if(path===`/api/v1/profiles/${profile}/messages`){messagesRead++;includedGroupSpeech=new URL(route.request().url()).searchParams.get('include_group_speech')==='true';return route.fulfill({json:[
   {id:native,conversation_id:'native-room',conversation_type:'group',text:'Native message',occurred_at:'2026-01-01T12:00:00Z',evidence_id:native,evidence_revision:7,citation:`/evidence/${native}?revision=7`},
   {id:legacy,conversation_id:'legacy-room',text:'Legacy message',occurred_at:'2026-01-01T11:00:00Z',citation:`/evidence/${legacy}?revision=3`},
  ]});}
  if(path==='/api/v1/ai/chat/completions'){
   const body=route.request().postDataJSON();modelCalls++;
   systemPrompt=JSON.stringify(body.messages.find((m:{role:string})=>m.role==='system'));
   const toolReplies=body.messages.filter((m:{role:string})=>m.role==='tool');
   if(toolReplies.length>=2){
    submittedEvidence=JSON.stringify(toolReplies.at(-1));
    return route.fulfill(completion({role:'assistant',content:`已找到[合成联系人](/profiles/${profile})。`}, 'stop'));
   }
   const id=`synthetic-tool-${toolReplies.length}`;
   return route.fulfill(toolReplies.length ? lookup(id,'search_messages',{profile_id:profile,query:'message'}) : lookup(id,'find_people',{query:'合成'}));
  }
  return route.fulfill({json:[]});
 });
 await page.goto('/ask');await expect(page.getByRole('heading',{name:'问 Monica',exact:true})).toBeVisible();
 await page.getByLabel('问题',{exact:true}).fill('找一下合成联系人');await page.getByRole('button',{name:'提问',exact:true}).click();
 await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();expect(modelCalls).toBe(3);expect(toolsRead).toBe(1);expect(messagesRead).toBe(1);expect(includedGroupSpeech).toBeTruthy();expect(submittedEvidence).toContain(`/evidence/${native}?revision=7`);expect(submittedEvidence).toContain(`/messages?conversation=legacy-room&message=${legacy}`);
 await page.reload();await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();expect(modelCalls).toBe(3);
 await page.getByRole('button',{name:'新会话',exact:true}).click();await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toHaveCount(0);
 const sessionPicker=page.getByLabel('选择会话');
 if(await sessionPicker.isVisible())await sessionPicker.selectOption({label:'找一下合成联系人'});
 else await page.getByRole('complementary',{name:'会话'}).getByRole('button',{name:'找一下合成联系人',exact:true}).click();
 await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();
 expect(systemPrompt).toContain('用户没有指定人物');
 await page.goto(`/ask?profile=${profile}`);
 await expect(page.getByRole('button',{name:'移除人物 合成联系人',exact:true})).toBeVisible();
 await page.getByLabel('问题',{exact:true}).fill('我们后来聊了什么？');
 await page.getByRole('button',{name:'提问',exact:true}).click();
 await expect.poll(()=>systemPrompt).toContain(`合成联系人（profile_id: ${profile}）`);
 await expect(page.getByRole('button',{name:'停止回答',exact:true})).toHaveCount(0);
 await page.setViewportSize({width:390,height:844});await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBeTruthy();
 await page.screenshot({path:'../../.local-validation/frontend-ask-mobile.png',fullPage:true});expect(errors).toEqual([]);
});
test('wording searches keep partial results and a turn that says nothing is asked to go on', async ({page}) => {
 const searched:string[]=[];let modelCalls=0;let nudged=false;
 await page.route('**/api/**',async route=>{
  const url=new URL(route.request().url());
  if(url.pathname==='/api/v1/me')return route.fulfill({json:{id:'synthetic',email:'synthetic@example.invalid'}});
  if(url.pathname==='/api/v1/auth/csrf')return route.fulfill({json:{csrf_token:'synthetic'}});
  if(url.pathname==='/api/v1/ai/browser-model')return route.fulfill({json:{enabled:true,model:'synthetic-model',context_window:32000,max_tokens:1000}});
  if(url.pathname==='/api/v1/messages/search'){
   const term=url.searchParams.get('q')!;searched.push(term);
   if(term==='面试'&&searched.filter(t=>t==='面试').length===1)return route.fulfill({status:503,json:{error:'busy'}});
   if(term==='跳槽')return route.abort('connectionreset');
   return route.fulfill({json:[{id:'m-'+term,conversation_id:'room',conversation_type:'direct',conversation_name:'合成联系人',text:'说到'+term,occurred_at:term==='offer'?'2026-01-02T12:00:00Z':'2026-01-01T12:00:00Z'}]});
  }
  if(url.pathname==='/api/v1/ai/chat/completions'){
   const body=route.request().postDataJSON();modelCalls++;
   const last=body.messages.at(-1);
   if(last.role==='user'&&String(last.content).startsWith('继续')){nudged=true;return route.fulfill(completion({role:'assistant',content:'两种说法都查到了。'},'stop'));}
   if(last.role==='tool'){
    const result=JSON.parse(last.content);
    expect(result.messages.map((row:{text:string})=>row.text)).toEqual(['说到offer','说到面试']);
    expect(result.not_searched).toEqual([{term:'跳槽',error:expect.stringContaining('连接中断')}]);
    return route.fulfill(completion({role:'assistant'}, 'stop'));
   }
   return route.fulfill(lookup('synthetic-tool','search_messages',{query:'面试|offer|跳槽'}));
  }
  return route.fulfill({json:[]});
 });
 await page.goto('/ask');await page.getByLabel('问题',{exact:true}).fill('聊过找工作吗');await page.getByRole('button',{name:'提问',exact:true}).click();
 await expect(page.getByText('两种说法都查到了。',{exact:true})).toBeVisible({timeout:16_000});
 expect(searched).toEqual(['面试','面试','offer',...Array(11).fill('跳槽')]);expect(nudged).toBeTruthy();expect(modelCalls).toBe(3);
 // The request to go on is not part of the saved conversation.
 await page.reload();await expect(page.getByText('两种说法都查到了。',{exact:true})).toBeVisible();await expect(page.getByText(/^继续：/)).toHaveCount(0);
});

test('the model chooses another lookup and reconnects automatically from existing results', async ({ page }) => {
  const profile = '00000000-0000-4000-8000-000000000042';
  const question = '找一下我们聊过的面试';
  const completionInputs: unknown[] = [];
  let searches = 0;
  let reads = 0;
  let unavailableCalls = 0;
  let csrfOutages = 0;

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/v1/me') return route.fulfill({ json: { id: 'synthetic', email: 'synthetic@example.invalid' } });
    if (path === '/api/v1/auth/csrf') {
      if (reads === 2 && csrfOutages === 0) {
        csrfOutages++;
        return route.fulfill({ status: 503, headers: { 'Retry-After': '0' } });
      }
      return route.fulfill({ json: { csrf_token: 'synthetic' } });
    }
    if (path === '/api/v1/ai/browser-model') {
      return route.fulfill({ json: { enabled: true, model: 'synthetic-model', context_window: 32000, max_tokens: 1000 } });
    }
    if (path === '/api/v1/messages/search') {
      searches++;
      return route.fulfill({ status: 503, json: { error: 'database busy' } });
    }
    if (path === `/api/v1/profiles/${profile}/messages`) {
      if (++reads === 1) return route.abort('connectionreset');
      return route.fulfill({ json: [{ id: 'evidence', conversation_id: 'room', text: '周一面试', occurred_at: '2026-01-01T12:00:00Z' }] });
    }
    if (path !== '/api/v1/ai/chat/completions') return route.fulfill({ json: [] });
    const { messages } = route.request().postDataJSON();
    if (messages.at(-1).role === 'user' && JSON.stringify(messages.at(-1).content).includes('再问一件事')) {
      unavailableCalls++;
      return route.fulfill({ status: 503, headers: { 'Retry-After': '0' }, json: { error: 'provider busy' } });
    }
    completionInputs.push(messages);
    switch (completionInputs.length) {
      case 1: return route.fulfill(lookup('search', 'search_messages', { query: '面试' }));
      case 2:
        expect(JSON.parse(messages.at(-1).content)).toEqual({
          messages: [], earlier: null,
          not_searched: [{ term: '面试', error: expect.stringContaining('HTTP 503') }],
        });
        return route.fulfill(lookup('read', 'read_messages', { profile_id: profile }));
      case 3: return route.abort('connectionreset');
      case 4: return route.fulfill(completion({
        role: 'assistant', content: '中断的回答', tool_calls: [
          { index: 0, id: 'interrupted-search', type: 'function', function: { name: 'monica_cli', arguments: JSON.stringify({ script: 'return cli("search_messages", {query:"面试"});' }) } },
        ],
      }, null));
      case 5: return route.fulfill({
        contentType: 'text/event-stream', body: 'data: {"error":{"message":"terminated"}}\n\n',
      });
      case 6: return route.fulfill({ status: 502, headers: { 'Retry-After': '0' } });
      default:
        expect(messages.at(-1).content).toContain('周一面试');
        return route.fulfill(completion({ role: 'assistant', content: '查到一条记录提到周一面试。' }, 'stop'));
    }
  });

  await page.goto('/ask');
  await page.getByLabel('问题', { exact: true }).fill(question);
  await page.getByRole('button', { name: '提问', exact: true }).click();
  await expect(page.getByText('查到一条记录提到周一面试。', { exact: true })).toBeVisible({timeout:16_000});
  await expect(page.getByRole('article').getByText(question, { exact: true })).toHaveCount(1);
  await expect(page.getByText('中断的回答', { exact: true })).toHaveCount(0);
  expect([searches, reads]).toEqual([3, 2]);
  expect(csrfOutages).toBe(1);
  expect(completionInputs.slice(2)).toEqual(Array(5).fill(completionInputs[2]));

  await page.getByLabel('问题', { exact: true }).fill('再问一件事');
  await page.getByRole('button', { name: '提问', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('模型连接持续失败');
  await expect(page.getByRole('button', { name: '重试回答', exact: true })).toHaveCount(0);
  expect(unavailableCalls).toBe(11);
  await page.reload();
  await expect(page.getByRole('article').getByText('再问一件事', { exact: true })).toBeVisible();
  await expect(page.getByText('查到一条记录提到周一面试。', { exact: true })).toBeVisible();
});

test('the model respects Retry-After and Stop cancels the pending retry', async ({ page }) => {
  let modelCalls = 0;
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/v1/me') return route.fulfill({ json: { id: 'synthetic', email: 'synthetic@example.invalid' } });
    if (path === '/api/v1/auth/csrf') return route.fulfill({ json: { csrf_token: 'synthetic' } });
    if (path === '/api/v1/ai/browser-model') {
      return route.fulfill({ json: { enabled: true, model: 'synthetic-model', context_window: 32000, max_tokens: 1000 } });
    }
    if (path === '/api/v1/ai/chat/completions') {
      modelCalls++;
      return route.fulfill({ status: 429, headers: { 'Retry-After': '5' }, json: { detail: 'Rate limited' } });
    }
    return route.fulfill({ json: [] });
  });
  await page.clock.install({ time: new Date('2026-01-01T12:00:00Z') });
  await page.goto('/ask');
  await page.getByLabel('问题', { exact: true }).fill('查一下最近的聊天');
  const submit = page.getByRole('button', { name: '提问', exact: true });
  await expect(submit).toBeEnabled();
  await page.clock.pauseAt(new Date('2026-01-01T12:01:00Z'));
  await submit.dispatchEvent('click');
  await expect(page.getByText('模型连接中断，正在重试（1/10）…')).toBeVisible();
  await page.clock.runFor(2_000);
  expect(modelCalls).toBe(1);
  await page.getByRole('button', { name: '停止回答', exact: true }).dispatchEvent('click');
  await expect(page.getByRole('button', { name: '停止回答', exact: true })).toHaveCount(0);
  await page.clock.runFor(4_000);
  expect(modelCalls).toBe(1);
  await expect(page.getByRole('button', { name: '重试回答', exact: true })).toHaveCount(0);
});

test('an oversized tool result is compacted and a length-limited answer continues in the same turn', async ({ page }) => {
  const question = '整理聊天里讨论的项目';
  let modelCalls = 0;
  let searchCalls = 0;
  let rejectedSize = 0;

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/v1/me') return route.fulfill({ json: { id: 'synthetic', email: 'synthetic@example.invalid' } });
    if (path === '/api/v1/auth/csrf') return route.fulfill({ json: { csrf_token: 'synthetic' } });
    if (path === '/api/v1/ai/browser-model') {
      return route.fulfill({ json: { enabled: true, model: 'synthetic-model', context_window: 32000, max_tokens: 1000 } });
    }
    if (path === '/api/v1/messages/search') {
      searchCalls++;
      return route.fulfill({ json: Array.from({ length: 36 }, (_, id) => ({
        id: `m-${id}`, conversation_id: 'room', text: '项目讨论'.repeat(50), occurred_at: '2026-01-01T12:00:00Z',
      })) });
    }
    if (path !== '/api/v1/ai/chat/completions') return route.fulfill({ json: [] });
    const { messages } = route.request().postDataJSON();
    switch (++modelCalls) {
      case 1: return route.fulfill(lookup('search', 'search_messages', { query: '项目' }));
      case 2:
        rejectedSize = JSON.stringify(messages).length;
        return route.fulfill({ status: 413, json: { detail: '模型上下文超出上限' } });
      case 3:
      case 4:
        expect(JSON.stringify(messages).length).toBeLessThan(rejectedSize);
        expect(JSON.stringify(messages)).toContain(question);
        expect(messages.at(-1)).toMatchObject({ role: 'tool', tool_call_id: 'search' });
        expect(messages.at(-1).content).toContain('/messages?conversation=room');
        if (modelCalls === 3) {
          rejectedSize = JSON.stringify(messages).length;
          return route.fulfill({ status: 413, json: { detail: '模型上下文超出上限' } });
        }
        return route.fulfill(completion({
          role: 'assistant', content: '已核对项目。'.repeat(1_500) + '\n\n这是断点前的结尾。\n\n',
        }, 'length'));
      default:
        expect(messages.at(-2).role).toBe('assistant');
        expect(JSON.stringify(messages.at(-2))).toContain('这是断点前的结尾。');
        expect(messages.at(-1).content).toContain('上一段回答达到输出上限');
        return route.fulfill(completion({ role: 'assistant', content: '接着整理了讨论内容。' }, 'stop'));
    }
  });
  await page.goto('/ask');
  await page.getByLabel('问题', { exact: true }).fill(question);
  await page.getByRole('button', { name: '提问', exact: true }).click();
  await expect(page.getByText('接着整理了讨论内容。', { exact: true })).toBeVisible();
  await expect(page.getByText('这是断点前的结尾。', { exact: true })).toHaveCount(1);
  await expect(page.getByRole('article').getByText(question, { exact: true })).toHaveCount(1);
  expect([modelCalls, searchCalls]).toEqual([5, 1]);
});

test('a stopped partial answer survives the next question and reload', async ({ page }) => {
  const stoppedText = '这是停止之前已经写出的部分。';
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/v1/me') return route.fulfill({ json: { id: 'synthetic', email: 'synthetic@example.invalid' } });
    if (path === '/api/v1/auth/csrf') return route.fulfill({ json: { csrf_token: 'synthetic' } });
    if (path === '/api/v1/ai/browser-model') {
      return route.fulfill({ json: { enabled: true, model: 'synthetic-model', context_window: 32000, max_tokens: 1000 } });
    }
    if (path === '/api/v1/ai/chat/completions') {
      expect(JSON.stringify(route.request().postDataJSON().messages)).not.toContain(stoppedText);
      return route.fulfill(completion({ role: 'assistant', content: '新问题已答复。' }, 'stop'));
    }
    return route.fulfill({ json: [] });
  });

  await page.goto('/ask');
  await page.getByLabel('导入会话文件').setInputFiles({
    name: 'stopped-conversation.json', mimeType: 'application/json',
    buffer: Buffer.from(JSON.stringify({ version: 1, messages: [
      { role: 'user', content: '旧问题', timestamp: 1 },
      { role: 'assistant', stopReason: 'aborted', content: [{ type: 'text', text: stoppedText }], timestamp: 2 },
    ] })),
  });
  await expect(page.getByText(stoppedText, { exact: true })).toBeVisible();
  await page.getByLabel('问题', { exact: true }).fill('再提一个问题');
  await page.getByRole('button', { name: '提问', exact: true }).click();
  await expect(page.getByText('新问题已答复。', { exact: true })).toBeVisible();
  await page.reload();
  await expect(page.getByText('新问题已答复。', { exact: true })).toBeVisible();
  await expect(page.getByText(stoppedText, { exact: true })).toBeVisible();
});

test('JavaScript shares the contacts filters, batches reads and keeps their citations', async ({ page }) => {
  const ids = [1, 2, 3].map(n => `00000000-0000-4000-8000-00000000000${n}`);
  const columns = ['id', 'type', 'name', 'summary', 'avatar_media_id', 'tag_ids', 'company_ids', 'school_ids',
    'aliases', 'providers', 'headline', 'city', 'birthday_md', 'last_contact_at', 'direct_message_count',
    'member_count', 'conversation_id', 'group_names'];
  const profiles = ['李丽', '李明', '李伟'].map((name, i) => [
    ids[i], 'person', name, null, null, i === 2 ? [0] : [0, 1], [0], [0],
    [], ['wechat'], null, '芝加哥', null, null, 12 - i, null, null, [],
  ]);
  const checked: string[] = [];
  let directoryReads = 0;
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/v1/me') return route.fulfill({ json: { id: 'synthetic', email: 'synthetic@example.invalid' } });
    if (path === '/api/v1/auth/csrf') return route.fulfill({ json: { csrf_token: 'synthetic' } });
    if (path === '/api/v1/ai/browser-model') {
      return route.fulfill({ json: { enabled: true, model: 'synthetic-model', context_window: 32000, max_tokens: 1000 } });
    }
    if (path === '/api/v1/directory') {
      directoryReads++;
      return route.fulfill({ json: {
        columns, profiles, tags: [['t1', '同事'], ['t2', '喜欢摄影']],
        companies: [['c1', '合成公司']], schools: [['s1', '合成大学']],
      } });
    }
    const match = path.match(/^\/api\/v1\/profiles\/([^/]+)\/messages$/);
    if (match) {
      checked.push(match[1]);
      return route.fulfill({ json: [{ id: `message-${match[1]}`, conversation_id: 'room', text: '有摄影记录' }] });
    }
    if (path !== '/api/v1/ai/chat/completions') return route.fulfill({ json: [] });
    const { tools, messages } = route.request().postDataJSON();
    expect(tools.map((tool: { function: { name: string } }) => tool.function.name)).toEqual(['monica_cli']);
    const last = messages.at(-1);
    if (last.role === 'user') {
      return route.fulfill(scriptCall('filtered-batch', `
        const data = cli("directory", {query:"li", tags:["同事","喜欢摄影"], company:"合成公司", school:"合成大学", provider:"wechat", sort:"messages"});
        const facets = cli("directory_facets", {kind:"tags", query:"摄影"});
        const seen = cli("batch", data.rows.map(p => ({command:"read_messages", args:{profile_id:p.profile_id}})));
        return {names: data.rows.map(p=>p.name), facets: facets.rows, links: seen.map(r=>r.messages[0].link), fetch: typeof fetch};
      `));
    }
    expect(JSON.parse(last.content)).toEqual({
      names: ['李丽', '李明'], facets: [{ id: 't2', name: '喜欢摄影' }],
      links: ids.slice(0, 2).map(id => `/messages?conversation=room&message=message-${id}`), fetch: 'undefined',
    });
    return route.fulfill(completion({ role: 'assistant', content: '找到两位符合筛选条件的联系人。' }, 'stop'));
  });
  await page.goto('/ask');
  await page.getByLabel('问题', { exact: true }).fill('找喜欢摄影的同事');
  await page.getByRole('button', { name: '提问', exact: true }).click();
  await expect(page.getByText('找到两位符合筛选条件的联系人。')).toBeVisible();
  expect(directoryReads).toBe(1);
  expect(checked.sort()).toEqual(ids.slice(0, 2));
});

test('switching conversations allows three independent in-flight prompts', async ({ page }) => {
  const questions = ['会话甲的问题', '会话乙的问题', '会话丙的问题'];
  const arrived: string[] = [];
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/v1/me') return route.fulfill({ json: { id: 'synthetic', email: 'synthetic@example.invalid' } });
    if (path === '/api/v1/auth/csrf') return route.fulfill({ json: { csrf_token: 'synthetic' } });
    if (path === '/api/v1/ai/browser-model') {
      return route.fulfill({ json: { enabled: true, model: 'synthetic-model', context_window: 32000, max_tokens: 1000 } });
    }
    if (path !== '/api/v1/ai/chat/completions') return route.fulfill({ json: [] });
    const payload = route.request().postDataJSON();
    const question = questions.find(q => JSON.stringify(payload.messages.at(-1).content).includes(q))!;
    expect(payload.messages.filter((message: { role: string }) => message.role === 'user')).toHaveLength(1);
    arrived.push(question);
    await gate;
    return route.fulfill(completion({ role: 'assistant', content: `${question}已经答复。` }, 'stop'));
  });
  await page.goto('/ask');
  for (const [index, question] of questions.entries()) {
    if (index) await page.getByRole('button', { name: '新会话', exact: true }).filter({ has: page.locator('svg') }).click();
    await page.getByLabel('问题', { exact: true }).fill(question);
    await page.getByRole('button', { name: '提问', exact: true }).click();
    await expect.poll(() => arrived.length).toBe(index + 1);
  }
  expect(arrived).toEqual(questions);
  release();
  for (const question of questions) {
    const picker = page.getByLabel('选择会话');
    if (await picker.isVisible()) await picker.selectOption({ label: question });
    else await page.getByRole('complementary', { name: '会话' }).getByRole('button', { name: question, exact: true }).click();
    await expect(page.getByText(`${question}已经答复。`, { exact: true })).toBeVisible();
  }
});

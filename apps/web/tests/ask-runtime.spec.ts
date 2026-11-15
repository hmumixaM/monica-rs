import { test, expect } from '@playwright/test';
test('PI browser loop executes tools, persists IndexedDB sessions, and restores after reload', async ({page}) => {
 const errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));let modelCalls=0;let toolsRead=0;let messagesRead=0;let includedGroupSpeech=false;let submittedEvidence='';
 const profile='00000000-0000-4000-8000-000000000042';
 const native='00000000-0000-4000-8000-000000000043';const legacy='00000000-0000-4000-8000-000000000044';
 await page.route('**/api/**',async route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/api/v1/me')return route.fulfill({json:{id:'synthetic',email:'synthetic@example.invalid'}});
  if(path==='/api/v1/auth/csrf')return route.fulfill({json:{csrf_token:'synthetic'}});
  if(path==='/api/v1/ai/browser-model')return route.fulfill({json:{enabled:true,model:'synthetic-model',context_window:32000,max_tokens:1000}});
  if(path==='/api/v1/profiles'){toolsRead++;return route.fulfill({json:[{id:profile,display_name:'合成联系人'}]});}
  if(path===`/api/v1/profiles/${profile}/messages`){messagesRead++;includedGroupSpeech=new URL(route.request().url()).searchParams.get('include_group_speech')==='true';return route.fulfill({json:[
   {id:native,conversation_id:'native-room',conversation_type:'group',text:'Native message',occurred_at:'2026-01-01T12:00:00Z',evidence_id:native,evidence_revision:7,citation:`/evidence/${native}?revision=7`},
   {id:legacy,conversation_id:'legacy-room',text:'Legacy message',occurred_at:'2026-01-01T11:00:00Z',citation:`/evidence/${legacy}?revision=3`},
  ]});}
  if(path==='/api/v1/ai/chat/completions'){
   const body=route.request().postDataJSON();modelCalls++;
   const toolReplies=body.messages.filter((m:{role:string})=>m.role==='tool');
   let chunks;
   if(toolReplies.length>=2){submittedEvidence=JSON.stringify(toolReplies.at(-1));chunks=[{choices:[{index:0,delta:{role:'assistant',content:`已找到[合成联系人](/profiles/${profile})。`},finish_reason:null}]},{choices:[{index:0,delta:{},finish_reason:'stop'}]}];}
   else {const tool=toolReplies.length?{name:'search_evidence',arguments:JSON.stringify({profile_id:profile})}:{name:'get_profiles',arguments:'{"query":"合成"}'};chunks=[{choices:[{index:0,delta:{role:'assistant',tool_calls:[{index:0,id:`synthetic-tool-${toolReplies.length}`,type:'function',function:tool}]},finish_reason:null}]},{choices:[{index:0,delta:{},finish_reason:'tool_calls'}]}];}
   return route.fulfill({contentType:'text/event-stream',body:chunks.map(c=>'data: '+JSON.stringify({id:'synthetic-completion',object:'chat.completion.chunk',created:1,model:'synthetic-model',...c})+'\n\n').join('')+'data: [DONE]\n\n'});
  }
  return route.fulfill({json:[]});
 });
 await page.goto('/ask');await expect(page.getByRole('heading',{name:'问 Monica',exact:true})).toBeVisible();
 await page.getByLabel('问题',{exact:true}).fill('找一下合成联系人');await page.getByRole('button',{name:'提问',exact:true}).click();
 await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();expect(modelCalls).toBe(3);expect(toolsRead).toBe(1);expect(messagesRead).toBe(1);expect(includedGroupSpeech).toBeTruthy();expect(submittedEvidence).toContain(`/evidence/${native}?revision=7`);expect(submittedEvidence).toContain(`/evidence/${legacy}?revision=3`);
 await page.reload();await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();expect(modelCalls).toBe(3);
 await page.getByRole('button',{name:'新会话',exact:true}).click();await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toHaveCount(0);
 await page.getByLabel('选择会话').selectOption({label:'找一下合成联系人'});await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();
 await page.setViewportSize({width:390,height:844});await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBeTruthy();
 await page.screenshot({path:'../../.local-validation/frontend-ask-mobile.png',fullPage:true});expect(errors).toEqual([]);
});

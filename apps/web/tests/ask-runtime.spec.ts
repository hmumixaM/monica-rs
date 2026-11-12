import { test, expect } from '@playwright/test';
test('PI browser loop executes tools, persists IndexedDB sessions, and restores after reload', async ({page}) => {
 const errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));let modelCalls=0;let toolsRead=0;
 const profile='00000000-0000-4000-8000-000000000042';
 await page.route('**/api/**',async route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/api/v1/me')return route.fulfill({json:{id:'synthetic',email:'synthetic@example.invalid'}});
  if(path==='/api/v1/auth/csrf')return route.fulfill({json:{csrf_token:'synthetic'}});
  if(path==='/api/v1/ai/browser-model')return route.fulfill({json:{enabled:true,model:'synthetic-model',context_window:32000,max_tokens:1000}});
  if(path==='/api/v1/profiles'){toolsRead++;return route.fulfill({json:[{id:profile,display_name:'合成联系人'}]});}
  if(path==='/api/v1/ai/chat/completions'){
   const body=route.request().postDataJSON();modelCalls++;
   const toolReply=body.messages.some((m:{role:string})=>m.role==='tool');
   const chunks=toolReply ? [{choices:[{index:0,delta:{role:'assistant',content:`已找到[合成联系人](/profiles/${profile})。`},finish_reason:null}]},{choices:[{index:0,delta:{},finish_reason:'stop'}]}]
    :[{choices:[{index:0,delta:{role:'assistant',tool_calls:[{index:0,id:'synthetic-tool',type:'function',function:{name:'get_profiles',arguments:'{"query":"合成"}'}}]},finish_reason:null}]},{choices:[{index:0,delta:{},finish_reason:'tool_calls'}]}];
   return route.fulfill({contentType:'text/event-stream',body:chunks.map(c=>'data: '+JSON.stringify({id:'synthetic-completion',object:'chat.completion.chunk',created:1,model:'synthetic-model',...c})+'\n\n').join('')+'data: [DONE]\n\n'});
  }
  return route.fulfill({json:[]});
 });
 await page.goto('/ask');await expect(page.getByRole('heading',{name:'问 Monica',exact:true})).toBeVisible();
 await page.getByLabel('问题',{exact:true}).fill('找一下合成联系人');await page.getByRole('button',{name:'提问',exact:true}).click();
 await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();expect(modelCalls).toBe(2);expect(toolsRead).toBe(1);
 await page.reload();await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();expect(modelCalls).toBe(2);
 await page.getByRole('button',{name:'新会话',exact:true}).click();await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toHaveCount(0);
 await page.getByLabel('选择会话').selectOption({label:'找一下合成联系人'});await expect(page.getByRole('link',{name:'合成联系人',exact:true})).toBeVisible();
 await page.setViewportSize({width:390,height:844});await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBeTruthy();
 await page.screenshot({path:'../../.local-validation/frontend-ask-mobile.png',fullPage:true});expect(errors).toEqual([]);
});

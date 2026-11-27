import {expect,test} from '@playwright/test';

for(const width of [390,1440])test(`imported months and explicit merge remain usable at ${width}px`,async({page})=>{
 test.setTimeout(60000);
 await page.setViewportSize({width,height:844});
 const source='00000000-0000-4000-8000-000000000001',target='00000000-0000-4000-8000-000000000002';
 const person=(id:string)=>({id,profile_type:'person',display_name:id===source?'原人物':'保留人物',fields:[],identities:[],structured_facts:[]});
 let posted:Record<string,unknown>|undefined;
 const errors:string[]=[];page.on('pageerror',error=>errors.push(error.message));
 await page.route('**/api/v1/**',async route=>{
  const url=new URL(route.request().url()),path=url.pathname;
  if(path==='/api/v1/me')return route.fulfill({json:{id:'synthetic',email:'fixture@example.invalid'}});
  if(path==='/api/v1/auth/csrf')return route.fulfill({json:{csrf_token:'synthetic'}});
  if(path==='/api/v1/link-review/contacts')return route.fulfill({json:[{profile_id:target,display_name:'保留人物'}]});
  if(path.endsWith('/merge-preview'))return route.fulfill({json:{source:{...person(source),generation:7},target:{...person(target),generation:8},counts:{confirmed_faces:10,suggested_faces:13,media_links:1},conflicts:[],can_merge:true}});
  if(path.endsWith('/merge')){posted=route.request().postDataJSON();return route.fulfill({json:{archived:true,target_id:target}});}
  if(path.endsWith('/summary'))return route.fulfill({json:{profile:person(path.includes(target)?target:source),overview:{},has_posts:false}});
  if(path===`/api/v1/profiles/${source}`||path===`/api/v1/profiles/${target}`)return route.fulfill({json:person(path.split('/').at(-1)!)});
  if(path.endsWith('/chat-months'))return route.fulfill({json:{months:[{month:'2026-01-01',sent:2,received:1,total:3,system:1,unknown:0,records:4}],sent:2,received:1,total:3,system:1,unknown:0,records:4,active_months:1,timezone:'America/Chicago',first:'2026-01-01T12:00:00Z',last:'2026-01-02T12:00:00Z',peak:{month:'2026-01-01',total:3}}});
  if(path.endsWith('/ribbon'))return route.fulfill({json:{timezone:'America/Chicago',months:[]}});
  return route.fulfill({json:[]});
 });
 await page.goto(`/profiles/${source}?tab=messages`);
 await expect(page.getByText('2026年1月',{exact:true})).toBeVisible({timeout:15000});
 await expect(page.getByRole('link',{name:'2026-01，3 条，我发送 2，对方发送 1',exact:true})).toBeVisible();
 await expect(page.getByText('系统消息',{exact:true})).toBeVisible();
 expect(await page.locator('body').innerText()).not.toContain('NaN');
 if(width<768){await page.getByRole('button',{name:'更多操作'}).click();await page.getByRole('menuitem',{name:'编辑资料'}).click();}
 else await page.getByRole('button',{name:'编辑',exact:true}).click();
 await page.getByRole('button',{name:'合并到其他人物'}).click();
 const dialog=page.getByRole('dialog',{name:'合并同一个人的资料'});
 await dialog.getByPlaceholder('输入姓名').fill('保留');
 await expect(dialog.getByRole('option',{name:'保留人物'})).toHaveCount(1);
 await dialog.getByLabel('保留这份人物资料').selectOption(target);
 await expect(dialog.getByText('已确认人脸：10',{exact:true})).toBeVisible();
 await dialog.getByLabel('我确认这两份资料是同一个人').check();
 await dialog.getByRole('button',{name:'合并并保留历史'}).click();
 await expect(page).toHaveURL(new RegExp(`/profiles/${target}$`));
 expect(posted).toMatchObject({target_id:target,source_generation:7,target_generation:8});
 expect(posted?.operation_id).toMatch(/^[0-9a-f-]{36}$/);
 expect(errors).toEqual([]);
 await page.screenshot({path:`../../.local-validation/linked-bugs-${width}.png`,fullPage:true});
});

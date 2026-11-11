import {test,expect} from '@playwright/test';
const directory={columns:['id','type','name','summary','avatar_media_id','tag_ids','company_ids','school_ids','aliases','providers','headline','city','birthday_md','last_contact_at','direct_message_count','member_count','conversation_id','group_names'],profiles:[['00000000-0000-4000-8000-000000000001','person','测试联系人',null,null,[],[],[],[],[],null,null,null,null,0,null,null,[]]],tags:[],companies:[],schools:[]};
for(const width of [1440,390])test(`router and search worker on ${width}px`,async({page})=>{
 await page.setViewportSize({width,height:900});
 const errors:string[]=[];page.on('pageerror',error=>errors.push(error.message));
 // Synthetic HTTP fixture verifies frontend hosting/router behavior only, not backend parity.
 await page.route('**/api/**',async route=>{const path=new URL(route.request().url()).pathname;
  if(path==='/api/v1/me')return route.fulfill({json:{id:'synthetic',email:'fixture@example.invalid'}});
  if(path==='/api/v1/directory')return route.fulfill({json:directory,headers:{ETag:'"synthetic-1"'}});
  if(path==='/api/v1/tags')return route.fulfill({json:[]});
  return route.fulfill({status:404,json:{detail:'Unsupported synthetic fixture route'}});
 });
 await page.goto('/profiles?sort=name');
 await expect(page.getByRole('heading',{name:/^联系人/})).toBeVisible();
 await expect(page.getByText('测试联系人',{exact:true}).first()).toBeVisible();
 const search=page.getByRole('searchbox',{name:'筛选联系人'});await search.fill('ceshi');
 await expect(page.getByText('测试联系人',{exact:true}).first()).toBeVisible();
 await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBeTruthy();
 await page.screenshot({path:`../../.local-validation/frontend-${width}.png`,fullPage:true});
 await page.reload();await expect(page.getByRole('heading',{name:/^联系人/})).toBeVisible();
 await page.getByText('测试联系人',{exact:true}).first().click();
 await expect(page).toHaveURL(/\/profiles\/00000000-0000-4000-8000-000000000001/);
 await expect(page.getByText('找不到这份资料')).toBeVisible(); // Actual failing API is exposed, never fabricated success.
 await page.goBack();await expect(page.getByRole('heading',{name:/^联系人/})).toBeVisible();
 expect(errors).toEqual([]);
});
test('unauthenticated deep link and local font/login',async({page})=>{
 await page.route('**/api/**',route=>route.fulfill({status:401,json:{detail:'Authentication required'}}));
 await page.goto('/profiles/00000000-0000-4000-8000-000000000002?tab=messages');
 await expect(page).toHaveURL(/\/login$/);await expect(page.getByRole('heading',{name:'欢迎回来'})).toBeVisible();
 await page.getByLabel('邮箱').fill('test@example.invalid');await page.getByLabel('密码',{exact:true}).fill('invalid-synthetic-password');await page.getByRole('button',{name:'登录',exact:true}).click();
 await expect(page.getByRole('alert')).toContainText('Authentication required');
});

test('failed directory is an error instead of an empty address book',async({page})=>{
 await page.route('**/api/**',route=>new URL(route.request().url()).pathname==='/api/v1/me'?route.fulfill({json:{id:'synthetic'}}):route.fulfill({status:404,json:{detail:'Unavailable fixture endpoint'}}));
 await page.goto('/profiles');await expect(page.getByText('联系人读取失败，请重试。')).toBeVisible();await expect(page.getByText('这里还没有人')).toHaveCount(0);
});

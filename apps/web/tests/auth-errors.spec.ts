import {test,expect} from '@playwright/test';

test('login renders validation errors and allows another attempt',async({page})=>{
 const errors:string[]=[];page.on('pageerror',error=>errors.push(error.message));
 await page.route('**/api/**',route=>{
  const path=new URL(route.request().url()).pathname;
  if(path.endsWith('/csrf'))return route.fulfill({json:{csrf_token:'synthetic'}});
  if(path.endsWith('/login'))return route.fulfill({status:422,json:{detail:[{type:'string_too_short',loc:['body','password'],msg:'Password must contain 12 characters'}]}});
  return route.fulfill({status:401,json:{detail:'login required'}});
 });
 await page.goto('/login');await page.getByLabel('邮箱').fill('test@example.invalid');await page.getByLabel('密码',{exact:true}).fill('short');
 await page.getByRole('button',{name:'登录',exact:true}).click();
 await expect(page.getByRole('alert')).toContainText('Password must contain 12 characters');
 await expect(page.getByRole('button',{name:'登录',exact:true})).toBeEnabled();
 expect(errors).toEqual([]);
});

for(const failedPath of ['/api/v1/auth/csrf','/api/v1/auth/login'])test(`login retries after network failure at ${failedPath}`,async({page})=>{
 let fail=true;
 await page.route('**/api/**',route=>{
  const path=new URL(route.request().url()).pathname;
  if(path===failedPath&&fail){fail=false;return route.abort('connectionfailed');}
  if(path.endsWith('/csrf'))return route.fulfill({json:{csrf_token:'synthetic'}});
  return route.fulfill({status:401,json:{detail:'invalid credentials'}});
 });
 await page.goto('/login');await page.getByLabel('邮箱').fill('test@example.invalid');await page.getByLabel('密码',{exact:true}).fill('synthetic-password');
 const submit=page.getByRole('button',{name:'登录',exact:true});await submit.click();
 await expect(page.getByRole('alert')).toContainText('检查网络后重试');await expect(submit).toBeEnabled();
 await submit.click();await expect(page.getByRole('alert')).toContainText('invalid credentials');await expect(submit).toBeEnabled();
});

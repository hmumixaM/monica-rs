import {test,expect} from '@playwright/test';
async function fixture(page:import('@playwright/test').Page, failure=false){
 await page.addInitScript(({failure})=>{
  let status={phase:'running',url:'http://127.0.0.1:17494',paused:false,draining:false,active_jobs:0,error:null,data_directory:'/tmp/synthetic-monica-rust',adapters_enabled:false,ai_enabled:false};
  (window as unknown as {__TAURI_INTERNALS__:unknown}).__TAURI_INTERNALS__={invoke:async(command:string,args:{action?:string})=>{
   if(command==='runtime_status')return status;
   if(command==='collector_status')return {instagram:{authenticated:false,login_running:false,enabled:false,state:'needs_login',error:null},linkedin:{authenticated:false,login_running:false,enabled:false,state:'needs_login',error:null}};
   if(command==='native_status')return {apple_contacts:{enabled:false,busy:false,error:null,delivered:0},apple_messages:{enabled:false,busy:false,error:null,delivered:0}};
   if(command==='autostart_enabled')return false;
   if(command==='runtime_action'){if(failure)throw '后台控制未完成（HTTP 503）';status={...status,paused:args.action==='pause'};return status;}
   if(command==='create_account')throw 'Account already initialized';
   return null;
  }};
 },{failure});await page.goto('/');
}
test('settings follows successful native state and retains explicit adapter limits',async({page})=>{
 await fixture(page);await expect(page.getByRole('heading',{name:'运行中',exact:true})).toBeVisible();
 await page.getByRole('button',{name:'暂停后台任务',exact:true}).click();await expect(page.getByRole('heading',{name:'后台任务已暂停'})).toBeVisible();
 await page.getByRole('button',{name:'恢复后台任务',exact:true}).click();await expect(page.getByRole('heading',{name:'运行中',exact:true})).toBeVisible();
 await expect(page.getByText(/默认不启用采集器或模型调用/)).toBeVisible();
});
test('failed control is shown without claiming pause',async({page})=>{
 await fixture(page,true);await page.getByRole('button',{name:'暂停后台任务',exact:true}).click();
 await expect(page.getByRole('alert')).toContainText('HTTP 503');await expect(page.getByRole('heading',{name:'运行中',exact:true})).toBeVisible();
});
test('bootstrap failure remains visible and password is not presented as saved',async({page})=>{
 await fixture(page);await page.getByRole('button',{name:'创建首次使用账号'}).click();await page.getByLabel('邮箱').fill('fixture@example.invalid');await page.getByLabel('密码',{exact:true}).fill('synthetic-password-42');await page.getByRole('button',{name:'创建账号',exact:true}).click();
 await expect(page.getByRole('alert')).toContainText('Account already initialized');await expect(page.getByText('账号已创建，可以在浏览器登录。')).toHaveCount(0);
});

test('sources expose explicit login and refuse collection before authentication',async({page})=>{
 await fixture(page);
 for(const provider of ['Instagram','LinkedIn']){const card=page.locator('article').filter({has:page.getByRole('heading',{name:provider,exact:true})});await expect(card.getByRole('button',{name:`连接 / 登录 ${provider}`})).toBeVisible();await expect(card.getByRole('button',{name:'启用采集'})).toBeDisabled();await expect(card.getByRole('button',{name:'查看状态'})).toBeEnabled();}
 await expect(page.getByLabel('登录 Mac 后自动启动')).not.toBeChecked();
});

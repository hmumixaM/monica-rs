import {test,expect} from '@playwright/test';
test.skip(!process.env.MONICA_REAL_API,'Run via scripts/frontend-smoke.py against a fresh synthetic database');
test('real Rust login, create profile, directory search and deep link',async({page,request})=>{
 const email='browser-fixture@example.invalid',password='browser-synthetic-password-42';
 const bootstrap=await request.post('/api/v1/auth/bootstrap',{data:{email,password}});expect(bootstrap.ok()).toBeTruthy();
 const errors:string[]=[];page.on('pageerror',error=>errors.push(error.message));
 await page.goto('/login');await page.getByLabel('邮箱').fill(email);await page.getByLabel('密码',{exact:true}).fill(password);await page.getByRole('button',{name:'登录',exact:true}).click();await expect(page).toHaveURL(/\/dashboard$/);
 await page.goto('/profiles');await expect(page.getByRole('heading',{name:/^联系人/})).toBeVisible();await page.getByRole('button',{name:'新建',exact:true}).click();await page.getByLabel('名称',{exact:true}).fill('Rust 测试联系人');await page.getByRole('textbox',{name:/^简介/}).fill('由隔离浏览器测试创建的虚构联系人');await page.getByRole('button',{name:'创建',exact:true}).click();await expect(page).toHaveURL(/\/profiles\/[0-9a-f-]{36}$/);
 await expect(page.getByRole('heading',{level:1,name:'Rust 测试联系人'})).toBeVisible();
 const profileUrl=page.url();await page.reload();await expect(page.getByRole('heading',{level:1,name:'Rust 测试联系人'})).toBeVisible();
 await page.goto('/profiles');await page.getByRole('searchbox',{name:'筛选联系人'}).fill('ceshi');await expect(page.getByText('Rust 测试联系人',{exact:true}).first()).toBeVisible();await page.getByText('Rust 测试联系人',{exact:true}).first().click();await expect(page).toHaveURL(profileUrl);
 await page.screenshot({path:'../../.local-validation/frontend-real-profile.png',fullPage:true});
 expect(errors).toEqual([]);
});

import {defineConfig} from '@playwright/test';
export default defineConfig({testDir:'tests',workers:1,use:{channel:'chrome',headless:true,baseURL:'http://127.0.0.1:4174'},webServer:{command:'npm run dev -- --port 4174',url:'http://127.0.0.1:4174',reuseExistingServer:false},reporter:'list',outputDir:'../../.local-validation/native-settings-results'});

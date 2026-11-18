import {defineConfig} from '@playwright/test';
const port=process.env.MONICA_NATIVE_TEST_PORT || '4174';
export default defineConfig({testDir:'tests',workers:1,use:{channel:'chrome',headless:true,baseURL:`http://127.0.0.1:${port}`},webServer:{command:`npm run dev -- --port ${port}`,url:`http://127.0.0.1:${port}`,reuseExistingServer:false},reporter:'list',outputDir:'../../.local-validation/native-settings-results'});

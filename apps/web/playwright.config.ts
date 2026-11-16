import {defineConfig} from '@playwright/test';
const mobileWebKit = process.env.MONICA_WEB_TEST_BROWSER === 'webkit';
export default defineConfig({
  testDir: 'tests', workers: 1, timeout: 30000,
  use: {
    browserName: mobileWebKit ? 'webkit' : 'chromium',
    channel: mobileWebKit ? undefined : 'chrome',
    headless: true,
    baseURL: process.env.MONICA_WEB_TEST_URL || 'http://127.0.0.1:4173',
    ...(mobileWebKit ? {viewport: {width: 390, height: 844}, isMobile: true, hasTouch: true, serviceWorkers: 'block' as const} : {}),
  },
  webServer: process.env.MONICA_WEB_TEST_URL ? undefined : {
    command: 'npm run preview -- --port 4173 --strictPort', url: 'http://127.0.0.1:4173', reuseExistingServer: false,
  },
  reporter: 'list', outputDir: '../../.local-validation/frontend-results',
});

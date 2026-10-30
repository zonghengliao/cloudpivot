import { defineConfig, devices } from '@playwright/test';

/**
 * Playwright 配置（E4 前端真实交互测试）。
 *
 * - 全部用例打**真实后端**（`E2E_BASE_URL`，默认生产 `http://172.18.10.144`），不 mock 任何接口；
 * - `workers: 1`：用例之间共享同一份生产数据（看板列计数、待办列表），串行才稳定；
 * - `screenshot: only-on-failure` + `trace: retain-on-failure`：失败必留现场。
 */
const BASE_URL = process.env.E2E_BASE_URL ?? 'http://172.18.10.144';

export default defineConfig({
  testDir: './e2e',
  // 拖拽 / SVG 画布交互比普通点击慢，单条用例给足时间
  timeout: 120_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI
    ? [['list'], ['html', { open: 'never', outputFolder: 'e2e-report' }], ['junit', { outputFile: 'e2e-report/junit.xml' }]]
    : [['list'], ['html', { open: 'never', outputFolder: 'e2e-report' }]],
  use: {
    baseURL: BASE_URL,
    viewport: { width: 1600, height: 1000 },
    locale: 'zh-CN',
    timezoneId: 'Asia/Shanghai',
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
    video: 'off',
    actionTimeout: 15_000,
    navigationTimeout: 30_000,
    ignoreHTTPSErrors: true,
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'], viewport: { width: 1600, height: 1000 } },
    },
  ],
});

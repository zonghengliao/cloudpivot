import { defineConfig, devices } from '@playwright/test';
import { assertIsolatedTarget } from './e2e/target-guard.mjs';

/**
 * Playwright 配置（E4 前端真实交互测试）。
 *
 * - **默认打隔离实例**（`http://127.0.0.1:5189`，用
 *   `bash scripts/verify/e2e_isolated_instance.sh` 起：prod 库副本 + profile=test 后端 18089
 *   + vite 5189 反代）。历史教训：本文件 2026-10-31 之前默认 `http://172.18.10.144`（prod），
 *   任何人一句 `pnpm test:e2e` 就在生产上造数 —— 11:34 那批 M-W1A-004 夹具虽出自 mac 浏览器，
 *   但「默认指向 prod」这个坑是同源的，已改成默认隔离 + 运行期硬判据；
 * - `assertIsolatedTarget` 是**硬判据**：目标不是回环隔离端口就直接抛错，配置加载期即红；
 *   线上验收确需打 prod 时走 `E2E_ALLOW_PROD=1 E2E_PROD_REASON='<谁批准/为什么>'`，
 *   并在跑完执行 `e2e/.auth/prod-target.flag` 要求的后置清理检查；
 * - `workers: 1`：用例之间共享同一份数据（看板列计数、待办列表），串行才稳定；
 * - `screenshot: only-on-failure` + `trace: retain-on-failure`：失败必留现场。
 */
const BASE_URL = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:5189';

// 硬判据：非隔离实例直接抛错（线上验收请带 E2E_ALLOW_PROD=1 E2E_PROD_REASON=...）
assertIsolatedTarget(BASE_URL, { projectRoot: process.cwd() });

export default defineConfig({
  testDir: './e2e',
  // 登录限流（10 次 / 60 秒 / 账号+IP）：全局只登录一次，令牌写进 storageState 复用
  globalSetup: './e2e/global-setup.ts',
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
    // 复用 globalSetup 存的登录态（见 e2e/global-setup.ts 的限流说明）
    storageState: 'e2e/.auth/state.json',
    viewport: { width: 1600, height: 1000 },
    locale: 'zh-CN',
    timezoneId: 'Asia/Shanghai',
    screenshot: process.env.E2E_SCREENSHOT === 'on' ? 'on' : 'only-on-failure',
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

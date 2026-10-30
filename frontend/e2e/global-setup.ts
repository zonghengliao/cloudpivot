/**
 * 全局准备：**只登录一次**，把登录态存成 Playwright 的 storageState 给所有用例复用。
 *
 * 为什么必须这么做：后端 `/api/system/auth/login` 有按 (账号, IP) 的限流
 * （10 次 / 60 秒，见 `backend/src/shared/constants.rs`
 * 的 `LOGIN_RATE_MAX_ATTEMPTS` / `LOGIN_RATE_WINDOW_SECS`）。每个用例都自己登一次的话，
 * 一轮 5 条用例 × (UI 登录 + API 客户端) 就会把自己限流成 429。
 *
 * 于是：这里登录 1 次 → 写 `.auth/state.json`（localStorage 里的 access/refresh token）
 * 与 `.auth/token.txt`（API 客户端直接用），用例里不再重复登录；
 * 只有「登录流程」那条用例（01）会显式用干净上下文再登一次。
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { request } from '@playwright/test';

const BASE_URL = process.env.E2E_BASE_URL ?? 'http://172.18.10.144';
const ACCOUNT = {
  username: process.env.E2E_USERNAME ?? 'laoliu_test',
  password: process.env.E2E_PASSWORD ?? 'Test@12345',
};
export const AUTH_DIR = resolve(process.cwd(), 'e2e/.auth');
export const STATE_PATH = resolve(AUTH_DIR, 'state.json');
export const TOKEN_PATH = resolve(AUTH_DIR, 'token.txt');

export default async function globalSetup(): Promise<void> {
  const ctx = await request.newContext({ baseURL: BASE_URL });
  const res = await ctx.post('/api/system/auth/login', { data: ACCOUNT });
  const body = await res.json().catch(() => ({}));
  if (res.status() !== 200 || body?.code !== 0) {
    throw new Error(`globalSetup 登录失败：${res.status()} / ${JSON.stringify(body).slice(0, 200)}`);
  }
  const { access_token: accessToken, refresh_token: refreshToken } = body.data;

  mkdirSync(dirname(STATE_PATH), { recursive: true });
  writeFileSync(
    STATE_PATH,
    JSON.stringify(
      {
        cookies: [],
        origins: [
          {
            origin: BASE_URL,
            localStorage: [
              { name: 'x_access_token', value: accessToken },
              { name: 'x_refresh_token', value: refreshToken },
              // E10「新功能引导」：没有这个键时首屏会浮一层 role=dialog 的引导卡，
              // 它会 intercept pointer events，把所有「首次进入后的第一次点击」挡掉
              // （流程 4 的「自动布局」就是这么超时的，反向对照已验证：去掉这行立刻复红）。
              // 测试账号视为「已看过」。
              { name: 'cp-onboarding-done', value: '1' },
            ],
          },
        ],
      },
      null,
      2,
    ),
  );
  writeFileSync(TOKEN_PATH, accessToken);
  await ctx.dispose();
}

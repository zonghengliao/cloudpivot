/**
 * E2E 公共支撑：地址 / 账号 / 登录 / 真实后端 API 预置与清理。
 *
 * 红线：**不打桩、不 mock**。所有 API 都是真打生产后端（同样的 `http://<host>/api/...`），
 * 页面里的每个断言都来自浏览器真实点出来的 DOM / 网络结果。
 * API 只用于两件事：① 给交互测试预置数据（如看板要拖的卡片）；② 用例结束后的清理。
 */
import { expect, request, type APIRequestContext, type Page } from '@playwright/test';

/** 被测站点（CI 用 `E2E_BASE_URL` 覆盖） */
export const BASE_URL = process.env.E2E_BASE_URL ?? 'http://172.18.10.144';
/** 测试账号（CI 用 `E2E_USERNAME` / `E2E_PASSWORD` 覆盖） */
export const ACCOUNT = {
  username: process.env.E2E_USERNAME ?? 'laoliu_test',
  password: process.env.E2E_PASSWORD ?? 'Test@12345',
};
/** 本次运行的数据戳：所有测试数据的名字里都带它，便于定位与清理 */
export const RUN = `e2e${Date.now().toString(36)}`;

/**
 * UI 登录：真实填表 + 点登录按钮。
 *
 * 生产后端对连续登录有节流，且登录请求偶发比 30s 慢；这里做「失败重来」的重试，
 * 但不绕过真实表单 —— 每次重试都是重新打开 /login 再点一次。
 */
export async function login(page: Page): Promise<void> {
  let lastError = '';
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    await page.goto('/login');
    await page.waitForSelector("input[placeholder='请输入用户名']");
    await page.fill("input[placeholder='请输入用户名']", ACCOUNT.username);
    await page.fill("input[placeholder='请输入密码']", ACCOUNT.password);
    await page.click('button.t-button--theme-primary');
    try {
      await page.waitForURL(/\/(dashboard|change-password)/, { timeout: 30_000 });
      return;
    } catch {
      lastError = (await page.locator('.t-alert').first().innerText().catch(() => '')) || '未跳转到工作台';
      await page.waitForTimeout(1500 * attempt);
    }
  }
  throw new Error(`登录失败（已重试 3 次）：${lastError}`);
}

/** TDesign 消息条文案（成功 / 失败提示都在这里） */
export function toast(page: Page, text: string | RegExp) {
  return page.locator('.t-message').filter({ hasText: text }).first();
}

/** 真实后端 API 客户端（登录态复用测试账号） */
export interface Api {
  ctx: APIRequestContext;
  createTask(body: Record<string, unknown>): Promise<{ id: string; task_no: string }>;
  deleteTask(id: string): Promise<void>;
  createFlowDef(body: Record<string, unknown>): Promise<{ id: string; version_id: string }>;
  saveFlowGraph(vid: string, nodes: unknown[], edges: unknown[]): Promise<void>;
  publishFlowDef(id: string): Promise<void>;
  deleteFlowDef(id: string): Promise<void>;
  createInstance(defId: string, title: string): Promise<{ instance_id: string; instance_status: string }>;
  getInstance(id: string): Promise<{ instance: { id: string; status: string } }>;
  /** 撤回在跑的实例（清理用：有在跑实例时流程定义删不掉） */
  withdrawInstance(id: string, reason?: string): Promise<void>;
  dispose(): Promise<void>;
}

/** 建一个带登录令牌的 API 客户端 */
export async function newApi(): Promise<Api> {
  const ctx = await request.newContext({ baseURL: BASE_URL });
  const res = await ctx.post('/api/system/auth/login', { data: ACCOUNT });
  expect(res.status(), 'API 登录应返回 200').toBe(200);
  const body = await res.json();
  const token: string = body?.data?.access_token;
  expect(token, 'API 登录应拿到 access_token').toBeTruthy();
  const headers = { Authorization: `Bearer ${token}` };

  /** 统一拆包：非 0 业务码直接抛出后端 msg，失败原因一眼可见 */
  const call = async (method: 'get' | 'post' | 'put' | 'delete', url: string, data?: unknown) => {
    const res = await ctx[method](url, { headers, data });
    const json = await res.json().catch(() => ({ code: -1, msg: `<非 JSON 响应 ${res.status()}>` }));
    if (json.code !== 0) {
      throw new Error(`API ${method.toUpperCase()} ${url} 失败：${res.status()} / ${json.code} / ${json.msg}`);
    }
    return json.data;
  };

  return {
    ctx,
    createTask: (body) => call('post', '/api/business/task', body),
    deleteTask: async (id) => {
      await call('delete', `/api/business/task/${id}`);
    },
    createFlowDef: (body) => call('post', '/api/business/flow/def', body),
    saveFlowGraph: async (vid, nodes, edges) => {
      await call('put', `/api/business/flow/version/${vid}/graph`, { nodes, edges });
    },
    publishFlowDef: async (id) => {
      await call('post', `/api/business/flow/def/${id}/publish`);
    },
    deleteFlowDef: async (id) => {
      await call('delete', `/api/business/flow/def/${id}`);
    },
    createInstance: (defId, title) => call('post', '/api/business/flow/instance', { def_id: defId, title }),
    getInstance: async (id) => call('get', `/api/business/flow/instance/${id}`),
    withdrawInstance: async (id, reason) => {
      await call('post', `/api/business/flow/instance/${id}/withdraw`, { reason: reason ?? 'E2E 用例清理' });
    },
    dispose: () => ctx.dispose(),
  };
}

/** 取元素中心点（拖拽 / 连线都要精确坐标） */
export async function centerOf(locator: import('@playwright/test').Locator): Promise<{ x: number; y: number }> {
  const box = await locator.boundingBox();
  if (!box) {
    throw new Error('元素不可见，取不到坐标');
  }
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

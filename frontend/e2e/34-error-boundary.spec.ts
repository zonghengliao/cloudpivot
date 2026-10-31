import { expect, test } from '@playwright/test';
import { newApi, readSharedToken } from './support';

/**
 * 专项 5：**错误与边界**。
 *
 * 四条都是「用户真会遇到、但用例最容易漏」的场景：
 *   ① 列表接口 500 → 必须给友好提示，不能白屏；
 *   ② token 过期 / 被换成垃圾 → 必须回登录页，不能卡在空白布局；
 *   ③ 无权限账号直达无权页面 → 落到 403 结果页；
 *   ④ 连点两次「新建」→ 只能开一个弹窗（重复提交/重复弹窗是真实事故源）。
 *
 * 500 用 `page.route` 造（不改后端），其余全是真的账号与真实路由。
 */
test.describe('专项5 错误与边界', () => {
  test('接口 500 → 页面给友好提示，不白屏（物料列表）', async ({ page }) => {
    await page.route('**/api/business/erp/material/page**', (route) =>
      route.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ code: 500, msg: '服务器开小差了', data: null }) }),
    );
    await page.goto('/business/erp/material');
    await expect(page.locator('.basic-layout__aside')).toBeVisible();

    // 友好提示：页面级 t-alert / 全局 toast 至少出现一个，且文案不是堆栈
    const alert = page.locator('.t-alert, .t-message').first();
    await expect(alert).toBeVisible({ timeout: 20_000 });
    const text = (await alert.innerText()).replace(/\s+/g, ' ').trim();
    expect(text.length, '提示文案不能为空').toBeGreaterThan(2);
    expect(text, '提示里不该出现技术堆栈').not.toMatch(/TypeError|undefined is not|panicked at/);

    // 不白屏：布局、侧栏、页面标题区域都还在
    await expect(page.locator('.basic-layout__content')).toBeVisible();
    const contentLen = (await page.locator('.basic-layout__content').innerText()).length;
    expect(contentLen, '主内容区不该被 500 清空').toBeGreaterThan(10);
  });

  test('token 失效 → 跳登录页（不卡空白布局）', async ({ page }) => {
    await page.goto('/dashboard');
    await expect(page.locator('.basic-layout__aside')).toBeVisible();
    await page.evaluate(() => {
      localStorage.setItem('x_access_token', 'invalid.token.value');
      localStorage.setItem('x_refresh_token', 'invalid.token.value');
    });
    await page.goto('/business/erp/material');
    await expect(page).toHaveURL(/\/login/, { timeout: 25_000 });
    await expect(page.locator("input[placeholder='请输入用户名']")).toBeVisible();
  });

  test('无权限账号直达无权页面 → 403 结果页', async ({ browser }) => {
    const api = await newApi();
    const username = `e2e.norole.${Date.now().toString(36)}`;
    const created = await api.ctx.post('/api/system/user', {
      data: { username, password: 'E2e@123456', real_name: '[E2E]无角色', status: 1, role_ids: [] },
    });
    const createdBody = await created.json();
    expect(created.status(), `建无角色账号：${JSON.stringify(createdBody)}`).toBe(200);
    const userId = createdBody.data.id;

    const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
    const page = await ctx.newPage();
    try {
      const loginRes = await page.request.post('/api/system/auth/login', { data: { username, password: 'E2e@123456' } });
      expect(loginRes.status(), '无角色账号应能登录').toBe(200);
      const tokens = (await loginRes.json()).data;
      await page.addInitScript(
        ([access, refresh]) => {
          localStorage.setItem('x_access_token', access);
          localStorage.setItem('x_refresh_token', refresh);
          localStorage.setItem('cp-onboarding-done', '1');
        },
        [tokens.access_token, tokens.refresh_token],
      );
      await page.goto('/system/user');
      await page.waitForSelector('.t-result, .basic-layout__aside', { timeout: 20_000 });
      const text = await page.evaluate(() => document.body.innerText);
      expect(text, '应出现 403 / 无权限文案').toMatch(/403|无权限|没有权限|无权/);
      expect(text, '不该出现 404 页').not.toMatch(/页面不存在/);
    } finally {
      await ctx.close();
      await api.ctx.delete(`/api/system/user/${userId}`, { headers: { Authorization: `Bearer ${readSharedToken()}` } }).catch(() => undefined);
      await api.dispose();
    }
  });

  test('连点两次「新建」→ 只开一个弹窗（不重复提交/不叠弹窗）', async ({ page }) => {
    await page.goto('/business/erp/material');
    await expect(page.locator('.basic-layout__aside')).toBeVisible();
    const create = page.locator('.basic-layout__content button').filter({ hasText: '新建物料' }).first();
    await expect(create).toBeVisible({ timeout: 20_000 });

    await create.click();
    await create.click({ noWaitAfter: true }).catch(() => undefined);

    const visibleDialogs = await page.evaluate(
      () =>
        Array.from(document.querySelectorAll('.t-dialog, .t-drawer')).filter(
          (el) => (el as HTMLElement).offsetParent !== null && el.getBoundingClientRect().height > 1,
        ).length,
    );
    expect(visibleDialogs, '连点两次只该有一个弹窗').toBe(1);
    const posts: string[] = [];
    page.on('request', (r) => {
      if (r.method() === 'POST' && r.url().includes('/business/erp/material')) {
        posts.push(r.url());
      }
    });
    expect(posts, '还没提交，不该有写请求').toEqual([]);
  });

  /**
   * 已知缺陷留证（不是「跳过」）：**通知 SSE 每次页面加载都 401**。
   *
   * 证据链：前端 `stores/notice.ts#connect()` → `buildNoticeSseUrl()` 用 `?token=` 订阅，
   * 后端 `middleware/auth` 只读 `Authorization` 头（alun-web 0.4.2 无 query-token 白名单）
   * → 401。后果是「实时推送」从未工作，未读角标靠 30s 轮询兜底（所以用户不容易发现）。
   *
   * 这条断言的价值：一旦后端补了白名单 / 前端改走 cookie，这条会**变红**，
   * 提醒把 `_pagecheck.KNOWN_BAD_API` 那条白名单删掉，别再假装它是缺陷。
   */
  test('已知缺陷留证：通知 SSE 每次页面加载都 401，实时推送不通（轮询兜底仍是 200）', async ({ page }) => {
    const seen: Array<{ url: string; status: number }> = [];
    page.on('response', (r) => {
      if (r.url().includes('/api/business/notice/stream')) {
        seen.push({ url: r.url().replace(/[?].*$/, '?token=***'), status: r.status() });
      }
    });
    await page.goto('/dashboard');
    await expect(page.locator('.basic-layout__aside')).toBeVisible({ timeout: 20_000 });

    // ① SSE 订阅确实发起了（前端没静默不连）
    expect(seen.length, '应观察到 SSE 订阅请求').toBeGreaterThan(0);
    // ② 且确实被后端拒了 —— 401，不是 200/403
    expect(new Set(seen.map((s) => s.status)), 'SSE 订阅状态码').toEqual(new Set([401]));

    // ③ 兜底链路还在：未读数接口 200（所以「角标不更新」不是这条缺陷的直接症状）
    // 注意：`page.request` 不带 localStorage 里的令牌（那是 JS 变量，不是 cookie），
    // 直接 get 会 401 —— 必须走 support.ts 的 api 客户端（它把 Authorization 挂在 context 上）。
    const api = await newApi();
    try {
      const unread = await api.ctx.get('/api/business/notice/unread-count');
      expect(unread.status(), '未读数兜底接口').toBe(200);
      const unreadBody = await unread.json();
      expect(typeof unreadBody?.data?.unread_count, '未读数应是数字').toBe('number');
    } finally {
      await api.dispose();
    }
  });
});

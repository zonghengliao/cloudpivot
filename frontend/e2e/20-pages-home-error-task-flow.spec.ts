import { expect, test } from '@playwright/test';
import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';
import { login, toast } from './support';

/**
 * 登录页（/login）+ 三张错误页 + 工作台 + 工作任务四视图 + 项目 + 审批流三页。
 *
 * 登录页单独手写（它必须用**干净上下文**跑，不能复用 globalSetup 的登录态）：
 * 空提交被拦、错账号报错、真账号进工作台 —— 全是真实点出来的。
 * 其余页面走 `_pagerunner` 的 6 项统一口径。
 */

test.describe('页面: 登录（/login）', () => {
  test('① 页面能打开：用户名/密码输入框与登录按钮都在，且没有 JS 异常', async ({ browser }) => {
    const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
    const page = await ctx.newPage();
    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    const res = await page.goto('/login');
    expect(res?.status()).toBe(200);
    await expect(page.locator("input[placeholder='请输入用户名']")).toBeVisible();
    await expect(page.locator("input[placeholder='请输入密码']")).toBeVisible();
    await expect(page.locator('button.t-button--theme-primary')).toHaveCount(1);
    expect(errors).toEqual([]);
    await ctx.close();
  });

  test('⑥ 空提交被拦：给出校验提示、不跳转（校验提示这条现在是绿的）', async ({ browser }) => {
    const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
    const page = await ctx.newPage();
    await page.goto('/login');
    await page.locator('button.t-button--theme-primary').click();
    await expect(page).toHaveURL(/\/login/);
    await expect(page.locator('.t-input__extra, .t-form-item-tips, .t-message').first()).toBeVisible();
    await ctx.close();
  });

  /**
   * 已知产品缺陷（2026-10-31 实测）：登录页 `<t-form @submit="handleSubmit">` 的
   * `handleSubmit` **没有接收并判断 `validateResult`**（`views/login/index.vue`），
   * 于是 TDesign 表单校验虽然会显示红字，但提交照样发出去 —— 空账号密码也会
   * `POST /api/system/auth/login`（靠后端返回错误兜底）。
   * 用 `test.fail()` 标注：修好（不再发请求）后这条会变红提醒摘标注。
   */
  test('★已知缺陷：空提交不该打 /auth/login（当前会打，靠后端兜底）', async ({ browser }) => {
    test.fail(true, '已知缺陷：登录页 handleSubmit 未判断 validateResult，空表单照样 POST /auth/login');
    const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
    const page = await ctx.newPage();
    const posts: string[] = [];
    page.on('request', (r) => {
      if (r.method() === 'POST' && r.url().includes('/auth/login')) {
        posts.push(r.url());
      }
    });
    await page.goto('/login');
    await page.locator('button.t-button--theme-primary').click();
    await page.waitForTimeout(800);
    expect(posts, '空提交不该打 /auth/login').toEqual([]);
    await ctx.close();
  });

  test('⑤ 错账号：报「用户名或密码错误」，停在登录页（用不存在的账号，避免锁真账号）', async ({ browser }) => {
    const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
    const page = await ctx.newPage();
    await page.goto('/login');
    await page.fill("input[placeholder='请输入用户名']", `no_such_user_${Date.now()}`);
    await page.fill("input[placeholder='请输入密码']", 'Wrong@123456');
    await page.locator('button.t-button--theme-primary').click();
    await expect(page.locator('.t-alert, .t-message').filter({ hasText: /用户名或密码错误|用户名不存在/ }).first()).toBeVisible({
      timeout: 15_000,
    });
    await expect(page).toHaveURL(/\/login/);
    await ctx.close();
  });

  test('⑤ 正确账号：真填表登录 → 进工作台，侧栏与顶栏渲染出来', async ({ browser }) => {
    const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
    const page = await ctx.newPage();
    await login(page);
    await expect(page.locator('.basic-layout__aside')).toBeVisible();
    await expect(page).toHaveURL(/\/(dashboard|change-password)/);
    await ctx.close();
  });
});

definePageChecks(
  pages([
    '403 无权限页',
    '404 页面不存在',
    '500 服务异常页',
    '工作台',
    '任务列表',
    '任务看板',
    '甘特图',
    '任务模板',
    '项目',
    '流程定义',
    '发起审批',
    '审批中心（待办）',
  ]),
);

// 登录后的统一提示组件在不在（顶栏通知铃铛是布局级件，登录页没有）
test.describe('布局: 登录后的全局件', () => {
  test('登录态下顶栏铃铛与侧栏菜单渲染（badge 数据来自 /api/system/badge/counts）', async ({ page }) => {
    const badgeCalls: string[] = [];
    page.on('response', (r) => {
      if (r.url().includes('/api/system/badge/counts')) {
        badgeCalls.push(`${r.status()}`);
      }
    });
    await page.goto('/dashboard');
    await expect(page.locator('.basic-layout__aside')).toBeVisible();
    // 侧栏菜单项的类名是 `.side-menu-item__title`（外层是 TDesign 的 t-menu-item / t-submenu，
    // 没有 `.side-menu-item` 这个类 —— 2026-10-31 实测，别按组件名猜类名）
    await expect(page.locator('.side-menu-item__title').first()).toBeVisible();
    await expect(toast(page, /./)).toHaveCount(0);
    expect(badgeCalls.length, '徽标接口应被调用').toBeGreaterThan(0);
    expect(badgeCalls.every((s) => s === '200'), `徽标接口状态：${badgeCalls.join(',')}`).toBe(true);
  });
});

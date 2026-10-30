import { expect, test } from '@playwright/test';
import { login } from './support';

// 这条用例本身就在测「登录」：必须用干净上下文，不能用 globalSetup 存下的登录态
// （否则进 /login 会被路由守卫直接弹到工作台，登录表单永远走不到）
test.use({ storageState: { cookies: [], origins: [] } });

/**
 * 流程 1：登录 → 任务列表
 *
 * 真实路径：打开 /login → 填账号密码 → 点「登 录」→ 落到工作台 →
 * 侧边栏点「工作任务」→ 断言列表真的加载出数据（行数 > 0 + 表头 + 汇总条）。
 */
test.describe('流程1 登录 → 任务列表', () => {
  test('登录后跳工作台，侧边栏进入工作任务并加载列表', async ({ page }) => {
    await login(page);

    // 断言落在工作台：URL + 工作台可见骨架（侧边栏菜单）
    await expect(page).toHaveURL(/\/dashboard$/);
    const taskGroup = page.locator('.t-submenu').filter({ hasText: '工作任务' }).first();
    await expect(taskGroup).toBeVisible();

    // 真实点击侧边栏：「工作任务」是带子菜单的目录，标题本身也是页面（/task）
    const taskTitle = page.locator('.side-menu-item__title').filter({ hasText: '工作任务' }).first();
    await expect(taskTitle).toBeVisible();
    await taskTitle.click();
    await expect(page).toHaveURL(/\/task(\?|$)/);
    await expect(page).toHaveTitle(/工作任务/);

    // 列表加载：表头「任务号」+ 至少一行数据 + 「共 N 条」汇总
    await expect(page.locator('th', { hasText: '任务号' })).toBeVisible();
    await expect(page.locator('tbody tr').first()).toBeVisible({ timeout: 20_000 });
    expect(await page.locator('tbody tr').count()).toBeGreaterThan(0);
    await expect(page.getByText(/共 \d+ 条/).first()).toBeVisible();

    // 登录态确实存在（刷新后不掉线，说明走的是真 token 而不是内存假态）
    await page.reload();
    await expect(page).toHaveURL(/\/task(\?|$)/);
    await expect(page.locator('tbody tr').first()).toBeVisible({ timeout: 20_000 });
  });
});

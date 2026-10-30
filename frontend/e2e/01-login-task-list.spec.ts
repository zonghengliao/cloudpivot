import { expect, test, type Page } from '@playwright/test';
import { assertNavigationOk, login, openAuthed } from './support';

/**
 * 侧边栏「工作任务」目录节点。
 *
 * V0130（commit a9c5a06）把「工作任务」从「自带页面的菜单」改成了**目录**
 * （`menu_type=1`、`component=NULL`），下挂三个子菜单：
 *   任务列表 `/task/list` · 任务看板 `/task/board` · 甘特图 `/task/gantt`
 * 目录标题（`.side-menu-item__title`）点击**只展开/收起**、不再导航到 `/task`，
 * 所以进列表必须点子菜单项。`/task` 现在只是 `redirect: '/task/list'` 的兼容入口。
 */
function taskDir(page: Page) {
  return page
    .locator('.t-submenu')
    .filter({ has: page.locator('.side-menu-item__title', { hasText: '工作任务' }) })
    .first();
}

/** 展开「工作任务」目录（已展开则不动）并点其中的子菜单项 */
async function clickTaskChild(page: Page, label: string | RegExp): Promise<void> {
  const dir = taskDir(page);
  await expect(dir).toBeVisible();
  const item = dir.locator('.t-submenu__item').filter({ hasText: label }).first();
  if (!(await item.isVisible())) {
    await dir.locator('.side-menu-item__title', { hasText: '工作任务' }).first().click();
  }
  await expect(item).toBeVisible();
  await item.click();
}

/**
 * 流程 1：登录 → 任务列表
 *
 * 真实路径：打开 /login → 填账号密码 → 点「登 录」→ 落到工作台 →
 * 侧边栏「工作任务」目录展开 → 点子菜单「任务列表」→
 * 断言列表真的加载出数据（行数 > 0 + 表头 + 汇总条）。
 */
test.describe('流程1 登录 → 任务列表', () => {
  // 只作用于本组：这条测登录，必须干净上下文
  test.use({ storageState: { cookies: [], origins: [] } });

  test('登录后跳工作台，侧边栏「工作任务 → 任务列表」加载出列表', async ({ page }) => {
    await login(page);

    // 断言落在工作台：URL + 工作台可见骨架（侧边栏菜单）
    await expect(page).toHaveURL(/\/dashboard$/);
    await expect(taskDir(page)).toBeVisible();

    // V0130 后「工作任务」是目录：点标题只展开子菜单，进列表要点子菜单项
    await clickTaskChild(page, '任务列表');
    await expect(page).toHaveURL(/\/task\/list(\?|$)/);
    await expect(page).toHaveTitle(/任务列表/);

    // 列表加载：表头「任务号」+ 至少一行数据 + 「共 N 条」汇总
    await expect(page.locator('th', { hasText: '任务号' })).toBeVisible();
    await expect(page.locator('tbody tr').first()).toBeVisible({ timeout: 20_000 });
    expect(await page.locator('tbody tr').count()).toBeGreaterThan(0);
    await expect(page.getByText(/共 \d+ 条/).first()).toBeVisible();

    // 登录态确实存在（刷新后不掉线，说明走的是真 token 而不是内存假态）
    assertNavigationOk(await page.reload(), '/task/list');
    await expect(page).toHaveURL(/\/task\/list(\?|$)/);
    await expect(page.locator('tbody tr').first()).toBeVisible({ timeout: 20_000 });
  });
});

/**
 * 流程 1b：工作任务目录的三个子菜单各进对应视图。
 *
 * 用 globalSetup 存的登录态（config 里 `storageState`），只管菜单导航本身。
 * 这一组是 V0130 目录化的回归网：三个子菜单任何一条路由 / 菜单项名字漏了都会红。
 */
test.describe('流程1b 工作任务目录三视图', () => {
  test('目录标题只展开不导航，三个子菜单各进对应视图且能来回切', async ({ page }) => {
    await openAuthed(page, '/dashboard');

    // ① 目录标题：点一下只展开子菜单，URL 仍停在工作台（这就是 V0130 的产品语义）
    const dir = taskDir(page);
    await expect(dir).toBeVisible();
    await dir.locator('.side-menu-item__title', { hasText: '工作任务' }).first().click();
    const listItem = dir.locator('.t-submenu__item').filter({ hasText: '任务列表' }).first();
    await expect(listItem).toBeVisible();
    await expect(page).toHaveURL(/\/dashboard$/);

    // ② 任务列表 → /task/list（表格视图）
    await listItem.click();
    await expect(page).toHaveURL(/\/task\/list(\?|$)/);
    await expect(page).toHaveTitle(/任务列表/);
    await expect(page.locator('.table-card')).toBeVisible();
    await expect(page.locator('th', { hasText: '任务号' })).toBeVisible();

    // ③ 任务看板 → /task/board（五列看板）
    await clickTaskChild(page, '任务看板');
    await expect(page).toHaveURL(/\/task\/board(\?|$)/);
    await expect(page).toHaveTitle(/任务看板/);
    await expect(page.locator('.board__cols')).toBeVisible();
    await expect(page.locator('.board__cols section.col')).toHaveCount(5);
    await expect(page.locator('.col__name').filter({ hasText: '待开始' }).first()).toBeVisible();
    await expect(page.locator('.col__name').filter({ hasText: '已完成' }).first()).toBeVisible();

    // ④ 甘特图 → /task/gantt（独立路由 + 独立组件）
    //    注意菜单文案带后缀：「甘特图（只读排期）」
    await clickTaskChild(page, /甘特图/);
    await expect(page).toHaveURL(/\/task\/gantt(\?|$)/);
    await expect(page).toHaveTitle(/甘特图/);
    await expect(page.locator('.gantt-page')).toBeVisible();
    await expect(page.locator('.gantt-page .view-switch')).toBeVisible();

    // ⑤ 从甘特直接切回列表：目录可来回切，不残留上一个视图
    await clickTaskChild(page, '任务列表');
    await expect(page).toHaveURL(/\/task\/list(\?|$)/);
    await expect(page.locator('.table-card')).toBeVisible();
    await expect(page.locator('tbody tr').first()).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('.board__cols')).toHaveCount(0);
  });
});

import { expect, test } from '@playwright/test';
import { login, newApi, toast, RUN, type Api } from './support';

/**
 * 流程 2：建任务 → 编辑 → 删除（**全程点界面**，只有删除失败兜底才用 API 清理）
 *
 * 1. 「新建任务」→ 填标题 / 优先级 P1 / 截止日 → 创建 → 列表出现；
 * 2. 行内「编辑」→ 改标题 → 保存 → 断言列表显示新标题；
 * 3. 行内「删除」→ 确认弹窗 → 删除 → 断言该行消失（关键词搜不到）。
 */
test.describe('流程2 建任务 → 编辑 → 删除', () => {
  const title = `[E2E]建改删-${RUN}`;
  const editedTitle = `[E2E]已改名-${RUN}`;
  let api: Api;
  let createdId = '';

  test.beforeAll(async () => {
    api = await newApi();
  });

  test.afterAll(async () => {
    // 兜底清理：用例中途失败也要把数据删掉（成功路径里已经由界面删过一次）
    if (createdId) {
      await api.deleteTask(createdId).catch(() => undefined);
    }
    await api.dispose();
  });

  test('新建 → 编辑 → 删除 全链路', async ({ page }) => {
    await login(page);
    await page.goto('/task');
    await expect(page.locator('th', { hasText: '任务号' })).toBeVisible();

    // ---------- ① 新建 ----------
    await page.getByRole('button', { name: '新建任务' }).first().click();
    const dialog = page.locator('.t-dialog').filter({ hasText: '新建任务' }).first();
    await expect(dialog).toBeVisible();
    await dialog.locator("input[placeholder='一句话说清要做什么（200 字以内）']").fill(title);

    // 优先级（TDesign 下拉：点开 → 选 P1 高）
    await dialog.locator('.t-form__item').filter({ hasText: '优先级' }).locator('.t-select').click();
    await page.locator('.t-select-option').filter({ hasText: 'P1' }).first().click();

    // 截止日（日期面板只读输入，必须点面板里的「本月最后一天」——一定 ≥ 今天）
    await dialog.locator("input[placeholder='留空默认本周五']").click();
    const dayCell = page
      .locator('.t-date-picker__panel .t-date-picker__cell:not(.t-date-picker__cell--additional)')
      .last();
    await dayCell.click();
    await expect(dialog.locator("input[placeholder='留空默认本周五']")).toHaveValue(/\d{4}-\d{2}-\d{2}/);

    await dialog.getByRole('button', { name: '创建' }).click();
    await expect(toast(page, '任务已创建')).toBeVisible({ timeout: 20_000 });
    await expect(dialog).toBeHidden();

    // 列表出现：用关键词搜索精确定位（避免分页/排序影响）
    const keyword = page.locator("input[placeholder='任务标题 / 描述关键词']");
    await keyword.fill(title);
    await page.getByRole('button', { name: '查询' }).first().click();
    const row = page.locator('tbody tr').filter({ hasText: title }).first();
    await expect(row).toBeVisible({ timeout: 20_000 });
    await expect(row).toContainText('P1');
    // 从行内「删除」反查 id：先点开编辑拿到详情 id 太绕，直接从 API 侧过滤一次（只读，不是 mock）
    const found = await page.evaluate(async (t) => {
      const token = localStorage.getItem('x_access_token');
      const res = await fetch(`/api/business/task/page?page=1&page_size=50&keyword=${encodeURIComponent(t)}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      const json = await res.json();
      return json?.data?.list?.[0]?.id ?? '';
    }, title);
    createdId = found;
    expect(createdId, '应从真实接口拿到新任务 id（供兜底清理）').toBeTruthy();

    // ---------- ② 编辑 ----------
    await row.getByText('编辑').click();
    const editDialog = page.locator('.t-dialog').filter({ hasText: '编辑任务' }).first();
    await expect(editDialog).toBeVisible();
    const titleInput = editDialog.locator("input[placeholder='一句话说清要做什么（200 字以内）']");
    await expect(titleInput).toHaveValue(title);
    await titleInput.fill(editedTitle);
    await editDialog.getByRole('button', { name: '保存' }).click();
    await expect(toast(page, '任务已更新')).toBeVisible({ timeout: 20_000 });

    await keyword.fill(editedTitle);
    await page.getByRole('button', { name: '查询' }).first().click();
    const editedRow = page.locator('tbody tr').filter({ hasText: editedTitle }).first();
    await expect(editedRow).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('tbody tr').filter({ hasText: title })).toHaveCount(0);

    // ---------- ③ 删除 ----------
    await editedRow.getByText('删除').click();
    const confirm = page.locator('.t-dialog').filter({ hasText: '删除任务' }).first();
    await expect(confirm).toBeVisible();
    await confirm.getByRole('button', { name: '删除' }).click();
    await expect(toast(page, '任务已删除')).toBeVisible({ timeout: 20_000 });

    // 断言消失：重新查一次关键词，行与空态都验
    await page.getByRole('button', { name: '查询' }).first().click();
    await expect(page.locator('tbody tr').filter({ hasText: editedTitle })).toHaveCount(0, { timeout: 20_000 });
    await expect(page.getByText(/没有找到|暂无数据|换个筛选条件/).first()).toBeVisible();
    createdId = ''; // 已是软删，afterAll 不用再删
  });
});

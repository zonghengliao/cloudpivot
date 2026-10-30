/**
 * F2 前端专项：任务列表 / 看板 / 甘特 标记「新任务」（A 口径：TODO ∧ 我 ∧ 7 天内）。
 *
 * 夹具由 `seed_ui.py` 预置（真后端 API + 一条 SQL 拨 created_at），标题都带 `F2UIxxxx` 前缀：
 *   NEW1 / NEW2 → TODO + admin + 今天      ⇒ 必须有 NEW 标记
 *   OLD         → TODO + admin + 8 天前    ⇒ 不能有
 *   DOING       → DOING + admin + 今天     ⇒ 不能有
 *
 * 关键断言（A 口径的核心）：`badge.counts.task_todo` 必须等于列表里 is_new=true 的条数。
 */
import { expect, test } from '@playwright/test';
import { openAuthed, readSharedToken } from './support';

const TAG = process.env.F2UI_TAG ?? '';
const API = '/api';

async function badgeTodo(page: import('@playwright/test').Page): Promise<number> {
  const token = readSharedToken();
  const res = await page.request.get(`${API}/system/badge/counts`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  expect(res.status()).toBe(200);
  const body = await res.json();
  return body.data.task_todo as number;
}

test.describe('F2 新任务标记（A 口径）', () => {
  test.skip(!TAG, '需要 F2UI_TAG（先跑 seed_ui.py）');
  test.setTimeout(120_000);

  test('列表：2 个 NEW 标记、加粗、与徽标数字一致；老任务/DOING 无标记', async ({ page }) => {
    await openAuthed(page, '/task');
    // 只留本夹具的 4 条：关键词过滤
    await page.fill("input[placeholder='任务标题 / 描述关键词']", TAG);
    await page.click('button:has-text("查询")');
    const rows = page.locator('tbody tr');
    await expect(rows).toHaveCount(4);

    const news = page.locator('.cell-title__new');
    await expect(news).toHaveCount(2);

    // 标题加粗（600）：给 .cell-title__text--new 上 computed font-weight
    const weight = await page
      .locator('.cell-title__text--new')
      .first()
      .evaluate((el) => getComputedStyle(el).fontWeight);
    expect(Number(weight)).toBeGreaterThanOrEqual(600);

    // 老任务（8 天前）与 DOING 都不该有 NEW
    const oldRow = page.locator('tbody tr', { hasText: `${TAG}-八天前的老任务` });
    await expect(oldRow.locator('.cell-title__new')).toHaveCount(0);
    const doingRow = page.locator('tbody tr', { hasText: `${TAG}-进行中的任务` });
    await expect(doingRow.locator('.cell-title__new')).toHaveCount(0);

    // ★ A 口径核心断言：徽标数字 == 列表里 NEW 的条数
    const todo = await badgeTodo(page);
    expect(todo).toBe(2);
    await expect(news).toHaveCount(todo);
  });

  test('看板：新任务卡片有 NEW 角标（2 个）', async ({ page }) => {
    await openAuthed(page, '/task?view=board');
    await expect(page.locator('.board-card').first()).toBeVisible();
    await expect(page.locator('.board-card__new')).toHaveCount(2);
  });

  test('甘特：新任务条有圆点标记（2 个，data-new 落在任务号上）', async ({ page }) => {
    await openAuthed(page, '/task/gantt');
    await expect(page.locator('rect.gantt-bar__body').first()).toBeVisible();
    await expect(page.locator('circle[data-new]')).toHaveCount(2);
    const nos = await page.locator('circle[data-new]').evaluateAll((els) =>
      els.map((el) => el.getAttribute('data-new')),
    );
    expect(nos.length).toBe(2);
  });

  test('口径联动：TODO → DOING 后 NEW 标记消失、徽标同步', async ({ page }) => {
    const token = readSharedToken();
    const state = JSON.parse(process.env.F2UI_IDS ?? '{}');
    const res = await page.request.patch(`${API}/business/task/${state.NEW1}/status`, {
      headers: { Authorization: `Bearer ${token}` },
      data: { status: 'DOING' },
    });
    expect(res.status()).toBe(200);

    await openAuthed(page, '/task');
    await page.fill("input[placeholder='任务标题 / 描述关键词']", TAG);
    await page.click('button:has-text("查询")');
    await expect(page.locator('tbody tr')).toHaveCount(4);
    await expect(page.locator('.cell-title__new')).toHaveCount(1);
    expect(await badgeTodo(page)).toBe(1);
  });
});

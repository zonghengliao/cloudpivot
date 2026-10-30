/**
 * T8-B 前端专项：任务列表树形改造（箭头移标题列 / 父行竖条 / 子行连接线 / 子任务进度条 / 逐节点折叠）。
 *
 * 造数：真后端 API 造「根 + 3 子 + 1 独立任务」，标题带 `[T8B-<run>]` 前缀；
 * 结束时按 id 倒序删干净（生产库只软删自己那条，绝不模糊删），最后按关键词复核残留 0。
 *
 * ⚠️ 只造 2 层：生产后端目前仍是「子任务最多 2 层」的二进制（T8-A 未发布），
 * 建第 3 层会被 400 + 63005 挡住。8 层的逐层展开见 T8-A 上线后补的深度用例。
 */
import { expect, request, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import { openAuthed, readSharedToken } from './support';

const RUN = `t8b${Date.now().toString(36)}`;
const PREFIX = `[T8B-${RUN}]`;

let api: APIRequestContext;
const created: string[] = [];

/** 造一条任务（可选父任务），返回 id */
async function createTask(title: string, parentId?: string): Promise<string> {
  const res = await api.post('/api/business/task', {
    data: { title: `${PREFIX}${title}`, parent_id: parentId ?? null, priority: 'P2' },
  });
  const body = await res.json().catch(() => ({}));
  expect(res.status(), `建任务 ${title} 应 200：${JSON.stringify(body).slice(0, 200)}`).toBe(200);
  expect(body.code).toBe(0);
  created.push(body.data.id);
  return body.data.id as string;
}

/** 推到 DONE（TODO → DOING → DONE，按后端流转白名单走，不跳级） */
async function markDone(id: string): Promise<void> {
  for (const to of ['DOING', 'DONE']) {
    const res = await api.patch(`/api/business/task/${id}/status`, { data: { to } });
    expect(res.status(), `流转 ${to}`).toBe(200);
  }
}

/** 列表里某一行（按标题精确落到自己造的那条） */
function rowOf(page: Page, title: string): Locator {
  return page.locator('.t-table__body > tr').filter({ has: page.locator('.cell-title__text', { hasText: `${PREFIX}${title}` }) });
}

/** 标题单元格（第 2 列 = 树形箭头所在列） */
const titleCellOf = (row: Locator): Locator => row.locator('td').nth(1);

/** 把主题 token 解析成浏览器真实 rgb（`--td-*` 是 hex，直接比字符串会比错） */
async function resolveToken(page: Page, name: string): Promise<string> {
  return page.evaluate((tokenName) => {
    const probe = document.createElement('div');
    probe.style.color = `var(${tokenName})`;
    document.body.appendChild(probe);
    const value = getComputedStyle(probe).color;
    probe.remove();
    return value;
  }, name);
}

/** 打开任务列表并按 RUN 过滤 */
async function openList(page: Page): Promise<void> {
  await openAuthed(page, '/task');
  await page.fill("input[placeholder='任务标题 / 描述关键词']", RUN);
  await page.click('button:has-text("查询")');
  await expect(rowOf(page, '根任务')).toBeVisible();
}

test.describe('T8-B 任务列表树形改造', () => {
  test.setTimeout(120_000);

  test.beforeAll(async () => {
    api = await request.newContext({
      baseURL: process.env.E2E_BASE_URL ?? 'http://172.18.10.144',
      extraHTTPHeaders: { Authorization: `Bearer ${readSharedToken()}` },
    });
    const root = await createTask('根任务');
    const c1 = await createTask('子1', root);
    await createTask('子2', root);
    await createTask('子3', root);
    await createTask('独立任务');
    await markDone(c1); // → 根任务行的子任务进度应为 1/3
  });

  test.afterAll(async () => {
    for (const id of [...created].reverse()) {
      await api.delete(`/api/business/task/${id}`).catch(() => undefined);
    }
    const res = await api.get(`/api/business/task/page?page=1&page_size=50&keyword=${encodeURIComponent(RUN)}`);
    const body = await res.json().catch(() => ({}));
    const leftover = (body?.data?.list ?? []).map((item: { title: string }) => item.title);
    expect(leftover, `清理残留：${JSON.stringify(leftover)}`).toEqual([]);
    await api.dispose();
  });

  test('箭头在标题列、父行 4px 竖条 + 浅底 + 加粗、进度列 1/3 小进度条', async ({ page }) => {
    await openList(page);
    const root = rowOf(page, '根任务');
    const firstCell = root.locator('td').nth(0);
    const titleCell = titleCellOf(root);

    // 箭头在标题列（第 2 列），任务号列没有树形容器
    await expect(titleCell.locator('.t-table__tree-op-icon svg')).toHaveCount(1);
    await expect(firstCell.locator('.t-table__tree-col')).toHaveCount(0);

    // 父行：4px 主色左竖条 + 品牌浅底 + 标题 600
    await expect(root).toHaveClass(/task-row--parent/);
    const bar = await firstCell.evaluate((el) => {
      const cs = getComputedStyle(el);
      return { width: cs.borderLeftWidth, color: cs.borderLeftColor };
    });
    expect(bar.width).toBe('4px');
    expect(bar.color).toBe(await resolveToken(page, '--td-brand-color'));
    const titleBg = await titleCell.evaluate((el) => getComputedStyle(el).backgroundColor);
    expect(titleBg).toBe(await resolveToken(page, '--td-brand-color-light'));
    const weight = await root.locator('.cell-title__text').evaluate((el) => getComputedStyle(el).fontWeight);
    expect(Number(weight)).toBeGreaterThanOrEqual(600);

    // 进度列：N/M + 小进度条（1/3 → 33%）
    const progressCell = root.locator('td').nth(4);
    await expect(progressCell.locator('.sub-progress__text')).toHaveText('1/3');
    expect(await progressCell.locator('.sub-progress__fill').evaluate((el) => (el as HTMLElement).style.width)).toBe('33%');

    // 非父任务：自己的百分比进度，没有子任务进度条
    const solo = rowOf(page, '独立任务');
    await expect(solo.locator('td').nth(4).locator('.sub-progress')).toHaveCount(0);
    await expect(solo.locator('td').nth(4)).toContainText('%');

    // 既有结构不受影响：10 列 + 4 个视图 Tab
    expect(await page.locator('.t-table thead th').count()).toBe(10);
    await expect(page.locator('.view-switch .t-tabs__nav-item')).toHaveCount(4);
  });

  test('逐节点折叠展开：展开出 3 条子任务（连接线 + 缩进 24px），收起后兄弟不受影响', async ({ page }) => {
    await openList(page);
    await expect(rowOf(page, '子1')).toHaveCount(0);

    await titleCellOf(rowOf(page, '根任务')).locator('.t-table__tree-op-icon').click();
    await expect(rowOf(page, '子1')).toBeVisible();
    await expect(rowOf(page, '子2')).toBeVisible();
    await expect(rowOf(page, '子3')).toBeVisible();

    const c1 = rowOf(page, '子1');
    await expect(c1).toHaveClass(/task-row--child/);
    await expect(c1).toHaveClass(/t-table-tr--level-1/);
    const c1TitleCell = titleCellOf(c1);
    // 连接线（::before 的 └─）与缩进
    const connector = await c1TitleCell.locator('.t-table__tree-op-icon').evaluate((el) => {
      const cs = getComputedStyle(el, '::before');
      return { borderLeft: cs.borderLeftWidth, width: cs.width };
    });
    expect(connector.borderLeft).toBe('1px');
    expect(connector.width).toBe('12px');
    expect(await c1TitleCell.locator('.t-table__tree-op-icon').evaluate((el) => (el.parentElement as HTMLElement).style.paddingLeft)).toBe('24px');
    expect(await c1TitleCell.evaluate((el) => getComputedStyle(el).backgroundColor)).toBe(
      await resolveToken(page, '--td-bg-color-secondarycontainer'),
    );

    // 收起根任务：子任务全部收起，同级独立任务仍在
    await titleCellOf(rowOf(page, '根任务')).locator('.t-table__tree-op-icon').click();
    await expect(rowOf(page, '子1')).toHaveCount(0);
    await expect(rowOf(page, '子2')).toHaveCount(0);
    await expect(rowOf(page, '独立任务')).toBeVisible();
  });
});

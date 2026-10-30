import { expect, test, type Page } from '@playwright/test';
import { newApi, openAuthed, readSharedToken, RUN, toast, type Api } from './support';

/**
 * 流程 6：任务详情抽屉（E9 把「5 区块」压成「4 主 Tab」后的回归）。
 *
 * 两段：
 * ① 真人任务（用例自己造、自己删）：头部信息单行 / 3 个主 Tab + 徽标数字 /
 *    描述折叠-展开（几何断言）/ 协作三段分段控件 / 子任务新建 / 评论发表 /
 *    附件上传 + 下载 / 工时登记 / 状态流转（PATCH /status 200 + 头部状态变化）/
 *    关抽屉重开 Tab 复位；
 * ② AI 任务（`exec_mode=AI`，生产已有数据，只读复用）：多出「AI 产出 N」Tab，
 *    徽标数字与该 Tab 列表条数相等。
 *
 * 全程真接口零 mock。两个坑（都踩过，别再踩）：
 * 1. 列表页搜索**不匹配任务号** → 按标题关键词搜，再用 `.filter({ hasText: taskNo })` 精确定位行；
 * 2. `DELETE /business/task/{id}` 只软删父任务，**子任务会留在列表里** → 清理必须按
 *    标题**前缀**（本用例的标题里带 RUN，天然唯一）把父 + 子逐条删，最后复核残留为 0；
 *    生产库里有别人的数据，绝不按关键词模糊删。
 */

const DESC = [
  'E2E 详情回归任务：详情抽屉是「主 Tab」结构（详情 / 子任务 / 协作 / AI 产出）。',
  '第 2 行：描述超过 5 行时应当折叠，并给「展开」。',
  '第 3 行：头部信息区压成单行（优先级 · 负责人 · 截止 · 进度 · 工时）。',
  '第 4 行：子任务数量在 Tab 徽标上。',
  '第 5 行：协作三个分段（评论 / 附件 / 工时）。',
  '第 6 行：AI 产出 Tab 只在 execMode=AI 时出现。',
  '第 7 行：这行是为了让描述稳定超过 5 行。',
  '第 8 行：最后一行 —— 展开后应当能看到它。',
].join('\n');

/** 主 Tab 上的所有文案（`详情` / `子任务 3` / `协作 4` / `AI 产出 29`） */
async function tabTexts(page: Page): Promise<string[]> {
  const items = page.locator('.main-tabs .t-tabs__nav-item');
  await expect(items.first()).toBeVisible({ timeout: 20_000 });
  const count = await items.count();
  const texts: string[] = [];
  for (let i = 0; i < count; i += 1) {
    texts.push(((await items.nth(i).innerText()) || '').replace(/\s+/g, ' ').trim());
  }
  return texts;
}

/** 打开详情抽屉：列表页按标题关键词搜到这一行，再用任务号精确定位（搜索不匹配任务号），点标题 */
async function openDrawer(page: Page, titleKeyword: string, taskNo: string): Promise<void> {
  await openAuthed(page, '/task');
  await expect(page.locator('th', { hasText: '任务号' })).toBeVisible();
  await page.locator("input[placeholder='任务标题 / 描述关键词']").fill(titleKeyword);
  await page.getByRole('button', { name: '查询' }).first().click();
  const row = page.locator('tbody tr').filter({ hasText: titleKeyword }).filter({ hasText: taskNo }).first();
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.locator('.cell-title__text').click();
  await expect(page.locator('.t-drawer .head__title')).toBeVisible({ timeout: 20_000 });
}

/** 按标题前缀把父 + 子任务逐条删掉，返回最终残留数（0 = 干净） */
async function purgeByPrefix(api: Api, prefix: string): Promise<number> {
  for (let round = 1; round <= 4; round += 1) {
    const rows = (await api.listTasks(prefix)).filter((row) => row.title.startsWith(prefix));
    if (!rows.length) {
      // 多等一拍再确认：软删是同步的，这里只是为了不吃「列表缓存」的亏
      return 0;
    }
    for (const row of rows) {
      await api.deleteTask(row.id).catch(() => undefined);
    }
  }
  const left = (await api.listTasks(prefix)).filter((row) => row.title.startsWith(prefix));
  return left.length;
}

/** 找一条真的 AI 任务（列表接口不下发 exec_mode，得逐个拉详情；要求有 AI 产出文件） */
async function findAiTask(api: Api): Promise<{ id: string; taskNo: string; title: string; total: number } | null> {
  for (const row of await api.listTasks('', 200)) {
    const detail = await api.getTaskDetail(row.id);
    if (detail.exec_mode !== 'AI') {
      continue;
    }
    const total = await api.aiFilesTotal(row.id);
    if (total > 0) {
      return { id: row.id, taskNo: row.task_no, title: row.title, total };
    }
  }
  return null;
}

test.describe('流程6 任务详情 4 主 Tab', () => {
  let api: Api;
  let headers: Record<string, string>;
  let taskId = '';
  let taskNo = '';
  const title = `[E2E]详情-${RUN}`;

  test.beforeAll(async () => {
    api = await newApi();
    headers = { Authorization: `Bearer ${readSharedToken()}` };
    const created = await api.createTask({
      title,
      description: DESC,
      priority: 'P1',
      due_date: '2026-11-06',
      estimate_hours: 8,
    });
    taskId = created.id;
    taskNo = created.task_no;
    for (let i = 1; i <= 3; i += 1) {
      await api.ctx.post(`/api/business/task/${taskId}/subtask`, { headers, data: { title: `${title}-子${i}` } });
    }
    await api.ctx.post(`/api/business/task/${taskId}/comments`, { headers, data: { content: `E2E 预置评论 1（${RUN}）` } });
    await api.ctx.post(`/api/business/task/${taskId}/comments`, { headers, data: { content: `E2E 预置评论 2（${RUN}）` } });
    await api.ctx.post(`/api/business/task/${taskId}/worklogs`, {
      headers,
      data: { hours: 1.5, work_date: '2026-10-30', note: 'E2E 预置工时' },
    });
  });

  test.afterAll(async () => {
    // 父任务 DELETE 只软删父：子任务（`${title}-子N` / `-UI追加`）会留在列表里，
    // 必须按前缀一起清掉，否则生产列表里会越积越多的 [E2E] 垃圾数据。
    const left = await purgeByPrefix(api, title);
    await api.dispose();
    expect(left, `清理后仍有 ${left} 条「${title}」残留`).toBe(0);
  });

  test('真人任务：3 个 Tab、徽标数量、描述折叠、协作内嵌、原功能全通', async ({ page }) => {
    await openDrawer(page, title, taskNo);

    // ---------- ① 头部信息区压成单行 ----------
    const meta = page.locator('.head__meta');
    await expect(meta).toBeVisible();
    const chipHeight = await page.locator('.head__chip').first().evaluate((el) => el.getBoundingClientRect().height);
    const metaHeight = (await meta.boundingBox())?.height ?? 0;
    // 单行 = 一个 chip 的高度量级；折成两行会到 2 倍
    expect(metaHeight, `信息区高度 ${metaHeight}px 应接近单行`).toBeLessThan(chipHeight + 8);
    await expect(meta).toContainText('P1');
    await expect(meta).toContainText('2026-11-06');
    await expect(meta.locator('.head__pct')).toBeVisible();
    await expect(meta).toContainText('预估 8 小时');

    // ---------- ② 主 Tab：真人任务没有「AI 产出」 ----------
    const before = await tabTexts(page);
    expect(before[0]).toBe('详情');
    expect(before[1]).toBe('子任务 3');
    expect(before[2], '协作徽标 = 评论2 + 附件0 + 工时1').toBe('协作 3');
    expect(before.some((t) => t.includes('AI 产出')), 'HUMAN 任务不该有 AI 产出 Tab').toBe(false);
    expect(before.length).toBe(3);

    // ---------- ③ 描述折叠 + 展开（8 行 > 5 行，几何断言） ----------
    const desc = page.locator('.desc');
    await expect(desc).toHaveClass(/desc--collapsed/);
    const collapsed = await desc.evaluate((el) => ({ client: el.clientHeight, scroll: el.scrollHeight }));
    expect(collapsed.scroll, '描述内容应超出折叠高度').toBeGreaterThan(collapsed.client);
    await expect(page.getByRole('button', { name: '展开' })).toBeVisible();
    await page.getByRole('button', { name: '展开' }).click();
    await expect(desc).not.toHaveClass(/desc--collapsed/);
    await expect(page.getByRole('button', { name: '收起' })).toBeVisible();
    const expanded = await desc.evaluate((el) => ({ client: el.clientHeight, scroll: el.scrollHeight }));
    expect(expanded.client).toBeGreaterThanOrEqual(expanded.scroll - 2);
    await page.getByRole('button', { name: '收起' }).click();
    await expect(desc).toHaveClass(/desc--collapsed/);

    // ---------- ④ 详情 Tab 里还有基本信息 / 协作人 ----------
    await expect(page.locator('.info')).toBeVisible();
    await expect(page.locator('.info')).toContainText('负责人');

    // ---------- ⑤ 子任务 Tab：列表 + 新建 ----------
    await page.locator('.main-tabs .t-tabs__nav-item', { hasText: '子任务' }).click();
    await expect(page.locator('.subtask__item')).toHaveCount(3);
    await page.getByRole('button', { name: '+ 新建子任务' }).click();
    const subDialog = page.locator('.t-dialog').filter({ hasText: '新建子任务' }).first();
    await expect(subDialog).toBeVisible();
    await subDialog.locator("input[placeholder='拆成一个能独立完成的小任务']").fill(`${title}-UI追加`);
    await subDialog.getByRole('button', { name: '创建' }).click();
    await expect(toast(page, '子任务已创建')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('.subtask__item')).toHaveCount(4);
    expect(await tabTexts(page)).toContain('子任务 4');

    // ---------- ⑥ 协作 Tab：三个分段（评论 / 附件 / 工时） ----------
    await page.locator('.main-tabs .t-tabs__nav-item', { hasText: '协作' }).click();
    const seg = page.locator('.collab__seg');
    await expect(seg).toBeVisible();
    await expect(seg.locator('.t-radio-button')).toHaveCount(3);
    // 协作三段是分段控件（v-show 常驻），不是「Tab 套 Tab」
    await expect(page.locator('.main-tabs .t-tabs__content .t-tabs__nav')).toHaveCount(0);

    // 评论：列表可见 + 真发一条
    await expect(page.locator('.timeline__item')).toHaveCount(2);
    await page.locator('.composer textarea').fill(`E2E UI 评论（${RUN}）`);
    await page.getByRole('button', { name: '发送' }).click();
    await expect(toast(page, '评论已发表')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('.timeline__item')).toHaveCount(3);
    expect(await tabTexts(page)).toContain('协作 4');

    // 附件：切分段 → 上传 → 下载（download 事件 = 真拿到文件流）
    await page.locator('.collab__seg .t-radio-button', { hasText: '附件' }).click();
    await page.locator('input.attach__input').setInputFiles({
      name: `e2e-${RUN}.txt`,
      mimeType: 'text/plain',
      buffer: Buffer.from(`E2E attachment ${RUN}\n`, 'utf8'),
    });
    await expect(page.locator('.attach__item')).toHaveCount(1, { timeout: 30_000 });
    await expect(page.locator('.collab__seg')).toContainText('附件 1');
    const downloadPromise = page.waitForEvent('download', { timeout: 30_000 });
    await page.locator('.attach__item').getByRole('button', { name: '下载' }).click();
    const download = await downloadPromise;
    expect(download.suggestedFilename()).toContain('e2e-');
    expect(await tabTexts(page)).toContain('协作 5');

    // 工时：切分段 → 登记 → 明细/合计变化
    await page.locator('.collab__seg .t-radio-button', { hasText: '工时' }).click();
    await expect(page.locator('.worklog__item')).toHaveCount(1);
    await expect(page.locator('.worklog__summary')).toContainText('1.5');
    await page.getByRole('button', { name: '登记工时' }).click();
    const wlDialog = page.locator('.t-dialog').filter({ hasText: '登记工时' }).first();
    await expect(wlDialog).toBeVisible();
    await wlDialog.getByRole('button', { name: '登记' }).click();
    await expect(toast(page, '工时已登记（实际工时由服务端汇总）')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('.worklog__item')).toHaveCount(2);
    expect(await tabTexts(page)).toContain('协作 6');

    // ---------- ⑦ 状态流转（原功能不丢）：PATCH /status 200 且头部状态真的变 ----------
    await page.locator('.main-tabs .t-tabs__nav-item', { hasText: '详情' }).click();
    const head = page.locator('.head__title-row').filter({ hasText: taskNo });
    const beforeStatus = (await head.innerText()).replace(/\s+/g, ' ');
    const actionBtn = page
      .locator('.head__actions .t-button')
      .filter({ hasText: /开始处理|提交验收|直接完成|验收通过|重开/ })
      .first();
    await expect(actionBtn, '待办任务应至少有一个可做的状态流转').toBeVisible();
    const actionLabel = (await actionBtn.innerText()).trim();
    const statusPATCH = page.waitForResponse(
      (res) => res.url().includes('/status') && res.request().method() === 'PATCH',
      { timeout: 30_000 },
    );
    await actionBtn.click();
    // 确认弹窗由 DialogPlugin 渲染，确认后弹窗立即销毁 → 点击本身可能报 detach，故容错
    await page
      .locator('.t-dialog')
      .filter({ hasText: actionLabel })
      .last()
      .getByRole('button', { name: actionLabel })
      .last()
      .click({ noWaitAfter: true })
      .catch(() => undefined);
    const statusRes = await statusPATCH;
    expect(statusRes.status(), `状态流转接口应 200（${actionLabel}）`).toBe(200);
    await expect
      .poll(async () => (await head.innerText()).replace(/\s+/g, ' '), { timeout: 20_000 })
      .not.toBe(beforeStatus);

    // ---------- ⑧ 关抽屉重开：Tab 复位到「详情」 ----------
    await page.keyboard.press('Escape');
    await expect(page.locator('.t-drawer .head__title')).toBeHidden({ timeout: 15_000 });
    await page.locator('tbody tr').filter({ hasText: taskNo }).first().locator('.cell-title__text').click();
    await expect(page.locator('.t-drawer .head__title')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('.main-tabs .t-tabs__nav-item').first()).toHaveClass(/t-is-active/);
  });

  test('AI 任务：多出「AI 产出 N」Tab，徽标数字 = 列表条数', async ({ page }) => {
    const ai = await findAiTask(api);
    test.skip(!ai, '生产库里没有可用的 AI 任务（exec_mode=AI 且有产出文件），跳过');

    await openDrawer(page, ai!.title, ai!.taskNo);

    // 徽标数字由子组件拉完列表后回抛，轮询等它到位（不是死等 sleep）
    await expect
      .poll(async () => (await tabTexts(page)).find((t) => t.startsWith('AI 产出')) ?? '', { timeout: 30_000 })
      .toMatch(/^AI 产出 \d+$/);

    const texts = await tabTexts(page);
    expect(texts.length, `AI 任务应有 4 个 Tab，实际：${JSON.stringify(texts)}`).toBe(4);
    const aiTab = texts.find((t) => t.startsWith('AI 产出')) ?? '';

    await page.locator('.main-tabs .t-tabs__nav-item', { hasText: 'AI 产出' }).click();
    await expect(page.locator('.aiws__item').first()).toBeVisible({ timeout: 30_000 });
    const rows = await page.locator('.aiws__item').count();
    expect(rows).toBeGreaterThan(0);
    // 徽标数字与列表条数同源（接口一次返回全量，应完全相等）
    expect(Number(aiTab.replace('AI 产出 ', ''))).toBe(rows);
  });
});

/**
 * T8-C 前端验收：任务列表 `root_only`（只返回顶层任务）+ 前端默认启用。
 *
 * 判据来自卡面 §五（真机 Playwright，不许只写「已验证」）：
 * ① 登录后 /task 列表第 1 页有 5 个带**真箭头**的父任务（`.t-table__tree-op-icon` 宽度 > 0 且有 svg）；
 * ② 点父任务箭头 → 出现 3 个子行（`task-row--child` + `└─` 连接线）；
 * ③ 父行进度列是 `N/M` 小进度条（SubProgressBar）；
 * ④ 父行有 4px 竖条（border-left）；
 * ⑤ 分页 total 从 43 变成 28（顶层数）；
 * ⑥ 看板 / 甘特 / 燃尽图**不传** root_only，数据量不变（43 条一条不少）。
 *
 * 数据：**不造数、不清理**（只读：本用例对任务只发 GET）。数据源是 CP-DEV 项目
 * 真实任务的只读副本库（43 = 5 父 + 15 子 + 23 独立）。
 */
import { expect, test, type Page } from '@playwright/test';
import { openAuthed } from './support';

/** 列表视图（pageSize = 10）第 1 页：10 条顶层里恰好有 5 个父任务（各 3 个子）。 */
const EXPECT_TOTAL_ROOTS = 28;
const EXPECT_TOTAL_ALL = 43;
const EXPECT_PARENTS_ON_PAGE1 = 5;
const EXPECT_CHILDREN_PER_PARENT = 3;

/** 表格行 */
const rows = (page: Page) => page.locator('.t-table__body > tr');

/** 某一行的标题单元格（第 2 列 = 树形箭头所在列） */
const titleCellOf = (row: ReturnType<typeof rows.first>) => row.locator('td').nth(1);

/** 收集 `/api/business/task/page` 的请求（用来证明「谁传了 root_only」） */
function watchTaskPage(page: Page) {
  const urls: string[] = [];
  const totals: number[] = [];
  page.on('request', (req) => {
    if (req.url().includes('/api/business/task/page')) {
      urls.push(req.url());
    }
  });
  page.on('response', async (res) => {
    if (!res.url().includes('/api/business/task/page')) {
      return;
    }
    try {
      const body = await res.json();
      totals.push(Number(body?.data?.total ?? -1));
    } catch {
      /* 非 JSON 忽略 */
    }
  });
  return {
    urls,
    totals,
    reset() {
      urls.length = 0;
      totals.length = 0;
    },
    withRootOnly() {
      return urls.filter((url) => /[?&]root_only=true/.test(url));
    },
  };
}

test.describe('T8-C 列表只返回顶层任务（root_only）', () => {
  test.setTimeout(120_000);

  test('列表第 1 页 = 5 个带真箭头的父任务；展开出 3 个子行；total 28', async ({ page }, testInfo) => {
    const net = watchTaskPage(page);
    await openAuthed(page, '/task');
    await expect(rows(page)).toHaveCount(10);

    // ---------- ⑥ 分页 total：43 → 28（顶层数）----------
    const countText = (await page.locator('.toolbar__count').first().innerText()).trim();
    expect(countText, `工具条计数应显示顶层数 28：${countText}`).toContain('共 28 条');
    const pageTotalText = (await page.locator('.t-pagination__total').first().innerText().catch(() => '')).trim();
    expect(pageTotalText, `分页器 total 应是 28 而不是 43：${pageTotalText}`).toContain(String(EXPECT_TOTAL_ROOTS));

    // 列表视图的请求必须带 root_only=true（前端默认启用）
    expect(net.withRootOnly().length, `列表视图应带 root_only=true：${JSON.stringify(net.urls)}`).toBeGreaterThan(0);

    // ---------- ① 5 个「真箭头」父任务 ----------
    // TDesign 树形表格给**每一行**都渲染 `.t-table__tree-op-icon` 容器；叶子行里它是空的。
    // 「真箭头」的判据 = 容器里有 svg（可点击）且宽度 > 0 —— 卡面原话。
    const arrowCells = rows(page).locator('.t-table__tree-op-icon');
    const arrowCount = await arrowCells.count();
    const arrowProbe = [];
    for (let index = 0; index < arrowCount; index += 1) {
      const cell = arrowCells.nth(index);
      const box = await cell.boundingBox();
      const svgCount = await cell.locator('svg').count();
      arrowProbe.push({ index, width: box ? Math.round(box.width) : 0, svgCount });
    }
    const realArrows = arrowProbe.filter((item) => item.svgCount === 1);
    expect(realArrows.length, `第 1 页应有 ${EXPECT_PARENTS_ON_PAGE1} 个带 svg 的真箭头：${JSON.stringify(arrowProbe)}`).toBe(
      EXPECT_PARENTS_ON_PAGE1,
    );
    for (const arrow of realArrows) {
      expect(arrow.width, `箭头 ${arrow.index} 宽度应 > 0（不是空占位）`).toBeGreaterThan(0);
    }
    testInfo.attach('t8c-arrow-probe.json', { body: JSON.stringify(arrowProbe, null, 2), contentType: 'application/json' });

    // ---------- ④ 父行 4px 竖条 ----------
    const parentClassRows = page.locator('.t-table__body > tr.task-row--parent');
    const parentRowCount = await parentClassRows.count();
    expect(parentRowCount, '父行应有 task-row--parent class').toBe(EXPECT_PARENTS_ON_PAGE1);
    const bar = await parentClassRows.first().locator('td').first().evaluate((el) => {
      const cs = getComputedStyle(el);
      return { width: cs.borderLeftWidth, style: cs.borderLeftStyle };
    });
    expect(bar.width, '父行首列应有 4px 竖条').toBe('4px');
    expect(bar.style).toBe('solid');

    // ---------- ③ 父行进度列 N/M（SubProgressBar）----------
    const firstParent = parentClassRows.first();
    const progressCell = firstParent.locator('td').nth(4);
    await expect(progressCell.locator('.sub-progress')).toHaveCount(1);
    const progressText = (await progressCell.locator('.sub-progress__text').innerText()).trim();
    expect(progressText, `父行进度应是 N/${EXPECT_CHILDREN_PER_PARENT}`).toMatch(
      new RegExp(`^\\d+/${EXPECT_CHILDREN_PER_PARENT}$`),
    );
    const fillWidth = await progressCell.locator('.sub-progress__fill').evaluate((el) => (el as HTMLElement).style.width);
    expect(fillWidth, '进度条宽度应是百分比').toMatch(/^\d+%$/);

    // ---------- ② 点箭头 → 3 个子行 + 连接线 ----------
    const parentTitle = (await firstParent.locator('.cell-title__text').innerText()).trim();
    await titleCellOf(firstParent).locator('.t-table__tree-op-icon').click();
    const childRows = page.locator('.t-table__body > tr.task-row--child');
    await expect(childRows, `展开「${parentTitle}」后应出现 ${EXPECT_CHILDREN_PER_PARENT} 个子行`).toHaveCount(
      EXPECT_CHILDREN_PER_PARENT,
    );
    const firstChild = childRows.first();
    await expect(firstChild).toHaveClass(/task-row--child/);
    await expect(firstChild).toHaveClass(/t-table-tr--level-1/);
    const connector = await titleCellOf(firstChild)
      .locator('.t-table__tree-op-icon')
      .evaluate((el) => {
        const cs = getComputedStyle(el, '::before');
        return { borderLeft: cs.borderLeftWidth, width: cs.width };
      });
    expect(connector.borderLeft, '子行应有 └─ 竖线').toBe('1px');
    expect(connector.width, '子行连接线宽度 12px').toBe('12px');
    const indent = await titleCellOf(firstChild)
      .locator('.t-table__tree-op-icon')
      .evaluate((el) => (el.parentElement as HTMLElement).style.paddingLeft);
    expect(indent, '子行缩进 24px（8 层 × 24px 的第一层）').toBe('24px');

    const childTitles = await childRows.locator('.cell-title__text').allInnerTexts();

    // ---------- 现场证据 ----------
    await page.screenshot({ path: testInfo.outputPath('t8c-list-expanded.png'), fullPage: true });
    const evidence = {
      countText,
      pageTotalText,
      arrowProbe,
      parentRowCount,
      parentBorderLeft: bar,
      parentProgress: progressText,
      parentProgressFill: fillWidth,
      expandedParent: parentTitle,
      childRows: childTitles.length,
      childTitles,
      childConnector: connector,
      childIndent: indent,
      listRequestsWithRootOnly: net.withRootOnly().length,
    };
    console.log('[T8C-EVIDENCE-LIST] ' + JSON.stringify(evidence, null, 2));
    await testInfo.attach('t8c-list-evidence.json', { body: JSON.stringify(evidence, null, 2), contentType: 'application/json' });
  });

  test('看板 / 甘特 / 燃尽图不传 root_only，数据量不变（全部 43 条）', async ({ page }, testInfo) => {
    const net = watchTaskPage(page);
    await openAuthed(page, '/task');
    await expect(rows(page)).toHaveCount(10);
    net.reset();

    // ---------- 看板 ----------
    await page.locator('.view-switch .t-tabs__nav-item', { hasText: '任务看板' }).click();
    await expect(page.locator('.board-card').first()).toBeVisible({ timeout: 20_000 });
    await page.waitForTimeout(1500); // 等 4 列各自的分页请求回来
    const boardUrls = [...net.urls];
    const boardTotals = [...net.totals];
    expect(net.withRootOnly(), `看板不该带 root_only：${JSON.stringify(boardUrls)}`).toEqual([]);
    const boardCardCount = await page.locator('.board-card').count();
    const boardSum = boardTotals.reduce((sum, value) => sum + value, 0);
    const boardCountText = (await page.locator('.toolbar__count').first().innerText()).trim();
    // 子任务清单是**异步**逐父加载的（`GET /business/task/{id}/subtasks`），**必须在看板 tab 里**等它落齐
    await expect(page.locator('.board .subtasks__row')).toHaveCount(EXPECT_TOTAL_ALL - EXPECT_TOTAL_ROOTS, {
      timeout: 30_000,
    });
    const nestedRows = await page.locator('.board .subtasks__row').count();
    await page.screenshot({ path: testInfo.outputPath('t8c-board-unaffected.png'), fullPage: true });

    // ---------- 甘特图 ----------
    net.reset();
    await page.locator('.view-switch .t-tabs__nav-item', { hasText: '甘特图' }).click();
    await page.waitForTimeout(2500);
    const ganttUrls = [...net.urls];
    expect(net.withRootOnly(), `甘特图不该带 root_only：${JSON.stringify(ganttUrls)}`).toEqual([]);

    // ---------- 燃尽图 ----------
    net.reset();
    await page.locator('.view-switch .t-tabs__nav-item', { hasText: '燃尽图' }).click();
    await page.waitForTimeout(2500);
    const burndownUrls = [...net.urls];
    expect(net.withRootOnly(), `燃尽图不该带 root_only：${JSON.stringify(burndownUrls)}`).toEqual([]);

    // ---------- 数据量对照：看板里父 + 子都在 ----------
    // 看板的展示口径是「父卡片 + 内嵌子任务清单」（E6c）：子任务不单独成卡，
    // 而是嵌在父卡片的 `.subtasks__list` 里 → 卡片数 28（顶层）+ 子任务行 15。
    expect(boardSum, `看板 4 列 total 之和应为全部 ${EXPECT_TOTAL_ALL} 条（含 15 个子任务）`).toBe(EXPECT_TOTAL_ALL);
    expect(boardCardCount, '看板卡片数应为 28 张顶层卡（子任务内嵌，不单独成卡）').toBe(EXPECT_TOTAL_ROOTS);
    expect(boardCountText, '看板计数文案应是全部条数').toContain(`共 ${EXPECT_TOTAL_ALL} 条`);
    expect(nestedRows, '看板里应能看到 15 条内嵌子任务（root_only 没把它们挡掉）').toBe(
      EXPECT_TOTAL_ALL - EXPECT_TOTAL_ROOTS,
    );

    const evidence = {
      boardUrls,
      boardTotals,
      boardSum,
      boardCardCount,
      boardCountText,
      ganttUrls,
      burndownUrls,
      listTouchedRootOnlyBeforeReset: true,
    };
    console.log('[T8C-EVIDENCE-OTHER-VIEWS] ' + JSON.stringify(evidence, null, 2));
    await testInfo.attach('t8c-other-views-evidence.json', { body: JSON.stringify(evidence, null, 2), contentType: 'application/json' });
  });
});

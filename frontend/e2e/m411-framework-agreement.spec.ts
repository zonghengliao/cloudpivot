/**
 * 框架协议页面真实交互（M4-11：协议台账 + 执行量）。
 *
 * # 这条 spec 为什么单独一份（不进 `_catalog.ts` 的通用巡检套件）
 *
 * 通用套件只验「页面能开、表格有行」；本卡要验的是**这一页独有的四条渲染口径**
 * （全部来自后端 `framework_agreement_execution_dto.rs` 头注释，最容易在渲染层被打回 0）：
 *
 * ① `null` = **不限量**，界面必须写「不限」而不是 0（写成 0 = 「一点都不剩」）；
 * ② `remaining_qty` 负数 = **超上限**，必须原样显形（「超 5」），不许截断到 0；
 * ③ `exec_state` 五态由后端给（`UNLIMITED / NOT_STARTED / PARTIAL / DONE / OVER`），
 *    执行率标签不能拿百分比反推；
 * ④ 空物料范围的协议是 **200 + `items=[]`**，抽屉里要给空态文案，不是错误。
 * 另加两条交互：**只看超限**筛选真的把入参 `over_only=true` 送出去；
 * 下钻里**作废单**要看得见并标「采购单已作废」（否则用户以为量凭空少了）。
 *
 * # 运行方式（必须打隔离实例，`target-guard` 会硬拒 prod）
 *
 * ```bash
 * bash scripts/verify/m411_framework_agreement_ui_drill.sh   # 一键：起库 + 夹具 + 起后端 + 起前端 + 跑本 spec
 * ```
 *
 * 夹具（六份协议 / 四条采购单 / 物料 / 供应商）由上面那个脚本造好，本 spec 只读。
 */
import { expect, test, type Page } from '@playwright/test';

/** 页面路由（`sys_menu` V0452 的 path，与 `router/index.ts` 同名） */
const PAGE_PATH = '/business/erp/framework-agreement';

/** 夹具里的六份协议（脚本 seed 的口径，改一处两边一起改） */
const AG_OVER = 'M411 超限协议';
const AG_PARTIAL = 'M411 部分执行协议';
const AG_UNLIM = 'M411 不限量协议';
const AG_DRAFT = 'M411 拟稿协议';
const AG_EXPIRED = 'M411 过期协议';
const AG_EMPTY = 'M411 无物料协议';

/** 台账 / 执行量两张表的行定位（按协议名，取第一行） */
function rowOf(page: Page, tableTestId: string, name: string) {
  return page.getByTestId(tableTestId).locator('tr', { hasText: name }).first();
}

/**
 * 切 Tab。
 *
 * TDesign 的 `t-tabs` 渲染成 `.t-tabs__nav-item` 的 div（**没有** `role="tab"`），
 * 而且面板是**懒挂载**（不切过去 `ListPage` 根本不存在）—— 所以必须点页签文字，
 * 且限定在 `fwa-tabs` 里，免得撞上侧栏菜单里的同名文字。
 */
async function clickTab(page: Page, label: string): Promise<void> {
  await page.getByTestId('fwa-tabs').getByText(label, { exact: true }).first().click();
}

/**
 * 打开页面并等首屏。
 *
 * ⚠ 必须先把两个弹窗标记塞进 localStorage / sessionStorage：
 * 空库首启时 `SetupWizardDialog` 会因「关键角色没人」自动弹出，它的浮层会 intercept
 * 掉页面上所有点击（表现为「按钮可见但点不动」）；登录后的「未读汇总」弹窗同理。
 * 这是**夹具环境**的特性（生产库角色都有人），不是页面缺陷。
 */
async function openPage(page: Page): Promise<void> {
  await page.addInitScript(() => {
    try {
      window.localStorage.setItem('cloudpivot.setup-wizard.skipped:admin', '1');
      window.sessionStorage.setItem('login-unread-shown', '1');
    } catch {
      /* 隐私模式下 setItem 可能抛错，忽略即可 */
    }
  });
  await page.goto(PAGE_PATH, { waitUntil: 'domcontentloaded' });
  await expect(page.getByTestId('fwa-tabs')).toBeVisible();
  // 兜底：万一标记没生效，把两个弹窗按掉再继续
  const unreadOk = page.getByTestId('login-unread-ok');
  if (await unreadOk.isVisible().catch(() => false)) {
    await unreadOk.click();
    await expect(unreadOk).toBeHidden();
  }
  const wizard = page.getByTestId('setup-wizard-dialog');
  if (await wizard.isVisible().catch(() => false)) {
    await page.keyboard.press('Escape');
    await expect(wizard).toBeHidden();
  }
}

/**
 * 等抽屉滑入动画结束。
 *
 * TDesign 的抽屉是 `transform` 过渡：`kpi` 一可见就截图会拍到「还在滑」的中途位置
 * （演练第一次取证就是这样 —— 抽屉看着只有 740px 宽、右边内容像被裁了，其实只是没滑完）。
 * 判据：`.t-drawer__content-wrapper` 的右边缘贴住视口右边缘（偏差 0）。
 */
async function waitDrawerSettled(page: Page): Promise<void> {
  await expect
    .poll(
      async () => {
        const box = await page
          .getByTestId('fwa-detail-drawer')
          .locator('.t-drawer__content-wrapper')
          .boundingBox();
        if (!box) {
          return -1;
        }
        const viewport = page.viewportSize()?.width ?? 0;
        return Math.round(viewport - (box.x + box.width));
      },
      { message: '抽屉滑入动画没停（右边缘没贴住视口）' },
    )
    .toBe(0);
}

test.describe('框架协议（M4-11）：协议台账 + 执行量', () => {
  test('① 页面能开：两个 Tab 都在，台账 6 份协议，接口全 200', async ({ page }) => {
    const calls: string[] = [];
    page.on('response', (res) => {
      const url = res.url();
      if (url.includes('/api/business/erp/framework-agreement/execution/page')) {
        calls.push(`${res.status()} ${url}`);
      }
    });

    await openPage(page);
    await expect(page.getByTestId('fwa-ledger-table')).toBeVisible();
    // 执行量 Tab 还没切过去 = 那个 ListPage 还没挂载（懒加载，不许多打一次接口）
    await expect(page.getByTestId('fwa-exec')).toHaveCount(0);

    // 六份夹具协议都在台账里
    for (const name of [AG_OVER, AG_PARTIAL, AG_UNLIM, AG_DRAFT, AG_EXPIRED, AG_EMPTY]) {
      await expect(rowOf(page, 'fwa-ledger-table', name)).toBeVisible({ timeout: 15_000 });
    }

    // 页脚的只读说明（协议维护接口未落地——别让人以为按钮丢了）
    await expect(page.getByTestId('fwa-ledger-note')).toContainText('只读台账');

    await clickTab(page, '执行量');
    await expect(page.getByTestId('fwa-exec-table')).toBeVisible();

    await expect.poll(() => calls.length, { message: '列表接口没被调用' }).toBeGreaterThan(1);
    expect(calls.every((line) => line.startsWith('200'))).toBe(true);
  });

  test('② 台账口径：不限量写「不限」、过期写天数、拟稿不因日期而「生效中」', async ({ page }) => {
    await openPage(page);

    // 不限量协议：明细与单头都没有上限 → 数量封顶 / 金额封顶都是「不限」
    const unlim = rowOf(page, 'fwa-ledger-table', AG_UNLIM);
    await expect(unlim).toBeVisible({ timeout: 15_000 });
    await expect(unlim).toContainText('不限');

    // 过期协议：APPROVED 但 2020 年就到期 → 「已过期 N 天」+ 过期标签
    const expired = rowOf(page, 'fwa-ledger-table', AG_EXPIRED);
    await expect(expired).toContainText('已过期');
    await expect(expired).toContainText('2020-12-31');

    // 拟稿协议：日期区间是「今天之内」，但状态是 DRAFT → 只能显示「拟稿（不生效）」
    const draft = rowOf(page, 'fwa-ledger-table', AG_DRAFT);
    await expect(draft).toContainText('拟稿');
    await expect(draft).not.toContainText('生效中');

    // 超限协议：封顶 = 明细合计 100 + 50
    const over = rowOf(page, 'fwa-ledger-table', AG_OVER);
    await expect(over).toContainText('150');
    await expect(over).toContainText('生效中');
  });

  test('③ 执行量口径：超限显示「超 5」+ 103.33%，不限量显示「不限」+ 不限量', async ({ page }) => {
    await openPage(page);
    await clickTab(page, '执行量');
    await expect(page.getByTestId('fwa-exec-table')).toBeVisible();

    // 超限协议：已用 155 / 封顶 150 → 剩余 **-5** 必须显形（不是 0），率 103.33%
    const over = rowOf(page, 'fwa-exec-table', AG_OVER);
    await expect(over).toBeVisible({ timeout: 15_000 });
    await expect(over).toContainText('超 5');
    await expect(over).toContainText('103.33%');
    await expect(over).toContainText('已超限');
    await expect(over).toContainText('超限 1');

    // 部分执行协议：剩余 30、40.00%、部分执行
    const partial = rowOf(page, 'fwa-exec-table', AG_PARTIAL);
    await expect(partial).toContainText('40.00%');
    await expect(partial).toContainText('部分执行');

    // 不限量协议：剩余 / 执行率都是 null → 界面上必须是一个「不限」也没有百分比
    const unlim = rowOf(page, 'fwa-exec-table', AG_UNLIM);
    await expect(unlim).toContainText('不限');
    await expect(unlim).toContainText('不限量');
    await expect(unlim).not.toContainText('0.00%');
  });

  test('④ 「只看超限」筛选：入参真的带 over_only=true，列表只剩 1 份', async ({ page }) => {
    const execCalls: string[] = [];
    page.on('response', (res) => {
      const url = res.url();
      if (url.includes('/api/business/erp/framework-agreement/execution/page')) {
        execCalls.push(url);
      }
    });

    await openPage(page);
    await clickTab(page, '执行量');
    await expect(page.getByTestId('fwa-exec-table')).toBeVisible();
    await expect(rowOf(page, 'fwa-exec-table', AG_PARTIAL)).toBeVisible({ timeout: 15_000 });

    await page.getByTestId('fwa-filter-over').click();

    await expect
      .poll(() => execCalls.some((url) => url.includes('over_only=true')), {
        message: '勾「只看超限」后没有把 over_only=true 发给后端',
      })
      .toBe(true);

    await expect(rowOf(page, 'fwa-exec-table', AG_OVER)).toBeVisible({ timeout: 15_000 });
    await expect(rowOf(page, 'fwa-exec-table', AG_PARTIAL)).toHaveCount(0);
    await expect(rowOf(page, 'fwa-exec-table', AG_UNLIM)).toHaveCount(0);
  });

  test('⑤ 下钻抽屉：KPI「超 5」、逐行「超 10」、来源里看得见那张作废单', async ({ page }) => {
    await openPage(page);
    await clickTab(page, '执行量');
    await expect(page.getByTestId('fwa-exec-table')).toBeVisible();

    const row = rowOf(page, 'fwa-exec-table', AG_OVER);
    await expect(row).toBeVisible({ timeout: 15_000 });
    await row.getByTestId('fwa-exec-detail').click();

    await expect(page.getByTestId('fwa-kpi')).toBeVisible();
    // 单头剩余 -5：KPI 上必须写「超 5」，并带 is-over 标红类
    const kpiRemaining = page.getByTestId('fwa-kpi-remaining');
    await expect(kpiRemaining).toHaveText('超 5.0000');
    await expect(kpiRemaining).toHaveClass(/is-over/);
    await expect(page.getByTestId('fwa-kpi-rate')).toHaveText('103.33%');

    // 逐行：第 1 行 100 封顶、已用 110 → 剩余「超 10」
    const items = page.getByTestId('fwa-detail-items');
    await expect(items.locator('tr')).toHaveCount(3); // 表头 + 2 行明细
    await expect(items).toContainText('M411-MAT-A');
    await expect(items).toContainText('超 10');
    // 第 2 行设了上限 50、已用 45 → 部分执行
    await expect(items).toContainText('部分执行');

    // 下钻来源：4 行（3 张计入 + 1 张作废），作废那行给原因
    const sources = page.getByTestId('fwa-detail-sources');
    await expect(sources).toContainText('PO-M411-0001');
    await expect(sources).toContainText('PO-M411-0004');
    await expect(sources).toContainText('采购单已作废');
    // 汇总行：计入 3 行 · 释放 1 行
    await expect(page.getByTestId('fwa-sources-summary')).toContainText('计入 3 行');
    await expect(page.getByTestId('fwa-sources-summary')).toContainText('释放 1 行');
  });

  test('⑥ 空物料范围的协议：抽屉给空态文案（200 + items=[]，不是 404）', async ({ page }) => {
    await openPage(page);

    const row = rowOf(page, 'fwa-ledger-table', AG_EMPTY);
    await expect(row).toBeVisible({ timeout: 15_000 });
    await row.getByTestId('fwa-ledger-detail').click();

    await expect(page.getByTestId('fwa-kpi')).toBeVisible();
    // 没有明细 → 没有封顶 → KPI 剩余「不限」（null 不是 0）
    await expect(page.getByTestId('fwa-kpi-remaining')).toHaveText('不限');
    await expect(page.getByTestId('fwa-detail-error')).toHaveCount(0);
    await expect(page.getByTestId('fwa-detail-items-empty')).toBeVisible();
    await expect(page.getByTestId('fwa-detail-sources-empty')).toBeVisible();
  });

  test('⑦ 取证截图（台账 / 执行量 / 下钻抽屉），给卡内汇报留现场', async ({ page }) => {
    const dir = process.env.M411_SHOT_DIR ?? 'test-results/m411-shots';
    await openPage(page);
    await expect(rowOf(page, 'fwa-ledger-table', AG_OVER)).toBeVisible({ timeout: 15_000 });
    await page.screenshot({ path: `${dir}/01-ledger-list.png`, fullPage: true });

    await clickTab(page, '执行量');
    await expect(page.getByTestId('fwa-exec-table')).toBeVisible();
    await expect(rowOf(page, 'fwa-exec-table', AG_OVER)).toBeVisible({ timeout: 15_000 });
    await page.screenshot({ path: `${dir}/02-execution-list.png`, fullPage: true });

    await rowOf(page, 'fwa-exec-table', AG_OVER).getByTestId('fwa-exec-detail').click();
    await expect(page.getByTestId('fwa-kpi')).toBeVisible();
    await waitDrawerSettled(page);
    await page.screenshot({ path: `${dir}/03-execution-drill.png`, fullPage: true });

    // 关抽屉：TDesign 的关闭图标没有可访问名，点类名又随版本变 —— 用 Esc（drawer 默认关）
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('fwa-kpi')).toBeHidden({ timeout: 10_000 });
    await rowOf(page, 'fwa-exec-table', AG_UNLIM).getByTestId('fwa-exec-detail').click();
    await expect(page.getByTestId('fwa-kpi-remaining')).toHaveText('不限');
    await waitDrawerSettled(page);
    await page.screenshot({ path: `${dir}/04-unlimited-drill.png`, fullPage: true });
  });
});

/**
 * 采购计划页面真实交互（M4-06：录入 + 执行率）。
 *
 * # 这条 spec 为什么单独一份（不进 `_catalog.ts` 的 52 页套件）
 *
 * 那套是「每页 6 项」的通用巡检，本卡要验的是**这个页面独有的四条链路**：
 * ① 列表能开、两个 Tab 都在（录入 / 执行率）；
 * ② 通过**表单真建一张计划**（真 POST，不是 mock），列表立刻能看到；
 * ③ 「物料 × 仓库」重复这类坏输入在**前端**就被拦住，且报文点出第几行撞第几行；
 * ④ 执行率 Tab 的数字与状态来自后端视图：已下满的计划显示 100.00% + 「已下满」，
 *    下钻抽屉里「来源单据」能看出这一格被哪张申请单 / 采购单吃掉。
 *
 * # 运行方式（必须打隔离实例，`target-guard` 会硬拒 prod）
 *
 * ```bash
 * bash scripts/verify/m406_purchase_plan_ui_drill.sh      # 一键：起库 + 起后端 + 起前端 + 跑本 spec
 * ```
 *
 * 夹具（计划 / 申请单 / 采购单 / 物料 / 仓库）由上面那个脚本用真接口造好，
 * 本 spec 只**读**它们 + 自己新建一张草稿（在临时库里，不影响任何人）。
 */
import { expect, test, type Page } from '@playwright/test';

/** 页面路由（`sys_menu` V0447 的 path，与 `router/index.ts` 同名） */
const PAGE_PATH = '/business/erp/purchase-plan';

/** 夹具里那张「已批准 + 已下满」的计划名（脚本用这个关键字造数据） */
const SEEDED_APPROVED = 'M406 已下满计划';

/** 夹具里的物料编码 / 仓库名（脚本 seed 的口径；下拉里物料是「编码 + 名称」、仓库只有名称） */
const MAT = 'M406-MAT-A';
const WH = 'M406 仓库 A';

/** 列表里某张计划所在的行 */
function rowOf(page: Page, planName: string) {
  return page.getByTestId('pp-table').locator('tr', { hasText: planName }).first();
}

/**
 * 切 Tab。
 *
 * TDesign 的 `t-tabs` 渲染成 `.t-tabs__nav-item` 的 div（**没有** `role="tab"`），
 * 所以不能用 `getByRole('tab')` —— 按页签文字点，且限定在 `pp-tabs` 里，
 * 免得撞上侧栏菜单里的同名文字。
 */
async function clickTab(page: Page, label: string): Promise<void> {
  await page.getByTestId('pp-tabs').getByText(label, { exact: true }).first().click();
}

/**
 * 选中一个下拉项。
 *
 * ⚠ 不能直接 `getByText(...)`：上一个下拉的浮层 DOM 还留在 body 里（只是 hidden），
 * `first()` 会命中那个 hidden 的旧选项（报「element is not visible」）。
 * 这里限定 `.t-select-option:visible`，只点当前打开的这一个浮层。
 */
async function pickOption(page: Page, text: string): Promise<void> {
  await page.locator('.t-select-option:visible').filter({ hasText: text }).first().click();
}

/**
 * 打开页面并等首屏（列表接口返回后表格才有行）。
 *
 * ⚠ 必须先把「首次启动向导」的跳过标记塞进 localStorage：
 * 空库首启时 `SetupWizardDialog` 会因为「关键角色没人」自动弹出，它的
 * `t-dialog__position` 会 intercept 掉页面上所有点击（表现为「按钮可见但点不动」）。
 * 这是**夹具环境**的特性（生产库里角色都有人），不是页面缺陷。
 */
async function openPage(page: Page): Promise<void> {
  await page.addInitScript(() => {
    try {
      // 首次启动向导（空库首启：关键角色没人时会弹）
      window.localStorage.setItem('cloudpivot.setup-wizard.skipped:admin', '1');
      // 登录后的「未读汇总」弹窗（会挡住第一次点击）
      window.sessionStorage.setItem('login-unread-shown', '1');
    } catch {
      /* 隐私模式下 setItem 可能抛错，忽略即可 */
    }
  });
  await page.goto(PAGE_PATH, { waitUntil: 'domcontentloaded' });
  await expect(page.getByTestId('pp-tabs')).toBeVisible();
  // 兜底：万一标记没生效（换了账号 / 不同会话），把两个弹窗按掉再继续
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

test.describe('采购计划（M4-06）：录入 + 执行率', () => {
  test('① 页面能开：两个 Tab 都在，列表接口 200 且表格有行', async ({ page }) => {
    const listCalls: string[] = [];
    page.on('response', (res) => {
      const url = res.url();
      if (url.includes('/api/business/erp/purchase-plan/page')) {
        listCalls.push(`${res.status()} ${url}`);
      }
    });

    await openPage(page);
    await expect(page.getByTestId('pp-table')).toBeVisible();
    await expect(page.getByTestId('pp-exec')).toHaveCount(0);

    // 切到执行率 Tab：另一个 ListPage 才挂载
    await clickTab(page, '执行率');
    await expect(page.getByTestId('pp-exec-table')).toBeVisible();

    await expect.poll(() => listCalls.length, { message: '列表接口没被调用' }).toBeGreaterThan(0);
    expect(listCalls.every((line) => line.startsWith('200'))).toBe(true);

    // 回到录入 Tab，夹具计划在列表里
    await clickTab(page, '采购计划');
    await expect(page.getByTestId('pp-table')).toBeVisible();
    await expect(rowOf(page, SEEDED_APPROVED)).toBeVisible();
  });

  test('② 通过表单真建一张计划（真 POST），列表立刻出现', async ({ page }) => {
    const name = `M406 UI 新建 ${Date.now()}`;
    const posts: string[] = [];
    page.on('response', (res) => {
      if (res.url().endsWith('/api/business/erp/purchase-plan') && res.request().method() === 'POST') {
        posts.push(`${res.status()} ${res.url()}`);
      }
    });

    await openPage(page);
    await page.getByTestId('btn-pp-new').click();
    await expect(page.getByTestId('pp-form-dialog')).toBeVisible();

    await page.getByTestId('pp-form-name').locator('input').fill(name);

    // 明细第 1 行：物料 / 仓库 / 计划量 / 单价
    await page.getByTestId('pp-line-material').first().click();
    await pickOption(page, MAT);
    await page.getByTestId('pp-line-warehouse').first().click();
    await pickOption(page, WH);
    await page.getByTestId('pp-line-qty').first().locator('input').fill('10');
    await page.getByTestId('pp-line-price').first().locator('input').fill('2.5');

    // 合计预览（与后端 Σ 明细同口径）—— 前端只显示，不当真值
    await expect(page.getByTestId('pp-form-totals')).toContainText('10.0000');

    await page.getByRole('button', { name: '保存草稿' }).click();
    await expect(page.getByText(/已保存 PPL-/)).toBeVisible({ timeout: 15_000 });
    expect(posts.some((line) => line.startsWith('200'))).toBe(true);

    // 列表刷新后能看到新建的这张（状态草稿）
    await expect(rowOf(page, name)).toBeVisible({ timeout: 15_000 });
    await expect(rowOf(page, name)).toContainText('草稿');
  });

  test('③ 同一个「物料 × 仓库」写两行：前端当场拦住并点出行号', async ({ page }) => {
    await openPage(page);
    await page.getByTestId('btn-pp-new').click();
    await expect(page.getByTestId('pp-form-dialog')).toBeVisible();
    await page.getByTestId('pp-form-name').locator('input').fill('M406 UI 重复行校验');

    // 第 1 行
    await page.getByTestId('pp-line-material').first().click();
    await pickOption(page, MAT);
    await page.getByTestId('pp-line-warehouse').first().click();
    await pickOption(page, WH);
    await page.getByTestId('pp-line-qty').first().locator('input').fill('1');

    // 加一行，选成同一格
    await page.getByTestId('btn-pp-add-line').click();
    await page.getByTestId('pp-line-material').nth(1).click();
    await pickOption(page, MAT);
    await page.getByTestId('pp-line-warehouse').nth(1).click();
    await pickOption(page, WH);
    await page.getByTestId('pp-line-qty').nth(1).locator('input').fill('2');

    await page.getByRole('button', { name: '保存草稿' }).click();
    // 报文与后端 check_cells_unique 同口径：第 2 行与第 1 行
    await expect(page.getByText(/第 2 行与第 1 行是同一个「物料 × 仓库」/)).toBeVisible();
  });

  test('④ 执行率 Tab：已下满的计划 100.00% + 已下满，下钻能看出被哪张单吃掉', async ({ page }) => {
    const execCalls: string[] = [];
    page.on('response', (res) => {
      if (res.url().includes('/api/business/erp/purchase-plan/execution/page')) {
        execCalls.push(`${res.status()} ${res.url()}`);
      }
    });

    await openPage(page);
    await clickTab(page, '执行率');
    await expect(page.getByTestId('pp-exec-table')).toBeVisible();

    const row = page.getByTestId('pp-exec-table').locator('tr', { hasText: SEEDED_APPROVED }).first();
    await expect(row).toBeVisible({ timeout: 15_000 });
    // 夹具：计划 10 + 4，全部转申请并合并下单 → 100.00% / 已下满
    await expect(row).toContainText('100.00%');
    await expect(row).toContainText('已下满');
    expect(execCalls.some((line) => line.startsWith('200'))).toBe(true);

    // 下钻抽屉：单头 KPI + 逐行 + 展开看来源单据
    await row.getByTestId('btn-pp-exec-drill').click();
    await expect(page.getByTestId('pp-exec-kpi')).toBeVisible();
    await expect(page.getByTestId('pp-exec-items')).toBeVisible();

    await page.getByTestId('pp-exec-items').locator('tr').nth(1).locator('.t-table__expand-box').click();
    await expect(page.getByText('已下单').first()).toBeVisible();
    // 来源里必须出现采购单号（PO-…），证明「已下单」是单据链算出来的
    await expect(page.getByTestId('pp-exec-items')).toContainText('PO-');
  });

  test('⑤ 转采购申请：已全转的计划剩余量为 0，直接确认被前端拦住', async ({ page }) => {
    await openPage(page);
    const row = rowOf(page, SEEDED_APPROVED);
    await expect(row).toBeVisible({ timeout: 15_000 });
    await row.getByTestId('btn-pp-to-req').click();

    await expect(page.getByTestId('pp-to-req-dialog')).toBeVisible();
    // 明细里剩余可转是 0.0000（夹具已全转），且默认不勾选 → 确认时被人话拦住
    await expect(page.getByTestId('pp-to-req-lines')).toContainText('0.0000');
    await page.getByRole('button', { name: '生成采购申请' }).click();
    await expect(page.getByText(/至少勾选一行要转出的明细/)).toBeVisible();
  });

  test('⑥ 取证截图（录入列表 / 执行率 / 下钻抽屉），给卡内汇报留现场', async ({ page }) => {
    const dir = process.env.M406_SHOT_DIR ?? 'test-results/m406-shots';
    await openPage(page);
    await expect(rowOf(page, SEEDED_APPROVED)).toBeVisible({ timeout: 15_000 });
    await page.screenshot({ path: `${dir}/01-plan-list.png`, fullPage: true });

    await clickTab(page, '执行率');
    await expect(page.getByTestId('pp-exec-table')).toBeVisible();
    await page.screenshot({ path: `${dir}/02-execution-overview.png`, fullPage: true });

    const row = page.getByTestId('pp-exec-table').locator('tr', { hasText: SEEDED_APPROVED }).first();
    await row.getByTestId('btn-pp-exec-drill').click();
    await expect(page.getByTestId('pp-exec-kpi')).toBeVisible();
    await page.screenshot({ path: `${dir}/03-execution-drill.png`, fullPage: true });

    await page.getByTestId('pp-exec-items').locator('tr').nth(1).locator('.t-table__expand-box').click();
    await expect(page.getByTestId('pp-exec-items')).toContainText('PO-');
    await page.screenshot({ path: `${dir}/04-execution-sources.png`, fullPage: true });
  });
});

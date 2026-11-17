/**
 * 采购申请（P2-02）建单明细：**插槽名 = colKey** 的真浏览器回归（t_8dcb79d5）。
 *
 * # 这条 spec 存在的唯一理由
 *
 * `t-table` 的单元格插槽名必须**逐字等于**列 `colKey`（TDesign 按 `colKey` 找 `#<colKey>`）。
 * 初版建单明细写成 `#lineMaterial` / `#lineQty` / …，而 `lineColumns` 的 `colKey` 是
 * `material_id` / `qty` / …，于是 7 个槽子**静默失效**：明细行渲染成裸值 ——
 * 物料列空白、数量是文本 `1`、下拉 / 日期选择器 / 删除链接全都不存在。
 *
 * `vue-tsc`（pnpm typecheck）与 `pnpm build` 对这种写法**都是绿的**：
 * 模板里多一个没人用的具名插槽不是类型错误。所以验收只能是真浏览器：
 * 「`getByTestId('prq-line-material')` 能点开下拉、数量输入框能填进去」= 槽子真的生效了。
 *
 * # 运行方式（必须打隔离实例，`target-guard` 会硬拒 prod）
 *
 * ```bash
 * bash scripts/verify/m4_prq_lines_ui_drill.sh   # 一键：建库 + 起后端 + 起前端 + 跑本 spec
 * ```
 *
 * 夹具（部门 / 仓库 / 物料 / 供应商）由上面那个脚本用 SQL 造好；本 spec 自己建单，
 * 在临时库里，不影响任何人。
 */
import { expect, test, type Page } from '@playwright/test';

/** 页面路由（`sys_menu` V0331 的 path，与 `router/index.ts` 同名） */
const PAGE_PATH = '/business/erp/purchase-requisition';

/** 夹具（脚本 seed 的口径）：物料下拉是「编码 + 名称」、仓库/部门/供应商只有名称 */
const MAT = 'M4PRQ-MAT-A';
const WH = 'M4PRQ 仓库 A';
const DEPT = 'M4PRQ 采购部';
const SUPPLIER = 'M4PRQ 供应商 A';

/** 建单弹窗 */
function dialog(page: Page) {
  return page.getByTestId('prq-create-dialog');
}

/** 明细表格里的第 n 行（0 基） */
function lineRow(page: Page, n: number) {
  return page.getByTestId('prq-lines-table').locator('tbody tr').nth(n);
}

/**
 * 选中一个下拉项。
 *
 * ⚠ 不能直接 `getByText(...)`：上一个下拉的浮层 DOM 还留在 body 里（只是 hidden），
 * `first()` 会命中那个 hidden 的旧选项（报「element is not visible」）。
 * 限定 `.t-select-option:visible`，只点当前打开的这个浮层。
 */
async function pickOption(page: Page, text: string): Promise<void> {
  await page.locator('.t-select-option:visible').filter({ hasText: text }).first().click();
}

/** 点开一个 `t-select` 并选中一项（testid 由页面挂在组件根上） */
async function selectByTestId(page: Page, testid: string, optionText: string, nth = 0): Promise<void> {
  await page.getByTestId(testid).nth(nth).click();
  await pickOption(page, optionText);
}

/**
 * 读 `t-select` 当前选中值。
 *
 * ⚠ 不能断言 `toContainText`：TDesign 把 `data-testid` 落在 `.t-select__wrap` 上，
 * 选中项的文字在里面的只读 `<input>` 的 value 里（`textContent` 是空串）。
 */
async function expectSelectValue(page: Page, testid: string, text: string, nth = 0): Promise<void> {
  await expect(
    page.getByTestId(testid).nth(nth).locator('input').first(),
    `下拉 ${testid} 没显示选中的「${text}」`,
  ).toHaveValue(new RegExp(text));
}

/**
 * 往 `t-input-number` 里填一个数。
 *
 * ⚠ 不能直接 `.fill()`：初版这么写，默认值 `1` 与 `fill('3')` 拼成了 `13`
 * （受控组件在 input 事件后还会用旧 `modelValue` 回写一次，fill 的一次性赋值会被吞）。
 * 真按键盘：点进去 → 全选 → 删除 → 逐字敲 → 失焦（失焦才 commit）。
 */
async function setNumberValue(page: Page, testid: string, value: string, nth = 0): Promise<void> {
  const input = page.getByTestId(testid).nth(nth).locator('input');
  await input.click();
  await input.press('ControlOrMeta+a');
  await input.press('Backspace');
  await input.pressSequentially(value, { delay: 20 });
  await input.blur();
  // ⚠ 别断言字面值：`t-input-number` 按 `decimal-places` 补齐（数量 4 位、单价 6 位），
  // 敲 `3` 显示的是 `3.0000`。按数值比。
  const shown = await input.inputValue();
  expect(Number(shown), `${testid} 没接受输入值 ${value}（显示「${shown}」）`).toBe(Number(value));
}

/**
 * 打开页面并等首屏。
 *
 * ⚠ 必须先把「首次启动向导」的跳过标记塞进 localStorage：
 * 空库首启时 `SetupWizardDialog` 会因为「关键角色没人」自动弹出，它的
 * `t-dialog__position` 会 intercept 掉页面上所有点击（表现为「按钮可见但点不动」）。
 * 这是**夹具环境**的特性（生产库里角色都有人），不是页面缺陷。
 */
async function openPage(page: Page): Promise<void> {
  await page.addInitScript(() => {
    try {
      window.localStorage.setItem('cloudpivot.setup-wizard.skipped:admin', '1');
      window.sessionStorage.setItem('login-unread-shown', '1');
      window.localStorage.setItem('cp-onboarding-done', '1');
    } catch {
      /* 隐私模式下 setItem 可能抛错，忽略即可 */
    }
  });
  await page.goto(PAGE_PATH, { waitUntil: 'domcontentloaded' });
  await expect(page.getByTestId('prq-table')).toBeVisible();
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

/** 打开「新建申请」弹窗 */
async function openCreate(page: Page): Promise<void> {
  await openPage(page);
  await page.getByTestId('btn-prq-new').click();
  await expect(dialog(page)).toBeVisible();
  await expect(page.getByTestId('prq-lines-table')).toBeVisible();
}

test.describe('采购申请（P2-02）：建单明细插槽 = colKey', () => {
  test('① 明细行真的有输入控件：物料/数量/单价/税率/需求日期/建议供应商/删除', async ({ page }) => {
    await openCreate(page);

    // 明细表格必须有 1 行（空表单默认 1 行）
    await expect(lineRow(page, 0)).toBeVisible();

    // —— 7 个单元格控件逐个「可见」：槽子失效时它们根本不在 DOM 里 ——
    const controls = [
      'prq-line-material',
      'prq-line-qty',
      'prq-line-price',
      'prq-line-tax',
      'prq-line-demand',
      'prq-line-supplier',
    ];
    for (const testid of controls) {
      await expect(page.getByTestId(testid).first(), `明细控件 ${testid} 没渲染（插槽名与 colKey 不一致 = 静默失效）`).toBeVisible();
    }
    // 删除链接也在（它是 #ops 槽子里的 t-link）
    await expect(lineRow(page, 0).getByText('删除', { exact: true })).toBeVisible();

    // —— 物料下拉能点开并选中（真交互） ——
    await page.getByTestId('prq-line-material').first().click();
    await expect(page.locator('.t-select-option:visible').first()).toBeVisible();
    await pickOption(page, MAT);
    // 选完以后单元格里显示的是「编码 + 名称」而不是空白
    await expectSelectValue(page, 'prq-line-material', MAT);

    // —— 数量 t-input-number 能填（这就是「明细行渲染成裸值」最直观的反证） ——
    await setNumberValue(page, 'prq-line-qty', '3');

    // —— 单价 / 税率同理 ——
    await setNumberValue(page, 'prq-line-price', '2.5');
    await setNumberValue(page, 'prq-line-tax', '13');

    // —— 需求日期是 t-date-picker：点开面板选一天，输入框要回填 ——
    await page.getByTestId('prq-line-demand').first().click();
    await expect(page.locator('.t-date-picker__panel')).toBeVisible();
    const dateInput = page.getByTestId('prq-line-demand').first().locator('input');
    await page
      .locator('.t-date-picker__panel .t-date-picker__cell:not(.t-date-picker__cell--additional)')
      .last()
      .click();
    await expect(dateInput).toHaveValue(/\d{4}-\d{2}-\d{2}/);

    // —— 建议供应商下拉也能用 ——
    await selectByTestId(page, 'prq-line-supplier', SUPPLIER);
    await expectSelectValue(page, 'prq-line-supplier', SUPPLIER);
  });

  test('② 空提交被拦：先「请选择申请部门」，补单头后「第 1 行：请选择物料」', async ({ page }) => {
    await openCreate(page);

    // 什么都不填直接保存 → 被前端校验拦下（弹窗不关，明细也没被清空）
    await page.getByRole('button', { name: '保存草稿' }).click();
    await expect(page.getByText('请选择申请部门').first()).toBeVisible();
    await expect(dialog(page)).toBeVisible();

    // 补上单头两个必填
    await selectByTestId(page, 'prq-create-dept', DEPT);
    await selectByTestId(page, 'prq-create-warehouse', WH);

    // 明细一行都没选物料（默认行 qty=1） → 仍被拦，且点出行号
    await page.getByRole('button', { name: '保存草稿' }).click();
    await expect(page.getByText(/第 1 行：请选择物料/).first()).toBeVisible();
    await expect(dialog(page)).toBeVisible();
  });

  test('③ 明细控件绑的值真的进了报文：填满一行 → 真 POST 200 → 列表出现单号', async ({ page }) => {
    await openCreate(page);

    await selectByTestId(page, 'prq-create-dept', DEPT);
    await selectByTestId(page, 'prq-create-warehouse', WH);
    await selectByTestId(page, 'prq-line-material', MAT);

    await setNumberValue(page, 'prq-line-qty', '3');
    await setNumberValue(page, 'prq-line-price', '2.5');

    const [resp] = await Promise.all([
      page.waitForResponse(
        (res) => res.url().endsWith('/api/business/erp/purchase-requisition') && res.request().method() === 'POST',
      ),
      page.getByRole('button', { name: '保存草稿' }).click(),
    ]);
    expect(resp.status(), '建单接口必须 200（槽子没生效时这里会 400 / 空白明细）').toBe(200);

    // 报文里的明细就是刚才在表格里填的值 —— 证明 v-model 真的接到了控件上
    const sent = JSON.parse(resp.request().postData() ?? '{}');
    const line = (sent.items ?? sent.lines ?? [])[0] ?? {};
    expect(String(line.qty ?? ''), '报文明细数量').toMatch(/^3(\.0+)?$/);
    expect(String(line.est_price ?? ''), '报文明细单价').toMatch(/^2\.5(0+)?$/);
    expect(line.material_id, '报文明细物料').toBeTruthy();

    const body = (await resp.json()) as { data?: { requisition?: { req_no?: string } } };
    const reqNo = body.data?.requisition?.req_no ?? '';
    expect(reqNo).toMatch(/^PRQ-/);
    await expect(dialog(page)).toBeHidden();

    // 列表刷新后能看到这张单
    await expect(page.getByTestId('prq-table')).toContainText(reqNo, { timeout: 15_000 });
  });

  test('④ 加一行 / 删一行：新行的控件同样在，删除回到 1 行', async ({ page }) => {
    await openCreate(page);

    await page.getByTestId('btn-prq-add-line').click();
    await expect(page.getByTestId('prq-line-material')).toHaveCount(2);
    // 第 2 行同样是真控件（不是裸值）
    await expect(page.getByTestId('prq-line-qty').nth(1).locator('input')).toBeVisible();

    await lineRow(page, 1).getByText('删除', { exact: true }).click();
    await expect(page.getByTestId('prq-line-material')).toHaveCount(1);
    await expect(lineRow(page, 0)).toBeVisible();
  });

  test('⑤ 取证截图（明细行控件 / 物料下拉展开），给卡内汇报留现场', async ({ page }) => {
    const dir = process.env.M4PRQ_SHOT_DIR ?? 'test-results/m4prq-shots';
    await openCreate(page);

    await selectByTestId(page, 'prq-create-dept', DEPT);
    await selectByTestId(page, 'prq-create-warehouse', WH);
    await selectByTestId(page, 'prq-line-material', MAT, 0);
    await setNumberValue(page, 'prq-line-qty', '3');
    await setNumberValue(page, 'prq-line-price', '2.5');
    await page.screenshot({ path: `${dir}/01-lines-controls.png`, fullPage: true });

    // 物料下拉真的能展开（槽子失效时这里连 .t-select 都没有，面板永远出不来）
    await page.getByTestId('prq-line-material').first().click();
    await expect(page.locator('.t-select-option:visible').first()).toBeVisible();
    await page.screenshot({ path: `${dir}/02-material-dropdown.png`, fullPage: true });
  });
});

/**
 * 12 · 复现 2026-10-31 11:34 那条「UI 业务闭环走查」（**只能在隔离实例上跑**）
 *
 * # 为什么要把它写成用例
 *
 * 11:34:11 prod 被从 172.18.10.7 的 mac 浏览器（admin 账号）点了一次
 * 「ERP → 序列号 → 批量生成」（M-W1A-004 / WH01 / 10 件）。**当时的接口**不录成本、也不写库位账，
 * 所以落库后 `erp_invariants.py` 当场 **B1（BLOCKER）+ C3（WARN）** 变红：
 *   ① `biz_material_serial` 10 行 IN_STOCK、source_type=PURCHASE、source_id IS NULL；
 *   ② `biz_inventory` qty 涨 10，但 amount / avg_cost 全 0，且**不写库位账**；
 *   ③ 同一次走查还建了一张调拨单（TK-20261031-000007，自动起审批实例）并批了一张审批单。
 *
 * # 断言口径（2026-10-31 拍板后改过一次，**别改回去**）
 *
 * ADR `docs/adr-20261031-erp-serial-batch-generate-semantics.md` §3（+§6 R1/R2）拍板：
 * **批量生成 = 无来源入库（期初 / 其他入库），必录成本单价，同事务写两本账（成本账 + 库位账）+ 凭证。**
 * 于是判据从旧的「生成 → 零成本 → B1/C3 红」改成：
 *
 *   生成 N 件 @unit_cost ⇒ `avg_cost = unit_cost`、`amount = N × unit_cost`、
 *   `biz_inventory.qty = N`、库位账合计 = N、在库序列号件数 = N ⇒ **B1 / B2 / C3 绿（32/32）**。
 *
 * 两侧都要断（少一侧就是漏测）：
 *   - 「不填单价」这条路：UI 本地拦（一个请求都不发）+ 后端直调 `400 / code 10001` 逐字报文，且零落库；
 *   - 「填了单价」这条路：UI 走查 → 出参回显 → 页面数字 → 库内两本账 / 流水逐个对（见 ②）。
 *
 * 三账不变量（32/32，含 B1/B2/C3）由本文件最后一条用例直接跑
 * `scripts/verify/erp_invariants.py` 断言：要 `E2E_RUN_INVARIANTS=1` 且 `NIK_DATABASE_*` 指向**副本库**
 * （没设就 skip，避免默认连到 prod 库）。完整闸门仍是 `bash scripts/e2e-postrun-check.sh`。
 *
 * 这条业务链路是真实用户会走的，**不能删**；要防的是「在 prod 上走」。所以：
 *   - 用例保留完整链路（登录 → 批量生成 → 调拨建单提交 → 待办同意），
 *     以后前端/后端改动都能在这条链路上回归；
 *   - 目标由 `frontend/e2e/target-guard.mjs` 硬判据兜住：BASE_URL 不是回环隔离端口
 *     直接抛错（本用例开头还有第二道自检，见 beforeAll）；
 *   - 用例跑完会在**目标库**留下一批**带真实成本的在库序列号** + 一张调拨单，
 *     因此只允许打在副本库（`scripts/verify/e2e_isolated_instance.sh`）。
 *     ⚠️ 这批库存是**真实入库**：`e2e_fixture_residue.py` 第 ④ 道「库存零价值」不会再命中它，
 *     走查点出来的数据要撤只能走红冲 / 盘点，**别指望 P10 清** —— 见 `scripts/verify/README.md`。
 *
 * # 数据前提（副本库来自 prod 快照）
 * - 物料 `M-W1A-004`：serial_required = true（序列号弹窗只列这类物料）；
 * - 仓库 `WH01`（总仓，非叶子）/ `WH02`（分仓，叶子）；
 * - 物料 `M-W1A-001`：非一物一码，且 WH01 有 8 件库存 → 调拨用它，免选序列号；
 * - 测试账号 `laoliu_test` 是 SUPER_ADMIN → 调拨单提交后自己就在「待我审批」里（可自审）。
 */
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { expect, test, request, type APIRequestContext } from '@playwright/test';
import { BASE_URL, openAuthed, toast, readSharedToken } from './support';

/** 本次运行戳：调拨原因里带它，便于在副本库里定位本次造的数据 */
const RUN = `E2E-P11G-${Date.now().toString(36).toUpperCase()}`;
const SERIAL_MATERIAL = 'M-W1A-004';
const PLAIN_MATERIAL = 'M-W1A-001';
const FROM_WH = 'WH01';
const TO_WH = 'WH02';
const BATCH_COUNT = 10;
/**
 * 成本单价（不含税，> 0，≤ 6 位小数）—— ADR §3：批量生成 = 无来源入库，**必录**。
 * 不填的后果见 ①（UI 本地拦 + 后端 400），填了的结果见 ②（两本账 / 流水 / 不变式①）。
 */
const UNIT_COST = 12.34;
/** 本次入库金额 = 件数 × 单价，4 位小数（与 `biz_inventory.amount` 同精度） */
const EXPECT_AMOUNT = (BATCH_COUNT * UNIT_COST).toFixed(4); // 123.4000 —— 反向对照时临时改成 '0.0000'
/** 单价 6 位小数形态（流水 `unit_cost` / 成本账 `avg_cost` 的口径） */
const EXPECT_UNIT_COST = UNIT_COST.toFixed(6); // 12.340000

interface Ctx {
  ctx: APIRequestContext;
  get: (url: string) => Promise<any>;
  /** 原样返回状态码 + 报文：要断 400 这类失败分支，不能走 `get()` 的 `code === 0` 断言 */
  post: (url: string, data: unknown) => Promise<{ status: number; json: any }>;
}

/**
 * 「物料 × 仓库」的账实快照（全部走只读接口，不碰数据库连接）。
 *
 * 对应 `erp_invariants.py` 的判据口径：
 *   - `qty` / `amount` ← `biz_inventory` 按 物料×仓库 **求和**（B1/B4/C5 都是 sum 口径）；
 *   - `locQty` ← `biz_inventory_location` 求和（B1 的等式另一边）；
 *   - `instock` ← `biz_material_serial` 里 `status=IN_STOCK AND deleted_at IS NULL` 的件数（不变式① / C5）；
 *   - `avgCost` ← 金额 / 数量（成本账均价；数量为 0 时按 0 算）。
 */
interface LedgerSnapshot {
  rows: any[];
  qty: number;
  amount: number;
  avgCost: number;
  locQty: number;
  instock: number;
}

async function ledgerSnapshot(c: Ctx, materialId: string, warehouseId: string): Promise<LedgerSnapshot> {
  const inv = await c.get(
    `/api/business/erp/inventory/page?material_id=${materialId}&warehouse_id=${warehouseId}&page=1&page_size=200`,
  );
  const rows: any[] = inv.list ?? [];
  const loc = await c.get(
    `/api/business/erp/inventory-location/page?material_id=${materialId}&warehouse_id=${warehouseId}&page=1&page_size=200`,
  );
  const ser = await c.get(
    `/api/business/erp/serial/page?material_id=${materialId}&warehouse_id=${warehouseId}&status=IN_STOCK&page=1&page_size=200`,
  );
  const qty = rows.reduce((s: number, r: any) => s + Number(r.qty), 0);
  const amount = rows.reduce((s: number, r: any) => s + Number(r.amount), 0);
  return {
    rows,
    qty,
    amount,
    avgCost: qty === 0 ? 0 : amount / qty,
    locQty: (loc.list ?? []).reduce((s: number, r: any) => s + Number(r.qty), 0),
    instock: Number(ser.total ?? (ser.list ?? []).length),
  };
}

async function api(): Promise<Ctx> {
  const ctx = await request.newContext({ baseURL: BASE_URL });
  const token = readSharedToken();
  expect(token, 'globalSetup 应已把 access_token 写进 e2e/.auth/token.txt').toBeTruthy();
  const headers = { Authorization: `Bearer ${token}` };
  const get = async (url: string): Promise<any> => {
    const res = await ctx.get(url, { headers });
    const json = await res.json().catch(() => ({ code: -1, msg: `<非 JSON ${res.status()}>` }));
    expect(json.code, `GET ${url} 业务码应为 0（实际 ${json.code} / ${json.msg}）`).toBe(0);
    return json.data;
  };
  const post = async (url: string, data: unknown) => {
    const res = await ctx.post(url, { headers, data });
    const json = await res.json().catch(() => ({ code: -1, msg: `<非 JSON ${res.status()}>` }));
    return { status: res.status(), json };
  };
  return { ctx, get, post };
}

/**
 * TDesign 下拉：点开输入框 → 点选项（按文案过滤）
 *
 * ⚠️ 必须限定在**可见**选项内：TDesign 的下拉 popper 关掉后仍留在 DOM 里（hidden），
 * 一个弹窗里有 3 个下拉（物料 / 调出仓 / 调入仓）时，`page.locator('.t-select-option')`
 * 会先命中**上一个下拉**的同名隐藏项 → `toBeVisible` 15s 超时（隔离实例首跑就是这么红的）。
 */
async function pickSelect(page: import('@playwright/test').Page, testId: string, text: string): Promise<void> {
  const input = page.locator(`[data-testid="${testId}"] input`).first();
  // TDesign 的下拉 popper 关掉后仍留在 DOM（hidden），同一弹窗有多个下拉时全局
  // `.t-select-option` 会命中历史下拉的隐藏同名项 → 必须先锁定「当前可见的 popper」，再在它里面选。
  // 另：关闭有动画，动画期内点另一个 select 会「开了立刻又关」→ 每次选完等可见 popper 归零。
  const popper = page.locator('.t-select__dropdown:visible').last();
  const option = () => popper.locator('.t-select-option').filter({ hasText: text }).first();

  const openAndPick = async (clickTimeout: number) => {
    await input.click();
    await expect(option()).toBeVisible({ timeout: 15_000 });
    await option().click({ timeout: clickTimeout });
  };

  try {
    await openAndPick(5_000);
  } catch {
    // 上一个下拉的关闭动画把这次点击吃掉（开了又关）→ 再点一次，稳了再选
    await openAndPick(10_000);
  }
  // 选完确认真的写回了：输入框里的文本应包含选项文案（漏选会让后续保存拿到空 id）
  await expect(input).toHaveValue(new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
  // 等下拉收尾，保证下一个 pickSelect 从「静止」状态开始
  await expect(page.locator('.t-select__dropdown:visible')).toHaveCount(0, { timeout: 8_000 });
}

test.describe('流程12 11:34 走查复现（批量生成 → 调拨 → 审批）', () => {
  let client: Ctx;
  let materialId = '';
  let warehouseFromId = '';
  let transferNo = '';
  let approvalInstanceId = '';

  test.beforeAll(async () => {
    client = await api();
    // 判据自检（第二道）：目标必须是隔离实例，否则不跑 —— 免得有人用 --grep 绕过 config
    const target = new URL(BASE_URL);
    expect(
      ['127.0.0.1', 'localhost', '::1'].includes(target.hostname),
      `本用例只允许打在隔离实例上，实际 BASE_URL=${BASE_URL}`,
    ).toBe(true);
    const port = Number(target.port);
    expect(
      (port >= 18000 && port <= 18999) || (port >= 5100 && port <= 5999),
      `隔离端口段应在 18000-18999 / 5100-5999，实际 ${port}`,
    ).toBe(true);

    const materials = await client.get('/api/business/erp/material/page?page=1&page_size=200');
    const serialMat = (materials.list ?? []).find((m: any) => m.material_code === SERIAL_MATERIAL);
    const plainMat = (materials.list ?? []).find((m: any) => m.material_code === PLAIN_MATERIAL);
    expect(serialMat, `副本库应有物料 ${SERIAL_MATERIAL}`).toBeTruthy();
    expect(serialMat.serial_required, `${SERIAL_MATERIAL} 必须是「一物一码」物料，批量生成弹窗才列它`).toBe(true);
    expect(plainMat, `副本库应有物料 ${PLAIN_MATERIAL}`).toBeTruthy();
    materialId = serialMat.id;

    const warehouses = await client.get('/api/business/erp/warehouse/page?page=1&page_size=200');
    const from = (warehouses.list ?? []).find((w: any) => w.warehouse_code === FROM_WH);
    const to = (warehouses.list ?? []).find((w: any) => w.warehouse_code === TO_WH);
    expect(from, `副本库应有仓库 ${FROM_WH}`).toBeTruthy();
    expect(to, `副本库应有仓库 ${TO_WH}`).toBeTruthy();
    warehouseFromId = from.id;
    console.log(`[SETUP] 目标=${BASE_URL} 物料=${SERIAL_MATERIAL}(${materialId}) 调出仓=${FROM_WH} 调入仓=${TO_WH}`);
  });

  test.afterAll(async () => {
    await client?.ctx.dispose().catch(() => undefined);
    console.log(
      `[数据] 本用例在【目标库】留下：${BATCH_COUNT} 个**带真实成本**的在库序列号（${SERIAL_MATERIAL} @${UNIT_COST}）+ 调拨单 ${transferNo}。` +
        'ADR 落地后这是「无来源入库」的真实库存（两本账 + 凭证都在），只允许留在副本库。' +
        '⚠️ 它在 e2e_fixture_residue.py 里**不算**夹具（第 ④ 道「库存零价值」不命中），要撤只能走红冲 / 盘点。',
    );
  });

  test('① 不填成本单价 ⇒ 两条路都被拦下（UI 本地拦 + 后端 400/10001），零落库', async ({ page }) => {
    const before = await ledgerSnapshot(client, materialId, warehouseFromId);

    await openAuthed(page, '/business/erp/serial');
    const metaBefore = Number((await page.locator('[data-testid="serial-meta"]').innerText()).replace(/\D/g, '') || 0);

    // 抓 XHR：本地校验拦住时**一个 batch-generate 请求都不该发**（发了就说明「空当 0」兜底回来了）
    const sent: string[] = [];
    const onRequest = (req: import('@playwright/test').Request) => {
      if (req.method() === 'POST' && req.url().includes('/serial/batch-generate')) sent.push(req.url());
    };
    page.on('request', onRequest);

    await page.locator('[data-testid="btn-batch-generate"]').click();
    const dialog = page.locator('[data-testid="serial-batch-dialog"]');
    await expect(dialog).toBeVisible();

    // 弹窗只列「一物一码」物料 —— 这正是 11:34 能选到 M-W1A-004 的原因
    await pickSelect(page, 'batch-material', `（${SERIAL_MATERIAL}）`);
    await pickSelect(page, 'batch-warehouse', `（${FROM_WH}）`);
    await page.locator('[data-testid="batch-count"] input').fill(String(BATCH_COUNT));
    // 故意不填「成本单价」；预览不许把空当 0 算成 0.0000
    await expect(page.locator('[data-testid="batch-unit-cost"] input')).toHaveValue('');
    await expect(page.locator('[data-testid="batch-amount-preview"]')).toContainText('—');

    await dialog.locator('button').filter({ hasText: '生成' }).first().click();

    // 弹窗不关 + 表单项下方红字（文案与 serial-logic 的本地校验一致）
    await expect(dialog).toBeVisible();
    await expect(dialog.getByText(/请填写成本单价/).first()).toBeVisible({ timeout: 10_000 });
    await page.waitForTimeout(800);
    expect(sent.length, `不填单价不该发出 batch-generate 请求（实际 ${sent.length} 条）`).toBe(0);
    await expect
      .poll(async () => Number((await page.locator('[data-testid="serial-meta"]').innerText()).replace(/\D/g, '')), {
        timeout: 5_000,
      })
      .toBe(metaBefore);
    page.off('request', onRequest);
    console.log(
      `[DOM] 不填单价：弹窗未关 + 红字「请填写成本单价…」+ batch-generate 请求 ${sent.length} 条 + 列表仍 ${metaBefore} 条`,
    );

    // ---- 后端契约（绕过 UI 直调）：缺字段 / =0 / 非数字 / 超上限 一律 400 + code 10001 + 逐字报文
    const base = { material_id: materialId, warehouse_id: warehouseFromId, count: BATCH_COUNT };
    const cases: Array<{ label: string; data: any; match: RegExp }> = [
      { label: '缺 unit_cost', data: base, match: /unit_cost 必填/ },
      { label: 'unit_cost=0', data: { ...base, unit_cost: 0 }, match: /必须 > 0/ },
      { label: 'unit_cost=abc', data: { ...base, unit_cost: 'abc' }, match: /不是合法数字/ },
      { label: 'unit_cost=100000000000', data: { ...base, unit_cost: 100000000000 }, match: /超出可记账范围/ },
    ];
    for (const c of cases) {
      const { status, json } = await client.post('/api/business/erp/serial/batch-generate', c.data);
      expect(status, `${c.label} 应被 HTTP 400 拦下`).toBe(400);
      expect(json.code, `${c.label} 业务码应为 10001`).toBe(10001);
      expect(String(json.msg), `${c.label} 报文`).toMatch(c.match);
      console.log(`[API] ${c.label} → HTTP ${status} / code ${json.code} / msg「${json.msg}」`);
    }

    // ---- 零落库（ADR §3 判据②）：号 / 成本账 / 库位账一个字都没动
    const after = await ledgerSnapshot(client, materialId, warehouseFromId);
    expect(after.qty, '被拦下后 biz_inventory.qty 不许变').toBeCloseTo(before.qty, 4);
    expect(after.amount, '被拦下后 biz_inventory.amount 不许变').toBeCloseTo(before.amount, 4);
    expect(after.locQty, '被拦下后库位账不许变').toBeCloseTo(before.locQty, 4);
    expect(after.instock, '被拦下后不许落新序列号').toBe(before.instock);
  });

  test('② 批量生成 10 件 @12.34（无来源入库）⇒ 两本账 + 流水写实成本 + 不变式①（11:34:11 那一次点击）', async ({
    page,
  }) => {
    const before = await ledgerSnapshot(client, materialId, warehouseFromId);
    console.log(
      `[API·before] qty=${before.qty} amount=${before.amount} 库位账=${before.locQty} 在库号=${before.instock} 均价=${before.avgCost}`,
    );

    await openAuthed(page, '/business/erp/serial');
    const metaBefore = Number((await page.locator('[data-testid="serial-meta"]').innerText()).replace(/\D/g, '') || 0);

    await page.locator('[data-testid="btn-batch-generate"]').click();
    const dialog = page.locator('[data-testid="serial-batch-dialog"]');
    await expect(dialog).toBeVisible();

    await pickSelect(page, 'batch-material', `（${SERIAL_MATERIAL}）`);
    await pickSelect(page, 'batch-warehouse', `（${FROM_WH}）`);
    await page.locator('[data-testid="batch-count"] input').fill(String(BATCH_COUNT));
    // 这就是 ADR 之后**必填**的那一步（不填的结果见 ①）
    await page.locator('[data-testid="batch-unit-cost"] input').fill(UNIT_COST.toFixed(2));
    // 提交前预览：10 × 12.34 = 123.4000（与账面 amount 同精度，4 位小数）
    await expect(page.locator('[data-testid="batch-amount-preview"]')).toContainText(EXPECT_AMOUNT);

    // 抓到真实响应：出参回显的数字必须与预览、与库内一致（前端不许自己算一套）
    const [resp] = await Promise.all([
      page.waitForResponse(
        (r) => r.request().method() === 'POST' && r.url().includes('/serial/batch-generate'),
        { timeout: 30_000 },
      ),
      dialog.locator('button').filter({ hasText: '生成' }).first().click(),
    ]);
    expect(resp.status(), '批量生成应 200').toBe(200);
    const body = await resp.json();
    expect(body.code, `业务码应为 0（${body.msg ?? ''}）`).toBe(0);
    const data = body.data ?? {};
    expect(data.count, '生成条数').toBe(BATCH_COUNT);
    expect(Number(data.unit_cost), '回显 unit_cost = 12.34').toBeCloseTo(UNIT_COST, 6);
    expect(Number(data.amount), '回显 amount = N × unit_cost').toBeCloseTo(Number(EXPECT_AMOUNT), 4);

    // 成功文案：生成了 N 个序列号（YYYYMM）：SN-… / SN-… / SN-… …
    await expect(toast(page, new RegExp(`生成了 ${BATCH_COUNT} 个序列号`))).toBeVisible({ timeout: 20_000 });
    await expect(dialog).toBeHidden();
    // 成功条把「件数 × 单价 = 金额」写到列表上方
    await expect(page.locator('[data-testid="batch-result-hint"]')).toContainText(EXPECT_AMOUNT);

    // 列表条数 +10（页面真的刷新了，不是只弹了个 toast）
    await expect
      .poll(async () => Number((await page.locator('[data-testid="serial-meta"]').innerText()).replace(/\D/g, '')), {
        timeout: 15_000,
      })
      .toBe(metaBefore + BATCH_COUNT);
    console.log(
      `[DOM] 序列号总数 ${metaBefore} → ${metaBefore + BATCH_COUNT}；预览/成功条金额 ${EXPECT_AMOUNT}；出参 amount=${data.amount} unit_cost=${data.unit_cost}`,
    );

    // ---- 库内对账（只读接口，口径与 erp_invariants.py 的 B1/B4/C5 相同：按 物料×仓库 求和）
    const after = await ledgerSnapshot(client, materialId, warehouseFromId);
    expect(after.qty - before.qty, 'biz_inventory.qty 增量 = N').toBeCloseTo(BATCH_COUNT, 4);
    expect(after.amount - before.amount, 'biz_inventory.amount 增量 = N × unit_cost').toBeCloseTo(
      Number(EXPECT_AMOUNT),
      4,
    );
    expect(after.locQty - before.locQty, '库位账合计增量 = N（B1 等式左边）').toBeCloseTo(BATCH_COUNT, 4);
    expect(after.instock - before.instock, '在库序列号件数增量 = N（不变式①）').toBe(BATCH_COUNT);
    // 绝对值形态：不变式①（qty == 在库号数）/ B1（库位账合计 == 成本账 qty）/ C3（有货则均价非零）
    expect(after.instock, '不变式①：biz_inventory.qty == 在库号数').toBeCloseTo(after.qty, 4);
    expect(after.locQty, 'B1：库位账合计 == 成本账 qty').toBeCloseTo(after.qty, 4);
    expect(after.avgCost, 'C3：有库存则成本价非零').toBeGreaterThan(0);

    const row = after.rows.find((r: any) => Number(r.qty) !== 0);
    expect(row, '应有成本账行（物料×仓库）').toBeTruthy();
    if (before.qty === 0) {
      // ADR §3 判据① 的字面口径：从零建账时 avg_cost 必须逐字等于单价
      expect(String(row.avg_cost), 'avg_cost = unit_cost（6 位小数）').toBe(EXPECT_UNIT_COST);
      expect(String(row.amount), 'amount = N × unit_cost（4 位小数）').toBe(EXPECT_AMOUNT);
      expect(Number(row.qty), 'qty = N').toBe(BATCH_COUNT);
    } else {
      // 二次生成：移动加权 new_avg = round((old_amount + N×cost) / (old_qty + N), 6)（ADR §3 判据③）
      const expAvg =
        Math.round(((before.amount + Number(EXPECT_AMOUNT)) / (before.qty + BATCH_COUNT)) * 1e6) / 1e6;
      expect(after.avgCost, `移动加权均价（期望 ${expAvg}）`).toBeCloseTo(expAvg, 6);
    }
    console.log(
      `[API·after] qty=${row.qty} amount=${row.amount} avg_cost=${row.avg_cost} 库位账=${after.locQty} 在库号=${after.instock}`,
    );

    // ---- 流水：保留 SERIAL_IN / SERIAL_BATCH 形态（残留判据第 ① 道依赖它），但必须写实成本
    const txnPage = await client.get(
      `/api/business/erp/inventory-txn/page?material_id=${materialId}&warehouse_id=${warehouseFromId}&doc_type=SERIAL_BATCH&page=1&page_size=10`,
    );
    const txn = (txnPage.list ?? [])[0];
    expect(txn, '应有 SERIAL_BATCH 流水').toBeTruthy();
    expect(txn.biz_type, '流水 biz_type').toBe('SERIAL_IN');
    expect(txn.txn_type, '流水方向').toBe('IN');
    expect(Number(txn.qty), '流水 qty').toBeCloseTo(BATCH_COUNT, 4);
    expect(Number(txn.unit_cost), '流水 unit_cost 写实（旧形态这里是 0）').toBeCloseTo(UNIT_COST, 6);
    expect(Number(txn.amount), '流水 amount 写实（旧形态这里是 0）').toBeCloseTo(Number(EXPECT_AMOUNT), 4);
    expect(String(txn.remark ?? ''), `流水 remark 应记「批量生成 ${BATCH_COUNT} 件」`).toContain(
      `批量生成 ${BATCH_COUNT} 件`,
    );
    console.log(
      `[API] 流水 ${txn.doc_type}/${txn.biz_type}/${txn.txn_type} qty=${txn.qty} unit_cost=${txn.unit_cost} amount=${txn.amount} remark=${txn.remark}`,
    );
  });

  test('③ 建调拨单并提交（11:35:03 那一次建单 + 自动起审批）', async ({ page }) => {
    await openAuthed(page, '/business/erp/transfer');
    await page.locator('[data-testid="btn-transfer-create"]').click();
    const dialog = page.locator('[data-testid="transfer-form-dialog"]');
    await expect(dialog).toBeVisible();

    await pickSelect(page, 'transfer-from-warehouse', `（${FROM_WH}）`);
    await pickSelect(page, 'transfer-to-warehouse', `（${TO_WH}）`);
    // t-input 的 data-testid 落在 wrapper div 上，真正的输入框是里面的 input
    await page.locator('[data-testid="transfer-reason"] input').fill(`[E2E] 复现 11:34 走查 ${RUN}`);
    await pickSelect(page, 'item-material-0', `（${PLAIN_MATERIAL}）`);
    await page.locator('[data-testid="item-qty-0"] input').fill('1');

    // 保存并提交（后端会同时 POST /transfer + PATCH /status，并自动发起审批实例）
    await page.locator('[data-testid="btn-save-submit"]').click();
    await expect(toast(page, /已提交审批/)).toBeVisible({ timeout: 25_000 });
    await expect(dialog).toBeHidden();

    // 行出现且状态是「待审批」
    const rows = await client.get('/api/business/erp/transfer/page?page=1&page_size=20&keyword=' + encodeURIComponent('TK-'));
    const mine = (rows.list ?? []).find((t: any) => (t.reason ?? '').includes(RUN));
    expect(mine, `按原因 ${RUN} 应能查到刚建的调拨单`).toBeTruthy();
    transferNo = mine.transfer_no;
    expect(mine.status, '提交后调拨单应为 PENDING').toBe('PENDING');

    // 明细里有实体行，且审批实例已经起来了（11:35:03 的「自动发起审批单」）
    const detail = await client.get(`/api/business/erp/transfer/${mine.id}`);
    expect(detail.approval_instance_id, '提交后必须自动起审批实例').toBeTruthy();
    approvalInstanceId = detail.approval_instance_id;
    console.log(
      `[API] 调拨单 ${transferNo} status=${mine.status} approval_instance=${detail.approval_instance_id}`,
    );
    await expect(page.locator(`[data-testid="transfer-no-${transferNo}"]`)).toBeVisible({ timeout: 15_000 });
  });

  test('④ 待我审批里同意（11:35:21 那一次审批）', async ({ page }) => {
    expect(transferNo, '③ 应已建好调拨单').toBeTruthy();
    await openAuthed(page, '/flow/task');
    await page.locator('.t-tabs__nav-item', { hasText: '待我审批' }).click();

    const card = page.locator('.task-card').filter({ hasText: transferNo }).first();
    await expect(card, `「待我审批」里应出现 ${transferNo} 的审批待办`).toBeVisible({ timeout: 25_000 });
    await card.locator('button').filter({ hasText: '同意' }).first().click();
    await expect(toast(page, /已同意/)).toBeVisible({ timeout: 25_000 });
    console.log(`[DOM] 已同意 ${transferNo} 的审批待办`);

    // 库内复核：这一票已落库（我的待办消失），且**单据状态与审批实例一致**
    //
    // ⚠️ 这里不能断言「单据一定变 APPROVED」：`stock_transfer` 的 n_approve 节点是
    // `assignee_type=ADMIN`（3 个超管）+ `join_mode=AND`（会签），一个超管点同意只推进一票，
    // 实例仍是 PENDING —— 11:34 那位客户在 prod 上批完一票后 TK-20261031-000007 也停在 PENDING
    // （同形态，见 docs/e2e-prod-fixture-incident-20261031-172.18.10.7.md §5.3）。
    // 硬期望按「一致性」写：实例 APPROVED ⇒ 单据必须被回调成 APPROVED；否则单据必须仍在 PENDING。
    await expect
      .poll(
        async () => {
          const todo = await client.get('/api/business/flow/task/todo?page=1&page_size=50');
          return (todo.list ?? []).filter((t: any) => String(t.title ?? '').includes(transferNo)).length;
        },
        { timeout: 20_000 },
      )
      .toBe(0);

    const inst = await client.get(`/api/business/flow/instance/${approvalInstanceId}`);
    const detail = await client.get(
      `/api/business/erp/transfer/page?page=1&page_size=20&keyword=${encodeURIComponent(transferNo)}`,
    );
    const row = (detail.list ?? []).find((t: any) => t.transfer_no === transferNo);
    expect(row, `应能查到 ${transferNo}`).toBeTruthy();
    const instanceStatus = inst.instance.status as string;
    if (instanceStatus === 'APPROVED') {
      expect(row.status, '审批实例通过 ⇒ 调拨单必须被回调成 APPROVED（settle_transfer）').toBe('APPROVED');
    } else {
      expect(row.status, '审批实例未通过 ⇒ 调拨单不许提前变成 APPROVED').toBe('PENDING');
    }
    console.log(
      `[API] ${transferNo} 实例=${instanceStatus} 单据=${row.status}（会签节点：我这一票已落库，其余超管待办仍在）`,
    );
  });

  /**
   * ⑤ 三账不变量：B1（库位账合计 = 成本账）/ B2（库存金额 = 1405 余额）/ C3（有货则成本价非零）
   *
   * 为什么放在最后一条：它跑的是 `scripts/verify/erp_invariants.py`（只读 SQL），
   * 必须等 ①②③④ 全部落库之后跑 —— 这样「批量生成 + 调拨提交/审批」对账务的影响一起被覆盖。
   *
   * 为什么要 `E2E_RUN_INVARIANTS=1`：脚本的默认库名是 `cloudpivot`（prod 库名）。
   * 不显式给 `NIK_DATABASE_*` 就跳，**绝不**让一条 e2e 顺手去读 prod 库。
   */
  test('⑤ 三账不变量 32/32：B1 / B2 / C3 全绿（ADR §3 判据①）', async () => {
    const dbName = process.env.NIK_DATABASE_NAME ?? '';
    test.skip(
      process.env.E2E_RUN_INVARIANTS !== '1' || !dbName,
      '未开：需要 E2E_RUN_INVARIANTS=1 且 NIK_DATABASE_* 指向副本库（完整闸门 bash scripts/e2e-postrun-check.sh）',
    );
    expect(dbName, '不变量必须打副本库，不许落到脚本默认的 prod 库名').not.toBe('cloudpivot');

    const script = resolve(process.cwd(), '../scripts/verify/erp_invariants.py');
    const run = spawnSync('python3', [script], { encoding: 'utf8', env: process.env, timeout: 180_000 });
    const out = `${run.stdout ?? ''}${run.stderr ?? ''}`;
    expect(run.error, `跑不起来：${run.error?.message ?? ''}`).toBeFalsy();

    const summary = out.split('\n').filter((l) => l.includes('结果：')).pop() ?? '';
    console.log(`[不变量] 库=${dbName} ${summary.trim()}`);
    expect(run.status, `erp_invariants 退出码应为 0（有 BLOCKER 才非 0）\n${out.slice(-3000)}`).toBe(0);
    const hit = summary.match(/(\d+)\/(\d+)\s*通过/);
    expect(hit?.[1], `通过条数（原始行：${summary.trim()}）`).toBe('32');
    expect(hit?.[2], `总条数（原始行：${summary.trim()}）`).toBe('32');
    expect(summary, 'BLOCKER 必须 0').toMatch(/BLOCKER 0/);
    expect(summary, 'WARN 必须 0').toMatch(/WARN 0/);

    // 不能只看汇总行：B1 / B2 / C3 三条要各自有 ✅（B2 是本卡新增的硬要求，光看 B1/C3 会漏）
    for (const key of ['B1 库位账合计 = 成本账 qty', 'B2 库存金额 = 存货科目 1405 余额', 'C3 有库存则成本价非零']) {
      const line = out.split('\n').find((l) => l.includes(key)) ?? '';
      expect(line, `应能读到「${key}」的运行行`).not.toBe('');
      expect(line, `「${key}」必须 ✅（实际：${line.trim()}）`).toContain('✅');
      console.log(`[不变量] ${line.trim().replace(/\s+/g, ' ')}`);
    }
  });
});

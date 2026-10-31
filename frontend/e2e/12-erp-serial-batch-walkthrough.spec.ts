/**
 * 12 · 复现 2026-10-31 11:34 那条「UI 业务闭环走查」（**只能在隔离实例上跑**）
 *
 * # 为什么要把它写成用例
 *
 * 11:34:11 prod 被从 172.18.10.7 的 mac 浏览器（admin 账号）点了一次
 * 「ERP → 序列号 → 批量生成」（M-W1A-004 / WH01 / 10 件），落库后
 * `erp_invariants.py` 当场 **B1（BLOCKER）+ C3（WARN）** 变红：
 *   ① `biz_material_serial` 10 行 IN_STOCK、source_type=PURCHASE、source_id IS NULL；
 *   ② `biz_inventory` qty 涨 10，但 amount / avg_cost 全 0，且**不写库位账**；
 *   ③ 同一次走查还建了一张调拨单（TK-20261031-000007，自动起审批实例）并批了一张审批单。
 *
 * 这条业务链路是真实用户会走的，**不能删**；要防的是「在 prod 上走」。所以：
 *   - 用例保留完整链路（登录 → 批量生成 → 调拨建单提交 → 待办同意），
 *     以后前端/后端改动都能在这条链路上回归；
 *   - 目标由 `frontend/e2e/target-guard.mjs` 硬判据兜住：BASE_URL 不是回环隔离端口
 *     直接抛错（本用例开头还有第二道自检，见 ①）；
 *   - 用例跑完会在**目标库**留下零成本序列号 + 一张调拨单（这正是它要证明的形态），
 *     因此只允许打在副本库（`scripts/verify/e2e_isolated_instance.sh`，库 `cloudpivot_e2e_iso`）。
 *     误打在 prod 上时，`e2e/.auth/prod-target.flag` + `e2e_fixture_residue.py` 会把它揪出来。
 *
 * # 数据前提（副本库来自 prod 快照）
 * - 物料 `M-W1A-004`：serial_required = true（序列号弹窗只列这类物料）；
 * - 仓库 `WH01`（总仓，非叶子）/ `WH02`（分仓，叶子）；
 * - 物料 `M-W1A-001`：非一物一码，且 WH01 有 8 件库存 → 调拨用它，免选序列号；
 * - 测试账号 `laoliu_test` 是 SUPER_ADMIN → 调拨单提交后自己就在「待我审批」里（可自审）。
 */
import { expect, test, request, type APIRequestContext } from '@playwright/test';
import { BASE_URL, openAuthed, toast, readSharedToken } from './support';

/** 本次运行戳：调拨原因里带它，便于在副本库里定位本次造的数据 */
const RUN = `E2E-P11G-${Date.now().toString(36).toUpperCase()}`;
const SERIAL_MATERIAL = 'M-W1A-004';
const PLAIN_MATERIAL = 'M-W1A-001';
const FROM_WH = 'WH01';
const TO_WH = 'WH02';
const BATCH_COUNT = 10;

interface Ctx {
  ctx: APIRequestContext;
  get: (url: string) => Promise<any>;
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
  return { ctx, get };
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
      `[数据] 本用例在【目标库】留下：${BATCH_COUNT} 个零成本序列号（${SERIAL_MATERIAL}）+ 调拨单 ${transferNo}。` +
        '这是 11:34 那批夹具的同形态数据；只允许留在副本库 cloudpivot_e2e_iso。',
    );
  });

  test('① 批量生成 10 个序列号（11:34:11 那一次点击）', async ({ page }) => {
    await openAuthed(page, '/business/erp/serial');

    const metaBefore = (await page.locator('[data-testid="serial-meta"]').innerText()).replace(/\D/g, '');
    const before = Number(metaBefore || 0);

    await page.locator('[data-testid="btn-batch-generate"]').click();
    const dialog = page.locator('[data-testid="serial-batch-dialog"]');
    await expect(dialog).toBeVisible();

    // 弹窗只列「一物一码」物料 —— 这正是 11:34 能选到 M-W1A-004 的原因
    await pickSelect(page, 'batch-material', `（${SERIAL_MATERIAL}）`);
    await pickSelect(page, 'batch-warehouse', `（${FROM_WH}）`);
    await page.locator('[data-testid="batch-count"] input').fill(String(BATCH_COUNT));

    await dialog.locator('button').filter({ hasText: '生成' }).first().click();
    // 成功文案：生成了 N 个序列号（YYYYMM）：SN-… / SN-… / SN-… …
    await expect(toast(page, new RegExp(`生成了 ${BATCH_COUNT} 个序列号`))).toBeVisible({ timeout: 20_000 });
    await expect(dialog).toBeHidden();

    // 列表条数 +10（页面真的刷新了，不是只弹了个 toast）
    await expect
      .poll(async () => Number((await page.locator('[data-testid="serial-meta"]').innerText()).replace(/\D/g, '')), {
        timeout: 15_000,
      })
      .toBe(before + BATCH_COUNT);
    console.log(`[DOM] 序列号总数 ${before} → ${before + BATCH_COUNT}`);

    // 库里真落成 IN_STOCK / source_id 为空（夹具形态，与 11:34 完全一致）
    const serials = await client.get(
      `/api/business/erp/serial/page?page=1&page_size=50&material_id=${materialId}`,
    );
    const fresh = (serials.list ?? []).filter((s: any) => s.status === 'IN_STOCK');
    expect(fresh.length, `${SERIAL_MATERIAL} 应有至少 ${BATCH_COUNT} 个在库序列号`).toBeGreaterThanOrEqual(
      BATCH_COUNT,
    );
    console.log(`[API] ${SERIAL_MATERIAL} 在库序列号 = ${fresh.length}（前 3 个：${fresh.slice(0, 3).map((s: any) => s.serial_no).join(' / ')}）`);
  });

  test('② 建调拨单并提交（11:35:03 那一次建单 + 自动起审批）', async ({ page }) => {
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

  test('③ 待我审批里同意（11:35:21 那一次审批）', async ({ page }) => {
    expect(transferNo, '② 应已建好调拨单').toBeTruthy();
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
});

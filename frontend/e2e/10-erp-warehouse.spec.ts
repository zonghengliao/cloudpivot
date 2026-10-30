/**
 * W4a · 仓库树 + 库位页 端到端验收（真后端，不打桩）。
 *
 * 覆盖卡面第三节 ①–⑦：
 * ① 登录 → 菜单「ERP 主数据 → 仓库管理」
 * ② 仓库树渲染（1 层总仓 → 2 层分仓 → 本用例新建的第 3 层库区，断言深度 = 3）
 * ③ 点节点 → 右侧库位列表刷新为该仓库的库位
 * ④ 折叠父节点 → 子节点消失；再展开 → 回来（逐节点独立，不联级）
 * ⑤ 新建仓库弹窗：填完能提交，树里出现新节点
 * ⑥ 新建库位：叶子仓库可建；非叶子仓库按钮禁用 + tooltip 说明
 * ⑦ 截图 2 张（首屏 + 新建弹窗）
 *
 * # 为什么用 `127.0.0.1:4174` 的本地 dist
 *
 * 生产 144 上的 dist 还没带本卡页面（发布是另一张卡的事）。这里把**本次构建的 dist**
 * 跑在本地静态服务上，`/api` 反代到真后端 172.18.10.144 —— 浏览器跑的是本次改动的代码，
 * 接口 / 权限 / 数据全是真生产后端。见 `w4a-verify/serve.mjs`。
 *
 * # 数据清理
 *
 * 用例自己建的仓库 / 库位在 `finally` 里按 API 删干净（先删库位再删仓库，
 * 有下级或挂库位的仓库后端会 409）。名字都带 `E2E-W4A-<run>` 前缀，便于人工兜底清理。
 */
import { expect, test, request } from '@playwright/test';
import { openAuthed, readSharedToken, toast, BASE_URL } from './support';

/** 本次运行戳（名字 / 编码都带它，避免连跑互相踩） */
const RUN = `E2E-W4A-${Date.now().toString(36).toUpperCase()}`;
/** 截图目录（`w4a-verify/shots`，证据文档引用这俩文件名） */
const SHOTS = process.env.W4A_SHOT_DIR ?? '/root/.hermes/workspace/frontend-developer/w4a-verify/shots';

/** 种子仓库编码（V0137 迁移种的 2 个：总仓 WH01 → 分仓 WH02） */
const SEED_ROOT = 'WH01';
const SEED_BRANCH = 'WH02';

interface TreeNode {
  id: string;
  warehouse_code: string;
  warehouse_name: string;
  is_leaf: boolean;
  level: number;
  children: TreeNode[];
}

/** 真后端 API 客户端（复用 globalSetup 的登录态，避免撞登录限流） */
interface Api {
  /** GET（返回后端 data） */
  get(url: string): Promise<any>;
  /** DELETE */
  del(url: string): Promise<any>;
  /** 释放连接池 */
  dispose(): Promise<void>;
}

async function newApi(): Promise<Api> {
  const ctx = await request.newContext({ baseURL: BASE_URL });
  const token = readSharedToken();
  expect(token, 'globalSetup 应已写入 e2e/.auth/token.txt').toBeTruthy();
  const headers = { Authorization: `Bearer ${token}` };

  /** 统一拆包：非 0 业务码直接抛后端文案，失败原因一眼可见 */
  const call = async (method: 'get' | 'delete', url: string): Promise<any> => {
    const res = await ctx[method](url, { headers });
    const json = await res.json().catch(() => ({ code: -1, msg: `<非 JSON ${res.status()}>` }));
    if (json.code !== 0) {
      throw new Error(`API ${method.toUpperCase()} ${url} 失败：${res.status()} / ${json.code} / ${json.msg}`);
    }
    return json.data;
  };

  return {
    get: (url) => call('get', url),
    del: (url) => call('delete', url),
    dispose: () => ctx.dispose(),
  };
}

/** 取整棵树 */
function fetchTree(api: Api): Promise<TreeNode[]> {
  return api.get('/api/business/erp/warehouse/tree');
}

/** 展平 */
function flatten(nodes: TreeNode[]): TreeNode[] {
  return nodes.flatMap((node) => [node, ...flatten(node.children ?? [])]);
}

/** 按编码找仓库 */
function byCode(nodes: TreeNode[], code: string): TreeNode | undefined {
  return flatten(nodes).find((node) => node.warehouse_code === code);
}

/** 清掉上一轮同名残留（先库位后仓库，按层级从深到浅删） */
async function sweepPrevious(api: Api): Promise<void> {
  const tree = await fetchTree(api);
  const stale = flatten(tree).filter((node) => node.warehouse_name.startsWith('E2E-W4A-'));
  if (stale.length === 0) {
    return;
  }
  for (const node of stale.sort((a, b) => b.level - a.level)) {
    const locations = await api.get(`/api/business/erp/location/list?warehouse_id=${node.id}`);
    for (const location of locations) {
      await api.del(`/api/business/erp/location/${location.id}`).catch(() => undefined);
    }
    await api.del(`/api/business/erp/warehouse/${node.id}`).catch(() => undefined);
  }
}

/** 弹窗定位（两个弹窗组件常驻 DOM，靠内部 testid 区分而不是靠顺序） */
function warehouseDialog(page: import('@playwright/test').Page) {
  return page.locator('.t-dialog').filter({ has: page.locator('[data-testid="wh-code"]') });
}

function locationDialog(page: import('@playwright/test').Page) {
  return page.locator('.t-dialog').filter({ has: page.locator('[data-testid="loc-code"]') });
}

test('W4a：仓库树 + 库位页（树渲染 / 按仓筛选 / 折叠 / 新建 / 叶子门禁）', async ({ page }) => {
  const api = await newApi();
  const created: { warehouseId?: string; locationId?: string } = {};

  try {
    await sweepPrevious(api);

    // ==================== ① 登录 → 菜单「ERP 主数据 → 仓库管理」 ====================
    await test.step('① 菜单进入「ERP 主数据 → 仓库管理」', async () => {
      await openAuthed(page, '/dashboard');
      await page.getByText('ERP 主数据').first().click();
      await page.getByText('仓库管理').first().click();
      await expect(page).toHaveURL(/\/business\/erp\/warehouse/);
      await expect(page.locator('[data-testid="warehouse-tree"]')).toBeVisible();
      console.log('[DOM] 菜单直达 URL =', page.url());
    });

    // ==================== ② 仓库树渲染 ====================
    await test.step('② 树渲染：种子 2 层 + 新建第 3 层后深度 = 3', async () => {
      await expect(page.locator('.wh-node[data-code="' + SEED_ROOT + '"]')).toBeVisible();
      await expect(page.locator('.wh-node[data-code="' + SEED_BRANCH + '"]')).toBeVisible();
      const meta = await page.locator('[data-testid="tree-meta"]').innerText();
      console.log('[DOM] tree-meta =', meta);
      expect(meta).toMatch(/\d+ 个仓库 · \d+ 层/);
      // 首屏默认展开第 1 层 → 分仓可见
      expect(meta).toContain('2 个仓库');
    });

    // ==================== ③ 点节点 → 右侧按该仓库过滤 ====================
    await test.step('③ 点「分仓」→ 右侧库位列表换成它的库位', async () => {
      await page.locator(`.wh-node[data-code="${SEED_BRANCH}"]`).click();
      await expect(page.locator('[data-testid="selected-title"]')).toContainText('分仓（种子）');
      const table = page.locator('[data-testid="location-table"]');
      await expect(table).toBeVisible();
      await expect(table).toContainText('A-01-01');
      const codes = await table.locator('.t-table__body tr td:first-child').allInnerTexts();
      console.log('[DOM] 分仓库位编码 =', codes.map((item) => item.trim()).join(', '));
      expect(codes.length).toBeGreaterThanOrEqual(3);
      // 换成总仓 → 列表跟着换（总仓没有库位，且是非叶子：出现提示条）
      await page.locator(`.wh-node[data-code="${SEED_ROOT}"]`).click();
      await expect(page.locator('[data-testid="selected-title"]')).toContainText('总仓（种子）');
      await expect(page.locator('[data-testid="non-leaf-hint"]')).toBeVisible();
      const totalText = await page.locator('.t-table__pagination, .t-pagination').first().innerText().catch(() => '');
      console.log('[DOM] 总仓库位分页文案 =', totalText.replace(/\s+/g, ' ').trim());
    });

    // ==================== ④ 折叠 / 展开（逐节点独立） ====================
    await test.step('④ 折叠总仓 → 分仓消失；展开 → 回来', async () => {
      const branchNode = page.locator(`.wh-node[data-code="${SEED_BRANCH}"]`);
      const caret = page.locator(`.t-tree__item:has(.wh-node[data-code="${SEED_ROOT}"]) [trigger="expand"]`);
      await caret.click();
      await expect(branchNode).toBeHidden();
      console.log('[DOM] 折叠总仓后：分仓节点可见 =', await branchNode.isVisible());
      await caret.click();
      await expect(branchNode).toBeVisible();
      console.log('[DOM] 再展开后：分仓节点可见 =', await branchNode.isVisible());
    });

    // ==================== ⑤ 新建仓库（挂在分仓下 → 第 3 层） ====================
    await test.step('⑤ 新建仓库弹窗提交 → 树里出现新节点', async () => {
      const tree = await fetchTree(api);
      const branch = byCode(tree, SEED_BRANCH);
      expect(branch, '种子分仓应存在').toBeTruthy();

      await page.locator(`.wh-node[data-code="${SEED_BRANCH}"]`).click();
      await page.locator('[data-testid="btn-new-warehouse"]').click();
      const dialog = warehouseDialog(page);
      await expect(dialog).toBeVisible();

      // 父仓库预选 = 当前选中（分仓）→ 层级自动推导成第 3 层
      await expect(dialog).toContainText('第 3 层');
      await dialog.locator('[data-testid="wh-code"] input').fill(`${RUN}-Q`);
      await dialog.locator('[data-testid="wh-name"] input').fill(`${RUN}-库区`);
      await dialog.locator('[data-testid="wh-address"] input').fill('北京市朝阳区测试路 1 号');
      // 是否叶子 = 派生只读（新建的仓库还没有下级 → 叶子）
      await expect(dialog.locator('[data-testid="wh-leaf"]')).toContainText('叶子');

      // ⑦ 截图之二：新建弹窗（填完、提交前）
      await page.screenshot({ path: `${SHOTS}/02-new-warehouse-dialog.png` });

      await dialog.getByRole('button', { name: '创建' }).click();
      await expect(toast(page, /已创建/)).toBeVisible({ timeout: 15_000 });
      await expect(dialog).toBeHidden();

      // 树里出现新节点 + 深度变 3 层
      await expect(page.locator(`.wh-node[data-code="${RUN}-Q"]`)).toBeVisible({ timeout: 15_000 });
      const meta = await page.locator('[data-testid="tree-meta"]').innerText();
      console.log('[DOM] 新建后 tree-meta =', meta);
      expect(meta).toMatch(/3 层/);

      const after = await fetchTree(api);
      const createdNode = byCode(after, `${RUN}-Q`);
      expect(createdNode, 'API 侧也应能查到新仓库').toBeTruthy();
      expect(createdNode?.level).toBe(3);
      created.warehouseId = createdNode?.id;
      // 新节点是叶子（还没有下级）
      expect(createdNode?.is_leaf).toBe(true);
      console.log('[API] 新仓库 =', createdNode?.warehouse_name, 'level =', createdNode?.level, 'is_leaf =', createdNode?.is_leaf);

      // ⑦ 截图之一：首屏（3 层树 + 右侧列表）
      await page.screenshot({ path: `${SHOTS}/01-warehouse-page.png`, fullPage: true });
    });

    // ==================== ⑥ 新建库位：非叶子禁用 / 叶子可用 ====================
    await test.step('⑥ 非叶子仓库「新建库位」禁用 + tooltip；叶子仓库能建', async () => {
      const newButton = page.locator('[data-testid="btn-new-location"]');

      // 分仓现在有下级了 → 非叶子 → 按钮禁用
      await page.locator(`.wh-node[data-code="${SEED_BRANCH}"]`).click();
      await expect(page.locator('[data-testid="selected-title"]')).toContainText('分仓（种子）');
      await expect(newButton).toBeDisabled();
      console.log('[DOM] 选中非叶子(分仓) → 新建库位 disabled =', await newButton.isDisabled());

      // tooltip 说明（悬停禁用按钮外层的 span）
      await page.locator('.gate-wrap').first().hover();
      const tooltip = page.locator('.t-tooltip:visible').first();
      await expect(tooltip).toContainText('叶子仓库');
      console.log('[DOM] tooltip =', (await tooltip.innerText()).replace(/\s+/g, ' ').trim().slice(0, 80));

      // 选新建的第 3 层库区（叶子）→ 按钮可用 → 建一个库位
      await page.locator(`.wh-node[data-code="${RUN}-Q"]`).click();
      await expect(newButton).toBeEnabled();
      await newButton.click();
      const dialog = locationDialog(page);
      await expect(dialog).toBeVisible();
      // 所属仓库只列叶子仓库，默认带上当前选中的库区（下拉当前值在 input.value 里，不是文本节点）
      const warehouseValue = await dialog.locator('[data-testid="loc-warehouse"] input').inputValue();
      console.log('[DOM] 库位弹窗所属仓库 =', warehouseValue);
      expect(warehouseValue).toContain(`${RUN}-库区`);
      await dialog.locator('[data-testid="loc-code"] input').fill(`${RUN}-A1`);
      await dialog.locator('[data-testid="loc-name"] input').fill('A 区一排一列');
      await dialog.getByRole('button', { name: '创建' }).click();
      await expect(toast(page, /已创建/)).toBeVisible({ timeout: 15_000 });
      await expect(dialog).toBeHidden();

      const table = page.locator('[data-testid="location-table"]');
      await expect(table).toContainText(`${RUN}-A1`, { timeout: 15_000 });
      await expect(table).toContainText('A 区一排一列');
      const rows = await table.locator('.t-table__body tr').count();
      console.log('[DOM] 新建库位后行数 =', rows);

      const locations = await api.get(
        `/api/business/erp/location/list?warehouse_id=${created.warehouseId}`,
      );
      expect(locations.length).toBe(1);
      created.locationId = locations[0].id;
      console.log('[API] 新库位 =', locations[0].location_code, 'type =', locations[0].location_type);
    });
  } finally {
    // ==================== 清理 ====================
    if (created.locationId) {
      await api.del(`/api/business/erp/location/${created.locationId}`).catch((error: unknown) =>
        console.warn('[cleanup] 删库位失败：', String(error).slice(0, 120)),
      );
    }
    if (created.warehouseId) {
      await api.del(`/api/business/erp/warehouse/${created.warehouseId}`).catch((error: unknown) =>
        console.warn('[cleanup] 删仓库失败：', String(error).slice(0, 120)),
      );
    }
    const left = await fetchTree(api).catch(() => []);
    const residue = flatten(left).filter((node) => node.warehouse_name.startsWith(RUN));
    console.log('[cleanup] 残留节点数 =', residue.length);
    await api.dispose();
  }
});

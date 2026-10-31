/**
 * P4-B · 库位管理页端到端验收（真后端，不打桩）。
 *
 * 覆盖卡面第八节 ①–⑤：
 * ① 访问 `/business/erp/location`
 * ② **不再 404**（本卡核心验收点：路由手工注册过，深链响应 200 且页面组件真的挂上了）
 * ③ 列表渲染（行数 / 编码与 `GET /business/erp/location/page` 一致）
 * ④ 仓库筛选生效（下拉选仓库 → 请求带 `warehouse_id`，行集换成该仓库的）
 * ⑤ 截图 1 张
 *
 * # 为什么打 `127.0.0.1:4174` 的本地 dist
 *
 * 生产 144 上的 dist 还没带本卡页面（发布是另一张卡的事）。这里把**本次构建的 dist**
 * 跑在本地静态服务上，`/api` 反代到真后端 172.18.10.144 —— 浏览器跑的是本次改动的代码，
 * 接口 / 权限 / 数据全是真生产后端。见 `p4b-verify/serve.mjs`。
 *
 * # 只读用例
 *
 * 本页验收不造数据（新建 / 编辑的写入口径由仓库页的库位弹窗承担，已有 W4a 用例覆盖），
 * 所以没有 finally 清理，连跑不会互相踩。
 */
import { expect, test, request } from '@playwright/test';
import { readSharedToken, BASE_URL } from './support';

/** 截图目录（证据文档引用这个文件名） */
const SHOTS = process.env.P4B_SHOT_DIR ?? '/root/.hermes/workspace/frontend-developer/p4b-verify/shots';

/** 后端库位列表行（`dto.rs::LocationCard` 子集） */
interface LocationRowDto {
  id: string;
  location_code: string;
  location_name: string | null;
  warehouse_id: string;
  location_type: string;
  status: number;
}

/** 后端仓库树节点（`dto.rs::WarehouseTreeNode` 子集） */
interface WarehouseNodeDto {
  id: string;
  warehouse_code: string;
  warehouse_name: string;
  is_leaf: boolean;
  children: WarehouseNodeDto[];
}

/** 展平仓库树 */
function flatten(nodes: WarehouseNodeDto[]): WarehouseNodeDto[] {
  return nodes.flatMap((node) => [node, ...flatten(node.children ?? [])]);
}

/** 真后端 API（复用 globalSetup 的令牌，避免撞登录限流） */
function newApi() {
  const ctx = request.newContext({ baseURL: BASE_URL });
  const token = readSharedToken();
  expect(token, 'globalSetup 应已写入 e2e/.auth/token.txt').toBeTruthy();
  const headers = { Authorization: `Bearer ${token}` };

  /** 统一拆包：非 0 业务码直接抛后端文案 */
  const call = async <T>(url: string): Promise<T> => {
    const res = await (await ctx).get(url, { headers });
    const json = await res.json().catch(() => ({ code: -1, msg: `<非 JSON ${res.status()}>` }));
    if (json.code !== 0) {
      throw new Error(`API GET ${url} 失败：${res.status()} / ${json.code} / ${json.msg}`);
    }
    return json.data as T;
  };

  return { call, dispose: async () => (await ctx).dispose() };
}

test('P4-B：库位管理页（深链不再 404 / 列表渲染 / 仓库筛选生效）', async ({ page }) => {
  const api = newApi();

  /** 本用例发出的 `location/page` 请求（断言「筛选真的落到请求参数上」） */
  const pageRequests: URL[] = [];
  page.on('request', (req) => {
    const url = req.url();
    if (url.includes('/api/business/erp/location/page')) {
      pageRequests.push(new URL(url));
    }
  });

  try {
    const allLocations = await api.call<{ list: LocationRowDto[]; total: number }>(
      '/api/business/erp/location/page?page=1&page_size=100',
    );
    const tree = await api.call<WarehouseNodeDto[]>('/api/business/erp/warehouse/tree');
    const warehouses = flatten(tree);
    // 有库位的仓库（用来验「筛了就只剩它的」）+ 没库位的仓库（用来验空态）
    const counts = new Map<string, number>();
    for (const warehouse of warehouses) {
      const list = await api.call<LocationRowDto[]>(
        `/api/business/erp/location/list?warehouse_id=${warehouse.id}`,
      );
      counts.set(warehouse.id, list.length);
    }
    const withLocations = warehouses.find((node) => (counts.get(node.id) ?? 0) > 0);
    const emptyWarehouse = warehouses.find((node) => (counts.get(node.id) ?? 0) === 0);
    expect(withLocations, '后端至少应有一个带库位的仓库（种子数据）').toBeTruthy();
    console.log(
      '[API] 库位总数 =', allLocations.total,
      '｜仓库数 =', warehouses.length,
      '｜带库位的仓库 =', withLocations?.warehouse_name, `(${withLocations?.warehouse_code})`,
    );

    // ==================== ① ② 深链直达：不能再 404 ====================
    await test.step('① 直连 /business/erp/location：深链响应 200，路由挂上了页面', async () => {
      const response = await page.goto('/business/erp/location');
      expect(response, '深链应有主文档响应').toBeTruthy();
      expect(
        response!.status(),
        '深链返回 404 = 路由没注册（本卡要修的就是这个）或 dist 正在替换',
      ).toBe(200);
      await expect(page).toHaveURL(/\/business\/erp\/location$/);
      // 页面组件真的挂上了（404 页不会有这个 testid）
      await expect(page.locator('[data-testid="loc-table"]')).toBeVisible({ timeout: 30_000 });
      await expect(page.getByText('库位列表').first()).toBeVisible();
      console.log('[DOM] 深链 status = 200，URL =', page.url());
    });

    // ==================== ③ 列表渲染 ====================
    await test.step('③ 列表：行数 / 首行编码与后端一致', async () => {
      const table = page.locator('[data-testid="loc-table"]');
      const rows = table.locator('.t-table__body tr');
      await expect(rows).toHaveCount(Math.min(allLocations.total, 20));
      const codes = (await table.locator('.t-table__body tr td:first-child').allInnerTexts())
        .map((text) => text.trim());
      console.log('[DOM] 首屏编码 =', codes.join(' | '));
      expect(codes[0]).toBe(allLocations.list[0].location_code);
      // 类型 / 启用位是中文（枚举没有漏翻成 STORAGE 之类）
      await expect(table).toContainText('存储位');
      await expect(table).toContainText('启用');
      // meta 文案：全部仓库
      await expect(page.locator('[data-testid="loc-meta"]')).toContainText('全部仓库');
    });

    // ==================== ④ 仓库筛选生效 ====================
    await test.step('④ 仓库筛选：选仓库 → 请求带 warehouse_id，行集换成该仓库的', async () => {
      const target = withLocations!;
      pageRequests.length = 0;
      await page.locator('[data-testid="loc-filter-warehouse"] input').first().click();
      const option = page
        .locator('.t-select-option')
        .filter({ hasText: `（${target.warehouse_code}）` })
        .first();
      await expect(option).toBeVisible();
      await option.click();

      // 请求参数真的带上了 warehouse_id（不是「前端本地过滤」）
      await expect
        .poll(() => pageRequests.map((url) => url.searchParams.get('warehouse_id')).filter(Boolean), {
          timeout: 15_000,
        })
        .toContain(target.id);
      const last = pageRequests[pageRequests.length - 1];
      console.log('[NET] 筛选后请求 =', `${last.pathname}?${last.searchParams.toString()}`);
      expect(last.searchParams.get('warehouse_id')).toBe(target.id);

      // 行集 = 该仓库的库位（数量与 API 一致；每行都属于它）
      const expected = await api.call<LocationRowDto[]>(
        `/api/business/erp/location/list?warehouse_id=${target.id}`,
      );
      const table = page.locator('[data-testid="loc-table"]');
      await expect(table.locator('.t-table__body tr')).toHaveCount(Math.min(expected.length, 20));
      await expect(page.locator('[data-testid="loc-meta"]')).toContainText('当前仓库');
      await expect(table).toContainText(target.warehouse_name);
      console.log('[DOM] 筛选「' + target.warehouse_name + '」后行数 =', expected.length);
    });

    // ==================== ④' 空态友好（该仓库没有库位） ====================
    await test.step("④' 选一个没有库位的仓库 → 空态文案是「没有匹配的库位」", async () => {
      if (!emptyWarehouse) {
        console.log('[SKIP] 后端每个仓库都有库位，跳过空态断言');
        return;
      }
      await page.locator('[data-testid="loc-filter-warehouse"] input').first().click();
      const option = page
        .locator('.t-select-option')
        .filter({ hasText: `（${emptyWarehouse.warehouse_code}）` })
        .first();
      await expect(option).toBeVisible();
      await option.click();
      await expect(page.locator('[data-testid="loc-empty"]')).toBeVisible({ timeout: 15_000 });
      await expect(page.locator('[data-testid="loc-empty"]')).toContainText('没有匹配的库位');
      console.log('[DOM] 空仓库「' + emptyWarehouse.warehouse_name + '」空态文案 =',
        (await page.locator('[data-testid="loc-empty"]').innerText()).replace(/\s+/g, ' '));
    });

    // ==================== ⑤ 重置 + 截图 ====================
    await test.step('⑤ 重置回跨仓总览 + 截图', async () => {
      await page.locator('[data-testid="btn-loc-reset"]').click();
      await expect(page.locator('[data-testid="loc-meta"]')).toContainText('全部仓库', { timeout: 15_000 });
      await expect(page.locator('[data-testid="loc-table"] .t-table__body tr'))
        .toHaveCount(Math.min(allLocations.total, 20));
      await page.waitForTimeout(300);
      await page.screenshot({ path: `${SHOTS}/01-location-page.png`, fullPage: true });
      console.log('[SHOT]', `${SHOTS}/01-location-page.png`);
    });
  } finally {
    await api.dispose();
  }
});

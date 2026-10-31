import { expect, test } from '@playwright/test';
import { BASE_URL, assertNavigationOk, newApi } from './support';

/**
 * 专项 1：**菜单路径 ↔ 前端路由全量对齐**（防 404）。
 *
 * 老板报过最多的问题就是「菜单点进去 404」（库位管理 / 计量单位 / 会计科目）。
 * 这里不看代码注释、不看菜单种子，直接：
 *   ① 用当前账号调 `GET /api/system/auth/menus` 拿**真菜单树**；
 *   ② 抽出所有可点叶子（有 path、menu_type=2、可见）逐个 navigate；
 *   ③ 断言每个路径都渲染出正常页面（不是 404 结果页）、且页面级 0 个 4xx/5xx 接口。
 *
 * 已知缺口（2026-10-31 实测）：
 *   `/business/erp/purchase-order-item`（采购明细）与 `/business/erp/seq`（单号序列）
 *   曾在侧栏菜单里存在、但 `src/router/index.ts` 里没有对应路由 → 落到 catch-all 的 404 页。
 *   **当天已由前端提交 cd8b054 补齐两条路由**，所以白名单现在是空的：
 *   `KNOWN_BROKEN` 一旦非空就说明又出现了「菜单有、路由无」的回归。
 *
 * 404 判定补了 `.result-page`（仓库自写的 403/404/500 页用它，TDesign 的 `.t-result` 是另一套）：
 * 只按 `.t-result` 判会把「真的 404」漏判成通过 —— 这条 2026-10-31 实测踩过。
 */
const KNOWN_BROKEN: string[] = [];

interface MenuNode {
  path?: string | null;
  title?: string;
  name?: string;
  menu_type?: number;
  status?: number;
  is_visible?: boolean;
  children?: MenuNode[];
}

function flatten(nodes: MenuNode[], out: Array<{ path: string; title: string }> = []): Array<{ path: string; title: string }> {
  for (const n of nodes) {
    const visible = n.is_visible !== false && (n.status === undefined || n.status === 1);
    if (visible && n.menu_type === 2 && n.path && n.path.startsWith('/')) {
      out.push({ path: n.path, title: n.title || n.name || n.path });
    }
    if (n.children?.length) {
      flatten(n.children, out);
    }
  }
  return out;
}

test.describe('专项1 菜单路径 ↔ 路由全量对齐', () => {
  test('所有侧栏菜单路径都能打开：无 404 结果页、无 4xx/5xx 接口（已知缺口单独断言）', async ({ page }) => {
    const api = await newApi();
    const res = await api.ctx.get('/api/system/auth/menus');
    expect(res.status(), '菜单接口应 200').toBe(200);
    const body = await res.json();
    const leaves = flatten(body?.data ?? []);
    expect(leaves.length, '菜单叶子数量').toBeGreaterThan(20);
    await api.dispose();

    const bad: Array<{ path: string; title: string; reason: string }> = [];
    const checked: string[] = [];

    for (const leaf of leaves) {
      const badApi: string[] = [];
      const errors: string[] = [];
      const onResp = (r: import('@playwright/test').Response) => {
        const path = r.url().replace(BASE_URL, '').split('?')[0];
        // 通知 SSE 的 401 是**已知缺陷**（后端不认 ?token=，见 _pagecheck.KNOWN_BAD_API），
        // 它每页都会出现，不能拿它判定「这个菜单页坏了」；单独由 34 号专项留证。
        if (r.status() >= 400 && r.url().includes('/api/') && !/^\/api\/business\/notice\/stream$/.test(path)) {
          badApi.push(`${r.status()} ${r.request().method()} ${path}`);
        }
      };
      const onErr = (e: Error) => errors.push(String(e).slice(0, 200));
      page.on('response', onResp);
      page.on('pageerror', onErr);
      try {
        const nav = await page.goto(leaf.path, { waitUntil: 'domcontentloaded' });
        assertNavigationOk(nav, leaf.path);
        await page.waitForSelector('.basic-layout__aside, .t-result', { timeout: 20_000 });
        const is404 = await page.evaluate(
          () =>
            /页面不存在/.test(document.title) ||
            // 两套结果页实现：TDesign `.t-result` 与仓库自写 `.result-page`（403/404/500 用它）
            Array.from(document.querySelectorAll('.t-result, .result-page')).some((el) =>
              /404|页面不存在/.test(el.textContent ?? ''),
            ),
        );
        if (is404) {
          bad.push({ path: leaf.path, title: leaf.title, reason: '渲染 404 结果页' });
        }
        if (badApi.length) {
          bad.push({ path: leaf.path, title: leaf.title, reason: `接口报错 ${badApi.join(' | ')}` });
        }
        if (errors.length) {
          bad.push({ path: leaf.path, title: leaf.title, reason: `JS 异常 ${errors.join(' | ')}` });
        }
        checked.push(leaf.path);
      } catch (e) {
        bad.push({ path: leaf.path, title: leaf.title, reason: `导航失败 ${String(e).slice(0, 160)}` });
      } finally {
        page.off('response', onResp);
        page.off('pageerror', onErr);
      }
    }

    const broken = bad.map((b) => b.path).sort();
    expect(checked.length + broken.length, '每个菜单叶子都必须真的访问过').toBe(leaves.length);

    // 白名单为空：任何一条菜单 404 / 接口报错都是回归。历史缺口（采购明细 / 单号序列）
    // 已由 cd8b054 补齐路由，写进报告即可，不再需要白名单。
    expect(KNOWN_BROKEN.length, '白名单应为空（历史缺口已修）').toBe(0);
    expect(bad, `菜单打开异常：${JSON.stringify(bad, null, 1)}`).toEqual([]);
    expect(checked.length, '实际打开成功的菜单叶子数').toBe(leaves.length);
  });
});

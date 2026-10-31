import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, test } from '@playwright/test';
import { PAGES } from './_catalog';

/**
 * 卡面（工单）写的路径 vs 真实路由：旧链接还能不能开。
 *
 * 为什么要单独一条 spec：卡面点名的 52 页里有 10 页写的是**旧路径**
 * （`/system/masking`、`/announcement/center`、`/business/project` …），
 * 真实路由是 `/system/field-mask`、`/announcement`、`/project`。
 * 逐页 6 项检查跑的是**真实路由**（否则测的就不是用户真正点到的页面），
 * 于是「卡面路径到底能不能开」这件事就没人验 —— 这条 spec 专门补这个洞：
 *
 * - 有 `alias` 的旧路径（router 里真配了）：必须**重定向**到真实路由并正常渲染；
 * - 没有 alias 的旧路径：必须落到**友好的 404 结果页**（有 404 文案 + 返回首页按钮），
 *   **不许白屏**。这就是老板说的「点了 404」在路由层的表现，实际会不会遇到取决于
 *   菜单/通知里存的路径，专项 1（`30-menu-alignment`）负责菜单那一侧。
 *
 * 结果落盘到 `e2e-report/legacy-paths.json`，报告生成器读它出「旧路径清单」。
 */

/** router 里给旧路径配了 alias 的（2026-10-31 读 `src/router/index.ts` 核实：`alias:` 只有这两条） */
const ALIASED = new Set(['/business/erp/purchase', '/business/erp/stockin']);

interface LegacyRow {
  page: string;
  legacy: string;
  canonical: string;
  aliased: boolean;
  navStatus: number;
  finalPath: string;
  is404: boolean;
  bodyText: string;
  pageErrors: string[];
}

const ROWS: LegacyRow[] = [];
const OUT = resolve(process.cwd(), 'e2e-probe/legacy-paths.json');

function flush(): void {
  mkdirSync(resolve(process.cwd(), 'e2e-probe'), { recursive: true });
  writeFileSync(OUT, JSON.stringify(ROWS, null, 2), 'utf8');
}

const entries = PAGES.filter((p) => p.legacyPath).map((p) => ({
  page: p.key,
  legacy: p.legacyPath as string,
  canonical: p.path,
}));

test.describe('卡面旧路径: 能开 / 重定向 / 友好 404（防白屏）', () => {
  test(`卡面里引用的旧路径共 ${entries.length} 条（少于 10 条说明目录被改瘦了）`, async () => {
    expect(entries.length, `旧路径清单：${entries.map((e) => e.legacy).join(', ')}`).toBeGreaterThanOrEqual(10);
  });

  for (const e of entries) {
    test(`${e.legacy} → ${e.canonical}（${ALIASED.has(e.legacy) ? '有 alias，应重定向' : '无 alias，应友好 404'}）`, async ({
      page,
    }) => {
      const pageErrors: string[] = [];
      page.on('pageerror', (err) => pageErrors.push(String(err).slice(0, 200)));

      const res = await page.goto(e.legacy);
      // SPA 入口永远 200（nginx/vite 的 fallback），所以这一条只能证明「服务活着」
      expect(res?.status(), `${e.legacy} 的文档请求`).toBe(200);

      // 布局或结果页必须出来：白屏（两者都没有）就是真缺陷
      await expect(
        page.locator('.basic-layout__aside, .result-page, .t-result').first(),
        `${e.legacy} 渲染出布局或结果页（白屏 = 缺陷）`,
      ).toBeVisible();

      const code = ((await page.locator('.result-page__code').first().textContent().catch(() => '')) ?? '').trim();
      const bodyText = ((await page.locator('body').innerText().catch(() => '')) ?? '').replace(/\s+/g, ' ').slice(0, 400);
      const finalPath = new URL(page.url()).pathname + (new URL(page.url()).search || '');
      const is404 = code.startsWith('404') || /访问的页面不存在/.test(bodyText);

      ROWS.push({
        page: e.page,
        legacy: e.legacy,
        canonical: e.canonical,
        aliased: ALIASED.has(e.legacy),
        navStatus: res?.status() ?? 0,
        finalPath,
        is404,
        bodyText: bodyText.slice(0, 200),
        pageErrors,
      });
      flush();

      expect(pageErrors, `${e.legacy} 不该有 JS 异常：${pageErrors.join(' | ')}`).toEqual([]);

      if (ALIASED.has(e.legacy)) {
        // vue-router 的 `alias` **不做 URL 重定向**：同一条 route 直接就近匹配，地址栏保持旧路径。
        // 所以有 alias 的判据是「渲染出真实页面（不是 404）」，不是「URL 变成新路径」。
        expect(finalPath, `${e.legacy} 走 alias，URL 保持旧路径`).toContain(e.legacy);
        expect(is404, `${e.legacy} 有 alias，不应落到 404 页（body: ${bodyText.slice(0, 80)}）`).toBe(false);
        await expect(page.locator('.basic-layout__content .t-table').first()).toBeVisible();
        await expect(page.locator('.basic-layout__content .t-table tbody tr').first()).toBeVisible();
      } else {
        // 现状（2026-10-31）：这 8 条旧路径在 router 里没有 alias，点了会进 404 页。
        // 断言「是**友好的** 404」而不是白屏/空内容 —— 白屏才是真缺陷。
        expect(is404, `${e.legacy} 无 alias 时应落到 404 结果页（body: ${bodyText.slice(0, 80)}）`).toBe(true);
        await expect(page.locator('.result-page__desc')).toContainText('抱歉，您访问的页面不存在');
        await expect(page.locator('.result-page button')).toHaveCount(1);
      }
    });
  }
});

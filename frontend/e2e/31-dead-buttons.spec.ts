import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, test } from '@playwright/test';
import { PAGES } from './_catalog';
import { closeOverlays, type Observation } from './_pagecheck';

/**
 * 专项 2：**按钮可达性全量**（防「有按钮没反应」）。
 *
 * 分两段，因为两类按钮的风险不同：
 *
 * 1. **汇总段**（不点，读现场）：55 个页面在 `_pagerunner` 里已经逐页点了每个
 *    「查询 / 重置 / 刷新 / 导出 / 新建 / 编辑 …」，每人 800ms 观察窗口内必须出现
 *    弹窗 / 抽屉 / 请求 / Toast / 路由变化之一。这里把所有页面的现场 JSON 汇总，
 *    断言**死按钮清单为空**，并在失败信息里逐页列出（报告就按这份清单出）。
 *
 * 2. **危险按钮段**（真点，但拦写请求）：删除 / 停用 / 发布 / 结账这类按钮一点就改
 *    生产数据，所以这里用 `page.route` 把**非 GET 的 /api 请求全部 abort**——
 *    要验的是「点了有没有反应（二次确认框 / 提示 / 请求发起）」，不是「数据改没改」。
 *    这样既能覆盖危险按钮，又不会真的删掉线上数据。
 */
const PROBE_DIR = resolve(process.cwd(), 'e2e-report/probe');

function loadObservations(): Observation[] {
  return readdirSync(PROBE_DIR)
    .filter((f) => f.endsWith('.json') && f !== 'dead-buttons.json')
    .map((f) => JSON.parse(readFileSync(resolve(PROBE_DIR, f), 'utf8')) as Observation);
}

test.describe('专项2 按钮可达性全量', () => {
  test('汇总 55 页现场：逐页点过的按钮里没有「死按钮」', async () => {
    const all = loadObservations();
    const expected = PAGES.filter((p) => !(p.note ?? '').includes('预期 404'));
    expect(all.length, `每页都要有现场 JSON（应有 ${expected.length} 份）`).toBe(expected.length);

    const dead = all.flatMap((o) =>
      o.interact.dead.map((d) => `${o.key}（${o.path}）→「${d.button}」：${d.reason}`),
    );
    const clicked = all.reduce((n, o) => n + o.interact.clicked.length, 0);
    const reacted = all.reduce((n, o) => n + o.interact.reactions.length, 0);
    // 断言不是恒真：点击总数与反应数都要真的 > 0
    expect(clicked, `本轮实际点击的按钮总数`).toBeGreaterThan(100);
    expect(reacted).toBeGreaterThan(100);
    expect(dead, `死按钮清单（${dead.length} 个）：\n${dead.join('\n')}`).toEqual([]);
  });

  test('危险按钮真点：删除 / 停用 / 发布等必须给出可观察反应（写请求已被用例 abort）', async ({ browser }) => {
    const all = loadObservations();
    const targets = all
      .filter((o) => o.interact.skippedDanger.length > 0 && !o.is404)
      .slice(0, 14);
    expect(targets.length, '有危险按钮的页面数量').toBeGreaterThan(0);

    const page = await browser.newPage();
    const aborted: string[] = [];
    await page.route('**/api/**', async (route) => {
      if (route.request().method() === 'GET') {
        await route.continue();
        return;
      }
      aborted.push(`${route.request().method()} ${new URL(route.request().url()).pathname}`);
      await route.abort();
    });

    const dead: string[] = [];
    const reacted: string[] = [];
    try {
      for (const obs of targets) {
        await page.goto(obs.path, { waitUntil: 'domcontentloaded' });
        await page.waitForSelector('.basic-layout__aside', { timeout: 20_000 });
        await page.waitForLoadState('networkidle', { timeout: 8_000 }).catch(() => undefined);

        for (const text of obs.interact.skippedDanger.slice(0, 6)) {
          const btn = page
            .locator('.basic-layout__content button')
            .filter({ hasText: text })
            .first();
          if (!(await btn.isVisible().catch(() => false))) {
            continue;
          }
          let reqs = 0;
          const onReq = () => {
            reqs += 1;
          };
          page.on('request', onReq);
          const before = await page.locator('.t-dialog, .t-drawer').evaluateAll(
            (els) => els.filter((el) => (el as HTMLElement).offsetParent !== null).length,
          );
          await btn.click({ timeout: 5_000 }).catch(() => undefined);
          const kind = await page
            .waitForFunction(
              (prev) =>
                Array.from(document.querySelectorAll('.t-dialog, .t-drawer')).filter(
                  (el) => (el as HTMLElement).offsetParent !== null,
                ).length > prev ||
                document.querySelectorAll('.t-message').length > 0,
              before,
              { timeout: 1_200 },
            )
            .then(() => '弹窗/Toast')
            .catch(() => '');
          page.off('request', onReq);
          const label = `${obs.key}（${obs.path}）→「${text}」`;
          if (kind || reqs > 0) {
            reacted.push(`${label}：${[kind, reqs ? `请求 ×${reqs}` : ''].filter(Boolean).join(' + ')}`);
          } else {
            dead.push(`${label}：1200ms 内无确认框、无提示、无请求`);
          }
          await closeOverlays(page);
        }
      }
    } finally {
      await page.context().close();
    }

    expect(reacted.length, `点过的危险按钮数`).toBeGreaterThan(4);
    expect(dead, `点不出任何反应的危险按钮：\n${dead.join('\n')}`).toEqual([]);
    // abort 生效的实证：写请求确实被拦下来了（这条不做硬断言，只记录在报告里）
    console.log(`[专项2] 被 abort 的写请求 ${aborted.length} 条：${aborted.slice(0, 10).join(', ')}`);
  });
});

/**
 * P4-A · ERP 主数据页（计量单位 + 供应商）端到端验收（真后端，不 mock）。
 *
 * 覆盖卡面第七节 ①–④：
 * ① `/business/erp/unit` 打开，列表渲染（种子 3 条：PCS 个 / KG 千克 / BOX 箱）
 * ② `/business/erp/supplier` 打开，列表渲染（种子 SUP-0001）
 * ③ 两页都无 404、无 4xx（拦所有 `/api` 响应逐条查状态码）
 * ④ 截图 2 张
 *
 * 额外各断一条：关键词筛选（真接口）+ 详情抽屉（`GET /erp/{x}/{id}`）+ 空态（筛不存在的词）。
 *
 * # 为什么打 `127.0.0.1:4174`
 *
 * 生产 144 上的 dist 还是上一版（本卡页面没发布），验不到本次改动。这里用
 * `p4a-unit-supplier-7c183fba/serve-dist.mjs` 把**本次构建的 dist** 跑在本地，
 * `/api` 反代到真后端 144 —— 浏览器跑新代码，接口 / 权限 / 数据全是真生产后端。
 *
 * # 只读
 *
 * 用例**不建数据**（生产是共享库）。写路径由 `tests/unit/erp-master-data.test.ts`
 * 守请求体口径，接口契约由 `handler.rs` 的 `#[nik::post/put/delete]` 声明。
 */
import { expect, test, type ConsoleMessage } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { openAuthed } from './support';

/** 截图目录（证据文档引用这些文件名） */
const SHOTS =
  process.env.P4A_SHOT_DIR ??
  '/root/.hermes/workspace/frontend-developer/p4a-unit-supplier-7c183fba/shots';
mkdirSync(SHOTS, { recursive: true });

/** 收集一页生命周期内所有失败的网络请求（4xx/5xx）与浏览器报错 */
interface NetAudit {
  /** 真正的 4xx/5xx 响应（`<status> <url>`） */
  failures: string[];
}

/**
 * 挂网络监听。
 *
 * 404 在 SPA 里最典型的来源不是接口，是**路由没注册** → 深链直接 404 HTML。
 * 所以这里既拦 `/api` 的状态码，也拦导航响应本身。
 */
function auditNetwork(page: import('@playwright/test').Page): NetAudit {
  const audit: NetAudit = { failures: [] };
  page.on('response', (response) => {
    const url = response.url();
    const status = response.status();
    // 通知 SSE 的 401 是**已知产品缺陷**（后端认证中间件只读 Authorization 头，不认 EventSource 的
    // `?token=`，见 docs/test-report-e2e.md 第四节）：每页都会命中，不能拿它判「这页坏了」。
    if (url.includes('/api/business/notice/stream')) {
      return;
    }
    if (url.includes('/api/') && status >= 400) {
      audit.failures.push(`${status} ${url}`);
    }
  });
  page.on('console', (message: ConsoleMessage) => {
    if (message.type() === 'error' && /404|Not Found/i.test(message.text())) {
      audit.failures.push(`console: ${message.text().slice(0, 160)}`);
    }
  });
  return audit;
}

test('① 计量单位页：路由不再 404，列表渲染种子数据，状态/单位类型 tag 正确', async ({ page }) => {
  const audit = auditNetwork(page);
  const response = await page.goto('/business/erp/unit');
  expect(response?.status(), '深链应返回 index.html（200），404 = 路由没注册').toBe(200);

  await expect(page.locator('.basic-layout__aside')).toBeVisible();
  await expect(page.locator('[data-testid="unit-table"]')).toBeVisible();

  // 标题 + 总数（种子 3 条）
  await expect(page.locator('.panel-head__title')).toHaveText('计量单位');
  await expect(page.locator('[data-testid="unit-meta"]')).toContainText('共');
  await expect(page.locator('[data-testid="unit-meta"]')).toContainText('3');

  // 种子行：PCS 个（基础单位）、BOX 箱（折算 12 个）
  const rows = page.locator('[data-testid="unit-table"] tbody tr');
  await expect(rows).toHaveCount(3);
  await expect(page.locator('[data-testid="unit-table"]')).toContainText('PCS');
  await expect(page.locator('[data-testid="unit-table"]')).toContainText('KG');
  await expect(page.locator('[data-testid="unit-table"]')).toContainText('BOX');
  await expect(page.locator('[data-testid="unit-table"]')).toContainText('基础单位');

  // 折算关系：BOX 那行是「1 = 12 个」（折算率走 base_unit_id 译名）
  const boxRow = rows.filter({ hasText: 'BOX' });
  await expect(boxRow).toContainText('12');

  // 状态 tag 是 green（启用）
  await expect(rows.first().locator('[data-testid="unit-status-tag"]')).toHaveText('启用');

  await page.screenshot({ path: `${SHOTS}/01-unit-list.png`, fullPage: true });

  expect(audit.failures, `不允许任何 4xx/5xx：\n${audit.failures.join('\n')}`).toEqual([]);
});

test('② 供应商页：路由不再 404，列表渲染种子数据，联系人/账期列有值', async ({ page }) => {
  const audit = auditNetwork(page);
  const response = await page.goto('/business/erp/supplier');
  expect(response?.status(), '深链应返回 index.html（200），404 = 路由没注册').toBe(200);

  await expect(page.locator('.basic-layout__aside')).toBeVisible();
  await expect(page.locator('[data-testid="supplier-table"]')).toBeVisible();

  await expect(page.locator('.panel-head__title')).toHaveText('供应商');
  await expect(page.locator('[data-testid="sup-meta"]')).toContainText('共');

  const table = page.locator('[data-testid="supplier-table"]');
  await expect(table).toContainText('SUP-0001');
  await expect(table).toContainText('示例供应商');

  // 账期 30 天（种子 credit_days = 30）
  await expect(table).toContainText('30 天');
  // 联系人 / 税号为空时统一 `—`，不出现 "null"
  await expect(table).not.toContainText('null');
  await expect(table.locator('[data-testid="sup-status-tag"]').first()).toHaveText('启用');

  await page.screenshot({ path: `${SHOTS}/02-supplier-list.png`, fullPage: true });

  expect(audit.failures, `不允许任何 4xx/5xx：\n${audit.failures.join('\n')}`).toEqual([]);
});

test('③ 计量单位：关键词筛选走真接口，查不到时是友好空态（不是白屏）', async ({ page }) => {
  const audit = auditNetwork(page);
  await openAuthed(page, '/business/erp/unit');
  await expect(page.locator('[data-testid="unit-meta"]')).toContainText('3');

  // 命中：搜 BOX
  await page.fill('[data-testid="unit-keyword"] input', 'BOX');
  await page.click('[data-testid="btn-unit-search"]');
  await expect(page.locator('[data-testid="unit-table"] tbody tr')).toHaveCount(1);
  await expect(page.locator('[data-testid="unit-table"]')).toContainText('箱');

  // 不命中：空态文案要分「没匹配」与「没数据」
  await page.fill('[data-testid="unit-keyword"] input', 'ZZZ-NOT-EXIST');
  await page.click('[data-testid="btn-unit-search"]');
  await expect(page.locator('[data-testid="unit-empty"]')).toBeVisible();
  await expect(page.locator('[data-testid="unit-empty"]')).toContainText('没有匹配的计量单位');

  // 重置回全量
  await page.click('[data-testid="btn-unit-reset"]');
  await expect(page.locator('[data-testid="unit-table"] tbody tr')).toHaveCount(3);

  expect(audit.failures, `不允许任何 4xx/5xx：\n${audit.failures.join('\n')}`).toEqual([]);
});

test('④ 两页详情抽屉：走 GET /erp/{unit,supplier}/{id}，字段与列表一致', async ({ page }) => {
  const audit = auditNetwork(page);

  await openAuthed(page, '/business/erp/unit');
  await page.locator('[data-testid="btn-unit-view"]').first().click();
  await expect(page.locator('[data-testid="unit-detail"]')).toBeVisible();
  await expect(page.locator('[data-testid="unit-detail"]')).toContainText('PCS');
  await expect(page.locator('[data-testid="unit-detail"]')).toContainText('单位编码');
  await page.screenshot({ path: `${SHOTS}/03-unit-detail.png`, fullPage: true });
  await page.click('[data-testid="btn-unit-drawer-close"]');

  await openAuthed(page, '/business/erp/supplier');
  await page.locator('[data-testid="btn-sup-view"]').first().click();
  await expect(page.locator('[data-testid="sup-detail"]')).toBeVisible();
  await expect(page.locator('[data-testid="sup-detail"]')).toContainText('SUP-0001');
  await page.screenshot({ path: `${SHOTS}/04-supplier-detail.png`, fullPage: true });

  expect(audit.failures, `不允许任何 4xx/5xx：\n${audit.failures.join('\n')}`).toEqual([]);
});

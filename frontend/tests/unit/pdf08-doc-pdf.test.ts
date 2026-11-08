/**
 * PDF-08 单测：「下载 PDF」的纯逻辑 + 12 个入口的页面契约。
 *
 * # 钉死的口径
 *
 * 1. **文件名只认 `filename*=`（RFC 5987）**：后端 ASCII 回退名会把中文替换成 `_`
 *    （`采购订单-PO-1.pdf` → `____-PO-1.pdf`），用它用户会存成一堆下划线。
 * 2. **403 必须说「无权限」**：验收⑤「无权限 → 403 + 提示」——提示里要能看出是权限问题，
 *    而不是「导出失败」四个字让用户去猜。
 * 3. **实现必须带 token**：`useDocPdfDownload` 只能 `fetch` + `Authorization: Bearer`，
 *    **不许** `window.open` / `<a href>` 直开（带不上鉴权头，必 401）。
 * 4. **页面契约**：12 个入口（9 张单据 + 审批实例 + 任务 + 3 张报表）都挂 `DocPdfButton`，
 *    路径带 `/api` 前缀 —— 漏前缀会命中 SPA fallback 拿回 HTML。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  docPdfErrorMessage,
  filenameFromContentDisposition,
  sanitizeFileName,
} from '../../src/utils/doc-pdf';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

/** 后端 `pdf::handler::content_disposition` 的真实输出形态（ASCII 回退 + RFC5987） */
const CD_CHINESE =
  'attachment; filename="____-PO-202601-0001.pdf"; filename*=UTF-8\'\'%E9%87%87%E8%B4%AD%E8%AE%A2%E5%8D%95-PO-202601-0001.pdf';

// ==================== ① 文件名解析 ====================

test('filename* 解出后端的中文下载名', () => {
  assert.equal(
    filenameFromContentDisposition(CD_CHINESE),
    '采购订单-PO-202601-0001.pdf',
  );
});

test('ASCII 回退名带下划线（中文被打码）→ 不用它，返回 null 让调用方回退', () => {
  assert.equal(
    filenameFromContentDisposition('attachment; filename="____-PO-1.pdf"'),
    null,
  );
});

test('纯 ASCII 的 filename= 可用（合同英文名那类）', () => {
  assert.equal(
    filenameFromContentDisposition('attachment; filename="contract-PO-1.pdf"'),
    'contract-PO-1.pdf',
  );
});

test('没有 Content-Disposition → null', () => {
  assert.equal(filenameFromContentDisposition(null), null);
  assert.equal(filenameFromContentDisposition(''), null);
  assert.equal(filenameFromContentDisposition('inline'), null);
});

test('坏掉的百分号编码不抛异常（落回 filename= 或 null）', () => {
  assert.equal(
    filenameFromContentDisposition("attachment; filename*=UTF-8''%E9%87%ZZ.pdf"),
    null,
  );
});

test('文件名去掉路径分隔符与控制字符（防目录穿越 / 换行注入）', () => {
  assert.equal(sanitizeFileName('../../etc/passwd'), '..-..-etc-passwd');
  assert.equal(sanitizeFileName('合同\n-PO-1.pdf'), '合同-PO-1.pdf');
  assert.equal(sanitizeFileName('   '), 'document.pdf');
});

test('filename* 里的中文名同样过一遍净化', () => {
  const header = `attachment; filename*=UTF-8''${encodeURIComponent('合同/a.pdf')}`;
  assert.equal(filenameFromContentDisposition(header), '合同-a.pdf');
});

// ==================== ② 错误文案 ====================

test('403 → 明确说「没有导出权限」并带上后端原因', () => {
  const text = docPdfErrorMessage(403, { code: 10003, msg: '缺少权限码：business:erp:contract:list' });
  assert.match(text, /没有导出该单据 PDF 的权限/);
  assert.match(text, /business:erp:contract:list/);
});

test('403 没有可读消息时也给「联系管理员分配」的落点', () => {
  const text = docPdfErrorMessage(403, '<html>403 Forbidden</html>');
  assert.match(text, /没有导出该单据 PDF 的权限/);
  assert.match(text, /联系管理员/);
  assert.doesNotMatch(text, /</);
});

test('401 → 提示重新登录', () => {
  assert.match(docPdfErrorMessage(401, null), /登录已过期/);
});

test('404 → 单据不存在', () => {
  assert.match(docPdfErrorMessage(404, { code: 63001, msg: '任务不存在' }), /任务不存在/);
});

test('5xx → 渲染服务暂时不可用（可重试语义）', () => {
  assert.match(docPdfErrorMessage(503, { code: 67001, msg: 'CDP 连不上' }), /PDF 渲染服务暂时不可用/);
  assert.match(docPdfErrorMessage(500, null), /稍后重试/);
});

test('其它状态码兜底带 HTTP 码，且不把 HTML 甩给用户', () => {
  assert.match(docPdfErrorMessage(418, null), /HTTP 418/);
  assert.doesNotMatch(docPdfErrorMessage(422, '<!DOCTYPE html>'), /DOCTYPE/);
});

test('错误体是 JSON 字符串时也能取到 msg', () => {
  assert.match(docPdfErrorMessage(400, '{"code":10001,"msg":"period 格式应为 yyyy-MM"}'), /yyyy-MM/);
});

// ==================== ③ 实现契约：必须 fetch + Authorization ====================

/** 去掉块注释 / 行注释后再断言，避免把「为什么不用 window.open」的说明当成实现 */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
}

test('下载器用 fetch 带 Bearer token，不许 window.open / 裸 <a href>', () => {
  const source = stripComments(readSource('composables/useDocPdfDownload.ts'));
  assert.match(source, /fetch\(/);
  assert.match(source, /Authorization.*Bearer/);
  assert.match(source, /saveBlob/);
  assert.match(source, /content-disposition/i);
  assert.doesNotMatch(source, /window\.open/);
});

test('按钮组件用同一份下载器（不在组件里另写 fetch）', () => {
  const source = stripComments(readSource('components/DocPdfButton.vue'));
  assert.match(source, /useDocPdfDownload/);
  assert.doesNotMatch(source, /window\.open/);
  assert.doesNotMatch(source, /fetch\(/);
});

// ==================== ④ 页面契约：12 个入口 ====================

interface PageCase {
  /** 页面文件（相对 src/） */
  file: string;
  /** 必须出现的路径片段（含 `/api` 前缀） */
  pathFragment: string;
  /** 必须出现的 testid */
  testid: string;
}

const PAGES: PageCase[] = [
  { file: 'views/business/erp/purchase/index.vue', pathFragment: '/api/business/erp/purchase-order/${detail.id}/pdf', testid: 'purchase-detail-pdf' },
  { file: 'views/business/erp/sales-order/index.vue', pathFragment: '/api/business/erp/sales-order/${detail.id}/pdf', testid: 'so-detail-pdf' },
  { file: 'views/business/erp/contract/index.vue', pathFragment: '/api/business/erp/contract/${detail.id}/pdf', testid: 'contract-detail-pdf' },
  { file: 'views/business/erp/stockin/index.vue', pathFragment: '/api/business/erp/stock-in/${detail.header.id}/pdf', testid: 'stockin-detail-pdf' },
  { file: 'views/business/erp/stock-out/index.vue', pathFragment: '/api/business/erp/stock-out/${detail.id}/pdf', testid: 'stockout-detail-pdf' },
  { file: 'views/business/erp/transfer/index.vue', pathFragment: '/api/business/erp/stock-transfer/${detail.id}/pdf', testid: 'transfer-detail-pdf' },
  { file: 'views/business/erp/stock-take/index.vue', pathFragment: '/api/business/erp/stock-take/${detail.id}/pdf', testid: 'stocktake-detail-pdf' },
  { file: 'views/business/erp/purchase-return/index.vue', pathFragment: '/api/business/erp/purchase-return/${detail.purchase_return.id}/pdf', testid: 'prt-detail-pdf' },
  { file: 'views/business/erp/sales-return/index.vue', pathFragment: '/api/business/erp/sales-return/${detail.id}/pdf', testid: 'return-detail-pdf' },
  { file: 'views/business/flow/instance/detail.vue', pathFragment: '/api/business/flow/instance/${detail.instance.id}/pdf', testid: 'flow-instance-pdf' },
  { file: 'views/business/task/detail.vue', pathFragment: '/api/business/task/${detail.id}/pdf', testid: 'task-detail-pdf' },
  { file: 'views/business/erp/report/balance-sheet/index.vue', pathFragment: '/api/business/erp/report/balance-sheet/pdf', testid: 'bs-export-pdf' },
  { file: 'views/business/erp/report/income-statement/index.vue', pathFragment: '/api/business/erp/report/income-statement/pdf', testid: 'is-export-pdf' },
  { file: 'views/business/erp/report/cash-flow/index.vue', pathFragment: '/api/business/erp/report/cash-flow/pdf', testid: 'cf-export-pdf' },
];

for (const page of PAGES) {
  test(`页面契约：${page.file} 有「下载 PDF」按钮且路径带 /api`, () => {
    const source = readSource(page.file);
    assert.match(source, /DocPdfButton/, `${page.file} 未引入 DocPdfButton`);
    assert.ok(
      source.includes(`<DocPdfButton`),
      `${page.file} 未渲染 <DocPdfButton>`,
    );
    assert.ok(
      source.includes(page.pathFragment),
      `${page.file} 的 PDF 路径应为 ${page.pathFragment}`,
    );
    assert.ok(
      source.includes(`testid="${page.testid}"`),
      `${page.file} 缺少 testid=${page.testid}`,
    );
    // 漏 /api 前缀会命中 SPA fallback（返回 index.html），必须拦住
    assert.match(source, /\/api\/business\//);
  });
}

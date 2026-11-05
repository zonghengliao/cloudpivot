/**
 * ERP 三张单据页「批量导出」的状态过滤契约单测（卡 t_f83ae18f）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * # 要钉住的是什么
 *
 * 后端卡 `t_93cba3f4`（commit `dce703b`）把导出状态过滤改成了**文本状态码契约**：
 *
 * | 字段 | 谁能用 | 用错会怎样 |
 * |------|--------|-----------|
 * | `status`（i16，1 启用 / 0 停用） | `system.dict_item` / `system.user` | ERP 单据传 → **422 + 10001** |
 * | `status_code`（文本 `POSTED` …） | `erp.stock_in` / `erp.payable` / `erp.voucher` | 字典项 / 用户传 → **422** |
 *
 * 而前端三张单据页（收货 / 应付 / 凭证）当时只发 `{ ids, keyword }`：列表页勾了状态筛选，
 * 「导出当前筛选结果」**并没有按状态筛** —— 一片静默的错数据。本文件从三层钉住：
 *
 * 1. **纯逻辑**：{@link exportFilterPayload} 对空筛选「整键缺席」、有筛选才带 `status_code`，
 *    并且**永远不带 `status`**（那是另一套语义）；
 * 2. **源码契约**：三页 `.vue` 的导出分支必须真的用上它（换成手拼 `{ ids, keyword }` 立刻红）；
 * 3. **跨栈漂移守卫**：前端三份状态下拉的可选码必须**逐字等于**后端导出白名单
 *    （`impexp::builtin::erp_doc` 的 `STOCK_IN_STATUS` / `PAYABLE_STATUS` / `VOUCHER_STATUS`）。
 *    任何一侧加一个状态而另一侧没跟上，导出侧就会出现「列表选得到、导出 422」——
 *    `PAYABLE_STATUS` 漏 `PENDING_CANCEL` 就是这么发生的（上游卡已修，这里防复发）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import { exportFilterPayload } from '@/utils/batch-action';
import { STOCKIN_STATUS_FILTER_OPTIONS } from '@/views/business/erp/stockin/stockin-logic';
import { PAYABLE_STATUS_FILTER_OPTIONS } from '@/views/business/erp/payable/payable-logic';
import { VOUCHER_STATUS_FILTER_OPTIONS } from '@/views/business/erp/voucher/voucher-logic';

/** 读前端仓库里的文件（`frontend/tests/unit/*` 起算）。 */
function readRepoFile(relativeFromFrontend: string): string {
  return readFileSync(new URL(`../../../${relativeFromFrontend}`, import.meta.url), 'utf8');
}

/** 后端导出定义里的状态白名单（`const NAME: &[EnumItem] = &[ … code: "X" … ]`）。 */
function backendStatusCodes(source: string, constName: string): string[] {
  const start = source.indexOf(`const ${constName}: &[EnumItem] = &[`);
  assert.ok(start >= 0, `后端 erp_doc.rs 里找不到 ${constName}（字段/常量改名了？本测要跟着改）`);
  const end = source.indexOf('];', start);
  assert.ok(end > start, `${constName} 的定义没有闭合`);
  const block = source.slice(start, end);
  const codes = [...block.matchAll(/code:\s*"([A-Za-z_]+)"/g)].map((match) => match[1]);
  assert.ok(codes.length > 0, `${constName} 里一个 code 都没解析出来（格式变了？）`);
  return codes;
}

/** 前端筛选项的码（`''` = 全部，不是状态码）。 */
function frontendCodes(options: ReadonlyArray<{ value: string }>): string[] {
  return options.map((option) => option.value).filter((value) => value !== '');
}

const sorted = (values: string[]): string[] => [...values].sort();

// ==================== 1. 纯逻辑 ====================

test('exportFilterPayload：空筛选 → 一个键都不发（全选行为不变）', () => {
  // 全部状态 + 空关键词：payload 里连 keyword / status_code 都不该出现
  assert.deepEqual(exportFilterPayload({ keyword: '', statusCode: '' }), {});
  assert.deepEqual(exportFilterPayload({ keyword: null, statusCode: null }), {});
  assert.deepEqual(exportFilterPayload({ keyword: undefined, statusCode: undefined }), {});
  // 纯空白同样是「没筛」：发 "  " 会让后端 LIKE %  % 变成一条都筛不到
  assert.deepEqual(exportFilterPayload({ keyword: '   ', statusCode: '  ' }), {});
});

test('exportFilterPayload：状态筛选 → status_code，且永远不带 status', () => {
  const payload = exportFilterPayload({ keyword: '', statusCode: 'POSTED' });
  assert.deepEqual(payload, { status_code: 'POSTED' });
  // `status`（i16 1/0）是字典项 / 用户的语义，ERP 单据传它后端直接 422 + 10001
  assert.equal('status' in payload, false, 'ERP 单据的导出 payload 不许出现 status');
});

test('exportFilterPayload：关键词与状态码同时带（顺序无关，值原样透传）', () => {
  const payload = exportFilterPayload({ keyword: ' IN-20261105 ', statusCode: 'SETTLED' });
  assert.deepEqual(payload, { keyword: 'IN-20261105', status_code: 'SETTLED' });
  // 大小写交给后端规范化（`check_export_filter` 的 trim + to_ascii_uppercase），
  // 前端再改一遍就是第二份口径 —— 这里刻意钉住「原样」
  assert.equal(exportFilterPayload({ statusCode: 'posted' }).status_code, 'posted');
});

test('exportFilterPayload：三张单据页的状态筛选码都能原样进 payload', () => {
  for (const options of [
    STOCKIN_STATUS_FILTER_OPTIONS,
    PAYABLE_STATUS_FILTER_OPTIONS,
    VOUCHER_STATUS_FILTER_OPTIONS,
  ]) {
    for (const code of frontendCodes(options)) {
      assert.equal(exportFilterPayload({ statusCode: code }).status_code, code);
    }
  }
});

// ==================== 2. 源码契约（页面必须真的用上） ====================

const PAGES = [
  { name: '收货入库单', path: 'frontend/src/views/business/erp/stockin/index.vue' },
  { name: '应付单', path: 'frontend/src/views/business/erp/payable/index.vue' },
  { name: '凭证', path: 'frontend/src/views/business/erp/voucher/index.vue' },
] as const;

for (const page of PAGES) {
  test(`${page.name}页：批量导出 payload 透传列表状态筛选`, () => {
    const source = readRepoFile(page.path);
    // ① 用统一构造器（三页同一个口径，不是各拼各的）
    assert.ok(
      source.includes('exportFilterPayload({ keyword:'),
      `${page.name}页的导出分支必须调 exportFilterPayload（否则状态筛选又会被丢掉）`
    );
    // ② 状态来源必须是列表页 filter.status（文本码），不是整型开关
    assert.ok(
      source.includes('statusCode: filter.status'),
      `${page.name}页必须把 filter.status 作为 statusCode 透传`
    );
    // ③ 老写法（只带 ids + keyword）不许残留
    assert.equal(
      /BatchExport\(\{\s*ids,\s*keyword:/.test(source),
      false,
      `${page.name}页还留着只带 ids + keyword 的旧导出 payload`
    );
  });
}

test('api 类型：BatchExportPayload / createExport 都声明了 status_code 且保留 status', () => {
  const batchApi = readRepoFile('frontend/src/api/business/batch.ts');
  const payload = batchApi.slice(
    batchApi.indexOf('export interface BatchExportPayload'),
    batchApi.indexOf('/** 批量审批入参')
  );
  assert.ok(payload.includes('status_code?: string;'), 'BatchExportPayload 缺 status_code');
  assert.ok(payload.includes('status?: number;'), 'BatchExportPayload 不该删掉 status（字典项/用户要用）');

  const impexpApi = readRepoFile('frontend/src/api/business/impexp/index.ts');
  const createExport = impexpApi.slice(
    impexpApi.indexOf('export function createExport'),
    impexpApi.indexOf('export function pageExportTasks')
  );
  assert.ok(createExport.includes('status_code?: string;'), 'createExport 入参缺 status_code');
  assert.ok(createExport.includes('status?: number;'), 'createExport 入参不该删掉 status');
});

// ==================== 3. 跨栈漂移守卫（前端筛选项 ⇄ 后端白名单） ====================

test('后端导出白名单：字段名与 status_code 契约逐字未变', () => {
  const batchRs = readRepoFile('backend/src/business/erp/batch.rs');
  // 批量导出请求体必须收 status_code（前端发的就是这个键）
  assert.ok(
    batchRs.includes('pub status_code: Option<String>'),
    'backend/batch.rs::BatchExportReq 没有 status_code 字段（契约变了，前端发出去会丢）'
  );
  assert.ok(
    batchRs.includes('pub status: Option<i16>'),
    'backend/batch.rs::BatchExportReq 的 status 类型变了（字典项/用户那套口径要跟着核对）'
  );
});

test('状态码漂移守卫：三页筛选项 = 后端导出白名单（集合相等）', () => {
  const source = readRepoFile('backend/src/business/impexp/builtin/erp_doc.rs');
  const pairs = [
    { name: 'erp.stock_in', backend: 'STOCK_IN_STATUS', frontend: STOCKIN_STATUS_FILTER_OPTIONS },
    { name: 'erp.payable', backend: 'PAYABLE_STATUS', frontend: PAYABLE_STATUS_FILTER_OPTIONS },
    { name: 'erp.voucher', backend: 'VOUCHER_STATUS', frontend: VOUCHER_STATUS_FILTER_OPTIONS },
  ];

  for (const pair of pairs) {
    assert.deepEqual(
      sorted(frontendCodes(pair.frontend)),
      sorted(backendStatusCodes(source, pair.backend)),
      `${pair.name}：前端状态下拉的码必须与后端导出白名单一致 —— ` +
        '少一个会「列表看得到、导出 422」，多一个会「选得到、导出筛不出」'
    );
  }
});

/**
 * 「关联业务」按钮的映射表单测（卡 t_3e59cb01）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * # 断言口径：全部抄源码，不抄卡面
 *
 * - **枚举字面量**：读后端 `backend/src/business/shared/doc_kind.rs` 的
 *   `impl FlowBizType { fn as_str }` 的 match 臂，与前端 `FLOW_BIZ_TYPES` 逐字比对。
 *   （卡面写「对照 `flow/dto.rs`」，但那个文件里没有枚举表 —— 值域唯一真相是 `doc_kind.rs`，
 *   `V0219` 迁移里已写明「不在迁移里抄第二套词表」。卡面提到 `ORDER` 这个类型，库里根本不存在。）
 * - **路由**：读 `src/router/index.ts`，确认每个 `path` 真的注册过。
 * - **深链键**：读各详情页自己的 `route.query.<key>` 读法 —— 深链键不是这里发明的。
 * - **探测接口**：读后端 `#[nik::get("…")]` 注解，逐字比对前端请求路径。
 * - **失败分类**：403 / 404 / 业务码 / 网络失败四种分支各钉一条，文案逐字钉住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { FLOW_BIZ_TYPES } from '@/api/business/erp/doc-kinds';
import {
  BIZ_LINKS,
  BIZ_LINK_TIPS,
  RESOURCE_NOT_FOUND,
  PERMISSION_DENIED,
  bizLinkRouteOf,
  bizLinkTip,
  classifyProbeFailure,
  edgeEntityOf,
  resolveBizLink,
  type BizLinkTarget,
} from '@/views/business/flow/instance/biz-link';

/** 读后端源码（`frontend/tests/unit` → 仓库根 → `backend/src/...`） */
function backend(relative: string): string {
  return readFileSync(new URL(`../../../backend/${relative}`, import.meta.url), 'utf8');
}

/** 读前端源码（`frontend/tests/unit` → `frontend/src/...`） */
function frontend(relative: string): string {
  return readFileSync(new URL(`../../${relative}`, import.meta.url), 'utf8');
}

/** `doc_kind.rs::FlowBizType::as_str` 里的落库字面量（顺序 = match 臂顺序 = `ALL` 顺序） */
function rustFlowBizTypes(): string[] {
  const source = backend('src/business/shared/doc_kind.rs');
  const start = source.indexOf('impl FlowBizType');
  assert.ok(start > 0, '找不到 impl FlowBizType');
  const fnStart = source.indexOf('pub const fn as_str', start);
  assert.ok(fnStart > 0, '找不到 FlowBizType::as_str');
  const body = source.slice(fnStart, source.indexOf('\n    }', fnStart));
  return [...body.matchAll(/FlowBizType::\w+ => "([A-Z_]+)"/g)].map((match) => match[1] ?? '');
}

test('后端 FlowBizType 的落库字面量 = 前端 FLOW_BIZ_TYPES（逐字、同序，13 个）', () => {
  const rust = rustFlowBizTypes();
  assert.equal(rust.length, 13, `解析到 ${rust.length} 个字面量：${rust.join(',')}`);
  assert.deepEqual([...FLOW_BIZ_TYPES], rust);
});

test('映射表的 key 只能取真实枚举字面量（不许自己编类型名）', () => {
  const keys = Object.keys(BIZ_LINKS);
  assert.ok(keys.length > 0, '映射表不能为空');
  for (const key of keys) {
    assert.ok(
      (FLOW_BIZ_TYPES as readonly string[]).includes(key),
      `${key} 不在后端 FlowBizType 值域里`,
    );
    // key 必须与 target.bizType 自洽（防止复制粘贴写错类型）
    assert.equal((BIZ_LINKS as Record<string, BizLinkTarget>)[key]?.bizType, key);
  }
});

test('登记的恰好是「有详情页」的 10 种；盘点/费用/其他故意不登记', () => {
  assert.deepEqual(Object.keys(BIZ_LINKS).sort(), [
    'CONTRACT_APPROVE',
    'FLOW_EDGE_CFG',
    'PAYABLE_CANCEL',
    'PAYABLE_PAY',
    'PO_APPROVE',
    'STOCK_IN_CANCEL',
    'STOCK_IN_POST',
    'STOCK_TRANSFER',
    'TASK_ACCEPT',
    'VOUCHER_REVERSE',
  ]);
  // 盘点过账：后端有接口但前端没有盘点单页面（`grep -rn "stock-take" frontend/src` 零命中）
  assert.equal(resolveBizLink('STOCK_TAKE_POST', { id: 'x', no: 'PD-1' }), null);
  // 通用流程：没有对应业务单据（生产库 `OTHER` 16 条全部 biz_id 为空）
  assert.equal(resolveBizLink('EXPENSE', { id: 'x', no: 'BX-1' }), null);
  assert.equal(resolveBizLink('OTHER', { id: 'x', no: 'QT-1' }), null);
  // 库外值 / 空值
  assert.equal(resolveBizLink('ORDER', { id: 'x', no: 'SO-1' }), null);
  assert.equal(resolveBizLink('', { id: 'x', no: 'SO-1' }), null);
  // 有类型但没 biz_id（生产 `OTHER` 就是这种）⇒ 不出按钮
  assert.equal(resolveBizLink('TASK_ACCEPT', { id: '', no: 'TSK-1' }), null);
});

test('每条目标的路由 path 都在 router/index.ts 里真的注册过', () => {
  const router = frontend('src/router/index.ts');
  const segments: Record<string, string> = {
    '/task/list': "path: 'task/list'",
    '/business/erp/contract': "path: 'business/erp/contract'",
    '/business/erp/purchase-order': "path: 'business/erp/purchase-order'",
    '/business/erp/stock-in': "path: 'business/erp/stock-in'",
    '/business/erp/transfer': "path: 'business/erp/transfer'",
    '/business/erp/payable': "path: 'business/erp/payable'",
    '/business/erp/voucher': "path: 'business/erp/voucher'",
    '/flow/def': "path: 'flow/def'",
  };
  for (const target of Object.values(BIZ_LINKS) as BizLinkTarget[]) {
    const needle = segments[target.path];
    assert.ok(needle, `路由表里没有登记 ${target.path} 的期望写法`);
    assert.ok(router.includes(needle), `router/index.ts 里找不到 ${needle}`);
  }
});

test('深链键取的是各详情页自己读的那个 query 参数（不发明新参数）', () => {
  // 值 = [页面文件, 页面里读 query 的写法, 该类型真实形状的 biz]
  const cases: Record<string, [string, string, { id: string; no: string }]> = {
    TASK_ACCEPT: ['src/views/business/task/index.vue', 'route.query.id', { id: 'id-1', no: 'TSK-1' }],
    CONTRACT_APPROVE: [
      'src/views/business/erp/contract/index.vue',
      'route.query.contract_id',
      { id: 'id-1', no: 'HT-1' },
    ],
    PO_APPROVE: [
      'src/views/business/erp/purchase/index.vue',
      'route.query.po_id',
      { id: 'id-1', no: 'PO-1' },
    ],
    STOCK_IN_POST: [
      'src/views/business/erp/stockin/index.vue',
      'route.query.in_id',
      { id: 'id-1', no: 'IN-1' },
    ],
    VOUCHER_REVERSE: [
      'src/views/business/erp/voucher/index.vue',
      'route.query.voucher_no',
      { id: 'id-1', no: 'PZ-1' },
    ],
    PAYABLE_PAY: [
      'src/views/business/erp/payable/index.vue',
      'route.query.payable_no',
      { id: 'id-1', no: 'AP-1' },
    ],
    // 生产实测的 biz_no 形状：`STOCK_IN DRAFT>POSTED`
    FLOW_EDGE_CFG: [
      'src/views/business/flow/def/index.vue',
      'route.query.entity',
      { id: 'id-1', no: 'STOCK_IN DRAFT>POSTED' },
    ],
  };
  for (const [bizType, [file, reader, biz]] of Object.entries(cases)) {
    const page = frontend(file);
    assert.ok(page.includes(reader), `${file} 里没有 ${reader}（深链参数对不上）`);
    const target = BIZ_LINKS[bizType as keyof typeof BIZ_LINKS];
    assert.ok(target, `${bizType} 未登记`);
    const query = target.queryOf(biz);
    assert.ok(query, `${bizType} 应该能构造深链`);
    const key = Object.keys(query)[0] ?? '';
    assert.equal(reader, `route.query.${key}`, `${bizType} 的深链键是 ${key}，页面读的是 ${reader}`);
  }
  // 调拨：页面通过 `transferNoFromQuery()` 读 `transfer_no`
  const transferLogic = frontend('src/views/business/erp/transfer/transfer-logic.ts');
  assert.ok(transferLogic.includes('query?.transfer_no'), '调拨页不是读 transfer_no');
  assert.deepEqual(BIZ_LINKS.STOCK_TRANSFER?.queryOf({ id: 'id-1', no: 'TK-1' }), { transfer_no: 'TK-1' });
});

test('探测接口路径 = 后端 #[nik::get(...)] 注解（逐字，含 /api 前缀）', () => {
  const cases: Array<[keyof typeof BIZ_LINKS, string, string]> = [
    ['TASK_ACCEPT', 'src/business/task/handler.rs', '/business/task/{id}'],
    ['CONTRACT_APPROVE', 'src/business/erp/contract_handler.rs', '/business/erp/contract/{id}'],
    ['PO_APPROVE', 'src/business/erp/handler.rs', '/business/erp/purchase-order/{id}'],
    ['STOCK_IN_POST', 'src/business/erp/stockin_handler_query.rs', '/business/erp/stock-in/{id}'],
    ['STOCK_IN_CANCEL', 'src/business/erp/stockin_handler_query.rs', '/business/erp/stock-in/{id}'],
    ['STOCK_TRANSFER', 'src/business/erp/transfer_handler.rs', '/business/erp/transfer/{id}'],
    ['PAYABLE_PAY', 'src/business/erp/payable_query.rs', '/business/erp/payable/{id}'],
    ['PAYABLE_CANCEL', 'src/business/erp/payable_query.rs', '/business/erp/payable/{id}'],
    ['VOUCHER_REVERSE', 'src/business/erp/voucher_query.rs', '/business/erp/voucher/{id}'],
  ];
  for (const [bizType, file, route] of cases) {
    const source = backend(file);
    assert.ok(
      source.includes(`#[nik::get("${route}")]`),
      `${file} 里没有 #[nik::get("${route}")]`,
    );
    const target = BIZ_LINKS[bizType];
    assert.ok(target, `${bizType} 未登记`);
    const plan = target.probeOf({ id: 'abc', no: 'NO-1' });
    assert.ok(plan, `${bizType} 应该有探测方案`);
    assert.equal(plan.url, `/api${route.replace('{id}', 'abc')}`);
  }
  // 边配置：不是「一条一查」，探的是实体（`GET /business/flow/state-edge?entity=`）
  const edgeAdmin = backend('src/business/flow/edge_admin.rs');
  assert.ok(edgeAdmin.includes('#[nik::get("/business/flow/state-edge")]'));
  assert.deepEqual(BIZ_LINKS.FLOW_EDGE_CFG?.probeOf({ id: 'abc', no: 'STOCK_IN DRAFT>POSTED' }), {
    url: '/api/business/flow/state-edge',
    params: { entity: 'STOCK_IN' },
  });
});

test('深链：任务/合同/采购/入库用 id，应付/凭证/调拨用单号，边配置定位实体', () => {
  const biz = { id: 'id-1', no: 'NO-1' };
  const route = (bizType: keyof typeof BIZ_LINKS) => {
    const target = BIZ_LINKS[bizType];
    assert.ok(target);
    return bizLinkRouteOf(target, biz);
  };
  assert.deepEqual(route('TASK_ACCEPT'), { path: '/task/list', query: { id: 'id-1' } });
  assert.deepEqual(route('CONTRACT_APPROVE'), {
    path: '/business/erp/contract',
    query: { contract_id: 'id-1' },
  });
  assert.deepEqual(route('PO_APPROVE'), {
    path: '/business/erp/purchase-order',
    query: { po_id: 'id-1' },
  });
  assert.deepEqual(route('STOCK_IN_POST'), { path: '/business/erp/stock-in', query: { in_id: 'id-1' } });
  assert.deepEqual(route('STOCK_IN_CANCEL'), { path: '/business/erp/stock-in', query: { in_id: 'id-1' } });
  assert.deepEqual(route('PAYABLE_CANCEL'), {
    path: '/business/erp/payable',
    query: { payable_no: 'NO-1' },
  });
  assert.deepEqual(route('VOUCHER_REVERSE'), {
    path: '/business/erp/voucher',
    query: { voucher_no: 'NO-1' },
  });
  // 生产实测的 FLOW_EDGE_CFG biz_no 形状：`STOCK_IN DRAFT>POSTED`
  assert.deepEqual(
    BIZ_LINKS.FLOW_EDGE_CFG?.queryOf({ id: 'id-1', no: 'STOCK_OUT DRAFT>POSTED' }),
    { entity: 'STOCK_OUT' },
  );
  // 实体名取不到：页面照常打开，只是不定位实体（不是不跳）
  assert.deepEqual(BIZ_LINKS.FLOW_EDGE_CFG?.queryOf({ id: 'id-1', no: '' }), {});
  assert.deepEqual(BIZ_LINKS.FLOW_EDGE_CFG?.queryOf({ id: 'id-1', no: 'NOPE DRAFT>POSTED' }), {});
});

test('只认单号的页面：没记单号 ⇒ 不构造深链（按钮置灰提示，不跳空列表）', () => {
  const noId = { id: 'id-1', no: '' };
  for (const bizType of ['STOCK_TRANSFER', 'PAYABLE_PAY', 'PAYABLE_CANCEL', 'VOUCHER_REVERSE'] as const) {
    const target = BIZ_LINKS[bizType];
    assert.ok(target);
    assert.equal(target.queryOf(noId), null, `${bizType} 没单号不该给深链`);
    assert.equal(bizLinkRouteOf(target, noId), null);
  }
});

test('edgeEntityOf：只认已知业务对象的第一段，别把半截字符串塞进 URL', () => {
  assert.equal(edgeEntityOf('STOCK_IN DRAFT>POSTED'), 'STOCK_IN');
  assert.equal(edgeEntityOf('  STOCK_OUT   DRAFT>POSTED '), 'STOCK_OUT');
  assert.equal(edgeEntityOf('STOCK_IN'), 'STOCK_IN');
  assert.equal(edgeEntityOf(''), '');
  assert.equal(edgeEntityOf('NOPE DRAFT>POSTED'), '');
  assert.equal(edgeEntityOf('TSK-202610-000851'), '');
});

test('探测失败分三支：403 归「没权限」，404/不存在业务码归「已清场」，其余归「说不清」', () => {
  // 403：审批岗常没有 ERP 权限 —— 不能混进「已清场」
  assert.equal(classifyProbeFailure({ status: 403, code: 30001 }), 'forbidden');
  assert.equal(classifyProbeFailure({ status: 403, code: -1 }), 'forbidden');
  assert.equal(classifyProbeFailure({ code: PERMISSION_DENIED }), 'forbidden');
  // 404：生产实测「任务软删 → HTTP 404 + 63001」「合同软删 → HTTP 404 + 66014」
  assert.equal(classifyProbeFailure({ status: 404, code: 63001 }), 'gone');
  assert.equal(classifyProbeFailure({ status: 404, code: 66014 }), 'gone');
  assert.equal(classifyProbeFailure({ code: RESOURCE_NOT_FOUND }), 'gone');
  assert.equal(classifyProbeFailure({ code: 63001 }, [63001]), 'gone');
  // 状态码丢了但业务码口径在（单测兜住「以后有人只加业务码」的情况）
  assert.equal(classifyProbeFailure({ code: 65005 }, [65005]), 'gone');
  assert.equal(classifyProbeFailure({ code: 65005 }), 'unknown');
  // 网络失败 / 超时 / 5xx：说不清，但仍不跳（fail-closed）
  assert.equal(classifyProbeFailure({ status: 500, code: 10004 }), 'unknown');
  assert.equal(classifyProbeFailure({}), 'unknown');
  // 404 优先于「不认识业务码」
  assert.equal(classifyProbeFailure({ status: 404, code: 99999 }), 'gone');
});

test('置灰文案逐字抄卡面，且不出现技术术语', () => {
  assert.equal(BIZ_LINK_TIPS.gone, '业务对象已清场，仅可查看审批快照');
  for (const [state, tip] of Object.entries(BIZ_LINK_TIPS)) {
    assert.ok(tip.length > 0, `${state} 的提示不能为空`);
    for (const term of ['biz_type', 'biz_id', 'biz_no', 'entity_id', 'entity', 'API', '404', '403']) {
      assert.ok(!tip.includes(term), `${state} 的提示里出现了技术术语「${term}」：${tip}`);
    }
  }
  // alive / idle 不弹提示（tooltip 用 disabled 关掉）
  assert.equal(bizLinkTip('alive'), '');
  assert.equal(bizLinkTip('idle'), '');
  assert.equal(bizLinkTip('gone'), BIZ_LINK_TIPS.gone);
});

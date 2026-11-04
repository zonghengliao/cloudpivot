/**
 * P2-7-F1 单测：采购单**供应商名口径对齐**（列表/详情显示现值 → 回落快照，**禁止落 uuid**）。
 *
 * 口径真源：`docs/编码规范.md` §1.10 S1–S6 +
 * `docs/architecture/P2-7-供应商名口径盘点.md` §3（父卡 `t_54ad4c5f` 拍板）。
 *
 * # 这次要钉死的三件事
 *
 * 1. **顺序**：主档现值 → 单据快照 → `—`。列表是「看现在」的地方，供应商改名后必须跟着变；
 *    快照只用来兜底（主档软删 / 主数据清单还没拉回来）。
 * 2. **绝不出现 uuid**：旧实现 `supplierNameMap.get(id) ?? shortId(id)` 在查不到时显示
 *    `0f8a3c1d…` —— 用户看不懂，而且**丢掉了后端已经下发的快照名**。这条兜底被彻底删掉。
 * 3. **详情页并列两行**：`供应商（现名）` + `下单时供应商快照`（范式 = 合同详情
 *    `contract/index.vue:405-409` 的「乙方」+「签约时乙方快照」）。
 *
 * # 为什么还要读 `.vue` 源码做断言
 *
 * `vue-tsc` 抓得到类型错，抓不到「模板里又偷偷改回 `shortId`」「快照栏放错位置」。
 * 本仓既有姿势是「源码即契约」（见 `purchase-approval-detail-position.test.ts`），这里沿用。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  shortId,
  supplierCurrentNameOf,
  supplierDisplayName,
} from '../../src/views/business/erp/purchase/purchase-logic.ts';

/** 一根真 uuid（用固定值，便于断言「输出里不含它的短号」） */
const SUPPLIER_ID = '0f8a3c1d-2b4e-4f60-9a71-8c5d3e2b1a09';
/** 后端 `supplier_name` = 快照名（下单那一刻的名字） */
const SNAPSHOT_NAME = '北京华信机电有限公司';
/** 主档当前名字（改名后） */
const CURRENT_NAME = '北京华信智造有限公司';

const SOURCE = readFileSync(
  join(resolve(process.cwd()), 'src/views/business/erp/purchase/index.vue'),
  'utf8',
);

/** 详情抽屉模板区（`<t-drawer …>` … `</t-drawer>`），断言只在这段里做，别命中列表列 */
const DRAWER_START = SOURCE.indexOf('<t-drawer');
assert.notEqual(DRAWER_START, -1, '找不到详情抽屉 <t-drawer>');
const DRAWER_END = SOURCE.indexOf('</t-drawer>', DRAWER_START);
assert.notEqual(DRAWER_END, -1, '找不到 </t-drawer>');
const DRAWER = SOURCE.slice(DRAWER_START, DRAWER_END);

/** 「uuid 短号」形态：`shortId()` 的输出 = 前 8 位 + `…` */
const SHORT_ID_PATTERN = /[0-9a-f]{8}…/i;

function mapOf(entries: Array<[string, string]>): Map<string, string> {
  return new Map(entries);
}

// ==================== ① 顺序：现值 → 快照 → — ====================

test('map 命中 → 显示主档**现值**（改名后列表跟着变，快照不参与显示）', () => {
  const display = supplierDisplayName(SUPPLIER_ID, SNAPSHOT_NAME, mapOf([[SUPPLIER_ID, CURRENT_NAME]]));
  assert.equal(display, CURRENT_NAME);
  assert.notEqual(display, SNAPSHOT_NAME, '有现值时不能显示快照（否则改名后列表还是旧名）');
});

test('map 未命中但有快照 → 显示**快照名**（主档软删 / 主数据没拉回来）', () => {
  assert.equal(supplierDisplayName(SUPPLIER_ID, SNAPSHOT_NAME, new Map()), SNAPSHOT_NAME);
});

test('两者都没有 → `—`（不留白、不报错）', () => {
  assert.equal(supplierDisplayName(SUPPLIER_ID, null, new Map()), '—');
  assert.equal(supplierDisplayName(SUPPLIER_ID, undefined, new Map()), '—');
  assert.equal(supplierDisplayName(null, null, new Map()), '—');
  assert.equal(supplierDisplayName(undefined, '', new Map()), '—');
});

test('空串不算名字：主档名为空串 → 回落快照；快照为空串 → `—`', () => {
  assert.equal(supplierDisplayName(SUPPLIER_ID, SNAPSHOT_NAME, mapOf([[SUPPLIER_ID, '']])), SNAPSHOT_NAME);
  assert.equal(supplierDisplayName(SUPPLIER_ID, '', mapOf([[SUPPLIER_ID, '   ']])), '—');
});

// ==================== ② 硬约束：任何情况下不出现 uuid ====================

test('任何输入组合下输出都不含 uuid / uuid 短号（禁落 uuid 的回归闸门）', () => {
  const cases: Array<[string | null | undefined, string | null | undefined, Map<string, string>]> = [
    [SUPPLIER_ID, SNAPSHOT_NAME, mapOf([[SUPPLIER_ID, CURRENT_NAME]])],
    [SUPPLIER_ID, SNAPSHOT_NAME, new Map()],
    [SUPPLIER_ID, null, new Map()],
    [SUPPLIER_ID, '', new Map()],
    [null, null, new Map()],
  ];
  for (const [id, snapshot, map] of cases) {
    const display = supplierDisplayName(id, snapshot, map);
    assert.equal(display.includes(SUPPLIER_ID), false, `输出里出现了完整 uuid：${display}`);
    assert.equal(SHORT_ID_PATTERN.test(display), false, `输出里出现了 uuid 短号：${display}`);
    assert.equal(display.includes(SUPPLIER_ID.slice(0, 8)), false, `输出里出现了 uuid 前 8 位：${display}`);
  }
});

test('对照：`shortId(SUPPLIER_ID)` 确实会长成被禁的形态（证明上面的闸门抓得到东西）', () => {
  const forbidden = shortId(SUPPLIER_ID);
  assert.equal(forbidden, '0f8a3c1d…');
  assert.equal(SHORT_ID_PATTERN.test(forbidden), true);
  assert.equal(SHORT_ID_PATTERN.test(supplierDisplayName(SUPPLIER_ID, null, new Map())), false);
});

test('详情「供应商（现名）」用 `supplierCurrentNameOf`：只认现值，查不到给 `—`（不给 uuid）', () => {
  assert.equal(supplierCurrentNameOf(SUPPLIER_ID, mapOf([[SUPPLIER_ID, CURRENT_NAME]])), CURRENT_NAME);
  assert.equal(supplierCurrentNameOf(SUPPLIER_ID, new Map()), '—');
  assert.equal(supplierCurrentNameOf(null, new Map()), '—');
  assert.equal(SHORT_ID_PATTERN.test(supplierCurrentNameOf(SUPPLIER_ID, new Map())), false);
});

// ==================== ③ 源码契约：列表列 + 详情并列两行 ====================

test('列表列走 `supplierDisplayOf(row)`（现名→快照→—），不再出现 `shortId(row.supplier_id)`', () => {
  const listSlot = /<template #supplier_id="\{ row \}">([\s\S]*?)<\/template>/.exec(SOURCE);
  assert.notEqual(listSlot, null, '找不到列表的 #supplier_id 插槽');
  const body = listSlot![1];
  assert.match(body, /supplierDisplayOf\(row\)/);
  assert.equal(/shortId\s*\(/.test(body), false, '列表列里又出现了 shortId 兜底');
});

test('`supplierDisplayOf` 的实现同时读 `supplier_id` 与 `supplier_name`（快照必须进排队列）', () => {
  const impl = /function supplierDisplayOf\([\s\S]*?\n\}/.exec(SOURCE);
  assert.notEqual(impl, null, '找不到 supplierDisplayOf 实现');
  assert.match(impl![0], /supplierDisplayName\(row\.supplier_id, row\.supplier_name/);
  assert.equal(/shortId\s*\(/.test(impl![0]), false, 'supplierDisplayOf 里不许有 shortId');
});

test('详情页并列两行：「供应商（现名）」在前、「下单时供应商快照」紧接其后', () => {
  const currentIdx = DRAWER.indexOf('供应商（现名）');
  const snapshotIdx = DRAWER.indexOf('下单时供应商快照');
  assert.notEqual(currentIdx, -1, '详情页缺「供应商（现名）」栏（S2）');
  assert.notEqual(snapshotIdx, -1, '详情页缺「下单时供应商快照」栏（S3）');
  assert.ok(currentIdx < snapshotIdx, '两栏顺序不对：应先现名、再快照（照抄合同详情）');

  const between = DRAWER.slice(currentIdx, snapshotIdx);
  assert.equal(/供应商<\/span>/.test(between), false, '详情里还留着老的单一「供应商」栏');
});

test('快照栏只读灰字、绑 `detail.supplier_name`，空则 `—`', () => {
  const m = /下单时供应商快照[\s\S]{0,320}?<\/div>/.exec(DRAWER);
  assert.notEqual(m, null, '找不到快照栏模板块');
  const block = m![0];
  assert.match(block, /detail\.supplier_name/);
  assert.match(block, /dash\(/);
  assert.match(block, /class="detail-field__value muted"/, '快照栏应为灰字只读');
  assert.match(block, /purchase-detail-supplier-snapshot/);
});

test('「供应商（现名）」栏绑 `supplierDisplayOf(detail)`（现值→快照→—），不再用裸 shortId', () => {
  const m = /供应商（现名）[\s\S]{0,320}?<\/div>/.exec(DRAWER);
  assert.notEqual(m, null, '找不到现名栏模板块');
  assert.match(m![0], /supplierDisplayOf\(detail\)/);
  assert.match(m![0], /purchase-detail-supplier"/);
});

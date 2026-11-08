/**
 * ATT-09 单测：把「附件必传闸门」接到其余单据页之后的**页面契约**。
 *
 * # 钉死的口径（逐页一对）
 *
 * | 页面 | doc_type | 动作 | 目标按钮 |
 * |------|----------|------|----------|
 * | `expense/ExpenseDetailDrawer.vue` | `EXPENSE_REPORT` | `SUBMIT` | 提交审批 |
 * | `purchase-return/index.vue` | `PURCHASE_RETURN` | `SUBMIT` | 提交审批 |
 * | `sales-return/index.vue` | `SALES_RETURN` | `SUBMIT` | 提交 |
 * | `stock-take/index.vue` | `STOCK_TAKE` | `CONFIRM` | 提交审批（只有这一条边受约束） |
 * | `asset/index.vue` | `FIXED_ASSET` | `ACTIVATE` | 确认启用 |
 * | `payable/index.vue` | `PAYMENT` | `ACTIVATE` | 执行付款 |
 * | `stockin/index.vue` | —— | —— | **没有规则 → 不许凭空造闸门** |
 *
 * 每页断言四件事（缺一条就会出「按钮亮着点下去 400」或「灰着没解释」）：
 *
 * 1. 闸门确实调了后端：`doc_type` 与 `at` 与 `biz_attachment_rule` 种子**逐字**一致；
 * 2. 目标按钮 `:disabled` **同时**含权限与闸门；
 * 3. 按钮旁有 `<AttachmentGateHint>`（红字，文案来自后端）；
 * 4. 附件面板 `@changed` 触发重查（验收②「传完自动变亮」）；
 * 5. 该页有上传入口（`DocAttachmentPanel`）—— 否则必传规则永远无法满足。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

/** 定位一个函数体的近似切片（够做「里面有没有那句话」的断言） */
function bodyOf(source: string, signature: string, length = 1600): string {
  const start = source.indexOf(signature);
  assert.ok(start > 0, `找不到 ${signature}`);
  return source.slice(start, start + length);
}

// ==================== ① 费用报销（EXPENSE_REPORT / SUBMIT） ====================

test('报销详情：提交按钮受「权限 ∪ 闸门」约束，红字 + tooltip + 上传入口都在同一页', () => {
  const page = readSource('views/business/erp/expense/ExpenseDetailDrawer.vue');
  assert.ok(page.includes("ref<DocType>('EXPENSE_REPORT')"), 'doc_type 必须是 EXPENSE_REPORT');
  assert.ok(page.includes("at: 'SUBMIT'"), '动作必须是 SUBMIT（biz_attachment_rule 种子）');
  assert.ok(
    page.includes(':disabled="!hasSubmitPermission || !attachGate.canProceed"'),
    '提交按钮必须同时受权限与闸门约束',
  );
  assert.ok(page.includes('<AttachmentGateHint :hint="attachGate.hint"'), '按钮旁必须有红字组件');
  assert.ok(page.includes('testid="expense-detail-gate-hint"'), '红字必须有稳定的 testid');
  assert.ok(page.includes(':content="submitGateTip"'), 'disabled 按钮必须包 tooltip 说明原因');
  assert.ok(page.includes('@changed="refreshAttachGate()"'), '上传/删除附件后必须重查（验收②）');
  assert.ok(page.includes(':doc-type="DOC_KIND.EXPENSE_REPORT"'), '必须能上传发票影像');
  // 只有草稿才需要闸门（已提交 / 已作废不再走提交边）
  assert.ok(page.includes("report.value?.status === 'DRAFT'"), '闸门只对草稿生效');
});

test('报销列表：行「提交」与「保存并提交」都先过闸门，缺附件不许打后端', () => {
  const page = readSource('views/business/erp/expense/index.vue');
  const gate = bodyOf(page, 'async function passAttachmentGate');
  assert.ok(gate.includes("checkDocAttachments('EXPENSE_REPORT'"), gate);
  assert.ok(gate.includes("'SUBMIT'"), gate);
  assert.ok(gate.includes('openDetail(row)'), '缺附件要把人送到详情（那里有上传面板）');
  const submit = bodyOf(page, 'async function handleSubmit');
  assert.ok(submit.includes('passAttachmentGate(row)'), '列表行提交必须先过闸门');
  const save = bodyOf(page, 'async function handleSave');
  assert.ok(save.includes("checkDocAttachments('EXPENSE_REPORT'"), '「保存并提交审批」也要先查一次');
  assert.ok(save.includes('已保存草稿'), '被拦时应当存成草稿并说清缺什么，而不是留一条 400');
});

// ==================== ② 采购退货（PURCHASE_RETURN / SUBMIT） ====================

test('采购退货：详情里的「提交审批」受权限 ∪ 闸门约束，带红字、tooltip 与上传入口', () => {
  const page = readSource('views/business/erp/purchase-return/index.vue');
  assert.ok(page.includes("ref<DocType>('PURCHASE_RETURN')"), 'doc_type 必须是 PURCHASE_RETURN');
  assert.ok(page.includes("at: 'SUBMIT'"), '动作必须是 SUBMIT');
  assert.ok(
    page.includes(':disabled="!hasSubmitPermission || !attachGate.canProceed"'),
    '提交按钮必须同时受权限与闸门约束',
  );
  assert.ok(page.includes('<AttachmentGateHint :hint="attachGate.hint"'), '按钮旁必须有红字组件');
  assert.ok(page.includes('testid="prt-detail-gate-hint"'), '红字必须有稳定的 testid');
  assert.ok(page.includes(':content="submitGateTip"'), 'disabled 按钮必须包 tooltip');
  assert.ok(page.includes('@changed="refreshAttachGate()"'), '上传后必须重查');
  assert.ok(page.includes(':doc-type="DOC_KIND.PURCHASE_RETURN"'), '必须能上传退货单 / 签收凭证');
  // 列表行提交先查闸门
  const gate = bodyOf(page, 'async function passAttachmentGate');
  assert.ok(gate.includes("checkDocAttachments('PURCHASE_RETURN'"), gate);
  assert.ok(gate.includes('openDetail(docId)'), '缺附件要把人送到详情');
});

// ==================== ③ 销售退货（SALES_RETURN / SUBMIT） ====================

test('销售退货：详情「提交」受权限 ∪ 闸门约束，列表行提交先查闸门', () => {
  const page = readSource('views/business/erp/sales-return/index.vue');
  assert.ok(page.includes("ref<DocType>('SALES_RETURN')"), 'doc_type 必须是 SALES_RETURN');
  assert.ok(page.includes("at: 'SUBMIT'"), '动作必须是 SUBMIT');
  assert.ok(
    page.includes(':disabled="!hasStatusPermission || !attachGate.canProceed"'),
    '提交按钮必须同时受权限与闸门约束',
  );
  assert.ok(page.includes('<AttachmentGateHint :hint="attachGate.hint"'), '按钮旁必须有红字组件');
  assert.ok(page.includes('testid="return-detail-gate-hint"'), '红字必须有稳定的 testid');
  assert.ok(page.includes(':content="submitGateTip"'), 'disabled 按钮必须包 tooltip');
  assert.ok(page.includes('@changed="refreshAttachGate()"'), '上传后必须重查');
  assert.ok(page.includes(':doc-type="DOC_KIND.SALES_RETURN"'), '必须能上传退货单 / 物流凭证');
  const submit = bodyOf(page, 'async function submitReturn');
  assert.ok(submit.includes('passAttachmentGate(row)'), '提交（列表行 / 详情共用）必须先过闸门');
});

// ==================== ④ 盘点单（STOCK_TAKE / CONFIRM） ====================

test('盘点单：只有「提交审批」那条边受闸门约束，动作按后端 CONFIRM（不是 SUBMIT）', () => {
  const page = readSource('views/business/erp/stock-take/index.vue');
  assert.ok(page.includes("ref<DocType>('STOCK_TAKE')"), 'doc_type 必须是 STOCK_TAKE');
  assert.ok(page.includes("at: 'CONFIRM'"), '动作必须是 CONFIRM（盘点提交审批 = 确认边）');
  assert.ok(
    page.includes(':disabled="!hasStatusPermission || actionGateBlocked(action)"'),
    '动作按钮必须同时受权限与闸门约束',
  );
  const blocked = bodyOf(page, 'function actionGateBlocked');
  assert.ok(blocked.includes("action.status === 'PENDING_POST'"), '只有 PENDING_POST（提交审批）受约束');
  assert.ok(blocked.includes('attachGate.value.canProceed'), '判据必须来自闸门');
  assert.ok(page.includes('<AttachmentGateHint :hint="attachGate.hint"'), '按钮旁必须有红字组件');
  assert.ok(page.includes('testid="stocktake-detail-gate-hint"'), '红字必须有稳定的 testid');
  assert.ok(page.includes('@changed="refreshAttachGate()"'), '上传后必须重查');
  assert.ok(page.includes(':doc-type="DOC_KIND.STOCK_TAKE"'), '必须能上传盘点表 / 盈亏说明');
});

// ==================== ⑤ 固定资产（FIXED_ASSET / ACTIVATE） ====================

test('固定资产：「确认启用」受闸门约束，红字在弹窗里，附件挂详情页签', () => {
  const page = readSource('views/business/erp/asset/index.vue');
  assert.ok(page.includes("ref<DocType>('FIXED_ASSET')"), 'doc_type 必须是 FIXED_ASSET');
  assert.ok(page.includes("at: 'ACTIVATE'"), '动作必须是 ACTIVATE（入账 / 启用）');
  assert.ok(
    page.includes('disabled: statusChangeKind === \'IN_USE\' && !assetGate.canProceed'),
    '「确认启用」必须受闸门约束（停用不受约束）',
  );
  assert.ok(page.includes('<AttachmentGateHint'), '弹窗里必须有红字组件');
  assert.ok(page.includes('testid="asset-enable-gate-hint"'), '红字必须有稳定的 testid');
  assert.ok(page.includes(':doc-type="DOC_KIND.FIXED_ASSET"'), '必须能上传发票 / 验收单');
  assert.ok(page.includes('@changed="refreshAssetGate()"'), '上传后必须重查');
  // 只有「启用」才查（停用与附件无关）
  const docId = bodyOf(page, 'const assetGateDocId');
  assert.ok(docId.includes("statusChangeKind.value === 'IN_USE'"), docId);
  // 后端兜底：弹窗按钮灰了也再判一次
  const submit = bodyOf(page, 'async function submitStatusChange');
  assert.ok(submit.includes('assetGate.value.canProceed'), '提交函数必须再兜一次闸门');
});

// ==================== ⑥ 付款单（PAYMENT / ACTIVATE） ====================

test('付款单：「执行付款」受「权限 ∪ 回单闸门」约束，红字按付款行出，回单挂 PAYMENT', () => {
  const page = readSource('views/business/erp/payable/index.vue');
  assert.ok(
    page.includes("checkDocAttachments('PAYMENT', item.id, 'ACTIVATE')"),
    '必须按付款行查 PAYMENT / ACTIVATE（与后端 payable_payment 闸门同口径）',
  );
  assert.ok(
    page.includes(':disabled="!hasPayPermission || !payGateOf(item).canProceed"'),
    '执行付款按钮必须同时受权限与闸门约束',
  );
  assert.ok(page.includes('<AttachmentGateHint'), '按钮旁必须有红字组件');
  assert.ok(page.includes('payable-payment-gate-hint-'), '红字必须有按付款单号区分的 testid');
  assert.ok(page.includes(':content="payGateTip(item)"'), 'disabled 按钮必须包 tooltip');
  assert.ok(page.includes(':doc-type="DOC_KIND.PAYMENT"'), '回单必须挂在付款行（PAYMENT）上');
  assert.ok(page.includes('@changed="refreshPaymentGates()"'), '上传后必须重查');
  // 降级口径与 use-attachment-gate 一致：查询失败不禁用按钮
  assert.ok(page.includes('degradedGate('), '查询失败必须降级（不禁用按钮）');
});

// ==================== ⑦ 收货入库：没有规则，不许造闸门 ====================

test('收货入库：规则表里没有 STOCK_IN 的必传规则 → 不许凭空加闸门', () => {
  const page = readSource('views/business/erp/stockin/index.vue');
  assert.ok(!page.includes('useAttachmentGate'), '没有规则的页面不应挂闸门（查了也只是多一次请求）');
  assert.ok(!page.includes('AttachmentGateHint'), '没有规则就不该画「缺附件」红字');
});

// ==================== ⑧ 三处词表必须一致（改一处必然漏另一处） ====================

test('词表一致：规则表种子的 8 类单据在页面上都有接线', () => {
  const rules = readFileSync(
    resolve(HERE, '../../../backend/migrations/business/V0347__attachment_required_rules.up.sql'),
    'utf8',
  );
  // 规则表里的 (doc_type, required_at) 必须与页面接线逐字对上
  const wired: Array<[string, string]> = [
    ['EXPENSE_REPORT', 'SUBMIT'],
    ['PURCHASE_RETURN', 'SUBMIT'],
    ['SALES_RETURN', 'SUBMIT'],
    ['STOCK_TAKE', 'CONFIRM'],
    ['FIXED_ASSET', 'ACTIVATE'],
    ['PAYMENT', 'ACTIVATE'],
    // ATT-10 补的两条：发票（ATT-07 起就在页面上）与收款单（本卡接线）
    ['INVOICE', 'SUBMIT'],
    ['RECEIPT', 'SUBMIT'],
  ];
  for (const [docType, at] of wired) {
    assert.ok(
      rules.includes(`'${docType}'`) && rules.includes(`'${at}'`),
      `规则表里必须有 ${docType} / ${at}`,
    );
  }
  // STOCK_IN 确实没有规则（收货页不接闸门的依据）
  assert.ok(
    !/\(\s*'[0-9a-f-]+'::uuid,\s*'STOCK_IN'/.test(rules),
    'STOCK_IN 不该有必传规则（收货页据此不加闸门）',
  );
});

// ==================== ⑨ 收款单（RECEIPT / SUBMIT，ATT-10） ====================

test('收款单：确认收款（DRAFT→CONFIRMED）受「权限 ∪ 银行回单闸门」约束', () => {
  const page = readSource('views/business/erp/receipt/index.vue');
  assert.ok(page.includes("ref<DocType>('RECEIPT')"), 'doc_type 必须是 RECEIPT');
  assert.ok(page.includes("at: 'SUBMIT'"), '动作必须是 SUBMIT（收款单的提交边 = 确认收款）');
  assert.ok(
    page.includes(':disabled="!hasStatusPermission || !attachGate.canProceed"'),
    '确认按钮必须同时受权限与闸门约束',
  );
  assert.ok(page.includes('<AttachmentGateHint :hint="attachGate.hint"'), '按钮旁必须有红字组件');
  assert.ok(page.includes('testid="rc-detail-gate-hint"'), '红字必须有稳定的 testid');
  assert.ok(page.includes(':content="detailConfirmTip"'), 'disabled 按钮必须包 tooltip');
  assert.ok(page.includes('@changed="refreshAttachGate()"'), '上传后必须重查（验收②）');
  assert.ok(page.includes(':doc-type="DOC_KIND.RECEIPT"'), '必须能上传银行回单');
  assert.ok(page.includes('default-role="BANK_RECEIPT"'), '收款单的必传件是银行回单，不是发票影像');
  // 闸门只对草稿生效（确认收款是唯一受约束的边；已确认/已过账不再查）
  const docId = bodyOf(page, 'const attachDocId');
  assert.ok(docId.includes("detail.value.status === 'DRAFT'"), docId);
  // 列表行「确认」先查闸门，缺回单把人送到详情
  const gate = bodyOf(page, 'async function passAttachmentGate');
  assert.ok(gate.includes("checkDocAttachments('RECEIPT'"), gate);
  assert.ok(gate.includes("'SUBMIT'"), gate);
  assert.ok(gate.includes('openDetail(row)'), '缺回单要把人送到详情（那里有上传面板）');
  const confirm = bodyOf(page, 'async function confirmReceipt');
  assert.ok(confirm.includes('passAttachmentGate(row.id)'), '确认（列表行 / 详情共用）必须先过闸门');
});

// ==================== ⑩ 8 个页面都有附件区，且都只挂在「已保存的单据」上 ====================

test('8 个单据页都有附件区（含已有的报销），且新建态不显示（doc-id 只来自已加载的单据）', () => {
  /** 页面 → [testid 前缀, doc-id 绑定的来源] */
  const panels: Array<[string, string, RegExp]> = [
    ['invoice/index.vue', 'inv-attach', /:doc-id="detail\.id"/],
    ['receipt/index.vue', 'rc-attach', /:doc-id="detail\.id"/],
    ['payable/index.vue', 'payable-attach', /:doc-id="detail\.id"/],
    ['expense/ExpenseDetailDrawer.vue', 'expense-attach', /:doc-id="report\.id"/],
    ['asset/index.vue', 'asset-attach', /:doc-id="detail\.asset\.id"/],
    ['stock-take/index.vue', 'stocktake-attach', /:doc-id="detail\.id"/],
    ['purchase-return/index.vue', 'prt-attach', /:doc-id="detail\.purchase_return\.id"/],
    ['sales-return/index.vue', 'return-attach', /:doc-id="detail\.id"/],
  ];
  for (const [page, prefix, docIdPattern] of panels) {
    const source = readSource(`views/business/erp/${page}`);
    assert.ok(source.includes(`testid-prefix="${prefix}"`), `${page} 必须有附件面板（验收①）`);
    assert.ok(
      docIdPattern.test(source),
      `${page} 的附件面板必须绑已加载的单据 id（新建态没有 id → 不出现附件区，验收④）`,
    );
    // 新建入口是独立弹窗：附件面板不能出现在里面（新建不强传）。
    // 只找模板标签 `<DocAttachmentPanel`，别命中脚本里的 import。
    assert.ok(
      !/v-model:visible="createVisible"[\s\S]{0,4000}?<DocAttachmentPanel/.test(source),
      `${page} 的新建弹窗里不许出现附件面板（新建不强传）`,
    );
  }
});

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
 * 2. 目标按钮 `:disabled` **同时**含权限与闸门（**固定资产启用是唯一例外** ——
 *    它已降级为「提示 + 允许 + 审计」，不置灰，见文末例外段）；
 * 3. 按钮旁有 `<AttachmentGateHint>`（红字，文案来自后端）；
 * 4. 附件面板 `@changed` 触发重查（验收②「传完自动变亮」）；
 * 5. 该页有上传入口（`DocAttachmentPanel`）—— 否则必传规则永远无法满足。
 *
 * **例外（唯一的非硬闸）：固定资产启用。** E2E05-FIX-2（635b07fe）把
 * `FIXED_ASSET` + `ACTIVATE` 从「硬闸」降级为「提示 + 允许 + 审计」：
 * 按钮**不置灰**，缺附件时弹窗给黄字（`t-alert`，testid `asset-enable-gate-hint`）
 * + 去上传的链接 + 点「确认启用」时二次确认，后端写 `ERP_ASSET_ENABLE_NO_ATTACH`
 * 审计。理由：建卡不查附件，「发票未到先建卡」的资产停用一次就再也启用不回来。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
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

test('固定资产：启用不再被闸门置灰，缺附件给黄字 + 二次确认 + 审计留痕，附件挂详情页签', () => {
  const page = readSource('views/business/erp/asset/index.vue');
  assert.ok(page.includes("ref<DocType>('FIXED_ASSET')"), 'doc_type 必须是 FIXED_ASSET');
  assert.ok(page.includes("at: 'ACTIVATE'"), '动作必须是 ACTIVATE（入账 / 启用）');

  // E2E05-FIX-2（635b07fe）方案 B：启用从「硬闸」降级为「提示 + 允许 + 审计」。
  // 旧口径 `disabled: statusChangeKind === 'IN_USE' && !assetGate.canProceed` 会让
  // 「发票未到先建卡」的资产停用一次就永远启用不了（E2E-05 实测 400），已删除。
  assert.ok(
    !page.includes("disabled: statusChangeKind === 'IN_USE' && !assetGate.canProceed"),
    '「确认启用」不能再按附件闸门置灰（E2E05-FIX-2 已降级为提示 + 允许 + 审计）',
  );
  // 两种写法都要挡：`:confirm-btn="{ disabled: … }"`（对象属性）与
  // `:confirm-btn-disabled="…"` / `:disabled="…"`（属性绑定）。只钉一种会留后门 ——
  // 本卡做变异验证时实测：只匹配 `disabled:` 的版本把闸门绑成属性写法仍能溜过。
  assert.ok(
    !/disabled\s*[:=][^\n]*assetGate/.test(page),
    '本页任何按钮都不得再被 assetGate 直接禁用（闸门只负责「提示」）',
  );

  // 缺附件时弹窗里仍有黄字（文案来自后端 assetGate.hint），但只是提示、不拦人。
  // 断言只钉「这个 t-alert 自身」的条件与主题，**不钉属性顺序 / 换行**（d9d25d83 那种
  // rustfmt 折行曾把 244 的逐字断言打成假红，源码契约不该靠排版）。
  const hintAt = page.indexOf('data-testid="asset-enable-gate-hint"');
  assert.ok(hintAt > 0, '缺附件提示必须有稳定 testid `asset-enable-gate-hint`');
  const hintTag = page.slice(page.lastIndexOf('<t-alert', hintAt), page.indexOf('/>', hintAt));
  assert.ok(
    hintTag.includes('!assetGate.canProceed'),
    '黄字必须只在缺附件（!assetGate.canProceed）时出现，不能变成常显',
  );
  assert.ok(hintTag.includes('theme="warning"'), '必须是 warning 黄字（提示），不能是红字 / 危险色');
  assert.ok(page.includes('assetGate.hint'), '缺口文案必须来自后端闸门（前端不拼句子）');
  assert.ok(page.includes('ERP_ASSET_ENABLE_NO_ATTACH'), '必须点名审计操作码（说清会被留痕）');
  assert.ok(page.includes('data-testid="asset-enable-upload-link"'), '提示旁必须给去上传的入口');
  assert.ok(page.includes(':doc-type="DOC_KIND.FIXED_ASSET"'), '必须能上传发票 / 验收单');
  assert.ok(page.includes('@changed="refreshAssetGate()"'), '上传后必须重查');

  // 只有「启用」才查（停用与附件无关）
  const docId = bodyOf(page, 'const assetGateDocId');
  assert.ok(docId.includes("statusChangeKind.value === 'IN_USE'"), docId);

  // 缺附件：不 return 静默失败，走二次确认 → 确认后照常提交（真留痕在后端）
  const submit = bodyOf(page, 'async function submitStatusChange');
  assert.ok(submit.includes('assetGate.value.canProceed'), '提交函数仍要判断闸门结果（缺附件走二次确认）');
  assert.ok(
    submit.includes('confirmEnableWithoutAttachment(target)'),
    '缺附件时不能直接拦掉，必须走二次确认放行',
  );
  const confirm = bodyOf(page, 'function confirmEnableWithoutAttachment');
  assert.ok(confirm.includes('无附件启用'), '二次确认按钮文案要点明「无附件启用」');
  assert.ok(confirm.includes('doSubmitStatusChange()'), '确认后必须真正提交');
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
  // N-5 验收④：付款记录在「登记付款」这一步还没有 id（提交动作内部才创建），
  // 查不了 `/attachment/check`，所以必须在弹窗里先把规则说清楚。
  assert.ok(
    page.includes('data-testid="payable-pay-attach-notice"'),
    '登记付款弹窗必须有「出款前必传银行回单」提示（新建态查不了接口，但要有提示）',
  );
  const payDialog = bodyOf(page, 'data-testid="payable-pay-dialog"', 1200);
  assert.ok(
    payDialog.includes('payable-pay-attach-notice'),
    '提示必须落在登记付款弹窗内（不是详情页别处）',
  );
  assert.ok(payDialog.includes('银行回单'), '提示要点名缺什么：银行回单');
  assert.ok(payDialog.includes('pdf/jpg/png'), '提示要写清格式白名单（与规则表 allow_exts 一致）');
  assert.ok(
    payDialog.includes('执行付款') && payDialog.includes('置灰'),
    '提示要说清缺回单的后果（「执行付款」按钮置灰）',
  );
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

// ==================== ⑫ N-5 验收④：8 个必传单据的「新建 / 登记」入口都有必传提示 ====================
//
// 新建态没有单据 id → 查不了 `/attachment/check`（也不该查）。但「规则已确定」这件事
// 必须在新建这一步就说清楚，否则用户填完一堆字段、点了提交才吃 400 —— 评审原文
// 「附件必传只在提交时拦，草稿无提示」正是这条。所以每页的**新建入口**（弹窗 / 抽屉）
// 里都要有一条醒目的必传提示，且带稳定 testid（e2e 与人工复核都能定位）。

test('N-5 ④：8 个必传单据的新建 / 登记入口都先说清「必传什么、什么格式」', () => {
  /**
   * 页面 → [新建入口提示的 testid, 期望主题]
   *
   * 主题差异是**有理由的**：7 个硬闸单据用 warning（缺了就提交不了）；
   * 固定资产启用已按 E2E05-FIX-2 降级为「提示 + 允许 + 审计」（见本文件 ⑤），
   * 所以建卡提示是 info —— 写成 warning 会让人以为缺附件就不能建卡/启用。
   */
  const notices: Array<[string, string, string]> = [
    ['invoice/index.vue', 'inv-create-attach-notice', 'warning'],
    ['receipt/index.vue', 'rc-create-attach-notice', 'warning'],
    ['payable/index.vue', 'payable-pay-attach-notice', 'warning'],
    ['expense/index.vue', 'expense-create-attach-notice', 'warning'],
    ['asset/index.vue', 'asset-create-attach-notice', 'info'],
    ['stock-take/index.vue', 'stocktake-create-attach-notice', 'warning'],
    ['purchase-return/index.vue', 'prt-create-attach-notice', 'warning'],
    ['sales-return/index.vue', 'return-create-attach-notice', 'warning'],
  ];
  assert.equal(notices.length, 8, '必传单据正好 8 个（V0347 种子）');
  for (const [page, testid, theme] of notices) {
    const source = readSource(`views/business/erp/${page}`);
    const at = source.indexOf(`data-testid="${testid}"`);
    assert.ok(at > 0, `${page} 的新建入口必须有必传提示（testid=${testid}，验收④）`);
    // 提示所在的标签必须是一条 warning 级 t-alert / 提示块，且文案里说清「格式」
    const tag = source.slice(source.lastIndexOf('<t-alert', at), source.indexOf('/>', at));
    assert.ok(tag.length > 0, `${page} 的必传提示应当是一条 t-alert`);
    assert.ok(
      tag.includes(`theme="${theme}"`),
      `${page} 的必传提示主题应为 ${theme}（缺了它用户就得等 400 才知道规则）`,
    );
    assert.ok(tag.includes('message='), `${page} 的必传提示必须带文案`);
    assert.ok(
      /pdf|ofd|jpg/.test(tag),
      `${page} 的必传提示必须写清格式白名单（用户在新建时就该知道传什么）`,
    );
    assert.ok(
      tag.includes('上传'),
      `${page} 的必传提示必须说清「去哪传 / 传完会怎样」`,
    );
  }
});

// ==================== ⑬ 规则表的每一条都要有页面接线（配了规则没人接 = 静默失效） ====================

test('N-5：V0347 种子里每个 (doc_type, required_at) 至少有一个页面接线', () => {
  const migration = readFileSync(
    resolve(HERE, '../../../backend/migrations/business/V0347__attachment_required_rules.up.sql'),
    'utf8',
  );
  const start = migration.indexOf('-- §3');
  const end = migration.indexOf('-- §4');
  assert.ok(start > 0 && end > start, 'V0347 必须有 §3 种子段与 §4 自检段');
  const seed = migration.slice(start, end);

  const pairs = new Set<string>();
  for (const match of seed.matchAll(
    /\(\s*'[0-9a-f-]{36}'::uuid,\s*'([A-Z_]+)',\s*NULL,\s*'([A-Z]+)'/g,
  )) {
    pairs.add(`${match[1]}/${match[2]}`);
  }
  // 8 类单据（INVOICE 拆电子 / 纸质两条规则 → 去重后 8 对）
  assert.deepEqual(
    [...pairs].sort(),
    [
      'EXPENSE_REPORT/SUBMIT',
      'FIXED_ASSET/ACTIVATE',
      'INVOICE/SUBMIT',
      'PAYMENT/ACTIVATE',
      'PURCHASE_RETURN/SUBMIT',
      'RECEIPT/SUBMIT',
      'SALES_RETURN/SUBMIT',
      'STOCK_TAKE/CONFIRM',
    ],
    'V0347 种子的 (doc_type, required_at) 必须正好是这 8 对',
  );

  // 全量扫描业务页面：某页同时出现 `'<DOC_TYPE>'` 与 `'<AT>'` 才算接线
  const dir = join(SRC, 'views/business/erp');
  const sources: Array<[string, string]> = [];
  const walk = (current: string): void => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.name.endsWith('.vue')) {
        sources.push([full.slice(dir.length + 1), readFileSync(full, 'utf8')]);
      }
    }
  };
  walk(dir);
  assert.ok(sources.length > 20, `应当扫到二十多个业务页面，实际 ${sources.length}`);

  for (const pair of pairs) {
    const [docType, at] = pair.split('/');
    const wired = sources.filter(
      ([, text]) => text.includes(`'${docType}'`) && text.includes(`'${at}'`),
    );
    assert.ok(
      wired.length > 0,
      `规则 ${pair} 没有任何页面接线（查了也无处提示 / 按钮不会灰，等于静默失效）`,
    );
  }
});

// ==================== ⑭ 验收⑤：公共 composable 复用，不是 8 份重复代码 ====================
//
// 8 个单据页各自维护一份「查接口 → 拼红字 → 丢弃乱序响应 → 失败降级」的话，
// 迟早出现「发票页传完变亮、报销页传完不变亮」这类分叉。所以闸门只能有一份实现
// （`views/business/erp/shared/use-attachment-gate.ts`），页面只许 import。
//
// **唯一例外：应付页（PAYMENT）。** 一张应付单可以有多笔付款，每笔都要出自己的
// 「缺回单」红字，单实例组合式函数只能盯一个 document id，所以那里用「按 id 缓存
// 多份闸门」的表结构；但判定与文案仍走同一套纯函数（`gateFromCheck` /
// `degradedGate`），不是第二套判据（见 payable/index.vue 的注释）。

test('N-5 ⑤：8 个单据页复用同一个 composable，全仓只有一份闸门实现', () => {
  const shared = 'views/business/erp/shared/use-attachment-gate.ts';
  const impl = readSource(shared);
  assert.ok(impl.includes('export function useAttachmentGate('), '组合式函数必须导出在 shared 里');
  // 判定/降级/文案全部来自纯函数模块，组合式函数自己不算第二套判据
  assert.ok(impl.includes("from './attachment-gate-pure'"), '判定必须复用 attachment-gate-pure');
  for (const [name, source] of [
    ['use-attachment-gate.ts', impl],
    ['attachment-gate-pure.ts', readSource('views/business/erp/shared/attachment-gate-pure.ts')],
    ['AttachmentGateHint.vue', readSource('views/business/erp/shared/AttachmentGateHint.vue')],
  ] as Array<[string, string]>) {
    assert.ok(
      !source.includes('checkDocAttachments(') || name === 'use-attachment-gate.ts',
      `${name} 不该自己再查一次接口（判据只留一处）`,
    );
  }

  /** 必须复用组合式函数的 7 个页面（应付页按行多实例，单独在 ⑥ 钉） */
  const pages = [
    'invoice/index.vue',
    'receipt/index.vue',
    'expense/ExpenseDetailDrawer.vue',
    'asset/index.vue',
    'stock-take/index.vue',
    'purchase-return/index.vue',
    'sales-return/index.vue',
  ];
  for (const page of pages) {
    const source = readSource(`views/business/erp/${page}`);
    assert.ok(
      source.includes("from '@/views/business/erp/shared/use-attachment-gate'"),
      `${page} 必须 import 公共组合式函数（不许就地复制一份）`,
    );
    assert.ok(source.includes('useAttachmentGate({'), `${page} 必须真的调用它`);
    assert.ok(
      !/function\s+useAttachmentCheck/.test(source),
      `${page} 不许自己再实现一份闸门（验收⑤）`,
    );
  }

  // 全仓只允许一处 `export function useAttachmentGate` 定义
  const dir = join(SRC, 'views/business/erp');
  const hits: string[] = [];
  const walk = (current: string): void => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (/\.(vue|ts)$/.test(entry.name)) {
        if (readFileSync(full, 'utf8').includes('export function useAttachmentGate(')) {
          hits.push(full.slice(dir.length + 1));
        }
      }
    }
  };
  walk(dir);
  assert.deepEqual(hits, ['shared/use-attachment-gate.ts'], `闸门实现必须只有一份，实际 ${hits.join(', ')}`);
});

// ==================== ⑪ ATT-11：建议传（不校验）的 6 个页面 ====================
//
// 卡面把「建议传」和「必传」分得很清：这里**不许**出现闸门（没有 @changed 重查、
// 没有 disabled ∪ canProceed），只有「能传 + 一句建议上传的提示」。
// 断言两件事：① 面板挂对了 doc_type 与已保存单据的 id；② 提示文案存在且写明「非必传」。

test('ATT-11：6 个建议传页面都挂对了 DocAttachmentPanel（doc_type / doc_id / 默认角色）', () => {
  const cases: Array<[string, string, RegExp, string, string]> = [
    // 页面, testid 前缀, doc-id 绑定, doc_type, 默认角色
    [
      'contract/index.vue',
      'contract-attach',
      /:doc-id="detail\.id"/,
      ':doc-type="DOC_KIND.CONTRACT"',
      'DEFAULT_ROLE_BY_DOC.CONTRACT',
    ],
    [
      'purchase/index.vue',
      'purchase-attach',
      /:doc-id="detail\.id"/,
      ':doc-type="DOC_KIND.PURCHASE_ORDER"',
      'DEFAULT_ROLE_BY_DOC.PURCHASE_ORDER',
    ],
    [
      'transfer/index.vue',
      'transfer-attach',
      /:doc-id="detail\.id"/,
      ':doc-type="DOC_KIND.STOCK_TRANSFER"',
      'DEFAULT_ROLE_BY_DOC.STOCK_TRANSFER',
    ],
    [
      'supplier/index.vue',
      'supplier-attach',
      /:doc-id="detail\.id"/,
      ':doc-type="DOC_KIND.SUPPLIER"',
      'DEFAULT_ROLE_BY_DOC.SUPPLIER',
    ],
    [
      'customer/index.vue',
      'customer-attach',
      /:doc-id="editing\.id"/,
      ':doc-type="DOC_KIND.CUSTOMER"',
      'DEFAULT_ROLE_BY_DOC.CUSTOMER',
    ],
    [
      'opening/index.vue',
      'opening-attach',
      /:doc-id="opening\?\.voucher\?\.id \?\? null"/,
      ':doc-type="DOC_KIND.OPENING_BALANCE"',
      'DEFAULT_ROLE_BY_DOC.OPENING_BALANCE',
    ],
  ];

  for (const [page, prefix, docIdPattern, docType, defaultRole] of cases) {
    const source = readSource(`views/business/erp/${page}`);
    assert.ok(source.includes(`testid-prefix="${prefix}"`), `${page} 必须有附件面板（验收①）`);
    assert.ok(source.includes(docType), `${page} 的 doc_type 必须是 ${docType}`);
    assert.ok(docIdPattern.test(source), `${page} 的附件面板必须绑已保存对象的 id`);
    assert.ok(
      source.includes(`:default-role="${defaultRole}"`),
      `${page} 的默认上传角色必须是 ${defaultRole}（按业务给）`,
    );
    // 建议上传提示（不是「必须」）
    assert.ok(source.includes('hint="'), `${page} 必须有「建议上传」提示文案（验收②）`);
    assert.ok(
      !source.includes('useAttachmentGate('),
      `${page} 是建议传：不许接闸门（接了就变成阻断）`,
    );
  }
});

test('ATT-11：客户页的附件区只在「编辑已保存客户」时出现（新建没有 doc id）', () => {
  const page = readSource('views/business/erp/customer/index.vue');
  // 附件块被 isEdit && editing 包住（新建客户时 form 里还没有 id）
  assert.ok(
    /v-if="isEdit && editing"[\s\S]{0,600}?<DocAttachmentPanel/.test(page),
    '客户附件区必须在 isEdit && editing 条件下渲染',
  );
  assert.ok(
    !/v-model:visible="dialogVisible"[\s\S]{0,3000}?<DocAttachmentPanel[\s\S]{0,200}?:doc-id="form\./.test(
      page,
    ),
    '附件不能绑未保存表单上的临时 id',
  );
});

test('ATT-11：期初页把期初凭证号写进提示（用户要知道附件挂在哪一期）', () => {
  const page = readSource('views/business/erp/opening/index.vue');
  assert.ok(page.includes('data-testid="opening-attach-voucher"'), '必须显示期初凭证号');
  assert.ok(page.includes("?? null"), '还没有凭证时 doc-id 必须传 null（面板显示「保存后可上传」）');
});

test('ATT-11 / V0368：词表 17 类单据 / 14 个可上传角色（前后端由 Rust 单测再钉一遍）', () => {
  const kinds = readSource('api/business/erp/doc-kinds.ts');
  const block = kinds.slice(
    kinds.indexOf('export const ATTACHMENT_DOC_TYPES'),
    kinds.indexOf('] as const;', kinds.indexOf('export const ATTACHMENT_DOC_TYPES')),
  );
  const codes = [...block.matchAll(/'([A-Z_]+)'/g)].map((match) => match[1]);
  assert.deepEqual(codes, [
    'CONTRACT',
    'PURCHASE_ORDER',
    'STOCK_IN',
    'PAYABLE',
    'PAYMENT',
    'FIXED_ASSET',
    'EXPENSE_REPORT',
    'INVOICE',
    'RECEIPT',
    'STOCK_TAKE',
    'PURCHASE_RETURN',
    'SALES_RETURN',
    'STOCK_TRANSFER',
    'SUPPLIER',
    'CUSTOMER',
    'OPENING_BALANCE',
    // NEAR-FIX-1 / V0368：退款凭据（追加在末尾，与 Rust ATTACHMENT_DOC_KINDS 顺序一致）
    'RECEIPT_REFUND',
  ]);

  const api = readSource('api/business/erp/attachment.ts');
  for (const role of [
    'QUOTATION',
    'CUSTOMER_PO',
    'BIZ_LICENSE',
    'BANK_ACCOUNT_PERMIT',
    'QUALIFICATION',
    'CREDIT_FILE',
    'TRANSFER_APPROVAL',
    'OPENING_EVIDENCE',
  ]) {
    assert.ok(api.includes(`value: '${role}'`), `FILE_ROLE_OPTIONS 必须含 ${role}`);
  }
  // 每类单据的默认角色都要有（Record<DocType, FileRole> 是穷举的，漏一个 typecheck 就红）
  for (const doc of ['STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE']) {
    assert.ok(api.includes(`${doc}: '`), `DEFAULT_ROLE_BY_DOC 必须含 ${doc}`);
  }
});

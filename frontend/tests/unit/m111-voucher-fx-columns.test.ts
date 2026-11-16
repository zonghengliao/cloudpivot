/**
 * M1-11：凭证页「币种 / 汇率」列（录入与展示）单测。
 *
 * 运行：`cd frontend && pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 这张卡钉的是什么
 *
 * M1-04 已经把**录入**做完（币种下拉 + 锁定汇率 + 本位币预览 + 分录原币），
 * 但录完之后列表 / 详情里看不见外币口径：一行 `借方 7,200.00` 到底是 CNY 还是
 * 「USD 1,000 折出来的 7,200」没人说得清。所以本卡只补**展示**，两条断言：
 *
 * 1. **纯逻辑**：外币判据（汇率 ≠ 1）、汇率文案、原币金额文案与合计 —— 都是
 *    「会算错钱」的地方，用单测钉死（含 JPY 量级牌价、4 位小数原币、尾差 0.0001）；
 * 2. **源码即契约**：列表必须真有 `currency_code` / `exchange_rate` 两列且带
 *    `data-testid`，详情必须有币种 / 汇率格与「原币」列，而这套 UI **只对
 *    外币凭证出现**（本位币凭证多一列都是回归）。录入侧（M1-04 的
 *    `voucher-create-currency` / `voucher-create-rate`）也一并守住，别在补展示时被删掉。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  entryFxAmountText,
  formatFxAmount,
  isForeignVoucher,
  sumEntryFx,
  toTenThousandths,
  voucherCurrencyTip,
  voucherRateDetailText,
  voucherRateText,
} from '@/views/business/erp/voucher/voucher-logic';

const ROOT = resolve(process.cwd());

/** 读一页 SFC / 模块源码（只做子串与正则断言，与既有源码契约测同风格） */
function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

const VOUCHER_PAGE = sourceOf('src/views/business/erp/voucher/index.vue');

// ==================== ① 外币判据 ====================

test('外币判据：锁定汇率 ≠ 1 才是外币（本位币凭证恒 1）', () => {
  assert.equal(isForeignVoucher({ currency_code: 'CNY', exchange_rate: 1 }), false);
  // 后端 `rust_decimal` 出 JSON 是字符串，形态必须都认
  assert.equal(isForeignVoucher({ currency_code: 'CNY', exchange_rate: '1.0000' }), false);
  assert.equal(isForeignVoucher({ currency_code: 'USD', exchange_rate: '7.20000000' }), true);
  assert.equal(isForeignVoucher({ currency_code: 'JPY', exchange_rate: 0.0481 }), true);
  // 缺字段 / 非法值：一律按本位币处理（**不是**「猜成外币」—— 猜错会把本位币金额当原币显示）
  assert.equal(isForeignVoucher(null), false);
  assert.equal(isForeignVoucher(undefined), false);
  assert.equal(isForeignVoucher({}), false);
  assert.equal(isForeignVoucher({ currency_code: 'USD', exchange_rate: null }), false);
  assert.equal(isForeignVoucher({ currency_code: 'USD', exchange_rate: '' }), false);
  assert.equal(isForeignVoucher({ currency_code: 'USD', exchange_rate: 0 }), false);
  assert.equal(isForeignVoucher({ currency_code: 'USD', exchange_rate: 'abc' }), false);
});

// ==================== ② 汇率文案 ====================

test('列表汇率：外币给锁定汇率（去尾随 0、保住 JPY 量级），本位币给 —', () => {
  assert.equal(voucherRateText({ currency_code: 'CNY', exchange_rate: '1.0000' }), '—');
  assert.equal(voucherRateText({ currency_code: 'USD', exchange_rate: '7.20000000' }), '7.2');
  // 0.04x 的牌价不能被压成 0.0400（那是「看起来填错了」的经典形态，见 M1-04 单测同口径）
  assert.equal(voucherRateText({ currency_code: 'JPY', exchange_rate: '0.04812345' }), '0.04812345');
  assert.equal(voucherRateText(null), '—');
});

test('详情汇率：带语义（外币说清「建单锁定」与折算方向，本位币说清「不折算」）', () => {
  assert.equal(
    voucherRateDetailText({ currency_code: 'USD', exchange_rate: 7.2 }),
    '7.2（1 USD 折本位币，建单锁定）',
  );
  assert.equal(voucherRateDetailText({ currency_code: 'CNY', exchange_rate: 1 }), '1（本位币凭证，不折算）');
  assert.equal(voucherRateDetailText(null), '1（本位币凭证，不折算）');
});

test('币种列 tooltip：外币必须点明「借方 / 贷方列是本位币」，本位币一句带过', () => {
  const foreign = voucherCurrencyTip({ currency_code: 'USD', exchange_rate: 7.2 });
  assert.match(foreign, /USD/);
  assert.match(foreign, /原币/);
  assert.match(foreign, /本位币/);
  const base = voucherCurrencyTip({ currency_code: 'CNY', exchange_rate: 1 });
  assert.match(base, /本位币/);
  assert.doesNotMatch(base, /折算后的本位币/);
});

// ==================== ③ 原币金额（4 位小数） ====================

test('元 → 万分之一元：与 NUMERIC(20,4) 同精度，字符串 / 浮点都认', () => {
  assert.equal(toTenThousandths(1000), 10_000_000);
  assert.equal(toTenThousandths('1000.0000'), 10_000_000);
  assert.equal(toTenThousandths('0.0481'), 481);
  assert.equal(toTenThousandths(1.23456), 12_346); // 五入到 4 位
  assert.equal(toTenThousandths(null), 0);
  assert.equal(toTenThousandths(undefined), 0);
  assert.equal(toTenThousandths(''), 0);
  assert.equal(toTenThousandths(-7.5), -75_000);
});

test('原币文案：能整除到分给 2 位，真有小数的币种给 4 位（不一律截断）', () => {
  assert.equal(formatFxAmount(10_000_000), '1,000.00');
  assert.equal(formatFxAmount(0), '0.00');
  assert.equal(formatFxAmount(481), '0.0481');
  assert.equal(formatFxAmount(100_000_050), '10,000.0050');
  assert.equal(formatFxAmount(-75_000), '-7.50');
});

test('分录原币文案：带币种前缀；本位币分录（两列 null）给 —', () => {
  assert.equal(
    entryFxAmountText({ fc_debit_amount: '1000.0000', fc_credit_amount: '0.0000' }, 'USD'),
    'USD 1,000.00',
  );
  // 贷方行（借方原币是 0，不是 null）
  assert.equal(
    entryFxAmountText({ fc_debit_amount: '0.0000', fc_credit_amount: '250.5000' }, 'USD'),
    'USD 250.50',
  );
  // 本位币分录：V0409 的 NULL 语义 = 该行没有原币口径
  assert.equal(entryFxAmountText({ fc_debit_amount: null, fc_credit_amount: null }, 'CNY'), '—');
  // 缺币种时别渲染成「null 1,000.00」
  assert.equal(entryFxAmountText({ fc_debit_amount: 1000, fc_credit_amount: 0 }, null), '1,000.00');
  assert.equal(entryFxAmountText(null, 'USD'), '—');
});

test('原币合计：逐行按万分之一元累加，尾差 0.0001 也要报出来', () => {
  const balanced = sumEntryFx([
    { fc_debit_amount: '1000.0000', fc_credit_amount: '0.0000' },
    { fc_debit_amount: '0.0000', fc_credit_amount: '1000.0000' },
  ]);
  assert.equal(balanced.debitTenThousandths, 10_000_000);
  assert.equal(balanced.creditTenThousandths, 10_000_000);
  assert.equal(balanced.balanced, true);

  // 后端尾差吸收可能让原币差 0.0001：界面必须说「不平」，不能四舍五入成平
  const tail = sumEntryFx([
    { fc_debit_amount: '1000.0000', fc_credit_amount: '0.0000' },
    { fc_debit_amount: '0.0000', fc_credit_amount: '999.9999' },
  ]);
  assert.equal(tail.balanced, false);
  assert.equal(tail.diffTenThousandths, 1);
  assert.equal(formatFxAmount(Math.abs(tail.diffTenThousandths)), '0.0001');

  // 空分录（详情还没回来）：0 = 0，不误报
  assert.equal(sumEntryFx([]).balanced, true);
});

// ==================== ④ 源码即契约：展示（列表 + 详情） ====================

test('列表：真有「币种」「汇率」两列，且单元格带 testid', () => {
  assert.match(VOUCHER_PAGE, /\{ colKey: 'currency_code', title: '币种'/);
  assert.match(VOUCHER_PAGE, /\{ colKey: 'exchange_rate', title: '汇率'/);
  assert.match(VOUCHER_PAGE, /<template #currency_code="\{ row \}">/);
  assert.match(VOUCHER_PAGE, /<template #exchange_rate="\{ row \}">/);
  assert.match(VOUCHER_PAGE, /data-testid="`voucher-currency-\$\{row\.voucher_no\}`"/);
  assert.match(VOUCHER_PAGE, /data-testid="`voucher-rate-\$\{row\.voucher_no\}`"/);
  // 文案与判据必须走纯函数（页面里不许再手写一套 `exchange_rate !== 1`）
  assert.match(VOUCHER_PAGE, /voucherRateText\(row\)/);
  assert.match(VOUCHER_PAGE, /isForeignVoucher\(row\)/);
});

test('详情：币种 / 汇率两格 + 外币 tag（本位币单不打 tag）', () => {
  assert.match(VOUCHER_PAGE, /data-testid="voucher-detail-currency"/);
  assert.match(VOUCHER_PAGE, /data-testid="voucher-detail-rate"/);
  assert.match(VOUCHER_PAGE, /v-if="detailForeign"[\s\S]{0,200}data-testid="voucher-detail-foreign-tag"/);
  assert.match(VOUCHER_PAGE, /voucherRateDetailText\(detail\)/);
});

test('分录表：外币才有「原币」列，借方 / 贷方标题这时才标（本位币）', () => {
  // 列是 computed，判据只有 `detailForeign` 一处
  assert.match(VOUCHER_PAGE, /const entryColumns = computed<PrimaryTableCol\[\]>/);
  assert.match(VOUCHER_PAGE, /if \(foreign\) \{[\s\S]{0,300}colKey: 'fc_amount'/);
  assert.match(VOUCHER_PAGE, /title: currencyCode \? `原币（\$\{currencyCode\}）` : '原币金额'/);
  assert.match(VOUCHER_PAGE, /title: foreign \? '借方（本位币）' : '借方'/);
  assert.match(VOUCHER_PAGE, /title: foreign \? '贷方（本位币）' : '贷方'/);
  assert.match(VOUCHER_PAGE, /<template #fc_amount="\{ row \}">/);
  assert.match(VOUCHER_PAGE, /entryFxAmountText\(row, detail\?\.currency_code\)/);
});

test('原币合计：只在 v-if="detailForeign" 出现，且不复用本位币那行的 testid', () => {
  assert.match(VOUCHER_PAGE, /v-if="detailForeign"[\s\S]{0,200}data-testid="voucher-balance-fx"/);
  assert.match(VOUCHER_PAGE, /data-testid="voucher-balance-fx-debit"/);
  assert.match(VOUCHER_PAGE, /data-testid="voucher-balance-fx-credit"/);
  assert.match(VOUCHER_PAGE, /data-testid="voucher-balance-fx-text"/);
  // 本位币合计行的 testid 一个字都不能改（既有 e2e / 单测按它定位）
  assert.match(VOUCHER_PAGE, /data-testid="voucher-balance"/);
  assert.match(VOUCHER_PAGE, /data-testid="voucher-balance-debit"/);
  assert.match(VOUCHER_PAGE, /data-testid="voucher-balance-credit"/);
});

// ==================== ⑤ 源码即契约：录入侧没被删（M1-04 回归） ====================

test('录入：币种下拉 / 锁定汇率 / 本位币预览仍在（补展示不许碰录入）', () => {
  assert.match(VOUCHER_PAGE, /data-testid="voucher-create-currency"/);
  assert.match(VOUCHER_PAGE, /data-testid="voucher-create-rate"/);
  assert.match(VOUCHER_PAGE, /data-testid="voucher-create-base-total"/);
  assert.match(VOUCHER_PAGE, /currency_code: isForeign\.value \? createForm\.currency_code : null/);
  // 外币无牌价时前端必须拦（后端也拦，但别让用户白跑一趟）
  assert.match(VOUCHER_PAGE, /isForeign\.value && !fxRateAvailable\.value/);
});

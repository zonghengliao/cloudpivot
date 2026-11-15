/**
 * 外币凭证（M1-04）前端纯逻辑单测：原币 → 本位币预览 / 汇率文案 / 合计。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 为什么这几条值得单测
 *
 * 录入界面上「本位币金额」这一列是**用户判断这笔账对不对的唯一依据**：
 * 原币 100、汇率 7.2，人说得出「应该是 720」。预览算错了，用户会以为后端记错了账，
 * 或者（更糟）看着错数字按保存。所以：
 *
 * * 换算必须与后端 `currency::scale_amount`（4 位小数、五入）**同口径**；
 * * 汇率文案不能把 0.04x 量级的牌价四舍五入成 0.0400（JPY 类币种会看着像填错）；
 * * 合计必须走整数（万分之一元），任何浮点残留都会变成「差 0.0001」的假警报。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  baseTenThousandths,
  formatFxRate,
  formatTenThousandths,
  fxPreview,
} from '@/views/business/erp/voucher/voucher-logic';

test('M1-04 换算：原币分 → 本位币万分之一元（4 位小数五入）', () => {
  // 原币 100.00（= 10000 分）× 8.5 = 850.0000 元 = 8_500_000 个万分之一元
  assert.equal(baseTenThousandths(10_000, 8.5), 8_500_000);
  // 原币 33.3333 元（3333.33 分不成立：界面按 2 位小数录入 ⇒ 用 33.33）
  assert.equal(baseTenThousandths(3_333, 8.5), 2_833_050); // 283.3050
  // 汇率非法 / 非正：一律 0（不猜、不按 1 算 —— 后端会 400 拒绝，界面不该先算出一个数）
  assert.equal(baseTenThousandths(10_000, 0), 0);
  assert.equal(baseTenThousandths(10_000, Number.NaN), 0);
  // 0 金额
  assert.equal(baseTenThousandths(0, 7.2), 0);
});

test('M1-04 换算文案：4 位小数 + 三位分组', () => {
  assert.equal(formatTenThousandths(8_500_000), '850.0000');
  assert.equal(formatTenThousandths(12_345_678), '1,234.5678');
  assert.equal(formatTenThousandths(1), '0.0001');
  assert.equal(formatTenThousandths(-2_500_000), '-250.0000');
});

test('M1-04 汇率文案：最多 8 位、去掉尾随 0（不把 JPY 压成 0.0400）', () => {
  assert.equal(formatFxRate('8.5'), '8.5');
  assert.equal(formatFxRate(7.2), '7.2');
  assert.equal(formatFxRate('7.12345678'), '7.12345678');
  // JPY 量级：不能被截成 0.04 —— 那是「看起来填错了」的经典形态
  assert.equal(formatFxRate('0.04812345'), '0.04812345');
  assert.equal(formatFxRate(1), '1');
  // 取不到牌价：破折号而不是 0 / 空
  assert.equal(formatFxRate(null), '—');
  assert.equal(formatFxRate(undefined), '—');
  assert.equal(formatFxRate(''), '—');
  assert.equal(formatFxRate(0), '—');
});

test('M1-04 合计预览：原币按分、本位币按万分之一元，各自独立', () => {
  const rows = [
    { debit: 33.33, credit: 0 },
    { debit: 66.67, credit: 0 },
    { debit: 0, credit: 100 },
  ];
  const preview = fxPreview(rows, 8.5);
  assert.equal(preview.debitCents, 10_000);
  assert.equal(preview.creditCents, 10_000);
  // 283.3050 + 566.6950 = 850.0000：与贷方 850.0000 相等（界面这一份不做尾差吸收）
  assert.equal(preview.baseDebitTenThousandths, 8_500_000);
  assert.equal(preview.baseCreditTenThousandths, 8_500_000);
  assert.equal(preview.baseDiffTenThousandths, 0);
});

test('M1-04 合计预览：汇率缺省（null）时只算原币，不给出假的 0 本位币', () => {
  const preview = fxPreview([{ debit: 100, credit: 0 }, { debit: 0, credit: 100 }], null);
  assert.equal(preview.debitCents, 10_000);
  assert.equal(preview.creditCents, 10_000);
  assert.equal(preview.baseDebitTenThousandths, 0);
  assert.equal(preview.baseCreditTenThousandths, 0);
  assert.equal(preview.baseDiffTenThousandths, 0);
});

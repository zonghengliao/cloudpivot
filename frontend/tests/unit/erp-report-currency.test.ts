/**
 * 报表币种切换的纯逻辑单测（M1-09）。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 「是不是外币口径」「折合列表头写什么」「页头那句话怎么说」「导出文件名带不带币种」
 * 全在 `currency-basis-logic.ts`；`.vue` 只负责摆位置。口径漂移时 `vue-tsc` 抓不到
 * （比如有人把 `rate_missing` 判断删掉、改成 `rate == null` 自己推，缺牌价就会显示成
 * 「折算 null」），只有写成函数才能钉住。
 *
 * # 断言口径与后端逐字对齐
 *
 * - `mode` 是后端给的（`BASE` / `FOREIGN`），前端不靠「币种 != 本位币」自己推；
 * - 缺牌价看 `rate_missing`，折合列必须**留空**（后端绝不回落 rate = 1）；
 * - 折合列表头写的是**账套本位币**，不写死 CNY。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  CURRENCY_MODE_BASE,
  CURRENCY_MODE_FOREIGN,
  baseCurrencyLabel,
  buildCurrencyOptions,
  convertedHeader,
  currencyModeText,
  exportCurrencySuffix,
  isForeignMode,
} from '@/views/business/erp/report/currency-basis-logic';

/** 本位币口径的币种块（后端 `ReportCurrencyRes` 形状）。 */
const baseBlock = {
  mode: CURRENCY_MODE_BASE,
  currency: 'CNY',
  currency_name: '人民币',
  base_currency: 'CNY',
  rate: '1',
  rate_date: null,
  rate_missing: false,
  entry_count: 120,
};

/** 外币口径、有牌价。 */
const usdBlock = {
  mode: CURRENCY_MODE_FOREIGN,
  currency: 'USD',
  currency_name: '美元',
  base_currency: 'CNY',
  rate: '7.10000000',
  rate_date: '2064-10-31',
  rate_missing: false,
  entry_count: 4,
};

/** 外币口径、缺期末牌价。 */
const hkdBlock = {
  mode: CURRENCY_MODE_FOREIGN,
  currency: 'HKD',
  currency_name: null,
  base_currency: 'CNY',
  rate: null,
  rate_date: '2064-10-31',
  rate_missing: true,
  entry_count: 2,
};

test('口径判断只看后端给的 mode（不靠币种码自己推）', () => {
  assert.equal(isForeignMode(baseBlock), false);
  assert.equal(isForeignMode(usdBlock), true);
  // 没拿到币种块（老接口 / PDF 路径）⇒ 按本位币处理，绝不出折合列
  assert.equal(isForeignMode(null), false);
  assert.equal(isForeignMode(undefined), false);
});

test('折合列表头写账套本位币，不写死 CNY', () => {
  assert.equal(convertedHeader(usdBlock), '折合CNY');
  assert.equal(
    convertedHeader({ ...usdBlock, base_currency: 'USD' }),
    '折合USD',
    '账套本位币换成 USD 时表头必须跟着变（写死 CNY 会把本位币标错）',
  );
  assert.equal(baseCurrencyLabel(null), '本位币', '拿不到本位币时给中性文案');
});

test('页头口径文案三种状态各说一句人话', () => {
  assert.equal(currencyModeText(baseBlock), '本位币 CNY');
  assert.equal(currencyModeText(usdBlock), '原币 USD（美元）·折算 7.10000000');
  assert.equal(
    currencyModeText(hkdBlock),
    '原币 HKD·缺期末牌价，折合列留空',
    '缺牌价必须说「留空」（否则用户以为报表坏了）',
  );
  assert.equal(currencyModeText(null), '');
});

test('缺牌价即便 rate 字段意外有值，也按 rate_missing 报留空', () => {
  // 后端契约是「缺牌价 ⇒ rate = null + rate_missing = true」，这里挡的是「只判 rate」的写法
  const contradictory = { ...hkdBlock, rate: '1' };
  assert.match(currencyModeText(contradictory), /留空/);
});

test('导出文件名后缀：外币带币种码、本位币为空', () => {
  assert.equal(exportCurrencySuffix('USD'), '_USD');
  assert.equal(exportCurrencySuffix('USD'.toLowerCase()), '_USD', '统一转大写');
  assert.equal(exportCurrencySuffix(''), '');
  assert.equal(exportCurrencySuffix(null), '');
  assert.equal(exportCurrencySuffix(undefined), '');
  // 同一天导本位币 + 美元两张报表不能重名（数字完全不同，覆盖就是丢报表）
  assert.notEqual(
    `资产负债表_2064-10${exportCurrencySuffix('USD')}.xlsx`,
    `资产负债表_2064-10${exportCurrencySuffix('')}.xlsx`,
  );
});

test('币种下拉：本位币恒第一项且值为空串（不传 currency 参数）', () => {
  const options = buildCurrencyOptions([
    { code: 'CNY', label: '人民币（CNY）', is_base: true },
    { code: 'USD', label: '美元（USD）', is_base: false },
    { code: 'EUR', label: '欧元（EUR）', is_base: false },
  ]);
  assert.deepEqual(options, [
    { label: '本位币', value: '' },
    { label: '美元（USD）（原币）', value: 'USD' },
    { label: '欧元（EUR）（原币）', value: 'EUR' },
  ]);
  assert.equal(options[0].value, '', '空串 = 后端不传 currency = 本位币口径');
});

test('币种下拉：只有本位币时也只剩「本位币」一项', () => {
  const options = buildCurrencyOptions([{ code: 'CNY', label: '人民币（CNY）', is_base: true }]);
  assert.deepEqual(options, [{ label: '本位币', value: '' }]);
});

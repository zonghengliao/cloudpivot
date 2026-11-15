/**
 * 汇兑损益科目配置的纯逻辑单测（M1-06 · V0411）。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「当前配置怎么显示」「保存能不能点」「66083 报什么」
 * 全在 `fx-settings-logic.ts`；`.vue` 只负责画。这些口径漂移时 `vue-tsc` 抓不到
 * （比如有人把 `v-if` 改成 `:disabled`，或把 66083 改成 66082），只有写成函数才能钉住。
 *
 * 断言口径与后端逐字对齐：
 * - 参数键 / 缺省值：`fx_settings::{CONFIG_FX_GAIN_LOSS_ACCOUNT, DEFAULT_FX_GAIN_LOSS_ACCOUNT}`
 * - 值来源：`fx_settings_handler::{SOURCE_CONFIG, SOURCE_DEFAULT}`
 * - 错误码：`error_code::ERP_FX_ACCOUNT_INVALID = 66083`（M1-05 的缺牌价是 66082，别混）
 * - 权限码：`business:erp:fx-settings:update`（V0411 种的菜单按钮行）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  FX_ACCOUNT_INVALID_CODE,
  PERM_FX_SETTINGS_READ,
  PERM_FX_SETTINGS_UPDATE,
  fxAccountLabel,
  fxCandidateOptions,
  fxErrorText,
  fxSaveBlockedReason,
  fxSettingsHint,
  fxSourceLabel,
  type FxSettingsView,
} from '@/views/business/erp/exchange-rate/fx-settings-logic';

/** 一份「配置正常」的后端响应 */
function settingsOf(overrides: Partial<FxSettingsView> = {}): FxSettingsView {
  return {
    config_key: 'erp.fx.gain_loss_account',
    account_code: '5603',
    account_name: '财务费用',
    account_id: '20000000-0000-0000-0000-000000000001',
    source: 'CONFIG',
    default_code: '5603',
    problem: null,
    candidates: [
      { code: '5601', name: '销售费用' },
      { code: '5603', name: '财务费用' },
    ],
    ...overrides,
  };
}

test('权限码与 V0411 逐字一致（读复用汇率列表码，写是独立新码）', () => {
  assert.equal(PERM_FX_SETTINGS_UPDATE, 'business:erp:fx-settings:update');
  assert.equal(PERM_FX_SETTINGS_READ, 'business:erp:fx-rate:list');
  // 写码不能与汇率编辑码混用：录牌价 ≠ 改账务口径
  assert.notEqual(PERM_FX_SETTINGS_UPDATE, 'business:erp:fx-rate:update');
});

test('错误码钉住 66083（66082 是期末缺牌价，语义不同不能用同一条提示）', () => {
  assert.equal(FX_ACCOUNT_INVALID_CODE, 66083);
});

test('当前科目展示：有名称显示「编码 名称」，不可用只显示编码 + 不可用', () => {
  assert.equal(fxAccountLabel(settingsOf()), '5603 财务费用');
  assert.equal(
    fxAccountLabel(settingsOf({ account_code: 'M106.99', account_name: null, account_id: null })),
    'M106.99（不可用）',
  );
});

test('来源文案区分「配过的」与「走缺省的」', () => {
  assert.equal(fxSourceLabel(settingsOf({ source: 'CONFIG' })), '系统参数（已配置）');
  assert.equal(fxSourceLabel(settingsOf({ source: 'DEFAULT' })), '缺省值 5603');
});

test('影响面文案要说清两条链路与「只影响之后的凭证」', () => {
  const hint = fxSettingsHint(settingsOf());
  assert.ok(hint.includes('5603 财务费用'), `要点名科目：${hint}`);
  assert.ok(hint.includes('M1-05'), `要点名期末调汇：${hint}`);
  assert.ok(hint.includes('SO-41'), `要点名应收重估：${hint}`);
  assert.ok(hint.includes('之后'), `要说清不影响历史凭证：${hint}`);
});

test('下拉选项 = 后端候选（编码 + 名称，顺序原样）', () => {
  assert.deepEqual(fxCandidateOptions(settingsOf()), [
    { label: '5601 销售费用', value: '5601' },
    { label: '5603 财务费用', value: '5603' },
  ]);
});

test('保存闸：没权限 / 没选 / 没变化 / 选了候选之外的都拦住', () => {
  const settings = settingsOf();
  assert.equal(fxSaveBlockedReason('5601', settings, true), null, '有权限 + 有效变更 ⇒ 可保存');

  assert.match(
    String(fxSaveBlockedReason('5601', settings, false)),
    /fx-settings:update/,
    '没权限要报出具体码（页面提示可复制去申请）',
  );
  assert.notEqual(fxSaveBlockedReason('', settings, true), null, '没选值不能保存');
  assert.match(String(fxSaveBlockedReason('5603', settings, true)), /相同/, '值没变不该写库');
  assert.match(
    String(fxSaveBlockedReason('1002', settings, true)),
    /损益类/,
    '候选之外（资产类）不能提交',
  );
  assert.notEqual(fxSaveBlockedReason('5601', null, true), null, '配置还没加载出来不能保存');
});

test('保存闸：配置本身坏了也允许改（换一个有效科目是修复手段）', () => {
  const broken = settingsOf({
    account_code: 'M106.99',
    account_name: null,
    account_id: null,
    problem: '科目 M106.99 不存在或已删除',
  });
  assert.equal(
    fxSaveBlockedReason('5603', broken, true),
    null,
    '坏配置下选一个有效科目必须能保存（否则用户没法自救）',
  );
});

test('错误文案：66083 直接给后端报文，其它码也回落到后端 msg，没有 msg 才用兜底', () => {
  const backend = '科目 1002 库存现金 是 ASSET 类科目：汇兑损益属于当期损益';
  assert.equal(fxErrorText(FX_ACCOUNT_INVALID_CODE, backend), backend);
  assert.equal(fxErrorText(10001, '科目编码不能为空'), '科目编码不能为空');
  assert.match(String(fxErrorText(undefined, null)), /保存失败/);
  // 没有报文（网络失败）时不能把 undefined 拼进提示
  assert.ok(!String(fxErrorText(undefined, null)).includes('undefined'));
});

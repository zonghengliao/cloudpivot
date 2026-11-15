/**
 * 汇兑损益科目配置的**纯逻辑**（M1-06 · 迁移 `V0411__fx_gain_loss_account`）。
 *
 * 为什么单独一个 `.ts`：`exchange-rate/index.vue` 已经有 800 多行，「当前配置怎么显示」
 * 「能不能点保存」「66083 报什么」这些判据如果写在模板里，`vue-tsc` 抓不到漂移
 * （与 `period/period-logic.ts` 同一套做法）。这里全是无副作用的纯函数，可单测。
 *
 * 口径与后端逐字对齐：
 * - 参数键：`fx_settings::CONFIG_FX_GAIN_LOSS_ACCOUNT` = `erp.fx.gain_loss_account`
 * - 缺省值：`fx_settings::DEFAULT_FX_GAIN_LOSS_ACCOUNT` = `5603`
 * - 值来源：`fx_settings_handler::{SOURCE_CONFIG, SOURCE_DEFAULT}` = `CONFIG` / `DEFAULT`
 * - 错误码：`error_code::ERP_FX_ACCOUNT_INVALID` = `66083`
 * - 权限码：`fx_settings_handler::PERM_FX_SETTINGS_UPDATE`
 */

/** 写权限码（与 `V0411` 的 `sys_menu.auth_code` 逐字一致） */
export const PERM_FX_SETTINGS_UPDATE = 'business:erp:fx-settings:update';

/** 读接口权限码（后端复用汇率列表码，页面据此判断要不要读配置） */
export const PERM_FX_SETTINGS_READ = 'business:erp:fx-rate:list';

/** 「汇兑损益科目配置不可用」（后端 `66083`） */
export const FX_ACCOUNT_INVALID_CODE = 66083;

/** 值来源：参数里配的 / 参数缺失走缺省 */
export type FxSettingsSource = 'CONFIG' | 'DEFAULT';

/** 可选科目 */
export interface FxAccountCandidate {
  code: string;
  name: string;
}

/** 后端 `FxSettingsRes` 的前端视图（字段名逐字一致，别改） */
export interface FxSettingsView {
  config_key: string;
  account_code: string;
  account_name: string | null;
  account_id: string | null;
  source: FxSettingsSource;
  default_code: string;
  problem: string | null;
  candidates: FxAccountCandidate[];
}

/** 「5603 财务费用」/ 科目不可用时只有编码「M106.99（不可用）」 */
export function fxAccountLabel(settings: FxSettingsView): string {
  const name = settings.account_name;
  if (!name) {
    return `${settings.account_code}（不可用）`;
  }
  return `${settings.account_code} ${name}`;
}

/** 「系统参数」/「缺省值 5603」—— 用户要能一眼看出「这条是配过的还是默认的」 */
export function fxSourceLabel(settings: FxSettingsView): string {
  return settings.source === 'CONFIG' ? '系统参数（已配置）' : `缺省值 ${settings.default_code}`;
}

/**
 * 这个科目的影响面（给页面做 help 文案）。
 *
 * 两处都读它（M1-05 期末调汇 / SO-41 应收重估），所以改它同时影响两张凭证。
 */
export function fxSettingsHint(settings: FxSettingsView): string {
  const label = fxAccountLabel(settings);
  return (
    `当前汇兑差额记在「${label}」。` +
    '期末调汇（M1-05）与期末应收重估（SO-41）都写这个科目，' +
    '改它只影响**之后生成**的凭证，已生成的凭证不变（要改历史账只能红冲重做）。'
  );
}

/** 下拉选项：编码 + 名称（按后端已排序的编码升序原样输出） */
export function fxCandidateOptions(settings: FxSettingsView): { label: string; value: string }[] {
  return settings.candidates.map((row) => ({ label: `${row.code} ${row.name}`, value: row.code }));
}

/**
 * 保存按钮能不能点（返回 `null` = 可以，返回字符串 = 不能的理由）。
 *
 * 五种拦截：没权限 / 没选值 / 没变化 / 当前配置本身就坏（要求先看提示再改，避免
 * 「配置坏了但用户以为点保存是修它」——保存的是**新选择**，不是修坏值）。
 */
export function fxSaveBlockedReason(
  selected: string,
  settings: FxSettingsView | null,
  canUpdate: boolean,
): string | null {
  if (!canUpdate) {
    return '当前账号没有 business:erp:fx-settings:update 权限';
  }
  if (!settings) {
    return '配置还没加载出来（先刷新）';
  }
  if (!selected) {
    return '请选择一个损益类科目';
  }
  if (selected === settings.account_code) {
    return '与当前配置相同，无需保存';
  }
  if (!settings.candidates.some((row) => row.code === selected)) {
    return '只能选启用中的末级损益类科目（且未标记外币核算）';
  }
  return null;
}

/** 错误 → 人话（66083 直接给后端报文，其它走 messageOf 的同口径） */
export function fxErrorText(code: number | undefined, message: string | null | undefined): string {
  if (code === FX_ACCOUNT_INVALID_CODE && message) {
    return message;
  }
  if (message) {
    return message;
  }
  return '保存失败（后端未返回原因，请刷新后重试）';
}

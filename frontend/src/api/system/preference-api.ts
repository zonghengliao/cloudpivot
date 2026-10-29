/**
 * 用户界面偏好接口（已落库并接入，后端 `V0090` + `V0091`）
 *
 * # 为什么需要它
 *
 * 第一版配色 / 明暗 / 侧边栏只存 `localStorage`（键 `cloudpivot-palette` / `cloudpivot-setting`），
 * 清缓存或换浏览器就回到默认。现在这三项（以及表格密度 / 分页条数 / 界面语言 / 两个通知开关）
 * 全部按 `user_id + pref_key` 存进 `sys_user_preference`，同一账号在任何浏览器都是同一套外观。
 *
 * # 加载优先级
 *
 * **后端 > localStorage > 默认**
 *
 * 1. 启动时先用 localStorage 立即渲染（不闪一帧默认色，见 `composables/usePalette.ts`）；
 * 2. 登录成功后拉后端偏好校正本地（`syncPreferenceFromServer`）；
 * 3. 后端不可用 / 未登录 / 401 → 完全回退 localStorage，界面照常可用（fail-safe）。
 *
 * # 归属
 *
 * 接口以令牌 `sub` 为准，**不接受前端传 user_id**（请求体里带 `user_id` 会被后端忽略），
 * 所以这里也不需要、不允许传用户标识。
 *
 * # 两代形态
 *
 * 后端同时接受旧形态（`{palette, theme_mode, side_compact}`）与新形态（`{items: {...}}`），
 * 前端本轮继续用旧字段做「三项核心偏好」的写入（改动面最小），新增偏好一律走 `items`。
 */
import { get, put, TOKEN_KEY } from '@/api/request';
import type { ColorMode, PaletteId } from '@/style/palettes';

/** 偏好值（对应后端 `pref_value` 的 jsonb：string / number / boolean / 对象 / 数组） */
export type PreferenceItemValue =
  | string
  | number
  | boolean
  | null
  | PreferenceItemValue[]
  | { [key: string]: PreferenceItemValue };

/**
 * 用户偏好（后端 GET / PUT 的响应体）。
 *
 * `items` 是**全量** KV：字典里每个启用键都在，用户没设置过的取字典默认值，
 * 因此前端不需要自己维护一份默认值表。
 */
export interface UserPreference {
  /** 兼容字段：配色方案 id（= items['ui.palette']） */
  palette: PaletteId;
  /** 兼容字段：明暗模式（= items['ui.theme_mode']） */
  theme_mode: ColorMode;
  /** 兼容字段：侧边栏是否折叠（= items['ui.side_compact']） */
  side_compact: boolean;
  /** 全量 KV 偏好 */
  items: Record<string, PreferenceItemValue>;
}

/** 保存偏好的局部更新体（只传要改的字段；两代形态都可，见文件头） */
export interface PreferencePatch {
  palette?: PaletteId;
  theme_mode?: ColorMode;
  side_compact?: boolean;
  items?: Record<string, PreferenceItemValue>;
}

/** 读取当前登录用户的偏好；没有记录时后端返回默认值（不会 404） */
export function fetchPreference(): Promise<UserPreference> {
  return get<UserPreference>('/api/system/auth/preference');
}

/** 保存当前登录用户的偏好（局部更新 + upsert），返回更新后的全量偏好 */
export function savePreference(data: PreferencePatch): Promise<UserPreference> {
  return put<UserPreference>('/api/system/auth/preference', data);
}

/** 是否处于登录态（有访问令牌）：没令牌既不该打接口，也无从同步到某个账号 */
export function isPreferenceSyncEnabled(): boolean {
  return Boolean(localStorage.getItem(TOKEN_KEY));
}

/**
 * 静默保存：未登录直接跳过；任何失败只 `console.warn`，**绝不抛出**。
 *
 * 界面的即时生效依赖 localStorage（此时已经写完了），后端失败不能阻塞用户操作 ——
 * 「后端挂了就点不动配色」比「暂时不同步」是更严重的回归（验收第 4 条）。
 */
export function savePreferenceQuietly(patch: PreferencePatch): void {
  if (!isPreferenceSyncEnabled()) {
    return;
  }
  void savePreference(patch).catch((error: unknown) => {
    console.warn('[preference] 偏好同步到后端失败（本次已保存到本地）：', error);
  });
}

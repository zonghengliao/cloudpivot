/**
 * 通用偏好 store（KV）：把后端 `items` 暴露成「有类型、带默认值」的读接口。
 *
 * # 它解决什么问题
 *
 * `ui.palette` / `ui.theme_mode` / `ui.side_compact` 三项由 `usePalette.ts` 与 `setting.ts`
 * 直接消费（它们要驱动 DOM 变量与布局）。其余偏好（表格密度 / 分页条数 / 界面语言 /
 * 两个通知开关）本轮只要求**能读能存**，等业务页面逐个接（那是后续增量）。
 * 这个 store 就是它们的统一入口——新增一个偏好只要：后端字典插一行 + 这里加一个 `PREF_KEYS` 常量。
 *
 * # 默认值从哪来
 *
 * **不在这里写死默认值**：后端 GET 会返回合并了字典默认值的全量 `items`，
 * 所以 `readXxx(key, fallback)` 的 `fallback` 只在「后端还没同步」（首屏、未登录、接口失败）
 * 时起作用 —— 那时给一个与字典一致的兜底值即可。
 */
import { computed, ref } from 'vue';
import { defineStore } from 'pinia';
import {
  fetchPreference,
  isPreferenceSyncEnabled,
  savePreferenceQuietly,
  type PreferenceItemValue,
  type UserPreference,
} from '@/api/system/preference-api';

/**
 * 已接入的偏好键（与后端 `sys_preference_def.pref_key` 逐字一致）。
 *
 * ⚠️ 改这里必须同时改 `backend/migrations/system/V0091__seed_preference_def.up.sql`，
 * 否则后端会以 `62020 未知的偏好项` 拒绝写入。
 */
export const PREF_KEYS = {
  /** 配色方案 */
  palette: 'ui.palette',
  /** 明暗模式 */
  themeMode: 'ui.theme_mode',
  /** 侧边栏折叠 */
  sideCompact: 'ui.side_compact',
  /** 表格密度 */
  tableDensity: 'ui.table_density',
  /** 默认分页条数 */
  pageSize: 'ui.page_size',
  /** 界面语言 */
  language: 'ui.language',
  /** 站内信开关 */
  notifyInbox: 'notify.inbox',
  /** 邮件通知开关 */
  notifyEmail: 'notify.email',
} as const;

/** 通用偏好 store：全量 KV 的内存镜像 + 静默落库 */
export const usePreferenceStore = defineStore('preference', () => {
  /** 全量 KV（后端 GET 的 `items`；未同步时为空对象） */
  const items = ref<Record<string, PreferenceItemValue>>({});
  /** 是否已经从后端同步过（首屏为 false；用于「后端 > localStorage」的判断） */
  const synced = ref(false);

  /** 读字符串偏好（类型不符或缺失时回退 `fallback`） */
  function readString(key: string, fallback: string): string {
    const value = items.value[key];
    return typeof value === 'string' ? value : fallback;
  }

  /** 读数字偏好 */
  function readNumber(key: string, fallback: number): number {
    const value = items.value[key];
    return typeof value === 'number' ? value : fallback;
  }

  /** 读布尔偏好 */
  function readBoolean(key: string, fallback: boolean): boolean {
    const value = items.value[key];
    return typeof value === 'boolean' ? value : fallback;
  }

  /** 后端响应落进内存（只读路径：不触发回写，避免读→写回环） */
  function apply(remote: UserPreference): void {
    items.value = { ...(remote.items ?? {}) };
    synced.value = true;
  }

  /**
   * 保存一组偏好：内存立即生效 + 异步 PUT（登录态才发，失败只 warn）。
   *
   * 只发 `items` 形态 —— 新增偏好键不需要改接口。
   */
  function save(patch: Record<string, PreferenceItemValue>): void {
    items.value = { ...items.value, ...patch };
    savePreferenceQuietly({ items: patch });
  }

  /** 主动拉一次后端偏好（未登录 / 失败返回 false，由调用方决定是否回退本地） */
  async function load(): Promise<boolean> {
    if (!isPreferenceSyncEnabled()) {
      return false;
    }
    try {
      apply(await fetchPreference());
      return true;
    } catch (error) {
      console.warn('[preference] 拉取后端偏好失败（继续使用本地/默认值）：', error);
      return false;
    }
  }

  /** 表格密度（`default` / `compact`） */
  const tableDensity = computed<string>(() => readString(PREF_KEYS.tableDensity, 'default'));
  /** 默认分页条数 */
  const pageSize = computed<number>(() => readNumber(PREF_KEYS.pageSize, 10));
  /** 界面语言 */
  const language = computed<string>(() => readString(PREF_KEYS.language, 'zh-CN'));
  /** 站内信开关 */
  const notifyInbox = computed<boolean>(() => readBoolean(PREF_KEYS.notifyInbox, true));
  /** 邮件通知开关 */
  const notifyEmail = computed<boolean>(() => readBoolean(PREF_KEYS.notifyEmail, false));

  return {
    items,
    synced,
    tableDensity,
    pageSize,
    language,
    notifyInbox,
    notifyEmail,
    readString,
    readNumber,
    readBoolean,
    apply,
    save,
    load,
  };
});

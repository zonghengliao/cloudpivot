import { ref } from 'vue';
import { defineStore } from 'pinia';
import { savePreferenceQuietly } from '@/api/system/preference-api';

/** 主题模式 */
type ThemeMode = 'light' | 'dark';

/**
 * 本地持久化键。
 *
 * 它只是**本地镜像**（首屏先用它渲染，避免闪一帧默认色）；登录用户的权威值在后端
 * `sys_user_preference`（`V0090`/`V0091`），由 `composables/usePalette.ts` 的
 * `syncPreferenceFromServer()` 在登录后校正。
 */
const SETTING_KEY = 'cloudpivot-setting';

/** 从 localStorage 恢复上次保存的偏好（损坏时静默回退默认值） */
function loadSettings(): { themeMode: ThemeMode; sideCompact: boolean } {
  try {
    const saved = localStorage.getItem(SETTING_KEY);
    if (saved) {
      const parsed = JSON.parse(saved) as Partial<{ themeMode: ThemeMode; sideCompact: boolean }>;
      return {
        themeMode: parsed.themeMode === 'dark' ? 'dark' : 'light',
        sideCompact: parsed.sideCompact === true,
      };
    }
  } catch {
    // 解析失败按默认值处理
  }
  return { themeMode: 'light', sideCompact: false };
}

/** 界面偏好：主题模式与侧边栏折叠状态（两个布局共用） */
export const useSettingStore = defineStore('setting', () => {
  const defaults = loadSettings();

  /** 主题模式：light / dark（TDesign 通过 html[theme-mode] 生效） */
  const themeMode = ref<ThemeMode>(defaults.themeMode);
  /** 侧边栏折叠状态（管理布局） */
  const sideCompact = ref(defaults.sideCompact);

  /** 把当前偏好写回 localStorage（隐私模式 / 配额满时静默忽略） */
  function saveSettings(): void {
    try {
      localStorage.setItem(
        SETTING_KEY,
        JSON.stringify({ themeMode: themeMode.value, sideCompact: sideCompact.value }),
      );
    } catch {
      // 忽略：本次会话内仍然生效
    }
  }

  /** 把当前主题写到 html 根元素上，全局生效 */
  function applyTheme(): void {
    document.documentElement.setAttribute('theme-mode', themeMode.value);
  }

  /** 切换明暗主题并持久化 */
  function toggleTheme(): void {
    setThemeMode(themeMode.value === 'dark' ? 'light' : 'dark');
  }

  /**
   * 设置明暗主题并持久化。
   *
   * 外观设置页的「浅色/深色」是单选而不是 toggle，需要显式指定目标值；
   * 与 toggleTheme 共用同一条落盘逻辑，避免两处各写一份。
   *
   * 落盘顺序：先本地（即时生效）→ 再异步同步后端（登录态才发；失败只 warn）。
   */
  function setThemeMode(mode: ThemeMode): void {
    themeMode.value = mode === 'dark' ? 'dark' : 'light';
    applyTheme();
    saveSettings();
    savePreferenceQuietly({ theme_mode: themeMode.value });
  }

  /** 切换侧边栏折叠并持久化（同样「本地即时 + 异步落库」） */
  function toggleCompact(): void {
    sideCompact.value = !sideCompact.value;
    saveSettings();
    savePreferenceQuietly({ side_compact: sideCompact.value });
  }

  /** 显式指定侧边栏折叠状态（布局的折叠按钮用它，避免各写一份取反逻辑） */
  function setSideCompact(compact: boolean): void {
    if (sideCompact.value === (compact === true)) {
      return;
    }
    toggleCompact();
  }

  /**
   * 应用后端下发的明暗模式：**不回写后端**。
   *
   * 用于「登录后校正」路径 —— 若这里也调 `savePreferenceQuietly`，
   * 读取 → 应用 → 再写会形成无意义的同步回环（还会覆盖别的设备刚改的值）。
   */
  function applyRemoteThemeMode(mode: ThemeMode): void {
    themeMode.value = mode === 'dark' ? 'dark' : 'light';
    applyTheme();
    saveSettings();
  }

  /** 应用后端下发的侧边栏折叠：同样不回写后端 */
  function applyRemoteSideCompact(compact: boolean): void {
    sideCompact.value = compact === true;
    saveSettings();
  }

  /** 应用启动时调用一次，恢复上次保存的主题 */
  function initTheme(): void {
    applyTheme();
  }

  return {
    themeMode,
    sideCompact,
    toggleTheme,
    setThemeMode,
    toggleCompact,
    setSideCompact,
    applyRemoteThemeMode,
    applyRemoteSideCompact,
    initTheme,
  };
});

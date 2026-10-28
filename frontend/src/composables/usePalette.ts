/**
 * 配色方案的应用 / 切换逻辑
 *
 * 职责边界：
 * - `style/palettes.ts` 只做纯计算（色板 → 变量表 + 对比度检查），不碰 DOM、不碰 Vue；
 * - 本文件负责「把变量表落地到 `<html>` 内联 style」+「持久化」+「与明暗模式联动」。
 *
 * 为什么写内联 style 而不是注入 `<style>`：
 *   TDesign 把品牌色写在 `:root[theme-mode='light'|'dark']` 里（特异性 0,1,1），
 *   `theme.less` 靠"同特异性 + 后加载"取胜。注入的 `<style>` 同样是 0,1,1，
 *   一旦有第三方样式晚于我们加载就会被压回去。内联 style 在层叠中优先级最高，
 *   任何选择器都盖不掉，所以换肤结果稳定、且与加载顺序无关。
 *
 * 与明暗模式的关系（需求里的"6 × 2 = 12 组"）：
 *   明暗模式仍由 `stores/setting.ts` 的 `themeMode` 单一控制（顶栏按钮、设置页、快捷面板
 *   都走同一个 setter），本文件只订阅它并在变化时**重算整套变量**。
 *   因此切换配色不会丢明暗状态，切换明暗也不会丢配色 —— 两者是正交的两个维度。
 */
import { computed, readonly, ref, watch } from 'vue';
import { useSettingStore } from '@/stores/setting';
import {
  DEFAULT_PALETTE_ID,
  PALETTES,
  buildPalette,
  getPalette,
  isPaletteId,
  type ColorMode,
  type PaletteBuild,
  type PaletteId,
} from '@/style/palettes';

/** 持久化键（与现有 `cloudpivot-setting` 命名保持一致） */
export const PALETTE_STORAGE_KEY = 'cloudpivot-palette';

/** localStorage 里存的结构 */
interface StoredPalette {
  /** 方案 id */
  palette: PaletteId;
  /** 明暗模式（仅作镜像记录，运行时仍以 setting store 为准） */
  mode: ColorMode;
  /** 最近更新时间（便于排查"用户到底选了啥"） */
  updatedAt: string;
}

/** 读取已保存的方案；任何异常/脏数据一律回退默认「雾蓝流金」 */
function readStoredPalette(): PaletteId {
  try {
    const raw = localStorage.getItem(PALETTE_STORAGE_KEY);
    if (!raw) return DEFAULT_PALETTE_ID;
    const parsed: unknown = JSON.parse(raw);
    // 兼容早期可能直接存字符串的形式
    if (typeof parsed === 'string') {
      return isPaletteId(parsed) ? parsed : DEFAULT_PALETTE_ID;
    }
    const candidate = (parsed as Partial<StoredPalette> | null)?.palette;
    return isPaletteId(candidate) ? candidate : DEFAULT_PALETTE_ID;
  } catch {
    return DEFAULT_PALETTE_ID;
  }
}

/** 当前选中的方案（模块级单例：全应用共享同一份状态） */
const paletteId = ref<PaletteId>(readStoredPalette());

/** 最近一次构建结果（设置页的对比度表格直接读它） */
const lastBuild = ref<PaletteBuild>(buildPalette(paletteId.value, 'light'));

/** 变量版本号：每次真正写入 CSS 变量自增，ECharts 等需要命令式刷新的地方据此重绘 */
const paletteVersion = ref(0);

/** 已应用到 DOM 的组合（避免同一次切换重复写上百条变量） */
let appliedKey = '';

/** 明暗模式订阅是否已建立（避免多次 usePalette 重复 watch） */
let watching = false;

/** 取 setting store（保证在 pinia 安装之后调用） */
function setting() {
  return useSettingStore();
}

/** 当前明暗模式：唯一来源是 setting store */
function currentMode(): ColorMode {
  return setting().themeMode;
}

/**
 * 把当前「方案 × 模式」写成 `<html>` 上的 CSS 变量。
 *
 * @param force 为 true 时即便组合未变也重写（设置页"重新应用"用）
 * @returns 本次构建结果（含对比度检查与告警）
 */
export function applyPalette(force = false): PaletteBuild {
  const mode = currentMode();
  const key = `${paletteId.value}:${mode}`;
  const build = buildPalette(paletteId.value, mode);
  lastBuild.value = build;

  if (!force && key === appliedKey) {
    return build;
  }

  const root = document.documentElement;
  for (const [name, value] of Object.entries(build.vars)) {
    root.style.setProperty(name, value);
  }
  // 附带两个状态属性：CSS 可据此做特例，自动化测试也靠它断言当前组合
  root.setAttribute('data-palette', paletteId.value);
  root.setAttribute('data-palette-mode', mode);
  appliedKey = key;
  paletteVersion.value += 1;

  // 自动修正/色值推断要在控制台留痕，避免"静默输出不合规配色"
  if (build.warnings.length) {
    console.warn(`[palette] ${getPalette(paletteId.value).name}（${mode}）已自动修正：`, build.warnings.join('；'));
  }
  return build;
}

/** 写入 localStorage（模式做镜像记录，SSR/隐私模式失败不影响主流程） */
function persist(): void {
  const payload: StoredPalette = {
    palette: paletteId.value,
    mode: currentMode(),
    updatedAt: new Date().toISOString(),
  };
  try {
    localStorage.setItem(PALETTE_STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // 隐私模式 / 配额满：忽略，本次会话内仍然生效
  }
  // P1（可选）：登录用户同步到后端用户偏好，见 `src/api/system/preference-api.ts`，
  // 后端接口未上线前保持空实现，不阻塞第一版本地持久化。
}

/** 切换方案：立即生效（无刷新、无需保存按钮）并持久化 */
export function setPalette(id: PaletteId): void {
  const next = isPaletteId(id) ? id : DEFAULT_PALETTE_ID;
  if (next === paletteId.value) {
    applyPalette(true);
    return;
  }
  paletteId.value = next;
  applyPalette();
  persist();
}

/** 切换明暗模式：交给 setting store（它负责写 `theme-mode` 属性与 `cloudpivot-setting`） */
export function setPaletteMode(mode: ColorMode): void {
  const store = setting();
  if (store.themeMode !== mode) {
    store.setThemeMode(mode);
  }
  // store 的 watch 会触发重算；这里显式再调一次，覆盖"模式未变但要刷新"的场景
  applyPalette(true);
  persist();
}

/** 回到默认方案 */
export function resetPalette(): void {
  setPalette(DEFAULT_PALETTE_ID);
}

/**
 * 应用启动时调用一次（main.ts 在 mount 之前）。
 *
 * 顺序很重要：先让 setting store 把 `theme-mode` 写到 `<html>`，再应用配色变量，
 * 这样首屏渲染直接用最终配色，不会先闪一帧默认蓝再跳色。
 */
export function initPalette(): void {
  if (!watching) {
    // 明暗模式变化 → 整套变量要重算（深色模式的底色/文字/品牌色都是另一套推导结果）
    watch(() => setting().themeMode, () => applyPalette(), { flush: 'sync' });
    watching = true;
  }
  applyPalette(true);
}

/**
 * 组合式入口。
 *
 * 在组件里用：
 * ```ts
 * const { paletteId, palettes, setPalette, mode, setPaletteMode, build } = usePalette();
 * ```
 */
export function usePalette() {
  if (!watching) {
    // 组件首次调用时兜底建立订阅（正常情况下 main.ts 已经初始化过）
    initPalette();
  }

  /** 当前方案定义 */
  const currentPalette = computed(() => getPalette(paletteId.value));

  /** 当前「方案 × 模式」的完整构建结果（变量表 + 对比度检查） */
  const build = computed<PaletteBuild>(() => buildPalette(paletteId.value, currentMode()));

  /** 各方案在当前模式下的构建结果（设置页色卡上的"是否需压暗版"标记用） */
  const buildsByPalette = computed<Record<string, PaletteBuild>>(() => {
    const map: Record<string, PaletteBuild> = {};
    for (const palette of PALETTES) {
      map[palette.id] = buildPalette(palette.id, currentMode());
    }
    return map;
  });

  return {
    /** 全部方案定义 */
    palettes: PALETTES,
    /** 当前方案 id（只读，切换请用 setPalette，避免绕过持久化） */
    paletteId: readonly(paletteId),
    /** 当前方案定义 */
    currentPalette,
    /** 当前明暗模式 */
    mode: computed<ColorMode>(() => currentMode()),
    /** 当前组合的构建结果 */
    build,
    /** 所有方案（当前模式）的构建结果 */
    buildsByPalette,
    /** 变量版本号（图表重绘依赖） */
    version: readonly(paletteVersion),
    /** 最近一次应用的构建结果 */
    applied: readonly(lastBuild),
    /** 切换方案 */
    setPalette,
    /** 切换明暗 */
    setMode: setPaletteMode,
    /** 恢复默认 */
    resetPalette,
    /** 强制重算并重写变量 */
    reapply: () => applyPalette(true),
  };
}

/** 图表配色（ECharts 的 `color` 数组）：随方案与模式变化 */
export function paletteChartColors(): string[] {
  const { vars } = buildPalette(paletteId.value, currentMode());
  return Array.from({ length: 10 }, (_, index) => vars[`--td-chart-color-${index + 1}`]);
}

/**
 * 成本核算模块开关（ARCH-9 §7.4 灰度 · COST-B4-4）
 *
 * 一个进程级（前端会话级）的布尔量 + 一次请求缓存，供「菜单之外」的散点入口判断：
 * 出库单表单的「成本对象」字段、成本模块页面的占位提示。
 *
 * # 与菜单的关系（不要重复做）
 *
 * 6 个成本页面菜单、20 个按钮权限点由**后端**在 `/system/auth/menus` 与
 * `/system/auth/me` 里按开关过滤掉了：侧边栏自然不显示，路由守卫也会因为
 * `meta.permission` 不在权限列表里而把直接输 URL 的人送去 `/result/403`。
 * 这里只负责后端无法替前端做的那部分 —— 同一个页面里的一个字段要不要渲染。
 *
 * # 缓存口径
 *
 * `load()` 成功后记住结果；`ensure()` 只在没成功过的时候请求一次。
 * 失败**不置 `loaded`**（下次进页面重试），且 `enabled` 保持 `false`（fail-safe：
 * 请求挂掉时宁可不显示成本字段，也不要在那里挂一个必然 403 的下拉）。
 * 管理员切换开关后不需要刷新前端代码：用户下次进入页面 `ensure()` 会重新取 ——
 * 若要做「同一会话内即时跟随」，调用方显式 `load(true)`。
 */

import { defineStore } from 'pinia';
import { ref } from 'vue';

import { fetchCostSwitch } from '@/api/business/erp/cost-switch';

export const useCostSwitchStore = defineStore('cost-switch', () => {
  /** 成本核算模块是否启用（请求失败 / 未加载时为 false） */
  const enabled = ref(false);
  /** 是否已经成功取过一次（决定 `ensure()` 要不要再请求） */
  const loaded = ref(false);
  /** 后端下发的提示语（关闭时点名参数键与开启路径） */
  const hint = ref('');
  /** 参数键（后端下发，前端不硬编码） */
  const configKey = ref('erp.cost.enabled');

  /** 进行中的请求（同一时刻只发一个，避免抽屉反复打开时打爆探针） */
  let inflight: Promise<void> | null = null;

  /**
   * 拉取开关状态。
   *
   * @param force 已加载过也强制重取（管理员刚改完开关、想在同一会话内看到效果时用）
   */
  async function load(force = false): Promise<void> {
    if (inflight) {
      return inflight;
    }
    if (loaded.value && !force) {
      return;
    }
    inflight = (async () => {
      try {
        const res = await fetchCostSwitch();
        enabled.value = res.enabled === true;
        hint.value = res.hint;
        configKey.value = res.config_key;
        loaded.value = true;
      } catch {
        // fail-safe：取不到就按关闭处理，且不置 loaded（下次进页面重试）
        enabled.value = false;
      } finally {
        inflight = null;
      }
    })();
    return inflight;
  }

  /** 没成功加载过就加载一次（页面 `onMounted` 里裸调即可） */
  async function ensure(): Promise<void> {
    if (loaded.value) {
      return;
    }
    await load();
  }

  /** 清空缓存（登出 / 切换账号时调用，避免把上一个会话的开关状态带到下一个用户） */
  function reset(): void {
    enabled.value = false;
    loaded.value = false;
    hint.value = '';
  }

  return { enabled, loaded, hint, configKey, load, ensure, reset };
});

/**
 * 成本核算模块开关 API 层（ARCH-9 §7.4 灰度 · COST-B4-4）。
 *
 * | 方法 | 路径 | 权限码 | 说明 |
 * |---|---|---|---|
 * | GET | `/api/business/erp/cost-switch` | 无（登录即可） | `{ enabled, config_key, hint }` |
 *
 * # 为什么单独一个探针接口
 *
 * 开关值存在 `sys_config.erp.cost.enabled`，而参数配置接口要 `support:config:list`
 * 权限 —— 财务 / 仓管没有。前端只需要知道「开没开」来决定渲染菜单外的零散入口
 * （出库单的「成本对象」字段、成本页的占位提示），所以后端开了一个只回布尔值的探针。
 *
 * # 关闭时它自己不会被 403
 *
 * 后端 `business::shared::cost_switch` 对该路径显式豁免（否则关掉开关就再也问不到
 * 「开关是关的」）。
 *
 * # 失败口径（fail-safe）
 *
 * 请求失败一律按 **已关闭** 处理（不渲染成本字段）。理由：这个字段一旦渲染出来就必然
 * 触发 `/stock-out/cost-object-options` 的 403，用户看到的是一串报错；不如先不显示。
 */

import { get } from '@/api/request';

/** 探针路径（后端注解不含 `/api`，漏了前缀会命中 SPA fallback 返回 HTML） */
const SWITCH_PATH = '/api/business/erp/cost-switch';

/** 开关状态（后端 `CostSwitchRes`） */
export interface CostSwitchRes {
  /** 当前是否启用成本核算模块 */
  enabled: boolean;
  /** 参数键（固定 `erp.cost.enabled`，用于提示语，避免前端硬编码） */
  config_key: string;
  /** 一句给界面用的说明（关闭时点名开启路径） */
  hint: string;
}

/** 读取成本核算模块开关（任意登录用户可调） */
export function fetchCostSwitch(): Promise<CostSwitchRes> {
  return get<CostSwitchRes>(SWITCH_PATH);
}

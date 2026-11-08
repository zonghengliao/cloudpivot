/**
 * 主数据编码策略的**前端唯一入口**（CODE-08）。
 *
 * # 一件事：编码栏能不能填
 *
 * 系统参数 `erp.master_data.allow_manual_code` 由后端
 * `GET /api/business/erp/master-data/code-policy` 下发：
 *
 * | 值 | 编码栏 | 「生成编码」按钮 | 提示 |
 * |---|---|---|---|
 * | `true`（默认） | 可填（编辑态仍只读） | 显示 | 不点生成则保存时自动生成 |
 * | `false` | **只读** | **隐藏** | 编码由系统自动生成 |
 *
 * # 为什么要缓存
 *
 * 6 个主数据表单（供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型）都要这条策略。
 * 每开一次弹窗打一次接口既慢又会在 6 个页面各写一遍 loading / 错误处理；
 * 这里把 Promise 缓存住：并发调用共用一次请求，之后全部命中内存。
 *
 * # 失败口径：**fail-open**
 *
 * 接口失败（网络抖动 / 老后端没有这个路由）时返回 `true`（= 允许手填），与后端
 * `DEFAULT_ALLOW_MANUAL_CODE` 同口径。方向是刻意的：开关关掉的**真正闸门在后端**
 * （`400 + 66054`），前端只是不让人白填；反过来（失败就锁死输入框）会让一次接口抖动
 * 卡住 6 个主数据的建单入口。
 *
 * 纯逻辑（兜底值 / 文案 / 禁用判定）在 `./code-policy-pure`，那边没有 API 依赖、可单测。
 */

import { fetchMasterDataCodePolicy } from '@/api/business/erp';

import {
  DEFAULT_ALLOW_MANUAL_CODE,
  MANUAL_CODE_FORBIDDEN_HINT,
  manualCodeDisabled,
  manualCodeHelp,
} from './code-policy-pure';

export { DEFAULT_ALLOW_MANUAL_CODE, MANUAL_CODE_FORBIDDEN_HINT, manualCodeDisabled, manualCodeHelp };

/** 缓存住的策略 Promise（`null` = 还没请求过）。 */
let policyPromise: Promise<boolean> | null = null;

/**
 * 是否允许手工指定编码（带进程内缓存）。
 *
 * @param force 忽略缓存强制重取（切换环境 / 管理端刚改完参数时用）
 * @returns `true` = 编码栏可填、生成编码按钮可见；`false` = 只读 + 隐藏按钮
 */
export function loadAllowManualCode(force = false): Promise<boolean> {
  if (force || policyPromise === null) {
    policyPromise = fetchMasterDataCodePolicy()
      .then((policy) => policy.allow_manual_code)
      .catch(() => {
        // 失败不缓存：下次进弹窗会重试；本次按兜底值渲染
        policyPromise = null;
        return DEFAULT_ALLOW_MANUAL_CODE;
      });
  }
  return policyPromise;
}

/** 清缓存（单测 / 切换环境用；页面不需要调）。 */
export function resetAllowManualCodeCache(): void {
  policyPromise = null;
}

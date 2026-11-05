/**
 * 「关联业务」存活探测的**接线层**（唯一发请求的地方）。
 *
 * 纯逻辑在 `./biz-link.ts`（映射表、提示文案、失败分类），这里只做三件事：
 *
 * 1. 按映射表给的 `probeOf()` 打**真接口**（不猜、不读本地缓存）；
 * 2. 把 `ApiError` 的 `{ status, code }` 交给 `classifyProbeFailure()` 分类；
 * 3. 把结论翻成按钮状态（`alive` / `gone` / `forbidden` / `unknown`）。
 *
 * # 为什么用 `ApiError.status`
 *
 * 业务「不存在」的码按域散落在各处（任务 `63001`、合同 `66014`、盘点/边配置 `65005` …），
 * 而且新增业务类型还会继续加码 —— 前端不该维护一张越来越长的码表。
 * HTTP 404 才是「这条记录没了」的统一口径，所以探测要看到**状态码**；
 * `ApiError` 原来只带业务码，本卡给它补了 `status`（可选字段，向后兼容）。
 */
import { ApiError, get } from '@/api/request';
import {
  classifyProbeFailure,
  type BizLinkState,
  type BizLinkTarget,
  type BizRef,
} from './biz-link';

/**
 * 探测目标业务对象是否还在。
 *
 * @param target 映射表里的目标（含 `probeOf` / `notFoundCodes`）
 * @param biz `{ id, no }`
 * @returns `alive` 才允许跳转；其余三种都是「置灰 + 提示」，不弹报错红条
 */
export async function probeBizLink(target: BizLinkTarget, biz: BizRef): Promise<BizLinkState> {
  const plan = target.probeOf(biz);
  if (!plan) {
    // 没有可打的接口（边配置取不到实体名）⇒ 不假装探测过，页面本身恒存在
    return 'alive';
  }
  try {
    const data = await get<unknown>(plan.url, plan.params);
    // 有些详情接口「不存在」时回 200 + data null（而不是 404）—— 同样算清场
    return data === null || data === undefined ? 'gone' : 'alive';
  } catch (error) {
    if (error instanceof ApiError) {
      return classifyProbeFailure({ status: error.status, code: error.code }, target.notFoundCodes);
    }
    return 'unknown';
  }
}

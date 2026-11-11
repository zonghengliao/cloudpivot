/**
 * 凭证详情「来源单据」这一格怎么显示 —— VTRACE-2。
 *
 * # 这一格为什么需要一个纯模块
 *
 * `biz_voucher` 早就有 `source_type / source_id / source_no`，VTRACE-1 也落了
 * 「类型 → 前端路由」映射表（`../shared/source-route.ts`）。但**能算出去哪**不等于
 * **该显示成什么**：
 *
 * - `MANUAL`（手工凭证）压根没有来源单据 → 必须纯文本 + 「手工凭证，无来源单据」；
 * - `COST_PERIOD`（成本核算）页面还没上线 → 也是纯文本，但理由完全不同；
 * - `by='id'` 而 `source_id` 是 `null`（历史脏数据 / 尚未回填）→ 纯文本 + 「缺定位信息」；
 * - 能跳的行，`source_no` 也可能为空（`by='id'` 的行只保证 id 在）→ 链接文字要退化成
 *   「查看<目标名>」，而不是一个只有图标的空链接。
 *
 * 四种「点不了」的理由要给出**不同的 tooltip** —— 一句统一的「不支持跳转」等于没说。
 * 判断放在这里（纯函数，`tests/unit/vtrace-voucher-source-link.test.ts` 逐条钉死），
 * `.vue` 里只读结论，模板里不出现第二个 `source_type` 分支。
 *
 * # 不在这里做的事
 *
 * - 不重算路由：目标页 / 参数名 / 深链档位一律来自 `SOURCE_ROUTE_TABLE`（VTRACE-1），
 *   本模块只做 `isSourceDocType` 反查「为什么跳不了」；
 * - 不碰 `router`：返回 `path` / `query`，跳转与「返回后列表状态」由页面负责。
 */

import { SOURCE_ROUTE_TABLE, isSourceDocType, resolveSourceRoute, type DeepLink } from '../shared/source-route';
import { dash } from '../shared/money';

/**
 * 「合法的、但没有来源单据」的唯一来源值。
 *
 * 写法与 `./voucher-logic.ts::sourceTypeLabel` 的 `case 'MANUAL'` 一致：取值串在
 * `doc-kinds.ts` 的七轴表里是 `VOUCHER_SOURCE_TYPES` 的成员，这里只借用一次做分支。
 */
const MANUAL_SOURCE_TYPE = 'MANUAL';

/** 卡面点名的另一个「不可跳」词（不是 `VoucherSourceType` 的落库值，见 source-route.ts 文件头对照表）。 */
const OTHER_SOURCE_TYPE = 'OTHER';

/** 手工凭证的固定说明（卡面逐字要求）。 */
export const MANUAL_NO_SOURCE_TIP = '手工凭证，无来源单据';

/**
 * 「来源单据」这一格的显示结论。
 *
 * `status` 是给单测 / 埋点看的原因码；页面上只用 `text` / `clickable` / `path` / `query` / `tip`。
 */
export type SourceDocLinkStatus =
  /** 可跳（`path` 一定有值） */
  | 'link'
  /** 手工凭证：合法来源，但没有对应单据 */
  | 'manual'
  /** 来源类型不认识（含 `OTHER`）：宁可纯文本，也不猜页面 */
  | 'unknown'
  /** 目标页还没上线（如 `COST_PERIOD`） */
  | 'page-missing'
  /** 类型有映射，但凭证上缺定位参数（`source_id` / `source_no` 为空） */
  | 'missing-key'
  /** 凭证上连 `source_type` 都没有 */
  | 'empty';

/** 一格的完整显示结论。 */
export interface SourceDocDisplay {
  /** 单元格文字（不可跳时可能是 `—`） */
  readonly text: string;
  /** 是否渲染成可点击链接 */
  readonly clickable: boolean;
  /** 目标页 path（仅 `clickable` 时有值） */
  readonly path: string | null;
  /** 目标页查询参数（`deepLink='none'` 时为空对象；仅 `clickable` 时有意义） */
  readonly query: Record<string, string>;
  /** 深链档位（tooltip 文案据此区分「直开详情」/「只到列表」） */
  readonly deepLink: DeepLink | null;
  /** 目标页中文名（tooltip / aria-label 用；未知时为空串） */
  readonly label: string;
  /** tooltip 文案：**必须能回答「为什么点不了 / 点过去会看到什么」** */
  readonly tip: string;
  /** 原因码（见 {@link SourceDocLinkStatus}） */
  readonly status: SourceDocLinkStatus;
}

/** 不可跳行的一格（五个字段一次填齐，避免各处漏 `clickable: false`）。 */
function deadCell(text: string, tip: string, status: SourceDocLinkStatus): SourceDocDisplay {
  return { text, clickable: false, path: null, query: {}, deepLink: null, label: '', tip, status };
}

/**
 * 能跳时的 tooltip：告诉用户**点过去会落到哪一步**。
 *
 * 三档措辞不同，是因为页面实际能力不同（`source-route.ts` 里逐页核实过）：
 * `detail` 直接开单据；`filter` 只按单号筛列表；`none` 连参数都不传（页面不读）。
 * 写成一样的「查看来源单据」，用户点进 `none` 那一档会以为跳错了。
 */
function linkTip(deepLink: DeepLink, label: string, value: string): string {
  const target = value ? `「${value}」` : '';
  switch (deepLink) {
    case 'detail':
      return `打开${label}详情${target}`;
    case 'filter':
      return `打开${label}列表并按单号${target}定位`;
    default:
      // 'none'：目标页暂不读参数 —— 只说打开列表，不承诺定位。
      return `打开${label}列表（该页暂不读取单号参数，需在列表里自行定位）`;
  }
}

/**
 * 「来源单据」这一格怎么显示（VTRACE-2 的唯一判断入口）。
 *
 * @param type `biz_voucher.source_type`（详情接口原值，大小写不敏感）
 * @param id   `biz_voucher.source_id`
 * @param no   `biz_voucher.source_no`（链接文字优先用它）
 *
 * 判断顺序刻意是「先排除、再解析」：`MANUAL` / 未知词 / 页面未上线都不该走
 * `resolveSourceRoute`（它对这些行返回 `null`，但拿不到「为什么」）。
 */
export function resolveSourceDocDisplay(
  type: string | null | undefined,
  id?: string | null,
  no?: string | null,
): SourceDocDisplay {
  const shown = dash(no?.trim() ?? null);
  const key = (type ?? '').trim().toUpperCase();

  if (!key) {
    return deadCell(shown, '无来源单据', 'empty');
  }
  if (key === MANUAL_SOURCE_TYPE) {
    return deadCell(shown, MANUAL_NO_SOURCE_TIP, 'manual');
  }
  if (!isSourceDocType(key)) {
    const tip =
      key === OTHER_SOURCE_TYPE
        ? '来源类型未识别（OTHER），暂不支持跳转'
        : `来源类型「${key}」未识别，暂不支持跳转`;
    return deadCell(shown, tip, 'unknown');
  }

  const spec = SOURCE_ROUTE_TABLE[key];
  if (spec.deepLink === 'page-missing') {
    // 页面/路由都还没有：跳过去就是 404，宁可纯文本。
    return deadCell(shown, `「${spec.label}」页面还没上线，暂不能跳转`, 'page-missing');
  }

  const route = resolveSourceRoute(key, id, no);
  if (!route) {
    // 到这里只剩一种可能：该映射要的参数凭证上没有（`by='id'` 缺 id / `by='no'` 缺号）。
    const need = spec.by === 'id' ? '单据主键' : '单据号';
    return deadCell(shown, `本张凭证缺少「${spec.label}」的${need}，暂不能跳转`, 'missing-key');
  }

  const paramValue = Object.values(route.query)[0] ?? '';
  // tooltip 里给人看的是**单号**（`IN-20261101-0001`），不是 uuid；只有没有单号时才退到
  // 解析出来的参数值（`YEAR_END` → `2026`）。uuid 给用户看没有意义。
  const value = shown === '—' ? paramValue : shown;
  return {
    // `by='id'` 的行不保证有单号：没有单号就用「查看<目标名>」当链接文字，
    // 不留一个只有箭头、看不出点什么的空链接。
    text: shown === '—' ? `查看${route.label}` : shown,
    clickable: true,
    path: route.path,
    query: route.query,
    deepLink: route.deepLink,
    label: route.label,
    tip: linkTip(route.deepLink, route.label, value),
    status: 'link',
  };
}

/**
 * 审批详情页「关联业务」按钮的**唯一真相**：`biz_type` → 业务详情页路由 + 存活探测方案。
 *
 * # 为什么单独一个模块（卡 t_3e59cb01）
 *
 * 以前按钮写死在 `detail.vue` 模板里：
 *
 * ```vue
 * @click="router.push(`/task?keyword=${detail.instance.biz_no}`)"
 * ```
 *
 * 不管 `biz_type` 是什么都跳**任务列表**，于是有两类错：
 *
 * 1. 业务对象被软删（生产实测 38 条 `TASK_ACCEPT` + 5 条 `CONTRACT_APPROVE`）⇒
 *    任务列表按 keyword 搜不到 ⇒ 一屏空列表，用户以为数据丢了；
 * 2. `CONTRACT_APPROVE` / `FLOW_EDGE_CFG` / `STOCK_IN_POST` / `PAYABLE_PAY` / `VOUCHER_REVERSE`
 *    这些**根本不在任务列表里**，路由本身就是错的。
 *
 * 现在映射表只在这里（`.ts`，纯函数，可单测），模板只做「渲染 / 置灰 / 点击」，
 * 不再出现类型字面量链。
 *
 * # 枚举口径（值域的唯一真相）
 *
 * key 只能取 {@link FlowBizType}（`@/api/business/erp/doc-kinds` 的 `FLOW_BIZ_TYPES`），
 * 它是后端 `backend/src/business/shared/doc_kind.rs::FlowBizType::as_str` 的逐字镜像，
 * 由后端单测 `doc_kind_tests.rs::frontend_ts_mirror_matches_rust` 钉死。
 *
 * 注意：卡面写的是「对照 `backend/src/business/flow/dto.rs` 的 biz_type 取值」，但那个文件里
 * **没有枚举表**（只有一句注释举例 `TASK_ACCEPT` / `ORDER`，其中 `ORDER` 在库里根本不存在）。
 * 值域的唯一真相是 `shared/doc_kind.rs`（`V0219` 迁移里已写明「为什么不再抄第二套词表」），
 * 单测直接读那个文件逐字比对。**不许自己编类型名**。
 *
 * # 未登记的类型 ⇒ 不渲染按钮（`resolveBizLink` 返回 `null`）
 *
 * - `STOCK_TAKE_POST`（盘点过账）：前端没有盘点单页面（`grep -rn "stock-take" frontend/src` 零命中），
 *   后端有接口但没页面；
 * - `EXPENSE` / `OTHER`：通用流程，没有对应业务单据（生产库里 `OTHER` 16 条全部 `biz_id` 为空）。
 *
 * 宁可不出按钮，也不猜一个「差不多的页面」把用户带去错地方。
 *
 * # 存活探测
 *
 * 卡片要求「点击前探测目标对象是否还活着」，用的是**该类型自己的详情接口**（真接口，不猜）：
 *
 * | biz_type | 探测接口 | 生产实测（2026-11-04） |
 * |---|---|---|
 * | `TASK_ACCEPT` | `GET /business/task/{id}` | 软删任务 → HTTP 404 + `63001 任务不存在或已删除` |
 * | `CONTRACT_APPROVE` | `GET /business/erp/contract/{id}` | 软删合同 → HTTP 404 + `66014 合同不存在或已删除` |
 * | `PO_APPROVE` | `GET /business/erp/purchase-order/{id}` | 活着 → `code 0` |
 * | `STOCK_IN_POST` / `STOCK_IN_CANCEL` | `GET /business/erp/stock-in/{id}` | 活着 → `code 0` |
 * | `STOCK_TRANSFER` | `GET /business/erp/transfer/{id}` | 活着 → `code 0` |
 * | `PAYABLE_PAY` / `PAYABLE_CANCEL` | `GET /business/erp/payable/{id}` | 后端注释：不存在 → `404 + 10002` |
 * | `VOUCHER_REVERSE` | `GET /business/erp/voucher/{id}` | 后端注释：不存在 → `404` |
 * | `FLOW_EDGE_CFG` | `GET /business/flow/state-edge?entity={实体}` | 无「单条配置」详情接口（`biz_id` 是变更指纹，不是库里的行）→ 探实体是否存在 |
 *
 * 探测结论只有四种（见 {@link classifyProbeFailure}）：
 * `alive`（可跳）/ `gone`（已清场）/ `forbidden`（没权限）/ `unknown`（探测失败，说不清）。
 * **403 单独一条分支**：审批岗常没有 ERP 权限，静默什么都不做会被当成「按钮坏了」。
 */
import { DOC_KINDS, type FlowBizType } from '@/api/business/erp/doc-kinds';

/** 审批实例上冻结的业务引用（`biz_flow_instance.biz_id` / `biz_no`） */
export interface BizRef {
  /** 业务 id（uuid 字符串；空串 = 没有关联业务） */
  id: string;
  /** 业务单号（如 `TSK-202610-000851` / `PO-20261104-000001`） */
  no: string;
}

/** 存活探测方案（一次 GET） */
export interface BizProbePlan {
  /** 请求路径（含 `/api` 前缀，与 `@/api/request` 的 `get()` 同一口径） */
  url: string;
  /** 查询参数（`FLOW_EDGE_CFG` 的实体名走这里） */
  params?: Record<string, string>;
}

/** 一种审批业务类型对应的业务详情页 */
export interface BizLinkTarget {
  /** 审批业务类型（取值只能是 `FLOW_BIZ_TYPES` 里的字面量） */
  bizType: FlowBizType;
  /** 中文业务名（按钮与提示里出现，**不含技术术语**：不出现 biz_type / entity_id） */
  label: string;
  /** 业务详情页路由 path（与 `router/index.ts` 逐字一致） */
  path: string;
  /**
   * 深链 query。
   *
   * 返回 `null` = 手上这单**没有可用的定位键**（例如只有 id 没有单号，而目标页只认单号），
   * 此时按钮置灰并提示「没有记录业务单号」，不跳空列表。
   */
  queryOf: (biz: BizRef) => Record<string, string> | null;
  /** 存活探测方案；返回 `null` = 不探测（直接可跳，例如边配置页恒存在） */
  probeOf: (biz: BizRef) => BizProbePlan | null;
  /**
   * 除 HTTP 404 之外的「对象不存在」业务码（该类型自己的口径，抄后端源码）。
   *
   * 生产实测：任务 `63001`、合同 `66014`；边配置的「实体不认识」是 `65005`（HTTP 400）。
   */
  notFoundCodes?: readonly number[];
}

/** 按钮的五种可渲染状态（`idle` = 没有关联业务，不渲染） */
export type BizLinkState = 'idle' | 'probing' | 'alive' | 'gone' | 'forbidden' | 'unknown' | 'noref';

/** 置灰原因 → 提示文案（**全是人话**，不出现 `biz_type` / `entity_id` 这类词） */
export const BIZ_LINK_TIPS: Readonly<Record<Exclude<BizLinkState, 'idle' | 'alive'>, string>> = {
  probing: '正在确认这条记录是否还在…',
  gone: '业务对象已清场，仅可查看审批快照',
  forbidden: '没有查看这条记录的业务权限，仅可查看审批快照',
  unknown: '暂时确认不了这条记录是否还在，仅可查看审批快照',
  noref: '审批单里没有记录业务单号，仅可查看审批快照',
};

/** 后端通用「资源不存在」码（`shared/error_code.rs::RESOURCE_NOT_FOUND`） */
export const RESOURCE_NOT_FOUND = 10002;

/** 后端「无权限访问该资源」码（`shared/error_code.rs::PERMISSION_DENIED`，HTTP 403） */
export const PERMISSION_DENIED = 30001;

/**
 * 从 `FLOW_EDGE_CFG` 实例的 `biz_no` 里取实体名。
 *
 * 后端 `edge_admin.rs` 造 `biz_no` 的写法是 `format!("{} {}>{}", entity, from, to)`，
 * 生产实测值形如 `STOCK_IN DRAFT>POSTED`。这里只取第一段，并**要求它是已知业务对象**
 * （`DOC_KINDS`），取不到就返回空串 —— 空串时照常跳「状态机边配置」页面（不定位实体），
 * 而不是把半截字符串塞进 URL。
 */
export function edgeEntityOf(bizNo: string): string {
  const first = (bizNo ?? '').trim().split(/\s+/)[0] ?? '';
  return (DOC_KINDS as readonly string[]).includes(first) ? first : '';
}

/**
 * 「关联业务」映射表。
 *
 * `STOCK_IN_POST` / `STOCK_IN_CANCEL` 同一条入库单；`PAYABLE_PAY` / `PAYABLE_CANCEL` 同一张应付单。
 * 深链键（`queryOf`）抄的是各详情页自己 `route.query` 的读法，不是自己发明：
 *
 * | 页面 | 深链 | 读法出处 |
 * |---|---|---|
 * | 任务列表 | `/task/list?id=` | `router/index.ts` 的 `task/:id` 重定向 + `task/index.vue` 读 `query.id` |
 * | 合同 | `/business/erp/contract?contract_id=` | `contract/index.vue::applyRouteQuery` |
 * | 采购单 | `/business/erp/purchase-order?po_id=` | `purchase/index.vue::applyRouteQuery` |
 * | 入库单 | `/business/erp/stock-in?in_id=` | `stockin/index.vue::applyRouteQuery` |
 * | 调拨单 | `/business/erp/transfer?transfer_no=` | `transfer/index.vue` + `transfer-logic.ts::transferNoFromQuery` |
 * | 应付单 | `/business/erp/payable?payable_no=` | `payable/index.vue::openByQuery` |
 * | 凭证 | `/business/erp/voucher?voucher_no=` | `voucher/index.vue::openByQuery`（预填单号筛选） |
 * | 状态机边配置 | `/flow/def?entity=` | `flow/def/index.vue::onMounted` 的深链 |
 */
export const BIZ_LINKS: Readonly<Partial<Record<FlowBizType, BizLinkTarget>>> = {
  TASK_ACCEPT: {
    bizType: 'TASK_ACCEPT',
    label: '任务',
    path: '/task/list',
    queryOf: (biz) => (biz.id ? { id: biz.id } : null),
    probeOf: (biz) => ({ url: `/api/business/task/${biz.id}` }),
    notFoundCodes: [63001],
  },
  CONTRACT_APPROVE: {
    bizType: 'CONTRACT_APPROVE',
    label: '合同',
    path: '/business/erp/contract',
    queryOf: (biz) => (biz.id ? { contract_id: biz.id } : null),
    probeOf: (biz) => ({ url: `/api/business/erp/contract/${biz.id}` }),
    notFoundCodes: [66014],
  },
  PO_APPROVE: {
    bizType: 'PO_APPROVE',
    label: '采购单',
    path: '/business/erp/purchase-order',
    queryOf: (biz) => (biz.id ? { po_id: biz.id } : null),
    probeOf: (biz) => ({ url: `/api/business/erp/purchase-order/${biz.id}` }),
  },
  STOCK_IN_POST: {
    bizType: 'STOCK_IN_POST',
    label: '入库单',
    path: '/business/erp/stock-in',
    queryOf: (biz) => (biz.id ? { in_id: biz.id } : null),
    probeOf: (biz) => ({ url: `/api/business/erp/stock-in/${biz.id}` }),
  },
  STOCK_IN_CANCEL: {
    bizType: 'STOCK_IN_CANCEL',
    label: '入库单',
    path: '/business/erp/stock-in',
    queryOf: (biz) => (biz.id ? { in_id: biz.id } : null),
    probeOf: (biz) => ({ url: `/api/business/erp/stock-in/${biz.id}` }),
  },
  STOCK_TRANSFER: {
    bizType: 'STOCK_TRANSFER',
    label: '调拨单',
    path: '/business/erp/transfer',
    queryOf: (biz) => (biz.no ? { transfer_no: biz.no } : null),
    probeOf: (biz) => ({ url: `/api/business/erp/transfer/${biz.id}` }),
  },
  PAYABLE_PAY: {
    bizType: 'PAYABLE_PAY',
    label: '应付单',
    path: '/business/erp/payable',
    queryOf: (biz) => (biz.no ? { payable_no: biz.no } : null),
    probeOf: (biz) => ({ url: `/api/business/erp/payable/${biz.id}` }),
  },
  PAYABLE_CANCEL: {
    bizType: 'PAYABLE_CANCEL',
    label: '应付单',
    path: '/business/erp/payable',
    queryOf: (biz) => (biz.no ? { payable_no: biz.no } : null),
    probeOf: (biz) => ({ url: `/api/business/erp/payable/${biz.id}` }),
  },
  VOUCHER_REVERSE: {
    bizType: 'VOUCHER_REVERSE',
    label: '凭证',
    path: '/business/erp/voucher',
    queryOf: (biz) => (biz.no ? { voucher_no: biz.no } : null),
    probeOf: (biz) => ({ url: `/api/business/erp/voucher/${biz.id}` }),
  },
  FLOW_EDGE_CFG: {
    bizType: 'FLOW_EDGE_CFG',
    label: '状态机配置',
    path: '/flow/def',
    // 边配置没有「一条配置」的详情页：目标是 `/flow/def` 的配置抽屉，
    // 实体名从 `biz_no`（`STOCK_IN DRAFT>POSTED`）里取；取不到就只打开页面。
    queryOf: (biz): Record<string, string> => {
      const entity = edgeEntityOf(biz.no);
      return entity ? { entity } : {};
    },
    probeOf: (biz) => {
      const entity = edgeEntityOf(biz.no);
      // 实体名都取不到 ⇒ 没法拿真接口确认，别假装探测过（页面照常可打开）
      return entity ? { url: '/api/business/flow/state-edge', params: { entity } } : null;
    },
    notFoundCodes: [65005],
  },
};

/**
 * 解析一条审批实例的「关联业务」目标。
 *
 * @param bizType `biz_flow_instance.biz_type`（后端原样字符串）
 * @param biz `{ id, no }`
 * @returns 未登记的类型 / 没有 `biz_id` → `null`（调用方不渲染按钮）
 */
export function resolveBizLink(bizType: string, biz: BizRef): BizLinkTarget | null {
  if (!bizType || !biz.id) {
    return null;
  }
  if (!Object.prototype.hasOwnProperty.call(BIZ_LINKS, bizType)) {
    return null;
  }
  return BIZ_LINKS[bizType as FlowBizType] ?? null;
}

/**
 * 深链目标（喂给 `router.push`）。
 *
 * @returns `null` = 这单没有可用的定位键（见 {@link BizLinkTarget.queryOf}）
 */
export function bizLinkRouteOf(target: BizLinkTarget, biz: BizRef): { path: string; query: Record<string, string> } | null {
  const query = target.queryOf(biz);
  if (query === null) {
    return null;
  }
  return { path: target.path, query };
}

/** 探测失败的口径（只取分类要用的两个字段，方便单测构造） */
export interface ProbeFailure {
  /** HTTP 状态码（`undefined` = 网络层失败 / 超时 / 业务码非 0 但 HTTP 200） */
  status?: number;
  /** 后端统一响应体的 `code` */
  code?: number;
}

/**
 * 探测失败 → 按钮状态（纯函数，单测直接钉这张表）。
 *
 * 三种结论都是「不跳转 + 置灰 + 提示」，但**文案必须不同**：
 * 没权限 ≠ 已清场 ≠ 探测失败。审批岗经常没有 ERP 权限（403），
 * 把它混进「已清场」会让 IT 收到「我的数据被删了」这种错判。
 */
export function classifyProbeFailure(
  failure: ProbeFailure,
  notFoundCodes: readonly number[] = [],
): 'gone' | 'forbidden' | 'unknown' {
  const { status, code } = failure;
  if (status === 403 || code === PERMISSION_DENIED) {
    return 'forbidden';
  }
  if (status === 404 || code === RESOURCE_NOT_FOUND || (code !== undefined && notFoundCodes.includes(code))) {
    return 'gone';
  }
  return 'unknown';
}

/** 置灰提示（`alive` / `idle` 没有提示，返回空串） */
export function bizLinkTip(state: BizLinkState): string {
  if (state === 'idle' || state === 'alive') {
    return '';
  }
  return BIZ_LINK_TIPS[state];
}

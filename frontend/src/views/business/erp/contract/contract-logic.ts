/**
 * 合同管理页的纯逻辑（A5）。
 *
 * # 为什么单拎一个 `.ts`
 *
 * 状态配色、合法边、表单校验这些「错了会误导人」的判定必须能在没有 DOM 的
 * `tsx --test` 里断言（本仓惯例：`*-logic.ts` 承载口径，`.vue` 只渲染）。
 *
 * # 口径来源（逐条抄真代码，不抄卡面）
 *
 * | 内容 | 唯一真相 |
 * |---|---|
 * | 6 个状态字面量 | `backend/src/business/erp/contract_model.rs::STATUSES` |
 * | 状态中文名 | `contract_model.rs::status_label` |
 * | 合法边 | `backend/src/business/shared/state_machine.rs::CONTRACT_EDGES` |
 * | 需要 reason 的边 | `state_machine.rs::CONTRACT_REASON_EDGES` |
 * | 边的按钮文案 | `state_machine.rs::CONTRACT_BASE`（`BaseEdge::default_label`） |
 * | 建单白名单 / 金额下限 / 日期顺序 | `contract_dto.rs::CreateContractReq::validate` |
 * | 乙方快照口径 | `contract_dto.rs::ContractRes`（`party_b_name` + `party_b_current_name`） |
 *
 * # 与 A5 卡面的三处出入（都以已落库的代码 / DDL 为准，卡里已报告）
 *
 * 1. **没有 `CANCELLED`**：DDL 白名单是 `DRAFT/PENDING_APPROVAL/SIGNED/ACTIVE/COMPLETED/TERMINATED`，
 *    草稿作废走软删除；卡面「DRAFT → [生效][取消]」在 R-P1-1 之后是
 *    `DRAFT → PENDING_APPROVAL`（提交审批），审批通过才 `PENDING_APPROVAL → SIGNED`。
 * 2. **金额是 `> 0` 不是 `≥ 0`**：`ck_biz_contract_amount CHECK (amount > 0)`，0 元合同
 *    后端 400（DEC-04 不支持不限额框架合同）——前端先拦，不让用户白填一遍。
 * 3. **没有「税率」「付款条件」列**：`V0195` 的 23 列里没有 `tax_rate` / `payment_terms`，
 *    所以表单不提供这两个字段（合同只存含税总额 `amount`）。
 */
import {
  statusLabel,
  statusMeta,
  statusOptions,
  type StatusTagTheme,
} from '@/constants/status-map';
import type {
  ContractDetail,
  ContractPageQuery,
  ContractRow,
  ContractStatus,
  ContractType,
  CreateContractPayload,
  UpdateContractPayload,
} from '@/api/business/erp';
import { formatMoney, toNumber } from '@/views/business/erp/shared/money';

/** 徽标域（`constants/status-map.ts`，配色不在这里硬写） */
export const CONTRACT_STATUS_DOMAIN = 'erp-contract';

/** 默认每页条数（后端 `DEFAULT_PAGE_SIZE` = 20，上限 100） */
export const CONTRACT_PAGE_SIZE = 20;

/** 名称长度上限（`contract_name VARCHAR(200)` + `contract_dto::MAX_NAME_LEN`） */
export const MAX_CONTRACT_NAME = 200;

/** 备注长度上限（`contract_dto::MAX_REMARK_LEN` = 2000 字） */
export const MAX_CONTRACT_REMARK = 2000;

/** 金额小数位上限（`NUMERIC(18,4)`） */
export const MAX_AMOUNT_SCALE = 4;

/**
 * 6 个状态（与 `contract_model::STATUSES` 逐字一致、顺序一致）。
 *
 * ⚠️ 没有 `CANCELLED`：草稿作废走软删（`DELETE /business/erp/contract/{id}`）。
 * `PENDING_APPROVAL`（审批中）由 R-P1-1 引入：单据交出去等人批，业务字段冻结。
 */
export const CONTRACT_STATUSES: readonly ContractStatus[] = [
  'DRAFT',
  'PENDING_APPROVAL',
  'SIGNED',
  'ACTIVE',
  'COMPLETED',
  'TERMINATED',
] as const;

/** 终态（`state_machine::CONTRACT_EDGES` 里没有出边的两个） */
export const CONTRACT_TERMINAL_STATUSES: readonly ContractStatus[] = ['COMPLETED', 'TERMINATED'] as const;

/**
 * 合法边（**与 `state_machine.rs::CONTRACT_EDGES` 逐条一致、顺序也一致**）。
 *
 * ```text
 * DRAFT             → PENDING_APPROVAL  提交审批（起 contract_approve 实例）
 * PENDING_APPROVAL  → SIGNED            审批通过签订（日期三件套必填）
 * PENDING_APPROVAL  → DRAFT             驳回 / 撤回（审批实例被终止）
 * SIGNED            → ACTIVE            生效
 * SIGNED            → TERMINATED        已签未生效即终止（必填原因）
 * ACTIVE            → COMPLETED         履约完成
 * ACTIVE            → TERMINATED        中途终止（必填原因）
 * ```
 *
 * ⚠️ R-P1-1 之后**没有 `DRAFT → SIGNED`**：没人审过的合同不能一步变成可用于
 * 采购付款的已签状态（后端直接 409 并点名「先提交审批」）。前端回落路径（列表行
 * 没拿到 `next_statuses` 时）也必须给出「提交审批」而不是「登记签订」。
 *
 * 单测 `⑦ 状态按钮` 会读 Rust 源码逐条比对 —— 后端加一条边、前端没跟，用例先红
 * （不是靠人眼记）。
 */
export const CONTRACT_EDGES: ReadonlyArray<readonly [ContractStatus, ContractStatus]> = [
  ['DRAFT', 'PENDING_APPROVAL'],
  ['PENDING_APPROVAL', 'SIGNED'],
  ['PENDING_APPROVAL', 'DRAFT'],
  ['SIGNED', 'ACTIVE'],
  ['SIGNED', 'TERMINATED'],
  ['ACTIVE', 'COMPLETED'],
  ['ACTIVE', 'TERMINATED'],
] as const;

/**
 * 需要 reason 的边（`state_machine::CONTRACT_REASON_EDGES`：两条终止边）。
 *
 * ⚠️ `PENDING_APPROVAL → DRAFT`（驳回 / 撤回）**不在**这张表里：后端只对两条终止边
 * 校验 reason（`transition()` 缺 reason 才 400）。审批驳回时的「意见」是**流程待办**
 * 的必填项（`POST /business/flow/task/{id}/reject`，`normalize_reject_comment` ≥ 5 字），
 * 不是合同状态流转的 reason —— 前端在这里弹出必填框会把「撤回」也一起挡住。
 */
export const CONTRACT_REASON_EDGES: ReadonlyArray<readonly [ContractStatus, ContractStatus]> = [
  ['SIGNED', 'TERMINATED'],
  ['ACTIVE', 'TERMINATED'],
] as const;

/**
 * 放行的合同类型（**与 `contract_dto::SUPPORTED_TYPES` 逐字一致 = 四类全开**）。
 *
 * CTR-02-F1（2026-11-09）起 `SERVICE` / `LEASE` 也放行：口径见
 * `docs/合同类型开放口径-决策.md`（服务 / 租赁乙方固定走 `biz_supplier`，不挂订单）。
 * 表单**不再置灰任何类型** —— 四类都是可选的正规选择，类型本身在签订后冻结。
 */
export const SUPPORTED_CONTRACT_TYPES: readonly ContractType[] = [
  'PURCHASE',
  'SALES',
  'SERVICE',
  'LEASE',
] as const;

/**
 * 合同类型 → 乙方主档（**与后端 `contract_model::required_party_b_type` 逐字一致**）。
 *
 * `SALES` 的乙方是客户（`biz_customer`）；`PURCHASE` / `SERVICE` / `LEASE` 的乙方都是
 * 供应商（`biz_supplier`，服务商 / 出租方在系统里就按供应商登记）。
 * 表单的乙方选择器按它切换数据源，校验文案也按它分叉。
 *
 * ⚠️ 返回值域**只有两值**（后端 `party_b_type` 只有 `SUPPLIER` / `CUSTOMER`）：
 * 「服务商 / 出租方」是**称谓**（见 {@link contractPartyBLabel}），不是第三种主档。
 */
export function contractPartyBKind(type: string | null | undefined): 'SUPPLIER' | 'CUSTOMER' {
  return (type ?? '').trim().toUpperCase() === 'SALES' ? 'CUSTOMER' : 'SUPPLIER';
}

/** 乙方称谓（表单 label、placeholder、校验文案统一用它）—— 四档 */
const PARTY_B_LABELS: Record<ContractType, string> = {
  PURCHASE: '乙方供应商',
  SALES: '乙方客户',
  SERVICE: '乙方服务商',
  LEASE: '乙方出租方',
};

/**
 * 乙方称谓：`乙方供应商` / `乙方客户` / `乙方服务商` / `乙方出租方`。
 *
 * 未知类型（脏数据 / 后端将来加类型）按 {@link contractPartyBKind} 的兜底口径给
 * `乙方供应商` / `乙方客户`，不抛错。
 */
export function contractPartyBLabel(type: string | null | undefined): string {
  const key = (type ?? '').trim().toUpperCase() as ContractType;
  return PARTY_B_LABELS[key] ?? (contractPartyBKind(type) === 'CUSTOMER' ? '乙方客户' : '乙方供应商');
}

/**
 * 乙方选择器下方的来源说明（只有服务 / 租赁需要解释「为什么下拉里是供应商」）。
 *
 * `PURCHASE` / `SALES` 各自的主档一眼就对，不需要解释 → 返回空串（页面不渲染这一行）。
 */
export function contractPartyBTip(type: string | null | undefined): string {
  const key = (type ?? '').trim().toUpperCase();
  return key === 'SERVICE' || key === 'LEASE' ? '服务商 / 出租方在系统中登记为供应商' : '';
}

/** 类型下拉旁的一句话提醒（类型是已签冻结字段，选错只能终止重签） */
export const CONTRACT_TYPE_GUIDE_TIP =
  '合同类型在签订后冻结，选错只能终止重签 —— 按「这笔业务会不会下订单」来选。';

/**
 * 类型选择引导（四行判定表）。文案与 `docs/对客验收手册.md` §6 的表格同口径，
 * 也与后端 `docs/合同类型开放口径-决策.md` 一致。
 *
 * | 会不会下订单 | 选哪类 |
 * |---|---|
 * | 下采购单（收货 / 入库 / 应付），货物或服务都算 | 采购合同 |
 * | 开销售单 | 销售合同 |
 * | 一个订单都不下，长期服务（物业 / 维保 / 外包年框 / 订阅） | 服务合同 |
 * | 一个订单都不下，租赁（房租 / 设备租用） | 租赁合同 |
 */
export const CONTRACT_TYPE_GUIDE: ReadonlyArray<{ type: ContractType; label: string; when: string }> = [
  {
    type: 'PURCHASE',
    label: '采购合同',
    when: '会下采购单（收货 / 入库 / 应付）的采购行为，货物或服务都算',
  },
  {
    type: 'SALES',
    label: '销售合同',
    when: '会开销售单的销售行为',
  },
  {
    type: 'SERVICE',
    label: '服务合同',
    when: '一个订单都不下的长期服务（物业、维保、外包年框、订阅）',
  },
  {
    type: 'LEASE',
    label: '租赁合同',
    when: '一个订单都不下的租赁（房租、设备租用）',
  },
] as const;

/** 合同类型中文名（`ck_biz_contract_type` 的 4 个值） */
const CONTRACT_TYPE_LABELS: Record<ContractType, string> = {
  PURCHASE: '采购合同',
  SALES: '销售合同',
  SERVICE: '服务合同',
  LEASE: '租赁合同',
};

/**
 * 状态 → 动作文案（**按目标态兜底**）。
 *
 * 一条边的文案优先看 {@link EDGE_LABELS}（同一目标态可能有多种语义，例如
 * `PENDING_APPROVAL → SIGNED` 是「审批通过签订」而目标态 `SIGNED` 本身叫「已签订」）。
 */
const TRANSITION_LABELS: Record<ContractStatus, string> = {
  DRAFT: '退回草稿',
  PENDING_APPROVAL: '提交审批',
  SIGNED: '登记签订',
  ACTIVE: '生效',
  COMPLETED: '完成',
  TERMINATED: '终止',
};

/**
 * 边 → 动作文案（**逐条抄 `state_machine.rs::CONTRACT_BASE` 的 `default_label`**）。
 *
 * 只有「目标态文案会误导」的边才单列：
 * - `PENDING_APPROVAL → SIGNED` 若按目标态取文案会写「登记签订」，但这条边是审批通过后
 *   的回调落地，用户看到「登记签订」会以为要自己填日期；
 * - `PENDING_APPROVAL → DRAFT` 若按目标态会写「退回草稿」，而这既可能是审批驳回、
 *   也可能是自己撤回。
 */
const EDGE_LABELS: Record<string, string> = {
  'DRAFT→PENDING_APPROVAL': '提交审批',
  'PENDING_APPROVAL→SIGNED': '审批通过签订',
  'PENDING_APPROVAL→DRAFT': '驳回/撤回',
};

/** 状态 → 动作按钮配色（`t-link` 用，按**目标态**上色，与状态徽标同一套语义） */
const TRANSITION_THEMES: Record<ContractStatus, 'primary' | 'success' | 'warning' | 'danger' | 'default'> = {
  DRAFT: 'default',
  PENDING_APPROVAL: 'primary',
  SIGNED: 'primary',
  ACTIVE: 'success',
  COMPLETED: 'success',
  TERMINATED: 'danger',
};

// ==================== 展示 ====================

/** 状态中文名（走 `status-map` 的唯一来源，未知值原样显示） */
export function contractStatusLabel(status: string): string {
  return statusLabel(CONTRACT_STATUS_DOMAIN, status);
}

/** 状态徽标主题（配色口径在 `status-map.ts`） */
export function contractStatusTheme(status: string): StatusTagTheme {
  return statusMeta(CONTRACT_STATUS_DOMAIN, status).theme;
}

/** 状态下拉选项（顺序 = 状态机推进顺序，`''` = 全部由页面自己加） */
export const CONTRACT_STATUS_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: string; theme: StatusTagTheme }> =
  statusOptions(CONTRACT_STATUS_DOMAIN);

/** 合同类型中文名 */
export function contractTypeLabel(type: string | null | undefined): string {
  if (!type) {
    return '—';
  }
  return CONTRACT_TYPE_LABELS[type as ContractType] ?? type;
}

/**
 * 合同类型下拉：四类**全可选**（CTR-02-F1）。
 *
 * 保留 `disabled` 字段（恒 `false`）是为了让调用方（筛选区 / 表单）的接线不用改；
 * 选项**不隐藏也不置灰** —— 服务 / 租赁是正规选择，只是选之前要看引导行。
 */
export function contractTypeOptions(): Array<{ label: string; value: ContractType; disabled: boolean }> {
  return (Object.keys(CONTRACT_TYPE_LABELS) as ContractType[]).map((value) => ({
    label: CONTRACT_TYPE_LABELS[value],
    value,
    disabled: !SUPPORTED_CONTRACT_TYPES.includes(value),
  }));
}

/** 履约口径（`contract_dto::ContractDetailRes::commitment_scope`） */
export type CommitmentScope = 'ORDER_LINKED' | 'NOT_APPLICABLE';

/** 服务 / 租赁的履约区灰字（详情页原样显示这一行，**不显示 0.00**） */
export const COMMITMENT_NOT_APPLICABLE_TEXT = '履约进度不适用（服务 / 租赁合同不走订单勾稽）';

/**
 * 详情里的履约区该长什么样（把「口径 → 渲染形态」收成纯函数，模板只铺）。
 *
 * - `ORDER_LINKED`：`PURCHASE` / `SALES`，四个履约字段是真值 → 渲染金额与未关闭单据；
 * - `NOT_APPLICABLE`：`SERVICE` / `LEASE`，按口径不挂订单 → 只渲染一行灰字；
 * - `HIDDEN`：字段缺失 / 未知取值（老后端、脏数据）→ **整块不渲染**，不抛错白屏。
 */
export type CommitmentView =
  | {
      kind: 'ORDER_LINKED';
      committedText: string;
      remainingText: string;
      /** 剩余额度为负 = 合同额被改小到低于已下单额（「超签」，不夹到 0） */
      remainingNegative: boolean;
      openOrderCount: number;
      openOrderNos: string[];
    }
  | { kind: 'NOT_APPLICABLE' }
  | { kind: 'HIDDEN' };

/** 履约区的四种取值 → 渲染形态（见 {@link CommitmentView}） */
export function commitmentViewOf(
  detail:
    | {
        commitment_scope?: string | null;
        committed_amount?: number | string | null;
        remaining_amount?: number | string | null;
        open_order_count?: number | null;
        open_order_nos?: string[] | null;
      }
    | null
    | undefined,
): CommitmentView {
  const scope = (detail?.commitment_scope ?? '').trim().toUpperCase();
  if (scope === 'NOT_APPLICABLE') {
    return { kind: 'NOT_APPLICABLE' };
  }
  if (scope !== 'ORDER_LINKED') {
    // 字段缺失 / 未知取值：降级为不渲染（后端将来加 `PAYMENT_PLAN` 时也走这里，
    // 直到前端按新的履约单据类型补渲染分支，而不是先显示一个会被误读的 0.00）
    return { kind: 'HIDDEN' };
  }
  const remaining = toNumber(detail?.remaining_amount);
  return {
    kind: 'ORDER_LINKED',
    committedText: contractAmountText(detail?.committed_amount),
    remainingText: contractAmountText(detail?.remaining_amount),
    remainingNegative: remaining < 0,
    openOrderCount: Number(detail?.open_order_count ?? 0) || 0,
    openOrderNos: (detail?.open_order_nos ?? []).filter((no): no is string => Boolean(no)),
  };
}

/**
 * 金额文案：`¥1,234,567.89`（千分位 + `¥` 前缀）。
 *
 * 走 `shared/money.ts::formatMoney`（整数分口径，避免浮点二次舍入）。
 */
export function contractAmountText(value: number | string | null | undefined): string {
  return `¥${formatMoney(value)}`;
}

/** 有效期文案：`2026-01-01 ~ 2026-12-31`（任一端缺 → 尽量显示已填的那端） */
export function contractPeriodText(start: string | null | undefined, end: string | null | undefined): string {
  if (!start && !end) {
    return '—';
  }
  return `${start || '—'} ~ ${end || '—'}`;
}

/** 乙方名称：优先主档现值（改名后跟着变），没有现值用签约快照，都没有给短 id */
export function contractPartyBName(row: Pick<ContractRow, 'party_b_name' | 'party_b_current_name'>): string {
  return row.party_b_current_name || row.party_b_name || '—';
}

// ==================== 状态流转 ====================

/** 一条可用动作 */
export interface ContractTransition {
  /** 目标状态 */
  target: ContractStatus;
  /** 按钮文案（`提交审批` / `审批通过签订` / `驳回/撤回` / `生效` / `完成` / `终止`） */
  label: string;
  /** 按钮配色 */
  theme: 'primary' | 'success' | 'warning' | 'danger' | 'default';
  /** 是否必须填原因（两条终止边） */
  needReason: boolean;
}

/** 该状态是不是终态（没有出边） */
export function isContractTerminal(status: string): boolean {
  return CONTRACT_TERMINAL_STATUSES.includes(status as ContractStatus);
}

/**
 * 当前状态能走的合法边。
 *
 * @param status 当前状态
 * @param nextStatuses 后端详情下发的 `next_statuses`（**有就优先用它**，见卡面第 2 点；
 *   列表行没有这个字段，才回落到 {@link CONTRACT_EDGES}）
 */
export function contractTransitions(status: string, nextStatuses?: readonly string[] | null): ContractTransition[] {
  const from = (status ?? '').trim().toUpperCase() as ContractStatus;
  const targets = nextStatuses && nextStatuses.length > 0
    ? nextStatuses.map((item) => (item ?? '').trim().toUpperCase())
    : CONTRACT_EDGES.filter(([edgeFrom]) => edgeFrom === from).map(([, to]) => to as string);

  return targets
    .filter((target) => target.length > 0)
    .map((target) => {
      const key = target as ContractStatus;
      return {
        target: key,
        label: EDGE_LABELS[`${from}→${key}`] ?? TRANSITION_LABELS[key] ?? contractStatusLabel(key),
        theme: TRANSITION_THEMES[key] ?? 'primary',
        needReason: needsReason(from, key),
      };
    });
}

/** 这条边是否必须填原因（`CONTRACT_REASON_EDGES`） */
export function needsReason(from: string, to: string): boolean {
  const f = (from ?? '').trim().toUpperCase();
  const t = (to ?? '').trim().toUpperCase();
  return CONTRACT_REASON_EDGES.some(([edgeFrom, edgeTo]) => edgeFrom === f && edgeTo === t);
}

/**
 * 流转请求体。
 *
 * `reason` 只在该边需要时带上（不需要的边也带会被后端写进
 * `biz_status_log.reason`，等于给审计塞噪声）。
 */
export function buildStatusPayload(
  from: string,
  target: string,
  reason?: string,
): { target_status: string; reason?: string } {
  const text = (reason ?? '').trim();
  const payload: { target_status: string; reason?: string } = { target_status: target };
  if (needsReason(from, target) && text) {
    payload.reason = text;
  }
  return payload;
}

/**
 * 边 → 确认弹窗的补充说明（说明**副作用**，空串 = 不补充）。
 *
 * 按 `from→to` 单列而不是按目标态分支：`PENDING_APPROVAL→SIGNED` 与
 * （历史上）`DRAFT→SIGNED` 的副作用完全不同，按目标态写文案必然误导其中一边。
 */
const EDGE_CONFIRM_TAILS: Record<string, string> = {
  'DRAFT→PENDING_APPROVAL':
    '提交后合同进入「审批中」：业务字段冻结，要等审批人通过才会变成「已签订」；没人审过的合同不能直接用于采购付款。',
  'PENDING_APPROVAL→SIGNED':
    '审批通过后合同变为「已签订」（可被采购单引用）；此后关键字段（乙方 / 金额 / 日期）冻结，只能作废重签。',
  'PENDING_APPROVAL→DRAFT':
    '驳回 / 撤回会把在跑的审批实例终止、合同退回草稿（可以再改再提交）；审批驳回的意见在流程待办里填。',
  'SIGNED→ACTIVE': '生效后合同可被新建采购单引用。',
  'SIGNED→TERMINATED':
    '终止是不可逆的终态：终止后这张合同不能再被新采购单引用，原因会写进状态日志与备注。',
  'ACTIVE→COMPLETED': '履约完成后合同不再接受新采购单。',
  'ACTIVE→TERMINATED':
    '终止是不可逆的终态：终止后这张合同不能再被新采购单引用，原因会写进状态日志与备注。',
};

/**
 * 服务 / 租赁（`commitment_scope = NOT_APPLICABLE`）的边说明。
 *
 * 上面那份通用文案里写了「可被采购单引用」，对不挂订单的两类**是错的**
 * （订单侧类型闸只收 `PURCHASE` / `SALES`）—— 所以这两类单独一套，只讲审批与冻结。
 */
const NO_ORDER_EDGE_TAILS: Record<string, string> = {
  'DRAFT→PENDING_APPROVAL':
    '提交后合同进入「审批中」：业务字段冻结，要等审批人通过才会变成「已签订」。',
  'PENDING_APPROVAL→SIGNED':
    '审批通过后合同变为「已签订」（服务 / 租赁合同不挂采购单、销售单，履约在付款侧跟踪）；此后关键字段（乙方 / 金额 / 日期）冻结，只能作废重签。',
  'PENDING_APPROVAL→DRAFT':
    '驳回 / 撤回会把在跑的审批实例终止、合同退回草稿（可以再改再提交）；审批驳回的意见在流程待办里填。',
  'SIGNED→ACTIVE': '生效后合同开始计算服务期 / 租期（不挂订单）。',
  'SIGNED→TERMINATED': '终止是不可逆的终态：原因会写进状态日志与备注。',
  'ACTIVE→COMPLETED': '履约完成后合同归档（不再接新单；本类合同本来就不挂订单）。',
  'ACTIVE→TERMINATED': '终止是不可逆的终态：原因会写进状态日志与备注。',
};

/** 该合同类型是否「不挂订单」（只有 `SERVICE` / `LEASE`） */
export function isNoOrderContractType(type: string | null | undefined): boolean {
  const key = (type ?? '').trim().toUpperCase();
  return key === 'SERVICE' || key === 'LEASE';
}

/**
 * 「归档前提示」：`ACTIVE → COMPLETED` 在途单据数（E2E07-FIX-4 ④）。
 *
 * # 口径
 *
 * 归档**不硬拦**在途单据（设计决策 **E4**：`ACTIVE → COMPLETED` 是「归档」，不是「履约完成」，
 * 见 `docs/architecture/ARCH-2-合同表数据模型与校验规则.md` §5.2 与
 * `docs/architecture/合同与勾稽关系设计.md` §6.4 E4）。所以后端照旧放行 200，
 * 由前端在确认弹窗上把「还有几张没关」说清楚，让用户自己决定要不要先处理。
 *
 * 数据来源是**详情接口**的 `open_order_count`（不另外发请求、不在前端重算口径）。
 *
 * @param count 未关闭下游单据数（`open_order_count`）
 * @param target 本次流转的目标态
 * @returns 空串 = 不提示；否则是要显示在弹窗里的整句提示
 */
export function openOrderWarning(count: number | null | undefined, target: string): string {
  if ((target ?? '').trim().toUpperCase() !== 'COMPLETED') {
    return '';
  }
  const open = Number(count ?? 0) || 0;
  if (open <= 0) {
    return '';
  }
  return `归档前请注意：这张合同仍有 ${open} 张未关闭单据。归档后不再接新单，这些单据仍可继续执行到底。`;
}

/** 流转确认文案（说明「从哪个状态到哪个状态」+ 副作用） */
export function transitionConfirmText(
  contractNo: string,
  status: string,
  action: ContractTransition,
  contractType?: string | null,
  openOrderCount?: number | null,
): string {
  const fromKey = (status ?? '').trim().toUpperCase();
  const from = contractStatusLabel(fromKey);
  const to = contractStatusLabel(action.target);
  const tails = isNoOrderContractType(contractType) ? NO_ORDER_EDGE_TAILS : EDGE_CONFIRM_TAILS;
  const tail = tails[`${fromKey}→${action.target}`] ?? '';
  const warning = openOrderWarning(openOrderCount, action.target);
  return `确定把 ${contractNo} 从「${from}」流转到「${to}」吗？${tail}${warning}`;
}

/** 行内/行级动作的显隐（卡面：只有草稿能编辑、只有草稿能删） */
export function contractActionsOf(status: string): { edit: boolean; remove: boolean } {
  const draft = (status ?? '').trim().toUpperCase() === 'DRAFT';
  return { edit: draft, remove: draft };
}

/**
 * 「编辑」入口是否显示。
 *
 * 优先级：**后端详情给的 `can_edit` > 状态判断**。列表行没有 `can_edit`，
 * 按「状态 = DRAFT」判（与后端 `ContractDetailRes::from` 的可编辑口径相同）。
 */
export function canEditContract(row: { status?: string | null; can_edit?: boolean | null }): boolean {
  if (typeof row.can_edit === 'boolean') {
    return row.can_edit;
  }
  return (row.status ?? '').trim().toUpperCase() === 'DRAFT';
}

/** 「删除」入口是否显示（卡面：仅 DRAFT；被引用时后端还会 409 + 66018） */
export function canDeleteContract(row: { status?: string | null }): boolean {
  return (row.status ?? '').trim().toUpperCase() === 'DRAFT';
}

// ==================== 列表 ====================

/** 筛选行（区间用两元素数组，空数组 = 没选） */
export interface ContractFilterState {
  /** 合同号（模糊） */
  contract_no: string;
  /** 名称（模糊） */
  name: string;
  /** 合同类型（`''` = 全部） */
  contract_type: string;
  /** 状态（`''` = 全部） */
  status: string;
  /** 乙方（供应商）id */
  party_b_id: string;
  /** 签订日期区间 `[from, to]` */
  sign_range: string[];
  /** 到期日期区间 `[from, to]` */
  expire_range: string[];
}

/** 空筛选 */
export function emptyContractFilter(): ContractFilterState {
  return {
    contract_no: '',
    name: '',
    contract_type: '',
    status: '',
    party_b_id: '',
    sign_range: [],
    expire_range: [],
  };
}

/** 是否处于筛选态（决定空态文案说「没有匹配」还是「还没有合同」） */
export function isContractFiltered(filter: ContractFilterState): boolean {
  return Boolean(
    filter.contract_no.trim()
      || filter.name.trim()
      || filter.contract_type
      || filter.status
      || filter.party_b_id
      || filter.sign_range.length > 0
      || filter.expire_range.length > 0,
  );
}

/** 区间数组 → 两个边界值（`t-date-range-picker` 的 `[]` / `[from]` / `[from, to]` 都兼容） */
function rangeBounds(range: string[] | null | undefined): [string, string] {
  const values = (range ?? []).map((item) => (item ?? '').trim()).filter(Boolean);
  return [values[0] ?? '', values[1] ?? ''];
}

/**
 * 拼分页查询。
 *
 * 空串 / 空数组一律**不发**（后端把 `?contract_no=` 当「等于空串」过滤，会查出 0 行）。
 * 到期日期区间落后端 `expire_date_from` / `expire_date_to`（对应 `end_date` 列）。
 */
export function buildContractQuery(
  filter: ContractFilterState,
  page: number,
  pageSize: number = CONTRACT_PAGE_SIZE,
): ContractPageQuery {
  const [signFrom, signTo] = rangeBounds(filter.sign_range);
  const [expireFrom, expireTo] = rangeBounds(filter.expire_range);
  const query: ContractPageQuery = {
    page: Math.max(1, Math.trunc(page)),
    page_size: Math.max(1, Math.trunc(pageSize)),
  };
  const contractNo = filter.contract_no.trim();
  const name = filter.name.trim();
  if (contractNo) {
    query.contract_no = contractNo;
  }
  if (name) {
    query.name = name;
  }
  if (filter.contract_type) {
    query.contract_type = filter.contract_type;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.party_b_id) {
    query.party_b_id = filter.party_b_id;
  }
  if (signFrom) {
    query.sign_date_from = signFrom;
  }
  if (signTo) {
    query.sign_date_to = signTo;
  }
  if (expireFrom) {
    query.expire_date_from = expireFrom;
  }
  if (expireTo) {
    query.expire_date_to = expireTo;
  }
  return query;
}

/** 列表区状态（行数 / 总数 / 是否空 / 总数文案） */
export interface ContractListState {
  /** 当前页行 */
  rows: ContractRow[];
  /** 总条数 */
  total: number;
  /** 是否空表 */
  isEmpty: boolean;
  /** 「共 N 条」文案 */
  totalText: string;
}

/**
 * 把一次分页响应用成列表区状态。
 *
 * 空表文案**分两种**（卡面第 1 点「空态友好提示」）：筛选后无结果 vs 一条都没有 ——
 * 两者的下一步动作完全不同（改条件 vs 新建成第一张）。
 */
export function contractListState(
  data: { list?: ContractRow[] | null; total?: number | null } | null | undefined,
): ContractListState {
  const rows = data?.list ?? [];
  const total = data?.total ?? rows.length;
  return {
    rows,
    total,
    isEmpty: rows.length === 0,
    totalText: `共 ${total} 条`,
  };
}

/** 空态文案 */
export function contractEmptyState(filtered: boolean): { title: string; desc: string } {
  return filtered
    ? { title: '没有匹配的合同', desc: '换个条件再查，或点右侧「重置」回全量' }
    : { title: '还没有合同', desc: '点左上角「新建合同」录入第一张' };
}

// ==================== 表单 ====================

/** 表单状态（`id` 为空 = 新建） */
export interface ContractFormState {
  /** 主键（空 = 新建） */
  id: string;
  /** 合同号（后端生成；编辑时只读展示，**不可改**） */
  contract_no: string;
  /** 合同名称 */
  name: string;
  /** 合同类型 */
  contract_type: string;
  /** 乙方（供应商）id */
  party_b_id: string;
  /** 合同金额（含税总额；`undefined` = 没填 —— `t-input-number` 的 v-model 不认 `null`） */
  amount: number | undefined;
  /** 签订日期 */
  sign_date: string;
  /** 生效日 */
  start_date: string;
  /** 到期日 */
  end_date: string;
  /** 备注 */
  remark: string;
}

/** 表单错误（键与字段同名；明细行不涉及） */
export type ContractFormErrors = Partial<Record<keyof ContractFormState, string>>;

/**
 * 空表单。
 *
 * 默认 `PURCHASE`：仓内合同以采购为主（历史 2253 行合同里 `PURCHASE`/`SALES` 占全部），
 * 默认值只是省一次点击，四类都能单选切换。
 */
export function emptyContractForm(): ContractFormState {
  return {
    id: '',
    contract_no: '',
    name: '',
    contract_type: 'PURCHASE',
    party_b_id: '',
    amount: undefined,
    sign_date: '',
    start_date: '',
    end_date: '',
    remark: '',
  };
}

/**
 * 详情 → 表单（编辑回填）。
 *
 * `contract_no` 只回填到只读展示位；改单请求里**没有**这个字段（后端 serde 会丢掉）。
 * 金额转 `number`：`Decimal` 出 JSON 是字符串，`t-input-number` 只认数字。
 */
export function contractFormFromDetail(detail: ContractDetail): ContractFormState {
  return {
    id: detail.id,
    contract_no: detail.contract_no,
    name: detail.name,
    contract_type: detail.contract_type,
    party_b_id: detail.party_b_id,
    amount: detail.amount === null || detail.amount === undefined ? undefined : toNumber(detail.amount),
    sign_date: detail.sign_date ?? '',
    start_date: detail.start_date ?? '',
    end_date: detail.end_date ?? '',
    remark: detail.remark ?? '',
  };
}

/** 日期比较（`YYYY-MM-DD` 直接比字符串即字典序 = 时间序，非法值视为空） */
function dateValue(text: string): string {
  return /^\d{4}-\d{2}-\d{2}/.test(text) ? text.slice(0, 10) : '';
}

/**
 * 提交前校验（**与后端 `CreateContractReq::validate` 同口径**，后端还会再校一遍）。
 *
 * 逐条对应：
 * 1. 名称非空 + ≤ 200 字；
 * 2. 合同类型必须是四类之一（`PURCHASE` / `SALES` / `SERVICE` / `LEASE`，`contract_dto::SUPPORTED_TYPES`）；
 * 3. 乙方必选；
 * 4. 金额必填且 **> 0**（DDL 是 `amount > 0`，卡面写的 `≥ 0` 与 DDL 不符）+ ≤ 4 位小数；
 * 5. 签订日期必填（卡面要求；后端「流转到 SIGNED 前必须三件套齐全」）；
 * 6. 到期日 ≥ 生效日（`ck_biz_contract_date_order`）；
 * 7. 签订日期 ≤ 生效日（`require_sign_not_after_start`，应用层规则）。
 */
export function validateContractForm(form: ContractFormState): ContractFormErrors {
  const errors: ContractFormErrors = {};

  const name = form.name.trim();
  if (!name) {
    errors.name = '合同名称不能为空';
  } else if (name.length > MAX_CONTRACT_NAME) {
    errors.name = `合同名称最长 ${MAX_CONTRACT_NAME} 个字符（当前 ${name.length} 个）`;
  }

  if (!form.contract_type) {
    errors.contract_type = '请选择合同类型';
  } else if (!SUPPORTED_CONTRACT_TYPES.includes(form.contract_type as ContractType)) {
    // 四类全开后这里只剩「脏值」一条路：文案中性点名可选值，不再说「本批次」
    errors.contract_type = `合同类型无效：只能是 ${SUPPORTED_CONTRACT_TYPES.map(contractTypeLabel).join(' / ')}`;
  }

  if (!form.party_b_id) {
    errors.party_b_id = `请选择${contractPartyBLabel(form.contract_type)}`;
  }

  if (form.amount === null || form.amount === undefined || Number.isNaN(Number(form.amount))) {
    errors.amount = '请填写合同金额';
  } else {
    const amount = Number(form.amount);
    if (amount <= 0) {
      errors.amount = '合同金额必须大于 0（后端 ck_biz_contract_amount 是 amount > 0，不支持 0 元合同）';
    } else if (String(amount).split('.')[1]?.length > MAX_AMOUNT_SCALE) {
      errors.amount = `合同金额最多 ${MAX_AMOUNT_SCALE} 位小数（NUMERIC(18,4)）`;
    }
  }

  const sign = dateValue(form.sign_date);
  const start = dateValue(form.start_date);
  const end = dateValue(form.end_date);

  if (!sign) {
    errors.sign_date = '请选择签订日期';
  }
  if (!start) {
    errors.start_date = '请选择有效期起';
  }
  if (!end) {
    errors.end_date = '请选择有效期止';
  }
  if (start && end && end < start) {
    errors.end_date = `有效期止（${end}）不能早于有效期起（${start}）：区间反了`;
  }
  if (sign && start && sign > start) {
    errors.sign_date = `签订日期（${sign}）不能晚于有效期起（${start}）：先签约再生效`;
  }

  if (form.remark.trim().length > MAX_CONTRACT_REMARK) {
    errors.remark = `备注最长 ${MAX_CONTRACT_REMARK} 个字符（当前 ${form.remark.trim().length} 个）`;
  }

  return errors;
}

/** 新建请求体（**不含** `contract_no` / `status`：后端一律忽略，前端干脆不发） */
export function buildCreatePayload(form: ContractFormState): CreateContractPayload {
  return {
    name: form.name.trim(),
    contract_type: form.contract_type,
    party_b_id: form.party_b_id,
    amount: form.amount ?? 0,
    sign_date: dateValue(form.sign_date) || null,
    start_date: dateValue(form.start_date) || null,
    end_date: dateValue(form.end_date) || null,
    remark: form.remark.trim() || null,
  };
}

/**
 * 改单请求体（**只有 DRAFT 能提交**，卡面「编辑时不允许改合同号」）。
 *
 * 空日期 / 空备注显式传 `null`（后端 `double_option`：`null` = 清空），
 * 这样「把到期日删掉」才真的能清空 —— 不传会被当成「保持原值」。
 */
export function buildUpdatePayload(form: ContractFormState): UpdateContractPayload {
  return {
    name: form.name.trim(),
    party_b_id: form.party_b_id,
    amount: form.amount ?? 0,
    sign_date: dateValue(form.sign_date) || null,
    start_date: dateValue(form.start_date) || null,
    end_date: dateValue(form.end_date) || null,
    remark: form.remark.trim() || null,
  };
}

/** 表单整体提示（错误条数文案，点保存时用） */
export function formErrorMessage(errors: ContractFormErrors): string {
  const count = Object.keys(errors).length;
  return count > 0 ? `表单还有 ${count} 处需要修正` : '';
}

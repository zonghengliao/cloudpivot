/**
 * 供应商准入 / 黑名单页面纯逻辑（M4-14 后端 + 本卡前端）。
 *
 * 与 `.vue` 分家的理由：这些判据要被 `pnpm test:unit` 直接打（前端历史上最容易漂的是
 * 「空串筛选被当成有效值」和「理由字数按字节算」两处），SFC 里的东西单测加载不到。
 *
 * # 口径来源（不在页面里重新发明）
 *
 * | 口径 | 出处 |
 * |------|------|
 * | 类型 / 状态闭集与中文名 | `supplier_access_model::{ACCESS_TYPES,ACCESS_STATUSES,type_label,status_label}` |
 * | 理由最小字数 5 | `supplier_access_model::MIN_REASON_CHARS`（= 库层 `ck_biz_sa_reason_on_submit`） |
 * | 关键字 ≤64 字符 | `AccessPageQuery::filter`（超了后端 400） |
 * | 启用 / 停用 1 / 0 | `SUPPLIER_ENABLED` / `SUPPLIER_DISABLED` |
 * | 可改 / 可提交 | 行上的 `editable` / `submittable`（**不自己推状态机**） |
 */
import type { AccessRow, AccessStatus, AccessType } from '@/api/business/erp/supplier-access';

/** 默认每页条数（与后端 `AccessPageQuery::filter` 的默认值一致） */
export const PAGE_SIZE = 20;

/** 关键字长度上限（后端 `MAX_KEYWORD`：超了 400，页面先提示不截断） */
export const MAX_KEYWORD = 64;

/** 提交时的最小理由字数（后端 `MIN_REASON_CHARS` = 库层 CHECK 的同一个 5） */
export const MIN_REASON_CHARS = 5;

/**
 * 申请类型下拉（顺序与后端 `ACCESS_TYPES` 一致）。
 *
 * `hint` 是给用户看的「这个类型什么时候合法」——判据在后端 `check_start`，
 * 这里只是文案（写错了不会导致按钮出现/消失，只会误导人）。
 */
export const TYPE_OPTIONS: { value: AccessType; label: string; hint: string }[] = [
  {
    value: 'ADMIT',
    label: '准入',
    hint: '供应商还没进合格名录（从未通过准入）且不在黑名单里 → 通过后启用（可下单）',
  },
  {
    value: 'BLACKLIST',
    label: '黑名单',
    hint: '供应商当前不在黑名单里 → 通过后停用（不可下单），必须写理由与评估等级',
  },
  {
    value: 'RELEASE',
    label: '解除黑名单',
    hint: '供应商当前在黑名单里 → 通过后恢复启用（可下单）',
  },
];

/** 状态下拉（值里**不放** `''` 当「全部」：TDesign 把空串当未选，会显示成 placeholder） */
export const STATUS_OPTIONS: { value: AccessStatus; label: string }[] = [
  { value: 'DRAFT', label: '拟稿' },
  { value: 'SUBMITTED', label: '审批中' },
  { value: 'APPROVED', label: '已生效' },
];

/** 评估等级快照下拉（**只在黑名单单上出现**，与 `ck_biz_sa_grade_only_blacklist` 同一条判定） */
export const GRADE_OPTIONS: { value: 'A' | 'B' | 'C' | 'D'; label: string }[] = [
  { value: 'A', label: 'A（优秀）' },
  { value: 'B', label: 'B（良好）' },
  { value: 'C', label: 'C（合格需改进）' },
  { value: 'D', label: 'D（不合格，拉黑候选）' },
];

/** 列表筛选状态（键 = `FilterDef.key`） */
export type AccessFilterState = {
  /** 关键字：单号 / 供应商编码 / 供应商名 / 理由 */
  keyword: string;
  /** 状态（`''` = 全部） */
  status: AccessStatus | '';
  /** 类型（`''` = 全部） */
  access_type: AccessType | '';
};

/** 空筛选（重置 / 首屏都用它，别在页面里写字面量） */
export function emptyAccessFilter(): AccessFilterState {
  return { keyword: '', status: '', access_type: '' };
}

/**
 * 分页入参拼装。
 *
 * * `keyword` trim 后为空 → **整个键不传**（其它页的教训：`?keyword=` 会查出 0 行）；
 * * `status` / `access_type` 空串 → 不传（后端空串也当没筛，少一个键少一次歧义）；
 * * `page` / `page_size` 归一成正整数（后端也会 clamp，但请求里别发 `page=0`）；
 * * 关键字超长**不截断**：后端 400 的报文比「悄悄截断后查出一堆不相干的行」有用。
 */
export function buildAccessQuery(
  filter: AccessFilterState,
  page: number,
  pageSize: number,
): Record<string, unknown> {
  const query: Record<string, unknown> = {
    page: Math.max(1, Math.trunc(page) || 1),
    page_size: Math.max(1, Math.trunc(pageSize) || PAGE_SIZE),
  };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.access_type) {
    query.access_type = filter.access_type;
  }
  return query;
}

/** 类型中文（后端 `access_type_label` 缺失时兜底；未知类型**不猜成准入**） */
export function accessTypeLabel(type: string): string {
  switch (type) {
    case 'ADMIT':
      return '准入';
    case 'BLACKLIST':
      return '黑名单';
    case 'RELEASE':
      return '解除黑名单';
    default:
      return '未知类型';
  }
}

/** 状态中文（后端 `status_label` 缺失时兜底） */
export function accessStatusLabel(status: string): string {
  switch (status) {
    case 'DRAFT':
      return '拟稿';
    case 'SUBMITTED':
      return '审批中';
    case 'APPROVED':
      return '已生效';
    default:
      return '未知状态';
  }
}

/** 状态标签主题（拟稿灰 / 审批中警示黄 / 已生效绿） */
export function statusTheme(status: string): 'default' | 'warning' | 'success' {
  switch (status) {
    case 'SUBMITTED':
      return 'warning';
    case 'APPROVED':
      return 'success';
    default:
      return 'default';
  }
}

/**
 * 类型标签主题。
 *
 * 收紧方向（黑名单）用红、恢复方向（准入 / 解除）用绿 —— 一眼能看出「这单会让供应商
 * 能不能下单」。不是状态语义，所以不与 `statusTheme` 共用。
 */
export function typeTheme(type: string): 'danger' | 'success' | 'default' {
  switch (type) {
    case 'BLACKLIST':
      return 'danger';
    case 'ADMIT':
    case 'RELEASE':
      return 'success';
    default:
      return 'default';
  }
}

/** 供应商现值状态中文（1 启用 / 0 停用） */
export function supplierStateLabel(status: number | null | undefined): string {
  return status === 1 ? '启用' : '停用';
}

/**
 * 「通过后」预览：`准入 / 解除 → 启用`、`黑名单 → 停用`。
 *
 * 读行上的 `target_supplier_status`（后端按 `access_type` 给的），**不在这里按类型重推**。
 */
export function targetStateLabel(row: Pick<AccessRow, 'target_supplier_status'>): string {
  return row.target_supplier_status === 1 ? '启用（可下单）' : '停用（不可下单）';
}

/** 申请人（姓名快照；没有就 `—`，不显示 uuid） */
export function applicantText(row: Pick<AccessRow, 'apply_by_name'>): string {
  return row.apply_by_name?.trim() ? row.apply_by_name : '—';
}

/** 结论人（姓名快照 + 结论时间；未结论显示 `—`） */
export function decidedText(row: Pick<AccessRow, 'decided_by_name' | 'decided_at'>): string {
  if (!row.decided_at) {
    return '—';
  }
  const who = row.decided_by_name?.trim() || '（已删除用户）';
  return `${who} · ${timeText(row.decided_at)}`;
}

/** 时间：`2026-11-18T09:30:00+08:00` → `2026-11-18 09:30`（解析不了就原样给） */
export function timeText(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const match = /^(\d{4}-\d{2}-\d{2})[T ](\d{2}:\d{2})/.exec(value);
  return match ? `${match[1]} ${match[2]}` : value;
}

/**
 * 理由字数（**按字符数**，不是 `String.length` 也不是字节数）。
 *
 * 后端 `check_reason` 用 `chars().count()`：`"供应商失联"` 是 5 个字（已够），
 * 而 `'😀😀😀😀'` 这类星星字符在 UTF-16 里长度是 8 —— 用 `.length` 会让用户
 * 看到「够 5 字」却被后端 400。
 */
export function reasonLength(reason: string | null | undefined): number {
  return [...(reason ?? '').trim()].length;
}

/**
 * 提交前的理由校验（**只拦后端一定会 400 的那一条**，不做多余拦截）。
 *
 * 返回 `null` = 通过；否则是给用户看的文案。
 */
export function reasonError(reason: string | null | undefined): string | null {
  const length = reasonLength(reason);
  if (length === 0) {
    return `提交审批必须写申请理由（至少 ${MIN_REASON_CHARS} 个字：谁、为什么、依据什么）`;
  }
  if (length < MIN_REASON_CHARS) {
    return `申请理由至少 ${MIN_REASON_CHARS} 个字（当前 ${length} 个）`;
  }
  return null;
}

/** 关键字超长提示（不拦提交，只提示后端会 400 —— 与列表筛选的 `keywordWarning` 同款） */
export function keywordWarning(keyword: string): string {
  const length = [...keyword.trim()].length;
  return length > MAX_KEYWORD ? `关键字 ${length} 个字符，超过后端上限 ${MAX_KEYWORD}，查询会 400` : '';
}

/** 这行现在能改 / 能删（读后端 `editable`，草稿之外一律不能） */
export function canEditRow(row: Pick<AccessRow, 'editable'>): boolean {
  return row.editable === true;
}

/** 这行现在能提交（读后端 `submittable`：草稿 + 理由够字） */
export function canSubmitRow(row: Pick<AccessRow, 'submittable'>): boolean {
  return row.submittable === true;
}

/**
 * 为什么「提交」按钮是灰的（tooltip 文案；能提交时返回空串）。
 *
 * 判据与后端 `to_row_res` 的 `submittable` 逐字对应，不另立一套：
 * 非草稿 → 状态原因；草稿但理由不够 → 字数原因。
 */
export function submitBlockedTip(row: Pick<AccessRow, 'status' | 'reason' | 'submittable'>): string {
  if (canSubmitRow(row)) {
    return '';
  }
  if (row.status !== 'DRAFT') {
    return `只有草稿能提交（当前：${accessStatusLabel(row.status)}）`;
  }
  return `理由至少 ${MIN_REASON_CHARS} 个字才能提交（当前 ${reasonLength(row.reason)} 个）：先点「编辑」补上`;
}

/** 列表空态文案（分「有筛选」与「真没有单」两种情况） */
export function emptyListText(filter: AccessFilterState): string {
  if (filter.keyword.trim() || filter.status || filter.access_type) {
    return '没有符合条件的申请单：放宽关键字 / 状态 / 类型再试';
  }
  return '还没有准入 / 黑名单申请单。点「新建申请」发起一张，审批通过后供应商状态才会变';
}

/** 类型的一句话说明（新建表单里跟着单选走） */
export function typeHint(type: AccessType | ''): string {
  return TYPE_OPTIONS.find((item) => item.value === type)?.hint ?? '先选类型：准入 / 黑名单 / 解除黑名单';
}

/** 详情抽屉标题 */
export function detailTitle(
  row: Pick<AccessRow, 'access_no' | 'access_type' | 'access_type_label'> | null,
): string {
  if (!row) {
    return '申请单详情';
  }
  return `${row.access_no} · ${row.access_type_label || accessTypeLabel(row.access_type)}`;
}

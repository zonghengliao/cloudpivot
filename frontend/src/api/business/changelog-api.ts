/**
 * 变更日志接口（批次 C · C6）。
 *
 * 路径与后端 `backend/src/business/changelog/handler.rs` 逐条对应。
 *
 * # 四条口径
 *
 * 1. **权限码是 `system:change-log:*`**（`list` / `export` / `clean`），不是 PRD 的
 *    `business:changelog:*`；前端一律以后端注解 + `V0085` 种子为准。
 * 2. **库内永不存明文**：`fields[].masked` / `redacted` 为 true 时展示的就是脱敏值，
 *    页面只加标记，不再做二次打码。
 * 3. **大字段摘要化**：超 2 KiB 的字段不出原值，出 `{len, sha256, preview, redacted}`；
 *    页面必须显示「已截断，共 N 字节」，而不是假装这是完整内容。
 * 4. **导出返回 xlsx 字节流**（不走 `Res<T>` 统一响应体），必须用 `responseType: 'blob'`，
 *    并且要带鉴权头 —— 所以不能 `<a href>`，统一取 blob 再 `saveBlob`。
 */
import service, { get, type PageData } from '@/api/request';

/**
 * 把强类型查询对象交给 axios 的 `params`。
 *
 * 两点处理（都踩过）：
 * 1. `Record<string, unknown>` 需要索引签名，接口类型没有，所以做一次断言；
 * 2. **丢掉空串 / null / undefined**：页面里 `query.xxx = ''` 表示「不过滤」，
 *    把空串发过去会被后端当成「xxx 等于空串」的条件（实测会查出 0 条）。
 */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/** 分页查询（对应后端 `ChangeLogPageQuery`） */
export interface ChangeLogPageQuery {
  /** 起始时间（`YYYY-MM-DD` / `YYYY-MM-DD HH:mm:ss` / RFC3339） */
  start_time?: string;
  /** 结束时间 */
  end_time?: string;
  /** 操作人 id */
  operator_id?: string;
  /** 操作人部门 id */
  operator_dept_id?: string;
  /** 操作人名字（模糊） */
  operator_name?: string;
  /** 模块（`system` / `business` …） */
  module?: string;
  /** 资源表名（`sys_user` …） */
  resource?: string;
  /** 业务主键 */
  resource_id?: string;
  /** 动作（`CREATE` / `UPDATE` / `DELETE` …） */
  action?: string;
  /** 结果（`SUCCESS` / `FAILED` …） */
  result?: string;
  /** 全局关键字 */
  keyword?: string;
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 单页上限（与 page_size 二选一） */
  limit?: number;
}

/** 列表行（对应后端 `ChangeLogListItem`） */
export interface ChangeLogListItem {
  /** 行 id */
  id: string;
  /** 发生时刻（RFC3339） */
  created_at: string;
  /** 模块 */
  module: string;
  /** 资源表名 */
  resource: string;
  /** 业务主键 */
  resource_id: string;
  /** 动作码 */
  action: string;
  /** 动作中文 */
  action_label: string;
  /** 操作人 id */
  operator_id: string | null;
  /** 操作人名字（冗余存储） */
  operator_name: string | null;
  /** 来源（`API` / `JOB` / `IMPORT` / `SYSTEM`） */
  trace_source: string;
  /** 变化的字段名 */
  changed_fields: string[];
  /** 是否含脱敏值 */
  masked: boolean;
  /** 结果 */
  result: string;
}

/** 逐字段前后值（对应后端 `FieldDiffRes`） */
export interface FieldDiff {
  /** 字段名 */
  field: string;
  /** 旧值（`{b,a}` 的 b） */
  before: unknown;
  /** 新值（`{b,a}` 的 a） */
  after: unknown;
  /** 该字段是否含脱敏值 */
  masked: boolean;
  /** 是否整列被遮蔽（如密码字段） */
  redacted: boolean;
}

/** 大字段摘要（对应后端 `BigSummaryRes`） */
export interface BigSummary {
  /** 原始字节数 */
  len: number;
  /** 原文 sha256（可用于「内容是否真的变了」的比对） */
  sha256: string;
  /** 前若干字节的预览 */
  preview: string;
  /** 是否被脱敏 */
  redacted: boolean;
}

/** 大字段前后摘要（对应后端 `BigFieldRes`） */
export interface BigField {
  /** 字段名 */
  field: string;
  /** 旧摘要 */
  before: BigSummary;
  /** 新摘要 */
  after: BigSummary;
}

/** 详情（对应后端 `ChangeLogDetailRes`） */
export interface ChangeLogDetail {
  /** 行 id */
  id: string;
  /** 发生时刻 */
  created_at: string;
  /** 模块 */
  module: string;
  /** 资源表名 */
  resource: string;
  /** 业务主键 */
  resource_id: string;
  /** 动作码 */
  action: string;
  /** 动作中文 */
  action_label: string;
  /** 请求 ID（可与 `operation_logs` join） */
  request_id: string | null;
  /** 来源 */
  trace_source: string;
  /** 操作人 id */
  operator_id: string | null;
  /** 操作人名字 */
  operator_name: string | null;
  /** 操作人部门 id */
  operator_dept_id: string | null;
  /** 客户端 IP */
  client_ip: string | null;
  /** 变化字段名 */
  changed_fields: string[];
  /** 逐字段 diff */
  fields: FieldDiff[];
  /** 大字段摘要 */
  big: BigField[];
  /** 结果 */
  result: string;
  /** diff 模式（`WHITELIST` …） */
  diff_mode: string;
  /** 耗时（ms） */
  duration_ms: number | null;
  /** 是否含脱敏值 */
  masked: boolean;
  /** 原始 diff JSON（调试用） */
  diff: unknown;
}

/** 时间线查询（对应后端 `ChangeLogTimelineQuery`） */
export interface ChangeLogTimelineQuery {
  /** 表名 */
  table?: string;
  /** 业务主键 */
  target_id?: string;
  /** 起始时间 */
  start_time?: string;
  /** 结束时间 */
  end_time?: string;
  /** 条数上限 */
  limit?: number;
}

/** 时间线条目（对应后端 `ChangeLogTimelineItem`） */
export interface ChangeLogTimelineItem {
  /** 行 id */
  id: string;
  /** 发生时刻 */
  created_at: string;
  /** 动作码 */
  action: string;
  /** 动作中文 */
  action_label: string;
  /** 操作人名字 */
  operator_name: string | null;
  /** 变化字段名 */
  changed_fields: string[];
  /** 原始 diff JSON */
  diff: unknown;
}

/** 时间线响应（对应后端 `ChangeLogTimelineRes`） */
export interface ChangeLogTimeline {
  /** 总条数 */
  total: number;
  /** 取用上限 */
  limit: number;
  /** 条目 */
  items: ChangeLogTimelineItem[];
}

/** 统计桶（对应后端 `CountBucketRes`） */
export interface CountBucket {
  /** 分组键 */
  key: string;
  /** 计数 */
  count: number;
}

/** 统计响应（对应后端 `ChangeLogStatsRes`） */
export interface ChangeLogStats {
  /** 总数 */
  total: number;
  /** 按模块 */
  by_module: CountBucket[];
  /** 按操作人 */
  by_operator: CountBucket[];
  /** 按天 */
  by_day: CountBucket[];
}

/** 登记表条目（对应后端 `TableRegistryEntryRes`） */
export interface TableRegistryEntry {
  /** 表名 */
  table: string;
  /** 模块归属 */
  module: string;
  /** 白名单字段 */
  fields: string[];
  /** 大字段列 */
  big_fields: string[];
}

/** 登记表清单（对应后端 `TableRegistryRes`） */
export interface TableRegistry {
  /** 登记表 */
  tables: TableRegistryEntry[];
}

/** 全局检索分页（`system:change-log:list`） */
export function pageChangeLogs(query: ChangeLogPageQuery = {}): Promise<PageData<ChangeLogListItem>> {
  return get<PageData<ChangeLogListItem>>('/api/business/changelog/page', asParams(query));
}

/** 单条详情（`created_at` 传了能少扫分区） */
export function getChangeLog(id: string, createdAt?: string): Promise<ChangeLogDetail> {
  return get<ChangeLogDetail>(`/api/business/changelog/${id}`, createdAt ? { created_at: createdAt } : undefined);
}

/** 某对象的变更时间线（谁在什么时候改过它） */
export function getChangeLogTimeline(query: ChangeLogTimelineQuery): Promise<ChangeLogTimeline> {
  return get<ChangeLogTimeline>('/api/business/changelog/timeline', asParams(query));
}

/** 统计（按模块 / 操作人 / 天） */
export function getChangeLogStats(query: ChangeLogPageQuery = {}): Promise<ChangeLogStats> {
  return get<ChangeLogStats>('/api/business/changelog/stats', asParams(query));
}

/** 已登记的表与字段清单（不查库，编译期常量） */
export function getTableRegistry(): Promise<TableRegistry> {
  return get<TableRegistry>('/api/business/changelog/table-registry');
}

/**
 * 导出变更日志（xlsx 字节流，导出前已跑过 L1+L3 再脱敏）。
 *
 * # 参数
 * - `query`：与列表同口径的筛选条件（分页字段会被忽略）。
 */
export async function exportChangeLogs(query: ChangeLogPageQuery = {}): Promise<Blob> {
  const response = await service.get<Blob>('/api/business/changelog/export', {
    params: asParams(query),
    responseType: 'blob',
  });
  return response.data;
}

/** 变更日志接口集合 */
export const changelogApi = {
  pageChangeLogs,
  getChangeLog,
  getChangeLogTimeline,
  getChangeLogStats,
  getTableRegistry,
  exportChangeLogs,
};

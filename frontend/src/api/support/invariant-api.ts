import { get, post, type PageData } from '@/api/request';

/** 一条检查项（与后端 `CheckItem` / `scripts/verify/erp_invariants.py` 同名字段） */
export interface CheckItem {
  /** 检查项名称 */
  name: string;
  /** 检查分组（会计 / 账账 / 账实 / 单据链 / 状态 / 期间 / 闭环 / 审批 / 盘点 / 固资） */
  group: string;
  /** 级别：BLOCKER / WARN / INFO */
  level: string;
  /** 是否通过 */
  ok: boolean;
  /** 说明（失败时「差多少、差在哪几条」） */
  detail: string;
}

/** 一次巡检运行的摘要 */
export interface InvariantResultRes {
  /** 结果 id */
  id: string;
  /** 本次巡检开始时间 */
  run_at: string;
  /** 触发方式：SCHEDULED / MANUAL */
  trigger_type: string;
  /** 运行结论：OK / FAIL / ERROR */
  status: string;
  /** 检查总数 */
  total: number;
  /** 通过数 */
  passed: number;
  /** 失败数 */
  failed: number;
  /** BLOCKER 数（>0 表示账已错） */
  blockers: number;
  /** WARN 数 */
  warns: number;
  /** 耗时（毫秒） */
  duration_ms: number;
  /** 巡检目标库名 */
  database_name: string;
  /** 实际执行的脚本路径 */
  script_path: string;
  /** status=ERROR 时的原因（其它情况为空串） */
  error: string;
}

/** 一次巡检运行的完整记录 */
export interface InvariantResultDetailRes extends InvariantResultRes {
  /** 全量检查明细 */
  detail: CheckItem[];
  /** 失败项子集 */
  failures: CheckItem[];
}

/** 趋势点（某一天最后一次运行 + 当天运行次数） */
export interface TrendPointRes {
  /** 业务日 YYYY-MM-DD（北京时间） */
  date: string;
  /** 当天运行次数 */
  runs: number;
  /** 当天最后一次运行的结论 */
  status: string;
  /** 检查总数 */
  total: number;
  /** 通过数 */
  passed: number;
  /** 失败数 */
  failed: number;
  /** BLOCKER 数 */
  blockers: number;
  /** WARN 数 */
  warns: number;
}

/** 内置调度口径 */
export interface ScheduleRes {
  /** 任务名 */
  task_name: string;
  /** cron 表达式 */
  cron: string;
  /** 人话描述 */
  description: string;
}

/** 看板总览 */
export interface InvariantOverviewRes {
  /** 趋势窗口天数 */
  window_days: number;
  /** 装配时间 */
  generated_at: string;
  /** 最近一次运行（从未跑过为 null） */
  latest: InvariantResultRes | null;
  /** 每日趋势（升序；没跑的天不补零） */
  trend: TrendPointRes[];
  /** 当前失败项（取最近一次运行的 failures） */
  failures: CheckItem[];
  /** 连续非 OK 的运行次数 */
  fail_streak: number;
  /** 窗口内运行次数 */
  runs_in_window: number;
  /** 内置调度口径 */
  schedule: ScheduleRes;
}

/** 运行历史分页参数 */
export type InvariantPageQuery = {
  /** 页码，从 1 开始 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 结论过滤：OK / FAIL / ERROR */
  status?: string;
};

/** 看板总览：最近一次结论 + 近 N 天趋势 + 当前失败项 */
export function getInvariantOverview(days = 30): Promise<InvariantOverviewRes> {
  return get<InvariantOverviewRes>('/api/support/invariant/overview', { days });
}

/** 运行历史分页 */
export function pageInvariantResults(query: InvariantPageQuery): Promise<PageData<InvariantResultRes>> {
  return get<PageData<InvariantResultRes>>('/api/support/invariant/page', query);
}

/** 单次运行完整明细 */
export function getInvariantResult(id: string): Promise<InvariantResultDetailRes> {
  return get<InvariantResultDetailRes>(`/api/support/invariant/${id}`);
}

/** 立即执行一次巡检并落库（同步等待，最长 300s） */
export function runInvariantAudit(): Promise<InvariantResultDetailRes> {
  return post<InvariantResultDetailRes>('/api/support/invariant/run');
}

/** 账务巡检模块接口集合 */
export const invariantApi = {
  getInvariantOverview,
  pageInvariantResults,
  getInvariantResult,
  runInvariantAudit,
};

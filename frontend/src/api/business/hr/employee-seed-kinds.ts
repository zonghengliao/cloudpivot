/**
 * 「从系统用户生成员工档案」（`seed-from-users`）的契约与纯逻辑（HR-17 后端 → 本卡前端化）。
 *
 * 纯逻辑文件：**不 import 请求层**，`tsx --test` 里可以直接 import 做单测。
 * 请求函数在 `./employee.ts::{seedFromUsersPreview, seedFromUsersApply}`，
 * 那里 `export * from './employee-seed-kinds'` —— 页面只认
 * `@/api/business/hr/employee` 一个入口。
 *
 * # 唯一真源：`backend/src/business/hr/employee_seed.rs`
 *
 * | 常量 | 后端 |
 * |---|---|
 * | `SEED_ROUTE` | `ROUTE_SEED = "/business/hr/employee/seed-from-users"` |
 * | `SEED_PERM` | `PERM_SEED = "business:hr:employee:create"` |
 * | `SEED_MAX_CANDIDATES` | `MAX_CANDIDATES = 500` |
 * | `SEED_SKIP_REASONS` | `SeedSkip::code()` 的 6 个码 |
 * | `SEED_FAILED_REASON` | 落库失败行的 `reason = "CREATE_FAILED"` |
 *
 * 单测（`tests/unit/hr-employee-seed.test.ts`）直接读那份 Rust 源码逐条对拍，
 * 后端改码值 / 改上限，前端先红。
 *
 * # 三个「面板上必须说清楚」的语义
 *
 * 1. **`dry_run` 缺省 `true`**（后端 `is_dry_run()` = `unwrap_or(true)`）：
 *    不传参数只出一份报告，预览路径一次 `INSERT` 都不发、也不占工号
 *    （`next_no` 是 `peek`，不推进 `biz_seq`）。所以预览行里的 `emp_no` 恒为 `null`。
 * 2. **跳过 ≠ 失败**：`SKIP` 是「这个账号本来就不该建档」（已有档案 / 非人类账号 /
 *    停用 / 没姓名 / 姓名超长 / 没部门），不是错误；`FAILED` 是「判定通过但库层拒了」
 *    （唯一键 / CHECK），逐条回滚、只影响这一条。
 * 3. **卡面写的「冲突 K 人（身份证 / 手机重复）」在后端不存在**：`build_write` 明确
 *    **不猜敏感字段**（`id_card` 留空，因此不可能撞 `uk_biz_employee_idcard`），
 *    `biz_employee` 也没有手机号唯一键。真正可能撞的是 `emp_no`（取号器保证）
 *    和 `user_id`（候选查询已 `LEFT JOIN` 排除）—— 撞了会落 `FAILED` 桶并带库层人话。
 *    所以面板给的是**四桶**：扫描 / 将新建 / 跳过 / 失败，跳过再按原因分组。
 */

// ==================== 契约常量 ====================

/** 路由（不含 `/api` 前缀，由后端 `[router].prefix` 注入；前端调用时必须补全） */
export const SEED_ROUTE = '/business/hr/employee/seed-from-users';

/** 权限码：与「新建员工」共用 `business:hr:employee:create` */
export const SEED_PERM = 'business:hr:employee:create';

/** 单次上限（后端超限直接 400：逐条开事务，一次几千个会把连接池按几分钟） */
export const SEED_MAX_CANDIDATES = 500;

/** 一次性可选项里的「每批」提示（超限时面板让 HR 按部门分批） */
export const SEED_BATCH_HINT = '请先选一个部门分批生成（每批逐条开事务，一次几千个会把连接池按几分钟）';

/** 预览态横幅文案（卡面要求「dry-run 明确显示不会写库」） */
export const SEED_PREVIEW_NOTICE =
  '预览 · 不会写库：本次调用 dry_run=true，一次 INSERT 都不发，也不占工号（下一号只是 peek，不推进号段）。';

/** 执行态横幅文案 */
export const SEED_APPLIED_NOTICE =
  '已落库：每条一个事务，失败的那条单独回滚、不影响其它条（失败的账号修正后可再点一次，已建档的会自动跳过）。';

/** 横幅文案（一条整句，交给 `t-alert` 的 message 槽；别在模板里再拼一遍前因，会重复） */
export function seedNoticeText(dryRun: boolean): string {
  return dryRun ? SEED_PREVIEW_NOTICE : SEED_APPLIED_NOTICE;
}

// ==================== 动作 / 原因码 ====================

/** 一条候选的处置动作（后端 `SeedAction`，`SCREAMING_SNAKE_CASE`） */
export const SEED_ACTIONS = ['CREATE', 'SKIP', 'FAILED'] as const;
/** 动作码值 */
export type SeedAction = (typeof SEED_ACTIONS)[number];

/** 跳过原因（后端 `SeedSkip::code()`，顺序与后端判定顺序一致；**别改名**） */
export const SEED_SKIP_REASONS = [
  'HAS_EMPLOYEE',
  'NOT_HUMAN',
  'USER_DISABLED',
  'NAME_MISSING',
  'NAME_TOO_LONG',
  'DEPT_MISSING',
] as const;
/** 跳过原因码值 */
export type SeedSkipReason = (typeof SEED_SKIP_REASONS)[number];

/** 落库失败行的原因码（后端硬编码 `"CREATE_FAILED"`） */
export const SEED_FAILED_REASON = 'CREATE_FAILED';

/** 预览行上出现过的所有原因码 */
export type SeedReason = SeedSkipReason | typeof SEED_FAILED_REASON;

/**
 * 原因码 → 中文短名（**面板标签**，不是后端报文）。
 *
 * 后端 `preview[].message` 已经是完整人话（带账号名 / 已有工号 / 怎么修），
 * 这里只给「分组计数」和「窄列」用的短标签；两处都显示，别只显示这个短名。
 */
export const SEED_REASON_LABELS: Record<SeedReason, string> = {
  HAS_EMPLOYEE: '已有档案',
  NOT_HUMAN: '非人类账号',
  USER_DISABLED: '账号已停用',
  NAME_MISSING: '缺真实姓名',
  NAME_TOO_LONG: '姓名超长',
  DEPT_MISSING: '缺部门',
  CREATE_FAILED: '落库失败',
};

/** 原因码 → 短名的兜底（后端新增码值时不让界面显示空白 / `undefined`） */
export function reasonLabel(reason: string | null | undefined): string {
  if (!reason) {
    return '';
  }
  return (SEED_REASON_LABELS as Record<string, string>)[reason] ?? reason;
}

// ==================== 入参 / 出参（逐字段对齐 employee_seed.rs） ====================

/**
 * 入参（后端 `EmployeeSeedReq`：三个字段全可选，缺省都取安全值）。
 *
 * - `dry_run` 缺省 `true`：**不传不敢写**；
 * - `hire_date` 缺省今天（后端本地时区）—— 存量账号的真实入职日期机器不知道，
 *   面板默认填业务日（`+08:00`），HR 可改；
 * - `dept_id` 缺省 `None`：既作**筛选**（只处理「本部门 + 无部门」的账号），
 *   也作**缺省部门**（无部门的账号挂到这里）。不传 = 用账号自己的部门。
 */
export interface SeedFromUsersReq {
  /** 目标部门（`sys_dept.id`） */
  dept_id?: string;
  /** 入职日期 `yyyy-MM-dd` */
  hire_date?: string;
  /** `true` = 只预览（缺省 `true`，后端 `unwrap_or(true)` 兜底） */
  dry_run?: boolean;
}

/** 一条候选的处置结果（`preview[]` 的一行，后端 `SeedItem`） */
export interface SeedItem {
  /** 账号 id */
  user_id: string;
  /** 登录账号 */
  username: string;
  /** 账号上的真实姓名（空串 = 账号没填姓名） */
  real_name: string;
  /** 账号部门 */
  dept_id: string | null;
  /** 账号部门名（无部门时为空串） */
  dept_name: string;
  /** 本轮对这条候选做了什么 */
  action: SeedAction;
  /** 跳过 / 失败的原因码（`CREATE` 时为空） */
  reason: string | null;
  /** 人话说明（成功 / 跳过 / 失败都有，后端已带上下文） */
  message: string;
  /** 落库后的工号（dry-run 恒为 `null`） */
  emp_no: string | null;
  /** 落库后的员工档案 id（dry-run 恒为 `null`） */
  employee_id: string | null;
  /** 失败时的业务码（与 `ApiError.code` 一致） */
  error_code: number | null;
}

/**
 * 出参（后端 `EmployeeSeedRes`）。
 *
 * 比设计 §5.2 #10 多三个字段，都是「不给前端就得再查一次」的：`failed`（失败的桶）、
 * `planned_create`（dry-run 下 `created` 恒 0，但 HR 要知道点下去会建几个）、
 * `next_no`（dry-run 顺手给的下一号，`peek` 不耗号；批量不逐个预测）。
 */
export interface SeedResult {
  /** 本次是否 dry-run（回显） */
  dry_run: boolean;
  /** 实际使用的入职日期 */
  hire_date: string;
  /** 实际使用的目标部门（回显） */
  dept_id: string | null;
  /** 扫描到的候选账号数（数据范围 + 部门筛选之后） */
  scanned: number;
  /** 判定为「可建档」的条数（dry-run 与真实执行都给） */
  planned_create: number;
  /** **实际写入**的员工数（dry-run 恒 0） */
  created: number;
  /** 跳过的条数 */
  skipped: number;
  /** 建档失败的条数 */
  failed: number;
  /** 下一个工号（仅 dry-run 且有计划建档时给） */
  next_no: string | null;
  /** 逐条处置明细（顺序 = 候选查询顺序：账号创建时间、用户名） */
  preview: SeedItem[];
}

// ==================== 汇总 / 分桶（面板全部走这里，模板里不写算术） ====================

/** 表格上方的筛选项（「只看将新建」最常用：跳过行常常占满一屏） */
export const SEED_BUCKETS = ['all', 'create', 'skip', 'failed'] as const;
/** 分桶键 */
export type SeedBucket = (typeof SEED_BUCKETS)[number];

/** 分桶 → 中文（执行后 `create` 一档改叫「已建档」） */
export function bucketLabel(bucket: SeedBucket, dryRun = true): string {
  switch (bucket) {
    case 'create':
      return dryRun ? '将新建' : '已建档';
    case 'skip':
      return '跳过';
    case 'failed':
      return '失败';
    default:
      return '全部';
  }
}

/** 跳过原因分组的明细行（面板上「跳过 M 人」下面那几行小字） */
export interface SeedReasonCount {
  /** 原因码 */
  reason: string;
  /** 短名 */
  label: string;
  /** 条数（> 0 才会出现在汇总里） */
  count: number;
}

/** 面板汇总（数字全部取自后端回执，不从明细里再数一遍） */
export interface SeedSummary {
  /** 扫描到的候选数 */
  scanned: number;
  /** 将新建（dry-run）/ 本次新建（执行后看 `created`） */
  plannedCreate: number;
  /** 实际写入数（dry-run 恒 0） */
  created: number;
  /** 跳过数 */
  skipped: number;
  /** 失败数 */
  failed: number;
  /** 跳过原因分组（按后端判定顺序，0 条的组不出现） */
  skipBreakdown: SeedReasonCount[];
  /** 失败原因分组（通常只有 `CREATE_FAILED` 一组） */
  failedBreakdown: SeedReasonCount[];
  /** 明细里是否还有「将新建 / 已建档」的行 */
  hasCreatable: boolean;
}

/**
 * 汇总回执。
 *
 * 数字直接用后端的 `scanned / planned_create / created / skipped / failed`：
 * 前端从 `preview[]` 重数一遍会给出第二套真值，两边一旦不一致（后端版本漂移）
 * 面板就会自相矛盾。分组明细才从 `preview[]` 数 —— 那是后端没给的维度。
 */
export function summarizeSeed(res: SeedResult): SeedSummary {
  const skipCounts = new Map<string, number>();
  const failCounts = new Map<string, number>();
  let creatable = 0;
  for (const item of res.preview) {
    if (item.action === 'CREATE') {
      creatable += 1;
      continue;
    }
    const bucket = item.action === 'FAILED' ? failCounts : skipCounts;
    const key = item.reason ?? (item.action === 'FAILED' ? SEED_FAILED_REASON : '');
    bucket.set(key, (bucket.get(key) ?? 0) + 1);
  }
  return {
    scanned: res.scanned,
    plannedCreate: res.planned_create,
    created: res.created,
    skipped: res.skipped,
    failed: res.failed,
    skipBreakdown: orderedCounts(skipCounts, SEED_SKIP_REASONS),
    failedBreakdown: orderedCounts(failCounts, [SEED_FAILED_REASON]),
    hasCreatable: creatable > 0,
  };
}

/** 按给定顺序输出非零分组（顺序 = 后端判定顺序，不按条数排 —— 排了每次刷新都在跳） */
function orderedCounts(counts: Map<string, number>, order: readonly string[]): SeedReasonCount[] {
  const rows: SeedReasonCount[] = [];
  for (const reason of order) {
    const count = counts.get(reason) ?? 0;
    if (count > 0) {
      rows.push({ reason, label: reasonLabel(reason), count });
    }
  }
  // 后端将来新增、前端词表还没跟上的码值：追加在后面，别静默丢掉（丢了计数就对不上）
  for (const [reason, count] of counts) {
    if (!order.includes(reason) && count > 0) {
      rows.push({ reason, label: reasonLabel(reason), count });
    }
  }
  return rows;
}

/** 分桶后的明细行（`all` 原样返回，保持后端顺序） */
export function filterSeedRows(res: SeedResult, bucket: SeedBucket): SeedItem[] {
  switch (bucket) {
    case 'create':
      return res.preview.filter((item) => item.action === 'CREATE');
    case 'skip':
      return res.preview.filter((item) => item.action === 'SKIP');
    case 'failed':
      return res.preview.filter((item) => item.action === 'FAILED');
    default:
      return res.preview;
  }
}

/** 行上的动作文案（`将建档` / `已建档` / `跳过` / `失败`） */
export function rowActionLabel(item: SeedItem, dryRun: boolean): string {
  switch (item.action) {
    case 'CREATE':
      return dryRun ? '将建档' : '已建档';
    case 'FAILED':
      return '失败';
    default:
      return '跳过';
  }
}

/** 行上的动作配色（`SKIP` 用 default：跳过不是错误，别染成红色吓人） */
export function rowActionTheme(item: SeedItem): 'success' | 'warning' | 'default' {
  switch (item.action) {
    case 'CREATE':
      return 'success';
    case 'FAILED':
      return 'warning';
    default:
      return 'default';
  }
}

/**
 * 「确认生成」按钮能不能点。
 *
 * 三条都要满足：这是预览回执（`dry_run=true`）、确实有计划建档的行、当前没有请求在飞。
 * 第三项由调用方传入（`busy`）—— 纯函数里不放全局状态。
 */
export function canApplySeed(
  res: SeedResult | null,
  busy = false,
): res is SeedResult {
  return Boolean(res) && !busy && res !== null && res.dry_run && res.planned_create > 0;
}

/** 「确认生成 N 人」按钮文案（N 取自预览回执，和面板上的数字是同一个来源） */
export function applyButtonText(res: SeedResult | null): string {
  const count = res?.planned_create ?? 0;
  return count > 0 ? `确认生成 ${count} 人` : '无可生成账号';
}

/**
 * 面板顶部的结论句。
 *
 * 预览：「将新建 N 人 · 跳过 M 人（已有档案 a / 缺部门 b …）· 失败 K 人」
 * 执行： 「已生成 N 人 · 失败 K 人」
 */
export function seedHeadline(res: SeedResult, summary: SeedSummary = summarizeSeed(res)): string {
  if (res.dry_run) {
    const parts = [`扫描 ${summary.scanned} 人`, `将新建 ${summary.plannedCreate} 人`, `跳过 ${summary.skipped} 人`];
    if (summary.failed > 0) {
      parts.push(`失败 ${summary.failed} 人`);
    }
    return parts.join(' · ');
  }
  const parts = [`已生成 ${summary.created} 人`, `跳过 ${summary.skipped} 人`];
  if (summary.failed > 0) {
    parts.push(`失败 ${summary.failed} 人`);
  }
  return parts.join(' · ');
}

/**
 * 预览为空（没有可新建的账号）时的解释句 —— 卡面验收「再点一次预览 → 将新建 0 人」。
 *
 * 要区分三种「0」：候选本来就空（数据范围 / 部门筛选后一个账号都没有）、
 * 全员已有档案（幂等成功）、混合原因跳过（一部分已有档案、一部分账号本身不满足条件）。
 * 第二种是最常见的「上一次已经生成过了」，文案要说成**成功信号**，不是报错。
 */
export function emptyPlanHint(res: SeedResult): string {
  if (res.scanned === 0) {
    return '没有扫描到候选账号：检查数据权限范围，或换一个部门试试。';
  }
  const breakdown = summarizeSeed(res).skipBreakdown;
  const hasEmployee = breakdown.find((row) => row.reason === 'HAS_EMPLOYEE')?.count ?? 0;
  if (hasEmployee > 0 && hasEmployee === res.skipped) {
    return `扫描到 ${res.scanned} 个账号，全部已有员工档案（跳过 ${res.skipped} 人）—— 无需重复生成。`;
  }
  if (hasEmployee > 0) {
    return `扫描到 ${res.scanned} 个账号，没有可新建的：其中 ${hasEmployee} 人已有员工档案，其余 ${res.skipped - hasEmployee} 人见下表原因。`;
  }
  return `扫描到 ${res.scanned} 个账号，没有可新建的（跳过 ${res.skipped} 人，原因见下表）。`;
}

/** 超过后端单次上限时的提示（后端报错文案更详细，这里只在**点按钮前**先拦一道） */
export function overLimitHint(scanned: number): string {
  return scanned > SEED_MAX_CANDIDATES
    ? `候选账号 ${scanned} 个，超过单次上限 ${SEED_MAX_CANDIDATES} 个：${SEED_BATCH_HINT}`
    : '';
}

/** 明细里「姓名」「部门」的空值占位（空串在表格里看着像渲染失败） */
export function cellOrDash(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  return text === '' ? '—' : text;
}

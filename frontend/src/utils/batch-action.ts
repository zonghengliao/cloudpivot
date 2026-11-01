/**
 * 批量操作纯逻辑（D7）。
 *
 * 这里只放**不碰 DOM、不碰请求**的判定：按钮能不能点、提示怎么写、结果怎么汇总、
 * 选中的是「本页」还是「全部筛选结果」、高危操作要不要填原因。
 * 组件 `BatchActionBar.vue` 只负责把这些结论画出来，`.vue` 里不留判据 —— 否则
 * `v-if` 改一行就悄悄换了行为，`vue-tsc` 抓不到（与 `year-end-logic.ts` 同一套理由）。
 *
 * # 口径（与后端逐字对齐）
 *
 * | 事情 | 后端 | 前端 |
 * |------|------|------|
 * | 单次批量上限 | `shared/batch.rs::MAX_BATCH_IDS = 200` | `BATCH_CHUNK = 20`，多条自动分批 |
 * | 失败条目 | `{ id, no, code, error }` | `failureLines` 原样列 `单号：原因` |
 * | 空 ids | `400` | 没勾选时按钮禁用（连请求都不发） |
 * | 幂等 | 已处理的行 409 | 结果里照实显示失败原因，不吞 |
 */

/** 后端单次批量上限（`shared/batch.rs::MAX_BATCH_IDS`）。 */
export const MAX_BATCH_IDS = 200

/**
 * 前端每次发多少条。
 *
 * 20 不是后端限制（200 也行），而是为了让**进度条真的在动**：一次 200 条要跑好几秒，
 * 分 10 批每批回来就刷一次 N/M，用户知道没卡死。
 */
export const BATCH_CHUNK = 20

/** 按钮主题（TDesign）。 */
export type BatchTheme = 'primary' | 'danger' | 'default' | 'success' | 'warning'

/** 一条批量动作的声明。 */
export interface BatchActionSpec {
  /** 动作 key（页面据此决定调哪个接口）。 */
  key: string
  /** 按钮文案。 */
  label: string
  /** 按钮主题。 */
  theme?: BatchTheme
  /** 只允许这些状态的行（`undefined` = 不限）。 */
  allowedStatus?: string[]
  /** 高危：点之前二次确认（并显示影响条数）。 */
  needConfirm?: boolean
  /** 必须填原因（与 `needConfirm` 一起用）。 */
  requireReason?: boolean
  /** 原因最少字数（默认 2；驳回这类由后端再判）。 */
  minReasonChars?: number
  /** 该动作会把行改成这个目标状态（批量改状态 / 批量审批用）。 */
  targetStatus?: string
  /** 需要选一个用户（批量指派用）。 */
  requireOwner?: boolean
  /** 导出类：结果不是「成功 X 条」而是「已提交任务」。 */
  exportKind?: boolean
}

/** 勾选行至少要有的字段。 */
export interface BatchRow {
  /** 主键。 */
  id: string
  /** 业务状态（没有状态概念的资源可以不填）。 */
  status?: string | null
}

/** 成功的一条（后端 `BatchOkItem`）。 */
export interface BatchOkItem {
  id: string
  no?: string | null
}

/** 失败的一条（后端 `BatchFailItem`）。 */
export interface BatchFailItem {
  id: string
  no?: string | null
  code: number
  error: string
}

/** 后端 `BatchResult`。 */
export interface BatchResult {
  succeeded: BatchOkItem[]
  failed: BatchFailItem[]
  total: number
  success_count: number
  fail_count: number
}

/** 批量导出的任务回执（后端 `BatchExportTask`）。 */
export interface BatchExportTask {
  id: string
  task_no: string
  biz_type: string
  biz_name: string
  export_name: string
  total: number
}

/** 一批（可能分多 chunk）跑完的结果。 */
export type BatchOutcome =
  | { kind: 'batch'; result: BatchResult }
  | { kind: 'export'; task: BatchExportTask }

/** 分块（不改原数组）。 */
export function chunk<T>(items: T[], size = BATCH_CHUNK): T[][] {
  const step = size > 0 ? size : items.length || 1
  const out: T[][] = []
  for (let index = 0; index < items.length; index += step) {
    out.push(items.slice(index, index + step))
  }
  return out
}

/** 行状态的展示文案（有 `labelOf` 就用它，否则原样）。 */
function statusOf(row: BatchRow): string | undefined {
  const raw = row.status
  return raw === undefined || raw === null || raw === '' ? undefined : String(raw)
}

/**
 * 动作对这批行**是否适用**：`allowedStatus` 里没有的状态都算不符合。
 *
 * 没声明 `allowedStatus`（导出这类）或行没有状态时一律算「适用」——
 * 猜错的代价是「能点的按钮变灰」，比「点了才报错」更烦人。
 */
export function isActionApplicable(action: BatchActionSpec, row: BatchRow): boolean {
  if (!action.allowedStatus || action.allowedStatus.length === 0) {
    return true
  }
  const status = statusOf(row)
  if (!status) {
    return true
  }
  return action.allowedStatus.includes(status)
}

/** 这批行里**不符合**该动作状态要求的行。 */
export function blockedRows(action: BatchActionSpec, rows: BatchRow[]): BatchRow[] {
  return rows.filter((row) => !isActionApplicable(action, row))
}

/**
 * 按钮禁用的原因（`undefined` = 可以点）。
 *
 * 报文要给「为什么」和「怎么办」：勾了 2 条已过账的单子去点「批量过账」，
 * 提示必须说清「这 2 条状态不对」，而不是让用户点了才看见红字。
 */
export function actionDisabledReason(
  action: BatchActionSpec,
  rows: BatchRow[],
  hasPermission = true
): string | undefined {
  if (!hasPermission) {
    return `没有「${action.label}」的权限（${action.key}）`
  }
  if (rows.length === 0) {
    return '先勾选要处理的行'
  }
  const blocked = blockedRows(action, rows)
  if (blocked.length === 0) {
    return undefined
  }
  const allowed = (action.allowedStatus ?? []).join(' / ')
  return `「${action.label}」只支持 ${allowed}：勾选里 ${blocked.length} 条状态不符合（${blocked
    .map((row) => statusOf(row) ?? '未知')
    .join(' / ')}）`
}

/** 行上的复选框能不能勾（任何动作都不适用的行直接禁用，避免「勾了也没用」）。 */
export function isRowSelectable(actions: BatchActionSpec[], row: BatchRow): boolean {
  return actions.some((action) => isActionApplicable(action, row))
}

/** 表头全选时能勾上的行（禁用行不计入）。 */
export function selectableIds(actions: BatchActionSpec[], rows: BatchRow[]): string[] {
  return rows.filter((row) => isRowSelectable(actions, row)).map((row) => row.id)
}

/** 勾选范围的提示文案：**必须**说清「本页」还是「全部筛选结果」（卡面 §三 ②）。 */
export function selectionText(
  selectedCount: number,
  scope: 'page' | 'filtered',
  filteredTotal = 0
): string {
  if (selectedCount === 0) {
    return '未勾选'
  }
  if (scope === 'filtered') {
    return `已选 ${selectedCount} 条（全部筛选结果）`
  }
  const suffix = filteredTotal > selectedCount ? `（本页，共 ${filteredTotal} 条筛选结果）` : '（本页）'
  return `已选 ${selectedCount} 条${suffix}`
}

/** 高危动作的二次确认正文：**带影响条数**。 */
export function confirmMessage(action: BatchActionSpec, count: number): string {
  const head = `确认对 ${count} 条执行「${action.label}」？`
  if (action.requireReason) {
    return `${head}该操作不可撤销，原因会写入单据留痕。`
  }
  if (action.theme === 'danger') {
    return `${head}该操作不可撤销。`
  }
  return head
}

/** 原因校验（`undefined` = 通过）。 */
export function reasonError(action: BatchActionSpec, reason: string): string | undefined {
  if (!action.requireReason) {
    return undefined
  }
  const text = reason.trim()
  const min = action.minReasonChars ?? 2
  if (text.length === 0) {
    return '必须填写原因：这条操作会改账 / 改单据状态，事后要能追溯为什么'
  }
  if (text.length < min) {
    return `原因至少 ${min} 个字（当前 ${text.length} 个）`
  }
  return undefined
}

/** 负责人校验（批量指派；`undefined` = 通过）。 */
export function ownerError(action: BatchActionSpec, ownerId: string | undefined): string | undefined {
  if (!action.requireOwner) {
    return undefined
  }
  if (!ownerId || ownerId.trim() === '') {
    return '必须选择新负责人：批量指派要把任务交到具体某个人手上'
  }
  return undefined
}

/** 进度文案（`N/M`）。 */
export function progressText(done: number, total: number): string {
  const safeTotal = Math.max(total, 0)
  const safeDone = Math.min(Math.max(done, 0), safeTotal || done)
  return `${safeDone}/${safeTotal}`
}

/** 进度百分比（0–100，用于进度条）。 */
export function progressPercent(done: number, total: number): number {
  if (total <= 0) {
    return 0
  }
  return Math.min(100, Math.max(0, Math.round((done / total) * 100)))
}

/** 合并多个 chunk 的结果（分批请求的收尾）。 */
export function mergeBatchResults(results: BatchResult[]): BatchResult {
  const merged: BatchResult = { succeeded: [], failed: [], total: 0, success_count: 0, fail_count: 0 }
  for (const item of results) {
    merged.succeeded.push(...item.succeeded)
    merged.failed.push(...item.failed)
    merged.total += item.total
  }
  merged.success_count = merged.succeeded.length
  merged.fail_count = merged.failed.length
  return merged
}

/** 结果标题：`成功 3 条 / 失败 2 条`。 */
export function resultHeadline(outcome: BatchOutcome): string {
  if (outcome.kind === 'export') {
    return `已提交导出任务 ${outcome.task.task_no}（${outcome.task.total} 行）`
  }
  const { result } = outcome
  return `共 ${result.total} 条：成功 ${result.success_count} 条，失败 ${result.fail_count} 条`
}

/** 结果副标题（导出任务要告诉用户去哪里下载）。 */
export function resultHint(outcome: BatchOutcome): string {
  if (outcome.kind === 'export') {
    return `已提交导出任务，请到「导入导出」页下载「${outcome.task.export_name}.xlsx」`
  }
  if (outcome.result.failed.length === 0) {
    return '全部处理成功，列表已刷新'
  }
  return '失败的行未做任何改动（逐条独立事务），可按原因修正后重试'
}

/**
 * 失败清单（一行一条，可整段复制给同事 / 贴进工单）。
 *
 * 单号优先，没有单号就退到 id —— 复制出去的清单必须能定位到具体哪一条。
 */
export function failureLines(outcome: BatchOutcome): string[] {
  if (outcome.kind === 'export') {
    return []
  }
  return outcome.result.failed.map((item) => {
    const name = item.no ?? item.id
    return `${name}：${item.error}`
  })
}

/** 失败清单的纯文本（复制按钮用）。 */
export function failureText(outcome: BatchOutcome): string {
  return failureLines(outcome).join('\n')
}

/** 一批的执行业务动作：`ids` 是当前 chunk，返回批量结果或导出任务回执。 */
export type BatchRunner = (
  action: BatchActionSpec,
  ids: string[],
  payload: { reason?: string; ownerId?: string }
) => Promise<BatchResult | BatchExportTask>

/** 类型收窄：导出回执 vs 批量结果（两者字段没有交集，只能按 `task_no` 判）。 */
export function isExportTask(value: BatchResult | BatchExportTask): value is BatchExportTask {
  return typeof (value as BatchExportTask).task_no === 'string'
}

/** 勾选切换：`all=true` 选中给定 id，`false` 取消（不清其它页已有的勾选）。 */
export function toggleSelection(current: string[], ids: string[], all: boolean): string[] {
  const set = new Set(current)
  for (const id of ids) {
    if (all) {
      set.add(id)
    } else {
      set.delete(id)
    }
  }
  return [...set]
}

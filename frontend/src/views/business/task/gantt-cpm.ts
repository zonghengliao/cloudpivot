/**
 * 甘特图关键路径（CPM）—— **纯算法**，不依赖 Vue / DOM / dayjs，单测直接跑。
 *
 * # 算什么
 *
 * 经典 CPM 三件套：最早开始（ES）/ 最早完成（EF）、最晚开始（LS）/ 最晚完成（LF）、
 * 总浮动时间（`totalFloat = LS - ES`）。**浮动 = 0 的任务 = 关键路径**：
 * 它一推迟，整个项目就推迟。
 *
 * # 三条口径（决定了算出来的数字对不对，别按「更标准的 CPM」自行改）
 *
 * 1. **计划开始 = 「不早于」约束**。库里的 `start_date` 是用户排好的计划日，
 *    条形就画在那儿；若把 ES 算成纯依赖推导值（可能比计划日更早），
 *    浮动就会大于「拖动条形还能拖几天」的真实余量 —— 用户看到「浮动 3 天」却拖不动，
 *    是不可接受的谎报。所以 `ES = max(计划开始, 前置约束)`。
 *    依赖把开始顶到晚于计划日时（排期自相矛盾）标 `conflict`，如实上报，不静默修正。
 * 2. **工期取自然日（含首含尾）**：`due - start + 1`。只有单边日期 → 按单日算
 *    （与 `gantt.vue::spanOfDates` 同口径，否则条形 1 天、算法算 0 天）。
 *    `EF` 用**独占**口径：占 [ES, ES+dur-1]，所以 `EF = ES + dur` 正是后继 FS 能开始的那天。
 * 3. **浮动相对「项目完成日」**：`项目完成 = max(EF)`，没有后继的任务 `LF = 项目完成`。
 *    因此「关键路径 = 决定项目工期的那条链」，不是「任务数最多的链」——
 *    3 条依赖链里最长的那条末端才会顶到项目完成日，浮动为 0。
 *
 * # 两个必须兜住的脏数据（后端本该拒绝，前端兜一层）
 *
 * - **依赖环**：Kahn 拓扑排序后剩下来的节点标 `inCycle`，`totalFloat = null`（不假装 0），
 *   并计入 `cyclicIds`。算不出来就不能说它「关键」—— 让 UI 报警，而不是画一条错的红色链路。
 * - **父子汇总条不参与 CPM**：父条是子任务区间的并集（`gantt.vue::summarySpan`），
 *   既是汇总又是节点会**把工期算两遍**。所以父任务不做节点，只做「子任务里有关键任务
 *   就整体标红」的 rollup（`summaryById`）；依赖边连到父任务时按**展开成它的每个叶子**
 *   处理（「等父任务的活全干完」正是汇总语义，与 MS Project 一致）。
 *
 * # 依赖类型
 *
 * 后端甘特接口的 SQL 硬过滤 `dep_type = 'FS'`，所以这里只按 FS 算（完成 → 开始）；
 * `lag_days` 如实计入（`ES_succ >= EF_pred + lag`）。真出现 SS/FF/SF 时要重算口径，
 * 不能沿用本实现 —— 那时本文件的 `edgeLag` 会成为唯一改动点。
 */

/** CPM 输入任务（只取算法要用的字段，`GanttTask` 结构兼容） */
export interface CpmTaskInput {
  id: string;
  parent_id?: string | null;
  start_date: string | null;
  due_date: string | null;
}

/** CPM 输入依赖边（`GanttDependency` 结构兼容） */
export interface CpmDepInput {
  from: string;
  to: string;
  lag_days?: number | null;
  dep_type?: string | null;
}

/** 一个叶子任务的 CPM 结果（所有天序号都相对 `origin`） */
export interface CpmNode {
  id: string;
  /** 计划开始（天序号） */
  plannedStart: number;
  /** 工期（天，含首含尾，≥ 1） */
  duration: number;
  /** 最早开始（= max(计划开始, 前置约束)） */
  earliestStart: number;
  /** 最早完成（独占口径：最后一个占用日 + 1） */
  earliestFinish: number;
  /** 最晚开始（依赖环里算不出来 → null） */
  latestStart: number | null;
  /** 最晚完成（独占口径；依赖环 → null） */
  latestFinish: number | null;
  /** 总浮动时间（天；依赖环 → null） */
  totalFloat: number | null;
  /** 关键路径（浮动 ≤ 0） */
  critical: boolean;
  /** 前置把开始顶到了计划日之后（排期冲突，如实上报） */
  conflict: boolean;
  /** 处在依赖环里 */
  inCycle: boolean;
}

/** 汇总条（父任务）的 rollup：父条不是节点，只回答「这一条要不要标红」 */
export interface CpmSummary {
  id: string;
  /** 有任一叶子子任务关键 → true */
  critical: boolean;
  criticalCount: number;
  leafCount: number;
  /** 子任务里最小的浮动（任一子任务算不出 → null） */
  minFloat: number | null;
}

/** CPM 结果 */
export interface CpmResult {
  /** 基准日（`YYYY-MM-DD`）：所有节点的最早计划开始；没有节点 → null */
  origin: string | null;
  /** 项目工期（天）：max(EF)，从 origin 起算 */
  projectDays: number;
  /** 叶子任务 id → 结果 */
  byId: Map<string, CpmNode>;
  /** 汇总条 id → rollup */
  summaryById: Map<string, CpmSummary>;
  /** 关键**依赖边**（用原始 `from->to` 作键，与模板里的 `link.key` 同形） */
  criticalEdges: Set<string>;
  /** 处在依赖环里的任务 id */
  cyclicIds: string[];
  /** 缺日期 / 缺结构、无法参与计算的任务 id */
  skippedIds: string[];
}

/** 一天的毫秒数 */
const DAY_MS = 86_400_000;

/** `YYYY-MM-DD` → UTC 天序号（用于整数天差，不受本地时区影响） */
function dayNumber(value: string | null | undefined): number | null {
  if (!value) {
    return null;
  }
  const matched = /^(\d{4})-(\d{2})-(\d{2})/.exec(value);
  if (!matched) {
    return null;
  }
  const year = Number(matched[1]);
  const month = Number(matched[2]);
  const date = Number(matched[3]);
  if (!(month >= 1 && month <= 12 && date >= 1 && date <= 31)) {
    return null;
  }
  const ms = Date.UTC(year, month - 1, date);
  return Number.isFinite(ms) ? Math.round(ms / DAY_MS) : null;
}

/** 天序号 → `YYYY-MM-DD` */
function formatDay(day: number): string {
  return new Date(day * DAY_MS).toISOString().slice(0, 10);
}

/** 输入依赖边的键（与 `gantt.vue` 的 `link.key` 完全一致） */
export function cpmEdgeKey(from: string, to: string): string {
  return `${from}->${to}`;
}

/** 空结果（没有数据 / 全被跳过时返回同一个形状，调用方不必判空） */
function emptyResult(origin: string | null, skipped: string[]): CpmResult {
  return {
    origin,
    projectDays: 0,
    byId: new Map(),
    summaryById: new Map(),
    criticalEdges: new Set(),
    cyclicIds: [],
    skippedIds: skipped,
  };
}

/**
 * 算关键路径。
 *
 * 复杂度：O(V + E)（Kahn 拓扑 + 一次正推 + 一次逆推），1000 条任务也在毫秒内，
 * 所以调用方可以直接放进 `computed()`，不必节流。
 */
export function computeCpm(input: { tasks: CpmTaskInput[]; deps: CpmDepInput[] }): CpmResult {
  const tasks = input.tasks ?? [];
  const deps = input.deps ?? [];
  if (tasks.length === 0) {
    return emptyResult(null, []);
  }

  const taskById = new Map<string, CpmTaskInput>();
  for (const task of tasks) {
    taskById.set(task.id, task);
  }

  /** 直接子任务（只认「父任务真的在本批数据里」的，与 `gantt.vue` 的分组口径一致） */
  const childIdsOf = new Map<string, string[]>();
  for (const task of tasks) {
    const parentId = task.parent_id;
    if (!parentId || !taskById.has(parentId)) {
      continue;
    }
    const list = childIdsOf.get(parentId) ?? [];
    list.push(task.id);
    childIdsOf.set(parentId, list);
  }

  /** 叶子任务（= CPM 节点候选）：没有在本批数据里的子任务 */
  const isLeaf = (id: string): boolean => (childIdsOf.get(id)?.length ?? 0) === 0;

  /** 某任务下的全部叶子（父任务展开用；带 visited 防 parent 环导致死循环） */
  function leavesOf(id: string): string[] {
    if (isLeaf(id)) {
      return taskById.has(id) ? [id] : [];
    }
    const out: string[] = [];
    const visited = new Set<string>([id]);
    const stack = [...(childIdsOf.get(id) ?? [])];
    while (stack.length > 0) {
      const current = stack.pop()!;
      if (visited.has(current)) {
        continue;
      }
      visited.add(current);
      if (isLeaf(current)) {
        out.push(current);
      } else {
        stack.push(...(childIdsOf.get(current) ?? []));
      }
    }
    return out;
  }

  /** 每个节点的日期区间（天序号） */
  const spanById = new Map<string, { start: number; duration: number }>();
  const skippedIds: string[] = [];
  let origin: number | null = null;
  for (const task of tasks) {
    if (!isLeaf(task.id)) {
      continue; // 汇总条不是节点（见文件头口径 3）
    }
    // 单边日期按单日算（与 gantt.vue::spanOfDates 同口径）
    const from = dayNumber(task.start_date ?? task.due_date);
    const to = dayNumber(task.due_date ?? task.start_date);
    if (from === null || to === null) {
      skippedIds.push(task.id);
      continue;
    }
    const start = Math.min(from, to);
    const duration = Math.max(1, Math.abs(to - from) + 1);
    spanById.set(task.id, { start, duration });
    origin = origin === null ? start : Math.min(origin, start);
  }

  if (spanById.size === 0) {
    // 一条能排期的都没有（全缺日期）：没有基准日，也没有关键路径
    return emptyResult(null, skippedIds);
  }

  const base = origin as number;

  /** 相邻表：pred → succ（只保留两端都是节点的边） */
  const nextOf = new Map<string, Array<{ to: string; lag: number }>>();
  const prevOf = new Map<string, Array<{ from: string; lag: number }>>();
  /** 原始边键 → 展开后的叶子边键（父任务展开成多个叶子） */
  const expandedOf = new Map<string, string[]>();

  for (const dep of deps) {
    if (!taskById.has(dep.from) || !taskById.has(dep.to)) {
      continue; // 指向不存在 / 不在本批数据的任务：忽略（不猜）
    }
    const rawLag = Number(dep.lag_days);
    const lag = Number.isFinite(rawLag) && rawLag > 0 ? Math.floor(rawLag) : 0;
    const preds = leavesOf(dep.from).filter((id) => spanById.has(id));
    const succs = leavesOf(dep.to).filter((id) => spanById.has(id));
    const key = cpmEdgeKey(dep.from, dep.to);
    const expanded: string[] = [];
    for (const predId of preds) {
      for (const succId of succs) {
        if (predId === succId) {
          continue; // 自环（含父任务展开后自己连自己）：忽略
        }
        const edgeKey = cpmEdgeKey(predId, succId);
        if (!expanded.includes(edgeKey)) {
          expanded.push(edgeKey);
        }
        const next = nextOf.get(predId) ?? [];
        const existing = next.find((item) => item.to === succId);
        if (existing) {
          // 重复边（用户可能从两个父任务各连一条）：滞后取大的那个，保守
          existing.lag = Math.max(existing.lag, lag);
        } else {
          next.push({ to: succId, lag });
          nextOf.set(predId, next);
        }
        const prev = prevOf.get(succId) ?? [];
        const existingPrev = prev.find((item) => item.from === predId);
        if (existingPrev) {
          existingPrev.lag = Math.max(existingPrev.lag, lag);
        } else {
          prev.push({ from: predId, lag });
          prevOf.set(succId, prev);
        }
      }
    }
    if (expanded.length > 0) {
      expandedOf.set(key, expanded);
    }
  }

  // ---------- Kahn 拓扑排序（同时识别依赖环） ----------
  const indegree = new Map<string, number>();
  for (const id of spanById.keys()) {
    indegree.set(id, prevOf.get(id)?.length ?? 0);
  }
  const queue: string[] = [];
  for (const [id, degree] of indegree) {
    if (degree === 0) {
      queue.push(id);
    }
  }
  const order: string[] = [];
  while (queue.length > 0) {
    const current = queue.shift()!;
    order.push(current);
    for (const edge of nextOf.get(current) ?? []) {
      const left = (indegree.get(edge.to) ?? 0) - 1;
      indegree.set(edge.to, left);
      if (left === 0) {
        queue.push(edge.to);
      }
    }
  }
  const cyclicIds = [...spanById.keys()].filter((id) => !order.includes(id));

  // ---------- 正推：ES / EF ----------
  const nodes = new Map<string, CpmNode>();
  for (const id of order) {
    const span = spanById.get(id)!;
    const plannedStart = span.start - base;
    let earliestStart = plannedStart;
    for (const edge of prevOf.get(id) ?? []) {
      const pred = nodes.get(edge.from);
      if (!pred || pred.inCycle) {
        continue;
      }
      earliestStart = Math.max(earliestStart, pred.earliestFinish + edge.lag);
    }
    nodes.set(id, {
      id,
      plannedStart,
      duration: span.duration,
      earliestStart,
      earliestFinish: earliestStart + span.duration,
      latestStart: null,
      latestFinish: null,
      totalFloat: null,
      critical: false,
      conflict: earliestStart > plannedStart,
      inCycle: false,
    });
  }
  let projectEnd = 0;
  for (const node of nodes.values()) {
    projectEnd = Math.max(projectEnd, node.earliestFinish);
  }

  // 依赖环里的节点：算不出来就明说算不出来（浮动 null，绝不假装 0）
  for (const id of cyclicIds) {
    const span = spanById.get(id)!;
    nodes.set(id, {
      id,
      plannedStart: span.start - base,
      duration: span.duration,
      earliestStart: span.start - base,
      earliestFinish: span.start - base + span.duration,
      latestStart: null,
      latestFinish: null,
      totalFloat: null,
      critical: false,
      conflict: false,
      inCycle: true,
    });
  }

  // ---------- 逆推：LF / LS / 浮动 ----------
  for (let index = order.length - 1; index >= 0; index -= 1) {
    const id = order[index]!;
    const node = nodes.get(id)!;
    // 起点是项目完成日：**没有后继**（或后继全在环里）的任务，最晚完成 = 项目完成日
    let latestFinish = projectEnd;
    for (const edge of nextOf.get(id) ?? []) {
      const succ = nodes.get(edge.to);
      if (!succ || succ.inCycle || succ.latestStart === null) {
        continue;
      }
      latestFinish = Math.min(latestFinish, succ.latestStart - edge.lag);
    }
    const latestStart = latestFinish - node.duration;
    node.latestFinish = latestFinish;
    node.latestStart = latestStart;
    node.totalFloat = latestStart - node.earliestStart;
    node.critical = node.totalFloat <= 0;
  }

  // ---------- 关键边：两端都关键 **且** 这条边真的在顶（ES_succ = EF_pred + lag） ----------
  //
  // 父任务展开成多叶子时，「在顶」的只有其中一个（EF 最大的那个）；
  // 所以判据是「**在顶的那些**展开边，其前置都关键」——
  // 若只要求「全部展开边关键」，链上真正在顶的那条会被没在顶的兄弟叶子否决，
  // 红线链路就断了（实测：K2 关键、K1 有 3 天余量，P→M 明明在顶却画成蓝线）。
  const criticalEdges = new Set<string>();
  for (const [key, expanded] of expandedOf) {
    const bindingPreds: string[] = [];
    let succCritical = true;
    for (const edgeKey of expanded) {
      const [fromId, toId] = edgeKey.split('->') as [string, string];
      const pred = nodes.get(fromId);
      const succ = nodes.get(toId);
      if (!succ?.critical) {
        succCritical = false;
        break;
      }
      if (!pred) {
        succCritical = false;
        break;
      }
      const lag = (nextOf.get(fromId) ?? []).find((item) => item.to === toId)?.lag ?? 0;
      if (succ.earliestStart !== pred.earliestFinish + lag) {
        continue; // 这条展开边没在顶（前置早就干完了）
      }
      if (!pred.critical) {
        succCritical = false;
        break;
      }
      bindingPreds.push(fromId);
    }
    if (succCritical && bindingPreds.length > 0) {
      criticalEdges.add(key);
    }
  }

  // ---------- 汇总条 rollup（父任务：有任一叶子关键就标红） ----------
  const summaryById = new Map<string, CpmSummary>();
  for (const task of tasks) {
    if (isLeaf(task.id)) {
      continue;
    }
    const leaves = leavesOf(task.id);
    let criticalCount = 0;
    let leafCount = 0;
    // `minFloat` 用「null = 还没赋值」会与「算不出来 = null」撞车，
    // 所以另开一个 unknown 标记：任一子任务在环里 → 整个汇总条浮动未知。
    let minFloat: number | null = null;
    let unknown = false;
    for (const leafId of leaves) {
      const node = nodes.get(leafId);
      if (!node) {
        continue; // 没日期 / 不在本批数据的子任务不参与
      }
      leafCount += 1;
      if (node.critical) {
        criticalCount += 1;
      }
      if (node.totalFloat === null) {
        unknown = true;
      } else {
        minFloat = minFloat === null ? node.totalFloat : Math.min(minFloat, node.totalFloat);
      }
    }
    summaryById.set(task.id, {
      id: task.id,
      critical: criticalCount > 0,
      criticalCount,
      leafCount,
      minFloat: leafCount === 0 || unknown ? null : minFloat,
    });
  }

  return {
    origin: formatDay(base),
    projectDays: projectEnd,
    byId: nodes,
    summaryById,
    criticalEdges,
    cyclicIds,
    skippedIds,
  };
}

/** 浮动时间文案（`null` = 依赖环算不出来，绝不显示 0 假装关键） */
export function floatLabel(totalFloat: number | null): string {
  if (totalFloat === null) {
    return '浮动 —（依赖成环）';
  }
  if (totalFloat <= 0) {
    return '浮动 0 天（关键路径）';
  }
  return `浮动 ${totalFloat} 天`;
}

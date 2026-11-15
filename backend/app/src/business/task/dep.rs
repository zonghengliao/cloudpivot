//! 任务依赖（FS）与甘特排期的**纯逻辑**（批次 C+ · T4）——不碰数据库。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md` §四（任务依赖）/ §五（甘特图）、
//! `docs/批次C+-工作任务管理-详细架构.md`（依赖只做 FS 软约束）、
//! `docs/云枢设计修正补丁-20261029.md` 修正 10（甘特拖拽改期必须重跑依赖校验）。
//!
//! # 为什么把这三件事抽成纯函数
//!
//! 1. **环检测**：`task_id` 依赖 `depends_on_id` 会不会成环，本质是「在已有依赖图上，
//!    从 `depends_on_id` 顺着前置链能否走回 `task_id`」。写成 SQL 递归 CTE 能查出来，
//!    但**查不出路径**（要额外一列数组）、也没法在无库环境里逐条断言「直环 / 长环 / 自环 /
//!    超深链」这四类边界。这里用**图遍历**实现一次（[`find_cycle`]），
//!    由它在事务内基于 `biz_task_dependency` 的边集合作判定 —— 判定与路径命名
//!    是**同一份实现**，不会出现「拦住了但说不清算不算环」的分裂。
//! 2. **阻塞判定**：`DONE` / `CANCELLED` 都算「已解除」，其余 3 态都阻塞。
//!    这条规则在两个地方用（进 DOING 前的 409、看板/甘特的下发标记），
//!    写两遍迟早不一致（典型事故：`REVIEW` 状态被当成「差不多完成了」放行）。
//! 3. **甘特排期**：把「日期 → SVG 横坐标」的换算（窗口裁剪、进度、今天线、逾期）
//!    做成纯函数，前端只负责画。日期口径统一走 CST（[`crate::business::task::model::today_cst`]）。
//!
//! # 方向约定（全文件唯一的定义，别再自己推一遍）
//!
//! [`DepEdge`] 表示「一条 FS 依赖」：**`depends_on_id` 是前置（先做完）**，
//! **`task_id` 是后继（被阻塞方）**。可视化写作 `前置 → 后继`，
//! 即 `A → B` 读作「A 做完 B 才能开始」。
//!
//! 于是「加一条 `T` 依赖 `D` 会成环」⇔ **D 顺着前置链能走到 T**
//! （D 依赖 x1 依赖 … 依赖 T ⇒ 加上 T 依赖 D 就把圈闭上了）。

use std::collections::{HashMap, HashSet, VecDeque};

use nik::prelude::chrono::{Duration, NaiveDate};
use nik::uuid::Uuid;

use crate::business::task::state::TaskStatus;

// ==================== 常量 / 枚举 ====================

/// 本版**唯一**支持的依赖类型（Finish-to-Start）。
///
/// `SS` / `FF` / `SF` 在 `biz_task_dependency.dep_type` 的 CHECK 里预留，
/// 但接口只接受 `FS`（核心设计 §四：其它三型要甘特与基线才有意义，做半套等于制造误报）。
pub const DEP_TYPE_FS: &str = "FS";

/// 依赖链的**深度上限**（防爆栈 / 防病态数据拖死接口）。
///
/// 命中上限时**不放行**（[`CycleVerdict::TooDeep`]）：宁可让一次「加依赖」失败，
/// 也不要在一个已经 100 层深的依赖链上再叠一层 —— 那种链本身就说明数据有问题。
pub const MAX_CYCLE_DEPTH: usize = 100;

/// 甘特图默认时间窗（今天起 30 天，含今天）。
pub const DEFAULT_RANGE_DAYS: i64 = 30;

/// 甘特图时间窗**上限**（180 天）。超过一律 400：这不是性能优化，是防一次
/// `?start=1970-01-01&end=2999-12-31` 把整张任务表画出来。
pub const MAX_RANGE_DAYS: i64 = 180;

/// 甘特图单次返回的任务条数上限（超出的按排序截断，出参 `truncated = true`）。
pub const MAX_GANTT_TASKS: i64 = 500;

/// 一条依赖边。字段顺序刻意与表列 `(task_id, depends_on_id)` 一致，便于对照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DepEdge {
    /// 后继（被阻塞方，`biz_task_dependency.task_id`）。
    pub task_id: Uuid,
    /// 前置（阻塞方，`biz_task_dependency.depends_on_id`）。
    pub depends_on_id: Uuid,
}

impl DepEdge {
    /// 构造。
    pub fn new(task_id: Uuid, depends_on_id: Uuid) -> Self {
        Self { task_id, depends_on_id }
    }
}

/// 环检测结论（三态，缺一不可）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleVerdict {
    /// 不成环，可以插入。
    Acyclic,
    /// 会成环。`path` = **依赖方向**的闭合路径（首尾同一个 id），
    /// 例如 `[A, B, C, A]` 读作「A 依赖 B 依赖 C 依赖 A」。
    Cycle {
        /// 闭合路径（`path.first() == path.last()`）。
        path: Vec<Uuid>,
    },
    /// 依赖链超过 [`MAX_CYCLE_DEPTH`]，无法在安全成本内判定 → **拒绝插入**（fail-closed）。
    TooDeep {
        /// 实际走到的深度。
        depth: usize,
    },
}

impl CycleVerdict {
    /// 是否允许插入。
    pub fn is_allowed(&self) -> bool {
        matches!(self, CycleVerdict::Acyclic)
    }
}

// ==================== 环检测 ====================

/// 判定「加入 `task_id` 依赖 `depends_on_id` 这条边后是否成环」，并给出闭合路径。
///
/// # 参数
///
/// - `edges`：**已有的** FS 依赖边（不含本次要插入的这条）；
/// - `task_id`：后继（被阻塞方）；
/// - `depends_on_id`：前置（阻塞方）。
///
/// # 返回值
///
/// 见 [`CycleVerdict`]：
/// - 自环（`task_id == depends_on_id`）→ `Cycle { path: [x, x] }`
///   （接口层会在更早的一步用 `400 + 63103` 挡掉，这里仍给出正确结论，保证纯函数自洽）；
/// - 从 `depends_on_id` 顺着前置链能走到 `task_id` → `Cycle { path }`；
/// - 走不到 → `Acyclic`；
/// - 链深超过 [`MAX_CYCLE_DEPTH`] → `TooDeep`（**不放行**）。
///
/// # 算法
///
/// BFS（`VecDeque`）+ `visited` 去重：依赖图里出现「菱形」很常见
/// （A 同时被 B、C 依赖），DFS 不加去重会指数爆炸。BFS 还顺带保证
/// 返回的是**最短**环路径 —— 报错文案给人看，越短越容易定位。
///
/// 复杂度：O(V + E)，V/E 是依赖表的规模（本表规模远小于任务表）。
pub fn find_cycle(edges: &[DepEdge], task_id: Uuid, depends_on_id: Uuid) -> CycleVerdict {
    // 自环：直接闭合（A 依赖 A）
    if task_id == depends_on_id {
        return CycleVerdict::Cycle {
            path: vec![task_id, task_id],
        };
    }

    // 邻接表：后继 → 它的前置们（顺着「依赖」方向走）
    let mut upstream: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for edge in edges {
        upstream.entry(edge.task_id).or_default().push(edge.depends_on_id);
    }

    // BFS：从 depends_on_id 出发，找 task_id
    let mut parent: HashMap<Uuid, Option<Uuid>> = HashMap::new();
    let mut depth: HashMap<Uuid, usize> = HashMap::new();
    parent.insert(depends_on_id, None);
    depth.insert(depends_on_id, 0);
    let mut queue = VecDeque::from([depends_on_id]);
    let mut too_deep: Option<usize> = None;

    while let Some(current) = queue.pop_front() {
        let current_depth = depth.get(&current).copied().unwrap_or(0);
        if current_depth >= MAX_CYCLE_DEPTH {
            // 继续往上走会超过上限：记下来，但**不从这里往下探**
            too_deep = Some(current_depth);
            continue;
        }
        let Some(nexts) = upstream.get(&current) else {
            continue;
        };
        for next in nexts {
            if *next == task_id {
                // 找到回边：depends_on_id … current → task_id，再闭合回 depends_on_id
                let mut chain = Vec::new();
                let mut cursor = Some(current);
                while let Some(node) = cursor {
                    chain.push(node);
                    cursor = parent.get(&node).copied().flatten();
                }
                chain.reverse(); // 变成 [depends_on_id … current]
                chain.push(task_id); // [depends_on_id … current, task_id]
                chain.push(depends_on_id); // 闭合
                return CycleVerdict::Cycle { path: chain };
            }
            if parent.contains_key(next) {
                continue; // 已访问（菱形去重）
            }
            parent.insert(*next, Some(current));
            depth.insert(*next, current_depth + 1);
            queue.push_back(*next);
        }
    }

    match too_deep {
        Some(depth) => CycleVerdict::TooDeep { depth },
        None => CycleVerdict::Acyclic,
    }
}

/// 把闭合路径**旋转到从 `task_id` 开始**（报错文案的阅读顺序）。
///
/// # 为什么需要这一步
///
/// [`find_cycle`] 的路径天然从 `depends_on_id` 出发（它是搜索起点），例如
/// `[C, B, A, C]`；但用户此刻在编辑的是 **A**（「给 A 加前置 C」被拒），
/// 文案从 A 读起最自然：`A → C → B → A`。
///
/// # 参数
///
/// `path`：闭合路径（**首尾相同**，长度 >= 2）。
///
/// # 返回值
///
/// 同一条环、从最后一个非重复节点开始并重新闭合。
/// 自环（`[x, x]`）原样返回。
pub fn orient_cycle_from(path: &[Uuid]) -> Vec<Uuid> {
    if path.len() < 3 {
        return path.to_vec();
    }
    let mut ring: Vec<Uuid> = path[..path.len() - 1].to_vec(); // 去掉重复的收尾节点
    if let Some(last) = ring.pop() {
        ring.insert(0, last); // 右旋一位 = 从 task_id 开始
    }
    let first = ring[0];
    ring.push(first); // 重新闭合
    ring
}

/// 一条依赖的可读标签（报错路径里点名用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepLabel {
    /// 任务主键。
    pub id: Uuid,
    /// 任务编号（`TSK-202610-000001`）。
    pub task_no: String,
    /// 标题。
    pub title: String,
}

/// 把闭合路径渲染成 `A「甲」→ B「乙」→ C「丙」→ A「甲」`。
///
/// # 参数
///
/// - `path`：`CycleVerdict::Cycle` 给出的闭合路径（首尾同 id）；
/// - `labels`：路径上各任务的编号 / 标题（查不到时退化为 id 前 8 位）。
///
/// # 返回值
///
/// 面向用户的字符串。**必须点名环上的每一个任务**：只回「依赖成环」等于让用户
/// 自己去几百条依赖里找环（红线：环检测要贴环路径证据）。
pub fn render_cycle(path: &[Uuid], labels: &[DepLabel]) -> String {
    let index: HashMap<Uuid, &DepLabel> = labels.iter().map(|label| (label.id, label)).collect();
    let render_one = |id: &Uuid| match index.get(id) {
        Some(label) => format!("{}「{}」", label.task_no, label.title),
        None => format!("{}…", &id.to_string()[..8.min(id.to_string().len())]),
    };
    path.iter().map(render_one).collect::<Vec<_>>().join(" → ")
}

// ==================== 阻塞判定 ====================

/// 前置状态是否**仍然阻塞**后继（进 DOING 会被拒）。
///
/// 口径（核心设计 §二 规则 1 + 卡面「联动」表）：
/// - `DONE`：已完成后继可以开始；
/// - `CANCELLED`：前置取消 = **依赖保留但不再阻塞**（查询时排除）；
/// - `TODO` / `DOING` / `REVIEW`：阻塞。
///
/// # 为什么解析不出来也按「阻塞」算
///
/// 库里状态被写坏（不在 5 态内）时，放行等于**静默丢失依赖约束**；
/// 阻塞只是让人多点一次「找管理员」。fail-closed。
pub fn is_blocking_status(status: &str) -> bool {
    match TaskStatus::parse(status) {
        Some(parsed) => !parsed.is_terminal(),
        None => true,
    }
}

/// 从「前置快照」里挑出**仍然阻塞**的那些（保持入参顺序，便于稳定文案）。
pub fn blocking_prerequisites<T, F>(rows: &[T], status_of: F) -> Vec<&T>
where
    F: for<'a> Fn(&'a T) -> &'a str,
{
    rows.iter().filter(|row| is_blocking_status(status_of(row))).collect()
}

// ==================== 甘特排期 ====================

/// 甘特图时间窗（闭区间，含首尾两天）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GanttRange {
    /// 窗口第一天。
    pub start: NaiveDate,
    /// 窗口最后一天。
    pub end: NaiveDate,
}

impl GanttRange {
    /// 窗口总天数（含首尾）。
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }
}

/// 时间窗非法的三种情形（handler 统一映射 `400 + 63105`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// `end < start`。
    Reversed,
    /// 窗口超过 [`MAX_RANGE_DAYS`] 天。
    TooLong {
        /// 实际天数。
        days: i64,
    },
}

impl RangeError {
    /// 面向用户的提示。
    pub fn message(&self) -> String {
        match self {
            RangeError::Reversed => "时间窗非法：end 必须不早于 start".to_string(),
            RangeError::TooLong { days } => {
                format!("时间窗过大：{days} 天，上限 {MAX_RANGE_DAYS} 天（请缩小区间后再查）")
            }
        }
    }
}

/// 规范化时间窗。
///
/// # 规则
///
/// - `start` 缺省 = `today`；
/// - `end` 缺省 = `start + (DEFAULT_RANGE_DAYS - 1)`（默认 30 天，含首尾）；
/// - `end < start` → [`RangeError::Reversed`]；
/// - `days > MAX_RANGE_DAYS` → [`RangeError::TooLong`]。
///
/// # 异常情况
///
/// 见 [`RangeError`]。**不做静默裁剪**：把 365 天的请求悄悄截成 180 天，
/// 前端会以为「这就是全部任务」。
pub fn resolve_range(
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    today: NaiveDate,
) -> std::result::Result<GanttRange, RangeError> {
    let start = start.unwrap_or(today);
    let end = end.unwrap_or_else(|| start + Duration::days(DEFAULT_RANGE_DAYS - 1));
    if end < start {
        return Err(RangeError::Reversed);
    }
    let days = (end - start).num_days() + 1;
    if days > MAX_RANGE_DAYS {
        return Err(RangeError::TooLong { days });
    }
    Ok(GanttRange { start, end })
}

/// 任务条在窗口里的横向位置（单位：天，0 = 窗口第一天）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarSpan {
    /// 距窗口第一天的天数（>= 0）。
    pub offset_days: i64,
    /// 条形长度（天，>= 1；单日任务也是 1 天）。
    pub length_days: i64,
}

/// 把任务的 `(start_date, due_date)` 换算成条形位置（**会被窗口裁剪**）。
///
/// # 口径
///
/// - 只有 `start_date` → 单日任务，画 1 天（等价于里程碑形态）；
/// - 只有 `due_date` → 单日任务，画在截止日；
/// - 两个都有 → `[start, due]`，`due < start`（脏数据）时按单日 `start` 处理；
/// - 与窗口**完全不重叠** → `None`（不画）。
///
/// # 为什么用「天数」而不是像素
///
/// 像素由前端按缩放比例换算（日 / 周视图列宽不同）。后端只给天数，
/// 前后端就不会出现「两边各算一遍偏移、差了 8 小时」这类事故。
pub fn bar_span(start_date: Option<NaiveDate>, due_date: Option<NaiveDate>, range: &GanttRange) -> Option<BarSpan> {
    let from = start_date.or(due_date)?;
    let to = match (start_date, due_date) {
        (Some(start), Some(due)) if due >= start => due,
        _ => from,
    };
    // 裁剪到窗口
    let clipped_from = from.max(range.start);
    let clipped_to = to.min(range.end);
    if clipped_to < clipped_from {
        return None;
    }
    Some(BarSpan {
        offset_days: (clipped_from - range.start).num_days(),
        length_days: (clipped_to - clipped_from).num_days() + 1,
    })
}

/// 今天在窗口里的偏移（`None` = 今天不在窗口内，前端不画今天线）。
pub fn today_offset(range: &GanttRange, today: NaiveDate) -> Option<i64> {
    if today < range.start || today > range.end {
        return None;
    }
    Some((today - range.start).num_days())
}

/// 是否逾期：截止日已过（严格早于今天）且任务未终态。
///
/// 与 T1 的逾期扫描口径一致（`model::scan_overdue`）：**当天到期不算逾期**。
pub fn is_overdue(due_date: Option<NaiveDate>, status: &str, today: NaiveDate) -> bool {
    let Some(due) = due_date else {
        return false;
    };
    if due >= today {
        return false;
    }
    match TaskStatus::parse(status) {
        Some(parsed) => !parsed.is_terminal(),
        // 状态写坏时不标红（标红是给人看的信号，宁可漏标也不要误标满屏）
        None => false,
    }
}

/// 一组任务里「我要参与的前置」是否全部完成（进 DOING 的判定内核，纯逻辑版）。
///
/// 用于单测与文档；运行时判定走 `model::blocking_prerequisites`（同一口径的两处调用
/// 共用 [`is_blocking_status`]，不存在第二份判断）。
pub fn all_prerequisites_done(statuses: &[&str]) -> bool {
    statuses.iter().all(|status| !is_blocking_status(status))
}

/// 去重后的 id 集合（甘特依赖边过滤用：只保留两端都在集合里的边）。
pub fn retain_inner_edges(edges: &[DepEdge], keep: &HashSet<Uuid>) -> Vec<DepEdge> {
    edges
        .iter()
        .filter(|edge| keep.contains(&edge.task_id) && keep.contains(&edge.depends_on_id))
        .copied()
        .collect()
}

// ==================== 单元测试（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn edge(task: u128, depends_on: u128) -> DepEdge {
        DepEdge::new(id(task), id(depends_on))
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("测试日期必须合法")
    }

    fn range(start: &str, end: &str) -> GanttRange {
        GanttRange {
            start: date(start),
            end: date(end),
        }
    }

    // ---------- 环检测 ----------

    #[test]
    fn direct_cycle_is_rejected() {
        // 已有 A → B（B 依赖 A）；再加 B 依赖 A？不，加的是 B→A 会成环：
        // 先有 B 依赖 A（edge(task=B, depends_on=A)），再加 A 依赖 B。
        let edges = [edge(2, 1)];
        let verdict = find_cycle(&edges, id(1), id(2));
        assert_eq!(
            verdict,
            CycleVerdict::Cycle {
                path: vec![id(2), id(1), id(2)]
            },
            "直环路径必须是 B → A → B"
        );
        assert!(!verdict.is_allowed());
    }

    #[test]
    fn long_cycle_is_rejected_with_full_path() {
        // 卡面验收 4 的例子：A→B、B→C 已存在（B 依赖 A、C 依赖 B），再加 C→A（A 依赖 C）
        let edges = [edge(2, 1), edge(3, 2)];
        let verdict = find_cycle(&edges, id(1), id(3));
        match verdict {
            CycleVerdict::Cycle { path } => {
                assert_eq!(path.first(), Some(&id(3)));
                assert_eq!(path.last(), Some(&id(3)), "闭合路径首尾同 id");
                assert_eq!(path.len(), 4, "三个任务 + 闭合点 = 4 个元素");
                // 依赖方向：A 依赖 C 依赖 B 依赖 A
                assert_eq!(path, vec![id(3), id(2), id(1), id(3)]);
            }
            other => panic!("长环必须被识别，实际：{other:?}"),
        }
    }

    #[test]
    fn self_cycle_is_rejected() {
        let verdict = find_cycle(&[], id(7), id(7));
        assert_eq!(
            verdict,
            CycleVerdict::Cycle {
                path: vec![id(7), id(7)]
            }
        );
        assert!(!verdict.is_allowed());
    }

    #[test]
    fn acyclic_and_diamond_are_allowed() {
        // 菱形：B、C 都依赖 A；D 依赖 B、C。加 D 依赖 E（无关）不成环
        let edges = [edge(2, 1), edge(3, 1), edge(4, 2), edge(4, 3)];
        assert_eq!(find_cycle(&edges, id(4), id(9)), CycleVerdict::Acyclic);
        // 加 E 依赖 D 也不成环
        assert_eq!(find_cycle(&edges, id(9), id(4)), CycleVerdict::Acyclic);
        // 菱形本身不误判：A 依赖 D 才成环
        assert!(matches!(find_cycle(&edges, id(1), id(4)), CycleVerdict::Cycle { .. }));
    }

    #[test]
    fn depth_limit_fails_closed() {
        // 造一条 MAX_CYCLE_DEPTH + 5 长的前置链：1 依赖 2 依赖 3 …（1→2 表示「1 依赖 2」）
        let mut edges = Vec::new();
        for node in 1..=(MAX_CYCLE_DEPTH as u128 + 5) {
            edges.push(edge(node, node + 1));
        }
        // 从链头 1 出发要走 100+ 层才到底；目标是一个**不在图上**的 id
        // → 不能判定为无环，必须 TooDeep（fail-closed）
        let verdict = find_cycle(&edges, id(999), id(1));
        assert!(
            matches!(verdict, CycleVerdict::TooDeep { .. }),
            "超深链必须 fail-closed，实际：{verdict:?}"
        );
        assert!(!verdict.is_allowed());
    }

    #[test]
    fn cycle_path_is_oriented_from_the_task_being_edited() {
        // find_cycle 给的路径从 depends_on 出发：[C, B, A, C]
        let raw = vec![id(3), id(2), id(1), id(3)];
        let oriented = orient_cycle_from(&raw);
        // 文案从用户正在编辑的 A 读起：A 依赖 C 依赖 B 依赖 A
        assert_eq!(oriented, vec![id(1), id(3), id(2), id(1)]);
        assert_eq!(oriented.first(), oriented.last());
        // 自环 / 长度不足时原样返回（不 panic）
        assert_eq!(orient_cycle_from(&[id(5), id(5)]), vec![id(5), id(5)]);
        assert_eq!(orient_cycle_from(&[]), Vec::<Uuid>::new());
    }

    #[test]
    fn cycle_label_renders_every_node() {
        let path = vec![id(1), id(2), id(3), id(1)];
        let labels = vec![
            DepLabel {
                id: id(1),
                task_no: "TSK-202610-000001".to_string(),
                title: "数据库设计".to_string(),
            },
            DepLabel {
                id: id(2),
                task_no: "TSK-202610-000002".to_string(),
                title: "接口联调".to_string(),
            },
            DepLabel {
                id: id(3),
                task_no: "TSK-202610-000003".to_string(),
                title: "前端开发".to_string(),
            },
        ];
        let text = render_cycle(&path, &labels);
        assert_eq!(
            text,
            concat!(
                "TSK-202610-000001「数据库设计」 → TSK-202610-000002「接口联调」 → ",
                "TSK-202610-000003「前端开发」 → TSK-202610-000001「数据库设计」"
            )
        );
        // 查不到的 id 退化为前 8 位，不 panic
        let unknown = render_cycle(&[id(9)], &labels);
        assert!(unknown.ends_with('…'));
    }

    // ---------- 阻塞判定 ----------

    #[test]
    fn only_done_and_cancelled_release_the_block() {
        assert!(!is_blocking_status("DONE"));
        assert!(!is_blocking_status("cancelled"), "大小写不敏感");
        assert!(is_blocking_status("TODO"));
        assert!(is_blocking_status("DOING"));
        assert!(is_blocking_status("REVIEW"), "待验收仍然阻塞");
        assert!(is_blocking_status("BROKEN"), "写坏的状态 fail-closed");
    }

    #[test]
    fn blocking_prerequisites_filters_terminals() {
        let rows = vec![("A", "DONE"), ("B", "DOING"), ("C", "CANCELLED"), ("D", "REVIEW")];
        let blocking = blocking_prerequisites(&rows, |row| row.1);
        let names: Vec<&str> = blocking.iter().map(|row| row.0).collect();
        assert_eq!(names, vec!["B", "D"]);
        assert!(!all_prerequisites_done(&["DONE", "DOING"]));
        assert!(all_prerequisites_done(&["DONE", "CANCELLED"]));
        assert!(all_prerequisites_done(&[]), "没有前置 = 不阻塞");
    }

    // ---------- 甘特排期 ----------

    #[test]
    fn range_defaults_and_limits() {
        let today = date("2026-10-29");
        let default = resolve_range(None, None, today).expect("缺省必须合法");
        assert_eq!(default.start, today);
        assert_eq!(default.end, date("2026-11-27"));
        assert_eq!(default.days(), DEFAULT_RANGE_DAYS);

        // 恰好 180 天：放行
        let full = resolve_range(Some(today), Some(today + Duration::days(179)), today);
        assert_eq!(full.expect("180 天恰好等于上限，应放行").days(), 180);
        // 181 天：拒绝
        assert_eq!(
            resolve_range(Some(today), Some(today + Duration::days(180)), today),
            Err(RangeError::TooLong { days: 181 })
        );
        // 倒置：拒绝
        assert_eq!(
            resolve_range(Some(today), Some(today - Duration::days(1)), today),
            Err(RangeError::Reversed)
        );
        assert!(RangeError::TooLong { days: 181 }.message().contains("180"));
    }

    #[test]
    fn bar_span_maps_dates_to_offsets() {
        let window = range("2026-10-29", "2026-11-27");
        // 完全在窗口内：10/31 ~ 11/02 → 偏移 2，长度 3
        let span = bar_span(Some(date("2026-10-31")), Some(date("2026-11-02")), &window);
        assert_eq!(
            span,
            Some(BarSpan {
                offset_days: 2,
                length_days: 3
            })
        );
        // 只有截止日 → 单日
        assert_eq!(
            bar_span(None, Some(date("2026-10-29")), &window),
            Some(BarSpan {
                offset_days: 0,
                length_days: 1
            })
        );
        // 只有开始日 → 单日
        assert_eq!(
            bar_span(Some(date("2026-11-27")), None, &window),
            Some(BarSpan {
                offset_days: 29,
                length_days: 1
            })
        );
        // 跨窗口左边界 → 裁剪到 0
        assert_eq!(
            bar_span(Some(date("2026-10-01")), Some(date("2026-10-30")), &window),
            Some(BarSpan {
                offset_days: 0,
                length_days: 2
            })
        );
        // 跨窗口右边界 → 裁剪到窗口末
        assert_eq!(
            bar_span(Some(date("2026-11-26")), Some(date("2026-12-20")), &window),
            Some(BarSpan {
                offset_days: 28,
                length_days: 2
            })
        );
        // 完全在窗口外 → 不画
        assert_eq!(
            bar_span(Some(date("2026-08-01")), Some(date("2026-08-05")), &window),
            None
        );
        // 脏数据（due < start）→ 按单日 start 处理，不 panic
        assert_eq!(
            bar_span(Some(date("2026-11-05")), Some(date("2026-11-01")), &window),
            Some(BarSpan {
                offset_days: 7,
                length_days: 1
            })
        );
        // 两个都没有 → 排不了期
        assert_eq!(bar_span(None, None, &window), None);
    }

    #[test]
    fn today_line_and_overdue() {
        let window = range("2026-10-29", "2026-11-27");
        assert_eq!(today_offset(&window, date("2026-10-29")), Some(0));
        assert_eq!(today_offset(&window, date("2026-11-10")), Some(12));
        assert_eq!(today_offset(&window, date("2026-10-28")), None);
        assert_eq!(today_offset(&window, date("2026-11-28")), None);

        let today = date("2026-10-29");
        assert!(is_overdue(Some(date("2026-10-28")), "DOING", today));
        assert!(
            !is_overdue(Some(date("2026-10-29")), "DOING", today),
            "当天到期不算逾期"
        );
        assert!(
            !is_overdue(Some(date("2026-10-01")), "DONE", today),
            "已完成的旧任务不标红"
        );
        assert!(!is_overdue(Some(date("2026-10-01")), "CANCELLED", today));
        assert!(!is_overdue(None, "DOING", today));
    }

    #[test]
    fn gantt_edges_keep_only_both_ends_visible() {
        let edges = [edge(2, 1), edge(3, 2), edge(9, 8)];
        let keep: HashSet<Uuid> = [id(1), id(2), id(3)].into_iter().collect();
        let kept = retain_inner_edges(&edges, &keep);
        assert_eq!(kept, vec![edge(2, 1), edge(3, 2)]);
    }
}

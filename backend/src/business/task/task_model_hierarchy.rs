// ============================================================================
// task_model_hierarchy.rs —— 父子链：深度校验 / 进度重算 / 状态级联 / 取消级联 / 巡检
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_model_base::*;
use super::task_model_query::*;
use super::task_model_rows::*;
use super::task_model_write::*;
use crate::business::task::state::{self, ChildSnapshot, TaskStatus};

/// 祖先链查询：**一次 `WITH RECURSIVE`** 把起点父任务及其全部祖先取回来。
///
/// 返回的每一行是链上一级：第 1 行 = 起点（`depth = 1`），越往上 `depth` 越大。
/// 一次查询拿全链，**不做「每层一次往返」的 N+1**（本卡红线）。
///
/// # 递归上限 `depth < MAX_TASK_DEPTH + 1`
///
/// ① 只要判定出「新任务会落到第 9 层」，再往上爬没有意义；
/// ② `parent_id` 被手工改成环（`A→B→A`）时，没有上限的递归会一直转 —— 上限保证它停。
///
/// 注意：链条**不按 `deleted_at` 截断**。被软删的中间层仍然占一层（宁可多算深度，
/// 也不让「删掉父任务再挂子任务」绕过层级限制），只会比树形展示的口径更严。
// SCOPE-EXEMPT: 只产出「向上爬祖先链」的 SQL 文本（供 validate_parent 用），返回 Ok/Err 不返回行集；调用方 handler 建/改任务时已按 id 定位本任务并过中间件闸 business:task:create（handler.rs:311）/ business:task:update（handler.rs:577）。
pub(super) fn ancestor_chain_sql() -> String {
    format!(
        "WITH RECURSIVE chain(id, parent_id, depth) AS ( \
           SELECT t.id, t.parent_id, 1::bigint FROM biz_task t WHERE t.id = ?::uuid \
           UNION ALL \
           SELECT p.id, p.parent_id, c.depth + 1 \
             FROM chain c JOIN biz_task p ON p.id = c.parent_id \
            WHERE c.depth < {cap} \
         ) SELECT id, depth FROM chain ORDER BY depth",
        cap = MAX_TASK_DEPTH + 1,
    )
}

/// 父任务校验：存在、未软删、深度不超过 [`MAX_TASK_DEPTH`] 层、不是自己 / 不是自己的子孙。
///
/// # 规则（老大拍板：父子深度 2 → 8 层）
///
/// - 目标父任务必须存在且未软删；
/// - 新任务落在「父任务的下一层」：父任务链长（含父任务自身）`>= MAX_TASK_DEPTH`
///   就拒绝 —— 也就是 **第 8 层建得出来、第 9 层建不出来**；
/// - 不能把自己设成父任务；
/// - 父任务不能是自己的**子孙**（我出现在它的祖先链里 = 挂上去成环，
///   `parent_id` 成环会让树形列表 / 详情页无限展开）；
/// - 祖先链本身已有环（历史脏数据 `A→B→A`）→ 直接拒绝，**绝不死循环**。
///
/// # 异常情况
///
/// - 父任务不存在 / 已删除 → `DbError::InvalidValue`（handler 转 400 + `TASK_PARENT_INVALID`）；
/// - 深度 / 成环 / 祖先链脏环 → 同上。
// SCOPE-EXEMPT: 父任务存在性 + 深度 + 成环校验：输入是用户指定的 parent_id，输出只有 Ok/Err，不向用户产出任何行；注入 scope 会改变语义（看不见的父任务被判「不存在」），是否允许挂到该父任务属业务规则而非数据可见性；权限：handler.rs:365（create，business:task:create）/ handler.rs:639（update，business:task:update）。
pub async fn validate_parent(parent_id: Uuid, self_id: Option<Uuid>) -> DbResult<()> {
    if Some(parent_id) == self_id {
        return Err(DbError::InvalidValue("父任务不能是任务自己".to_string()));
    }
    let rows = db()
        .find_with_params(
            "SELECT id, deleted_at FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&parent_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(DbError::InvalidValue(format!("父任务不存在：{parent_id}")));
    };
    let deleted_at: Option<chrono::DateTime<Utc>> = row.get_typed("deleted_at")?;
    if deleted_at.is_some() {
        return Err(DbError::InvalidValue(format!("父任务已删除：{parent_id}")));
    }
    // 祖先链（**一次**递归 CTE）：父任务自身是第 1 层
    let chain_sql = ancestor_chain_sql();
    let chain_rows = db().find_with_params(&chain_sql, vec![Value::from(&parent_id)]).await?;
    let mut seen: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    let mut chain_len: usize = 0;
    for chain_row in &chain_rows {
        let node: Uuid = chain_row.get_typed("id")?;
        let depth: i64 = chain_row.get_typed("depth")?;
        if !seen.insert(node) {
            // 同一个任务在链上出现两次 = 库里已有环；递归上限把它停下来了，
            // 但层级已经无从判定 → 直接拒绝，而不是猜一个深度。
            return Err(DbError::InvalidValue(format!(
                "父任务链成环（数据异常）：{node} 在 {parent_id} 的祖先链里重复出现"
            )));
        }
        chain_len = chain_len.max(depth.max(1) as usize);
    }
    // 防环：我要挂到的父任务，它的祖先链里出现了我 → 挂上去就成环
    if let Some(self_id) = self_id
        && seen.contains(&self_id)
    {
        return Err(DbError::InvalidValue("父任务不能是自己的子任务（会成环）".to_string()));
    }
    // 深度：新任务 = 第 chain_len + 1 层
    if chain_len >= MAX_TASK_DEPTH {
        return Err(DbError::InvalidValue(format!(
            "子任务最多 {MAX_TASK_DEPTH} 层：父任务链已到第 {chain_len} 层"
        )));
    }
    Ok(())
}

// ==================== 状态机 / 进度（T2） ====================
//
// 本段的每个函数都只做「一件事」，状态机规则本身全部在 [`crate::business::task::state`]
// （纯函数、可无库单测）。这里只负责把它翻译成 SQL。
//
// 三条写路径的边界（改这里之前先读）：
//   ① **状态流转** [`apply_status`]：只写 `biz_task` 的 5 个列，不碰子任务、不碰父任务；
//   ② **进度汇总** [`recalc_ancestors`]：**只由子任务变更触发**，沿 `parent_id` 向上逐级；
//   ③ **进度直改** [`set_progress`]：**有子任务时必须先被 handler 拦下来**（409 + 63012），
//      本函数不做这项判断 —— 它拿不到「谁在调」，把闸放在 handler 是刻意的（一处判定）。

/// 沿 `parent_id` 向上逐级重算进度（**写路径**：子任务变更后调用）。
///
/// # 为什么是「写路径重算」而不是「读时计算」
///
/// 读路径（列表 / 详情 / 甘特图）每次都要聚合子任务 = 每条父任务多一次聚合查询，
/// 一个 20 行的看板就是 20 次额外往返；而写操作是少数（一次状态变更 = 1 次重算）。
/// 代价是「父任务进度可能短暂过期」—— 所以**所有**会改子任务进度的写路径
/// （状态流转、进度直改、建子任务）都必须调用本函数，这是本段的唯一契约。
///
/// # 行为
///
/// - 从 `start` 的父任务开始，逐级向上（深度上限 [`MAX_PARENT_DEPTH`] + visited 防环）；
/// - 每级用 [`state::aggregate_progress`] 算出新进度，**变了才写**；
/// - 变了就写 `biz_task.progress` + 一条 `PROGRESS_AUTO` 评论（[`PROGRESS_AUTO_COMMENT`]）；
/// - **绝不改状态**（架构 D5：父任务进度到 100% 也不自动 DONE）。
///
/// # 返回值
///
/// 每一级的重算结果（顺序：由近及远）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（由调用方的事务整体回滚）。
pub async fn recalc_ancestors(ex: &Transaction, start: Uuid, operator: Uuid) -> DbResult<Vec<ProgressRecalc>> {
    let mut results = Vec::new();
    let mut visited = std::collections::BTreeSet::new();
    let mut cursor = parent_of(ex, start).await?;
    while let Some(parent_id) = cursor {
        if visited.len() >= MAX_PARENT_DEPTH {
            tracing::warn!(
                task_id = %start,
                depth = visited.len() as i64,
                "父任务链过深（疑似成环），停止向上重算"
            );
            break;
        }
        if !visited.insert(parent_id) {
            tracing::warn!(
                task_id = %start,
                parent_id = %parent_id,
                "父任务链成环，停止向上重算"
            );
            break;
        }
        let children = child_rows_with(ex, parent_id).await?;
        let snapshots: Vec<ChildSnapshot> = children.iter().map(ChildRow::snapshot).collect();
        let Some(new_progress) = state::aggregate_progress(&snapshots) else {
            // 没有（未删除的）子任务：这一级由手动进度维护，继续向上没有意义
            break;
        };
        let old_progress = scalar_progress(ex, parent_id).await?;
        let all_done = state::ready_to_close(&snapshots);
        let mode = state::aggregation_label(&snapshots);
        if old_progress != new_progress {
            set_progress(ex, parent_id, new_progress, operator).await?;
            if PROGRESS_AUTO_COMMENT {
                insert_comment(
                    ex,
                    parent_id,
                    operator,
                    &format!(
                        "进度自动汇总：{old_progress}% → {new_progress}%（{} 个子任务，{mode}）",
                        snapshots.len()
                    ),
                    COMMENT_TYPE_PROGRESS_AUTO,
                    operator,
                )
                .await?;
            }
            tracing::info!(
                task_id = %parent_id,
                old = i64::from(old_progress),
                new = i64::from(new_progress),
                children = snapshots.len() as i64,
                "父任务进度已重算"
            );
        }
        results.push(ProgressRecalc {
            task_id: parent_id,
            old: old_progress,
            new: new_progress,
            children: snapshots.len(),
            all_done,
            mode,
        });
        cursor = parent_of(ex, parent_id).await?;
    }
    Ok(results)
}

// ==================== 父任务状态级联（T10） ====================
//
// 老大的原始缺陷（2026-10-30）：「有些子任务已经完成了，但是父任务还是待开始状态」。
// 修复前 [`recalc_ancestors`] 只重算 `progress`，**状态始终独立**：W2 的 3 个子任务
// 完成 2 个时父任务还挂在 `TODO`，65% 的进度条和「待开始」自相矛盾。
//
// 本段只做**状态**级联；`progress` 仍由 [`recalc_ancestors`] 按原有口径算（红线：不改公式）。
// 三条规则与任务卡一一对应：
//
// | 规则 | 条件 | 动作 |
// |------|------|------|
// | ① | 父任务 `TODO` + 至少一个子任务不是 `TODO` | 父任务 → `DOING` |
// | ② | 子任务**全部**到终态（`DONE` / `CANCELLED`） | 父任务 → `REVIEW`（走人工验收，不自动 DONE） |
// | ⑨ | 父任务已是终态（`DONE` / `CANCELLED`） | 一律不动（终态不回退） |
//
// 为什么 ② 是「进 `REVIEW`」而不是「只打一个可提验收标记」：父任务也是任务，子任务全结束
// = 交付物齐了，该走验收流程（老大原话「完成的时候也按照工作流程进行提交直到完成」）。
// `REVIEW → DONE` 仍要人点（见 [`state::TRANSITIONS`]），所以自动进 `REVIEW` 不会把活
// 悄悄算成完成；而「00% 进度条 + 待开始」这种自相矛盾的状态必须被消灭。

/// 读一个任务的状态字面量（`None` = 行不存在 / 已软删 / 状态不是 5 态之一）。
///
/// 解析不出来时不猜、不 panic：调用方一律视为「这条任务不该被级联」，停止向上爬。
// SCOPE-EXEMPT: 事务内按 id 读单个状态字面量（父任务状态级联的内部读），不产出行集；调用链 cascade_parent_status ← handler.rs:1005 find_detail 可见性校验（scope ∪ 参与关系）后的 change_status 事务；管理端修复走超管（handler.rs:4697 is_super_admin）。
pub(super) async fn scalar_status(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<TaskStatus>> {
    let rows = ex
        .query_sql(
            "SELECT status FROM biz_task WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let raw: String = row.get_typed("status")?;
    Ok(TaskStatus::parse(&raw))
}

/// 沿 `parent_id` 向上级联父任务状态（**写路径**：子任务状态流转后调用）。
///
/// # 三条红线
///
/// 1. **同一事务**：`ex` 必须传业务事务（`&impl SqlExecutor`，**不许**在这里调 `db()`）。
///    级联失败与子任务状态变更一起回滚 —— 宁可这次状态没改成，也不接受「父任务状态错着」；
/// 2. **防环**：`visited` 集合 + 爬升上限 [`MAX_TASK_DEPTH`]（8 层）。脏数据成环
///    （`A.parent = B`、`B.parent = A`，任何接口都造不出来）时停下来并打 WARN；
/// 3. **不改 progress**：这里只写 `status`。进度归 [`recalc_ancestors`]，两件事分开。
///
/// # 行为
///
/// 从 `start` 的父任务开始逐级向上，每级按 [`cascade_target`] 判一次：
///
/// - 要改 → 写 `status` + 一条 `STATUS_CHANGE` 评论；**变更日志（`sys_change_log`）由
///   `handler::change_status` 在同一个事务闭包里补写**（C7 G5：`record` 的调用点必须
///   在 `db().transaction(|tx| ...)` 闭包内，模型层只负责状态与时间线）；
/// - **不管这级改没改都继续往上爬**：上层判定依赖下层的最新状态 —— 三层链路里
///   孙任务完成会让子任务进 `REVIEW`，祖父因此从 `TODO` 变 `DOING`（这条是
///   「孙任务变化 → 子级联 → 父级联」用例的行为依据）。
///
/// # 返回值
///
/// 真正改了状态的层级（顺序：由近及远）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（由调用方的事务整体回滚）。
pub async fn cascade_parent_status(ex: &impl SqlExecutor, start: Uuid, operator: Uuid) -> DbResult<Vec<StatusCascade>> {
    let mut results = Vec::new();
    // `visited` 从 `start` 起算：`A → B → A` 这种环在第二次回到 `A` 时立刻停
    let mut visited = std::collections::BTreeSet::new();
    visited.insert(start);
    let mut cursor = parent_of(ex, start).await?;
    while let Some(parent_id) = cursor {
        if visited.len() >= MAX_TASK_DEPTH {
            tracing::warn!(
                task_id = %start,
                depth = visited.len() as i64,
                limit = MAX_TASK_DEPTH as i64,
                "父任务链已达深度上限（疑似成环），停止状态级联"
            );
            break;
        }
        if !visited.insert(parent_id) {
            tracing::warn!(
                task_id = %start,
                parent_id = %parent_id,
                "父任务链成环，停止状态级联"
            );
            break;
        }
        // 下一级游标先算好：这一级不改状态也要继续往上爬
        let next = parent_of(ex, parent_id).await?;
        let children = child_rows_with(ex, parent_id).await?;
        let snapshots: Vec<ChildSnapshot> = children.iter().map(ChildRow::snapshot).collect();
        let all_terminal = !snapshots.is_empty() && state::open_children(&snapshots).is_empty();
        if let Some(parent_status) = scalar_status(ex, parent_id).await?
            && let Some(target) = cascade_target(parent_status, &snapshots)
        {
            let reason = if all_terminal {
                CASCADE_REASON_ALL_TERMINAL
            } else {
                CASCADE_REASON_FIRST_ACTIVE
            };
            let update = StatusUpdate {
                to: target.code().to_string(),
                // 红线：父任务进度由子任务汇总（`recalc_ancestors` 已在本事务里算过），
                // 级联不许顺手改它。
                progress: None,
                // `DOING` / `REVIEW` 没有完成时刻；级联也不会产出终态（规则⑨）。
                done_at: None,
                reject_reason: None,
                cancel_reason: None,
            };
            let affected = apply_status(ex, parent_id, &update, operator).await?;
            if affected == 0 {
                // 事务里被并发软删：不写评论、不写日志，也不再往上爬
                break;
            }
            insert_comment(
                ex,
                parent_id,
                operator,
                &format!(
                    "状态变更：{}（{}）→ {}（{}）（系统自动级联：{reason}）",
                    parent_status.label(),
                    parent_status.code(),
                    target.label(),
                    target.code()
                ),
                COMMENT_TYPE_STATUS_CHANGE,
                operator,
            )
            .await?;
            tracing::info!(
                task_id = %parent_id,
                from = parent_status.code(),
                to = target.code(),
                children = snapshots.len() as i64,
                reason,
                "父任务状态已级联"
            );
            results.push(StatusCascade {
                task_id: parent_id,
                from: parent_status,
                to: target,
                children: snapshots.len(),
                all_terminal,
                reason,
            });
        }
        cursor = next;
    }
    Ok(results)
}

// ==================== 父任务状态巡检 / 兜底对账（T13） ====================
//
// 为什么已经有 `settle` 里的级联，还要一段巡检：
//
// `flow::task_bridge::settle` 跑在**审批事务提交之后**（红线：审批结论已落库，
// 不许因任务侧失败而回滚），所以它里面的父任务级联只能开**独立事务**，与
// 「任务状态」不是同一个事务。极端情况（那一刻库故障 / 进程被杀 / 并发冲突）
// 仍会留下「子任务全终态、父任务还停在 DOING」的不一致 —— 这段巡检把它找出来
// 并修回去（与「一物一码 4 条不变式」同一思路：实时联动 + 巡检兜底）。
//
// 修复**不另写一份判定**：对每条命中的父任务，取它的一个子任务当起点跑
// [`cascade_parent_status`] —— 走的就是「子任务状态变更后」那条生产路径
// （含防环、含时间线评论），巡检与实时级联因此不可能判出两个口径。

/// 扫出「子任务全终态、父任务却还没到 `REVIEW`」的父任务（只读，不写任何东西）。
///
/// # 判据
///
/// - 父任务未删除，且状态是 `TODO` / `DOING`（`REVIEW` 已经是对的、终态不回退，
///   这两种都不算不一致）；
/// - 至少有一个未删除子任务，且**全部**子任务处于终态（[`TERMINAL_STATUS_CODES`]）；
/// - 命中即：「[`cascade_target`] 规则③本该把它推到 `REVIEW`，但它没到」。
///
/// # 参数
///
/// - `ex`：`&Db` 或业务事务（只读）；
/// - `limit`：最多返回多少条（调用方用 [`normalize_repair_limit`] 归一化）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。库里状态不是 5 态之一的行（历史脏值）**跳过不猜**。
// SCOPE-EXEMPT: 产出的是「父任务状态不一致」的**运维诊断清单**（父任务 id + 样本子任务），不是业务行集展示；唯一读者是批量修数接口 handler.rs:4689 repair_parent_status —— 中间件 business:task:update + handler.rs:4697 is_super_admin（非超管 403 + 63002），而超管的 scope 恒为「全部」，注入片段是 no-op；文档口径亦明确「这是跨全库的批量修数接口，不按单个任务的数据范围做闸」。
pub async fn scan_parent_status_gaps(ex: &impl SqlExecutor, limit: i64) -> DbResult<Vec<ParentStatusGap>> {
    let terminal = TERMINAL_STATUS_CODES
        .iter()
        .map(|code| format!("'{code}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT p.id AS parent_id, p.task_no, p.title, p.status, p.progress, \
                count(c.id) AS children, min(c.id::text)::uuid AS sample_child \
           FROM biz_task p \
           JOIN biz_task c ON c.parent_id = p.id AND c.deleted_at IS NULL \
          WHERE p.deleted_at IS NULL AND p.status IN ('TODO', 'DOING') \
          GROUP BY p.id, p.task_no, p.title, p.status, p.progress \
         HAVING count(c.id) FILTER (WHERE c.status NOT IN ({terminal})) = 0 \
          ORDER BY p.created_at, p.id \
          LIMIT ?::bigint"
    );
    let rows = ex.query_sql(sql, vec![Value::I64(limit)]).await?;
    let mut gaps = Vec::with_capacity(rows.len());
    for row in &rows {
        let raw: String = row.get_typed("status")?;
        let Some(status) = TaskStatus::parse(&raw) else {
            tracing::warn!(status = %raw, "任务状态不是 5 态之一，巡检跳过该行");
            continue;
        };
        gaps.push(ParentStatusGap {
            task_id: row.get_typed("parent_id")?,
            task_no: row.get_typed("task_no")?,
            title: row.get_typed("title")?,
            status,
            progress: row.get_typed("progress")?,
            children: row.get_typed("children")?,
            target: TaskStatus::Review,
            sample_child: row.get_typed("sample_child")?,
        });
    }
    Ok(gaps)
}
// ==================== 父任务取消 → 子任务级联取消（T11） ====================
//
// 老大的原始缺陷（2026-10-30，老六流程自检实测）：
//
// ```text
// ① 父任务 P 推到 CANCELLED → 200
// ② 子任务 C 仍是 DOING        ← 漏洞 1：父已取消，子还在跑
// ③ 子任务 C 还能推到 REVIEW   ← 漏洞 2：父已取消，子还能推进
// ```
//
// 后果：**用户以为整条线作废了，worker 还在按子任务干活**，列表里出现
// 「父已取消 / 子进行中」的矛盾状态。本段两条规则：
//
// | 规则 | 条件 | 动作 |
// |------|------|------|
// | ① | 本任务推到 `CANCELLED` | 同一事务里，全部**非终态**子孙（TODO/DOING/REVIEW）→ `CANCELLED` |
// | ② | 父任务已是 `CANCELLED` | 子任务的状态流转一律拒绝（`CANCELLED` 自己幂等放行） |
//
// 四条红线（本卡验收口径）：
//
// 1. **一条 SQL**：级联用 `WITH RECURSIVE … UPDATE … RETURNING` 一次搞定 ——
//    8 层 × 多个子任务时「每层每行一次 UPDATE」会爆炸（本卡红线 4）；
// 2. **`DONE` 不回退**：已完成的子任务保持 `DONE`（事实不能被抹掉），但它自己
//    下面还没结束的子孙照样取消；
// 3. **`progress` 一个字不动**：公式归 [`recalc_ancestors`]，级联只写 `status`
//    （与 T10 同一条红线）；
// 4. **防环上限 = [`MAX_TASK_DEPTH`]**：`parent_id` 被手工改成环（`A→B→A`）时
//    递归在第 8 层停下，**绝不死循环**（任何接口都造不出环，但脏数据可能在）。

/// 读「本任务的父任务」当前状态（`None` = 顶层任务 / 父任务不存在或已软删）。
///
/// 写路径传业务事务、读路径传 `db()`（同为 `&impl SqlExecutor`）。守卫要防的正是
/// 「查的时候父任务还没取消、写的时候已经取消了」，所以 handler 在**事务内**会
/// 用 `&tx` 再查一次（同一份实现，不留第二个真相源）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
// SCOPE-EXEMPT: 按 id 取父任务当前状态（「父任务已取消 → 子任务不许推进」的守卫），单值不产出行集；调用 handler.rs:1034 / :1205（change_status 事务内），可见性已在 handler.rs:1005 find_detail 过闸。
pub async fn parent_status(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<TaskStatus>> {
    let rows = ex
        .query_sql(
            "SELECT p.status FROM biz_task t JOIN biz_task p ON p.id = t.parent_id \
              WHERE t.id = ?::uuid AND p.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let raw: String = row.get_typed("status")?;
    Ok(TaskStatus::parse(&raw))
}

/// 级联取消的 SQL：**一条** `WITH RECURSIVE … UPDATE … RETURNING`。
///
/// # 三处非显然的写法
///
/// 1. `RETURNING b.id, v.status` 里的 `v.status` 是 CTE 快照里的**旧状态**：
///    PG 的 `RETURNING` 只能取新值（`b.status` 已经是 `CANCELLED`），而变更日志
///    要的正是「从哪个状态被取消的」—— 旧值必须从 `FROM` 那一侧拿；
/// 2. `tree.depth < MAX_TASK_DEPTH` 既是**层级上限**（第 8 层之后没有合法任务），
///    也是**防环闸**（脏数据成环时递归停在这里，不是靠运气）；
/// 3. `status NOT IN ('DONE','CANCELLED')` 落在 `victims` 里（`UPDATE` 的
///    `WHERE` 只能写目标表列，写 `v.status` 更清楚）：`DONE` 不回退、已取消不重复写。
// SCOPE-EXEMPT: 只产出级联取消的 SQL 文本（WITH RECURSIVE … UPDATE … RETURNING），是**写路径**：按父任务 id 把子孙写到 CANCELLED，不向用户产出可浏览行集；调用 link cascade_cancel_descendants ← handler.rs:1252 （change_status 事务内，可见性见 handler.rs:1005，角色/状态机判定见 handler.rs:1047 state::check_transition）。
pub(super) fn cascade_cancel_sql() -> String {
    format!(
        "WITH RECURSIVE tree(id, depth) AS ( \
           SELECT t.id, 1::bigint FROM biz_task t WHERE t.id = ?::uuid AND t.deleted_at IS NULL \
           UNION ALL \
           SELECT c.id, tree.depth + 1 FROM biz_task c JOIN tree ON c.parent_id = tree.id \
            WHERE c.deleted_at IS NULL AND tree.depth < {cap} \
         ), victims AS ( \
           SELECT b.id, b.status, b.task_no FROM biz_task b \
            WHERE b.id IN (SELECT id FROM tree WHERE depth > 1) \
              AND b.deleted_at IS NULL \
              AND b.status NOT IN ('DONE', 'CANCELLED') \
         ) \
         UPDATE biz_task b SET status = 'CANCELLED', cancel_reason = ?, \
                updated_at = now(), update_by = ?::uuid \
           FROM victims v WHERE b.id = v.id \
         RETURNING b.id, v.status, v.task_no",
        cap = MAX_TASK_DEPTH,
    )
}

/// 把 `parent_id` 下面**所有层级**的非终态子孙一次性改成 `CANCELLED`（规则①）。
///
/// # 行为
///
/// - 只改子孙，**不改 `parent_id` 自己**（本任务的状态由 handler 的 `apply_status` 写）；
/// - `DONE` / `CANCELLED` 的子孙不动（前者是事实、后者已经收敛）；
/// - 只写 `status` / `cancel_reason` / `updated_at` / `update_by`，**不碰 `progress`**；
/// - 返回值 = 真正被取消的那些任务及其旧状态（handler 据此逐条写变更日志）。
///
/// # 红线：同一事务
///
/// `ex` 必须传业务事务（`&impl SqlExecutor`，**不许**在这里调 `db()`）：级联失败
/// 要和父任务的取消一起回滚 —— 宁可直接拒了这次取消，也不接受「父任务 CANCELLED、
/// 子任务还在 DOING」这种半截状态（正是本卡要修的漏洞形态）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（由调用方的事务整体回滚）。
pub async fn cascade_cancel_descendants(
    ex: &impl SqlExecutor,
    parent_id: Uuid,
    operator: Uuid,
    reason: &str,
) -> DbResult<Vec<CancelCascade>> {
    let rows = ex
        .query_sql(
            cascade_cancel_sql(),
            vec![
                Value::from(&parent_id),
                Value::Text(reason.to_string()),
                Value::from(&operator),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let task_id: Uuid = row.get_typed("id")?;
        let task_no: String = row.get_typed("task_no")?;
        let raw: String = row.get_typed("status")?;
        match TaskStatus::parse(&raw) {
            Some(from) => out.push(CancelCascade { task_id, task_no, from }),
            // 状态不是 5 态之一（脏数据）：取消已经落库，但这一行没法写「从什么状态来的」，
            // 打 WARN 让运维看见，不 panic（不能因为一行脏数据把整笔取消回滚）。
            None => tracing::warn!(
                task_id = %task_id,
                status = %raw,
                "级联取消命中非 5 态状态，变更日志无法记录 from"
            ),
        }
    }
    tracing::info!(
        task_id = %parent_id,
        cancelled = out.len() as i64,
        reason,
        "父任务取消：级联取消子任务"
    );
    Ok(out)
}

/// 任务的父任务 ID（`None` = 顶层任务 / 不存在）。
// SCOPE-EXEMPT: 按 id 取 parent_id 单值（重算进度时向上走一层），不产出行集；调用 recalc_ancestors ← handler.rs:1342（change_status）/ :1558（change_progress，可见性 handler.rs:1509 find_detail）/ :1692（子任务创建）。
pub(super) async fn parent_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = ex
        .query_sql(
            "SELECT parent_id FROM biz_task WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("parent_id"),
        None => Ok(None),
    }
}

/// 任务的当前进度（重算前读旧值，只用于「变了才写」与评论）。
// SCOPE-EXEMPT: 按 id 读单个 progress 旧值（「变了才写」与评论），单值不产出行集；与 parent_of 同一调用链（handler.rs:1342 / :1558 / :1692），可见性在 handler 侧 find_detail 完成。
pub(super) async fn scalar_progress(ex: &impl SqlExecutor, id: Uuid) -> DbResult<i16> {
    let rows = ex
        .query_sql(
            "SELECT progress FROM biz_task WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("progress"),
        // 行不存在时按 0 处理：随后 `set_progress` 的 WHERE 会命中 0 行，
        // 不会凭空造出一条进度。
        None => Ok(0),
    }
}

// 说明（T3）：T2 在这里放过一个 `inbox_enabled(user_id)`（Rust 侧逐个判断偏好），
// 已**删除**。理由：T3 加分派 / 协作人通知后，通知偏好会变成「任务通知看这一份、
// 公告 / 系统通知看另一份」的双实现，迟早不一致（实测过的教训：两处实现 = 两个
// 真相源）。现在偏好过滤**只有一处**：`business/notice` 的
// `fanout::respecting_preference`，由 `notice::dispatch(draft.force = false)` 触发。
// 要改偏好口径，改 `notice`；**不要在本文件加回本地过滤**。

// ==================== 分派 / 转派 / 协作人（T3） ====================

//! 菜单徽标（红点 + 数字）的数据层：四类「待我处理」计数的**唯一来源**。
//!
//! # 一句话
//!
//! 侧边栏每个菜单后面的红点数字都来自 [`load`] 返回的四个桶；目录（父节点）徽标
//! 由**前端**把子菜单的桶去重后相加，后端不重复算任何一项。
//!
//! # 四个桶与口径
//!
//! | 字段 | 含义 | 口径 | 数据源 |
//! |------|------|------|--------|
//! | `announcement_unread` | 公告未读 | 可见 ∧ 未过期 ∧ 无我的已读回执 | `sys_announcement` ⨝ `sys_announcement_read` |
//! | `notice_unread` | 通知未读 | `read_flag = FALSE AND is_deleted = FALSE` | `sys_notice_receiver` |
//! | `flow_todo` | 待我审批 | `assignee_id = me AND status = 'PENDING'` ∧ 实例未删 ∧ 待办类型 | `biz_flow_task` ⨝ `biz_flow_instance` |
//! | `task_todo` | 待我处理 | `owner_id = me AND status = 'TODO'` ∧ 创建于 7 天内 | `biz_task` |
//!
//! # 为什么复用现有 model 函数而不是在这里重写 SQL
//!
//! `announcement_unread` / `notice_unread` 已经有实现（公告可见性谓词、通知部分索引谓词）。
//! 徽标若复制一份 SQL，公告可见性规则或通知软删规则一改，徽标就会**静默**与列表不一致
//! （用户点进去数不上），且这种漂移只在数据形态特殊时才暴露。所以这两项直接调用既有函数：
//!
//! - [`crate::business::announcement::model::load_audience`] + [`crate::business::announcement::model::count_unread`]
//! - [`crate::business::notice::model::count_unread`]
//!
//! `flow_todo` / `task_todo` 没有现成计数函数（待办只有分页接口），SQL 写在本文件，
//! 但 `status` / `task_kind` 的取值**取自** `flow` 模块的公开常量，口径仍然只有一份。
//!
//! # 数据权限
//!
//! SCOPE-EXEMPT: 徽标是「我自己有多少没读 / 多少个待办」的聚合计数，行级归属就是
//! `AuthClaims.sub`（公告另加「我可见」谓词，与公告列表同源）。给这几条 count 注入
//! `scope::` 会让「自定义数据范围」的用户看到与自己无关的待办数，或反过来把自己的
//! 待办漏掉 —— 这两种都是把「个人提醒」变成「部门统计」的错误语义。
//!
//! # 索引
//!
//! 四条查询全部命中既有索引，**本模块不新增任何索引 / 迁移**：
//!
//! | 桶 | 索引 | 说明 |
//! |----|------|------|
//! | `announcement_unread` | `idx_sys_announcement_read_user (user_id, announcement_id)` | `NOT EXISTS` 反连接走该索引 |
//! | `notice_unread` | `idx_sys_notice_receiver_unread (user_id, notice_id) WHERE read_flag = false AND is_deleted = false` | 谓词与部分索引**逐字一致** |
//! | `flow_todo` | `idx_biz_flow_task_mine (assignee_id, status, created_at DESC)` | 两个前导列即 `assignee_id + status` |
//! | `task_todo` | `idx_biz_task_owner_status_due (owner_id, status, due_date)` | 前两列 `owner_id + status` 走 Index Cond，`created_at` 在索引项上做 Filter |

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::announcement;
use crate::business::flow;
use crate::business::notice;

/// 「待我处理」任务的时间窗（天）。
///
/// # 为什么是 7 天
///
/// 任务表是长期累积的（一条 3 个月前建的 TODO 仍然 TODO），不设窗口时所有人的徽标
/// 只涨不落，用户会把它当装饰而不是提醒。7 天是产品口径（老大 2026-10-30 确认的 B 方案）：
/// 「最近一周交给我的、还没动的活儿」。
pub const TASK_TODO_WINDOW_DAYS: i32 = 7;

/// 四类徽标计数。
///
/// 字段名与前端 `src/stores/badge.ts` 的 `BadgeCounts` 逐字一致（`snake_case`），
/// 前端据此把每个菜单路径映射到其中一个桶。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BadgeCounts {
    /// 公告未读（可见 ∧ 未过期 − 已读）。
    pub announcement_unread: i64,
    /// 通知未读（`sys_notice_receiver.read_flag = FALSE`）。
    pub notice_unread: i64,
    /// 待我审批（`biz_flow_task` 中 `assignee_id = me` 且 `status = 'PENDING'`）。
    pub flow_todo: i64,
    /// 待我处理（`biz_task` 中 `owner_id = me`、`status = 'TODO'`、7 天内创建）。
    pub task_todo: i64,
}

impl BadgeCounts {
    /// 四个桶的和。
    ///
    /// # 用途
    ///
    /// 只用于日志 / 自检（「这次刷新一条都没变」）。**不是**任何菜单的徽标值 ——
    /// 目录徽标是子菜单**去重**后相加，不是四个桶相加（任务三视图共用一个桶，
    /// 直接相加会把 2 显示成 6）。
    pub fn total(&self) -> i64 {
        self.announcement_unread + self.notice_unread + self.flow_todo + self.task_todo
    }
}

/// 待我审批的 `WHERE` 片段（`?` 只有 1 个：`assignee_id`）。
///
/// 口径与 `flow::model::page_my_tasks` 的待办列表**同源**：状态取
/// [`flow::model::MY_TODO_STATUSES`]、类型取 [`flow::dto::TASK_KINDS`]，
/// 并且同样排除已软删的实例 —— 否则徽标数会比点进去看到的列表多，
/// 用户会认为「红点是假的」。常量取自模块，不是用户输入，格式化成 `IN (...)`
/// 字面量与既有实现同款（本仓没有绑定数组的 `IN` 语法糖）。
fn flow_todo_where() -> String {
    let statuses = flow::model::MY_TODO_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(",");
    let kinds = flow::dto::TASK_KINDS
        .iter()
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(",");
    format!(" WHERE t.assignee_id = ?::uuid AND t.status IN ({statuses}) AND t.task_kind IN ({kinds})")
}

/// 待我处理任务的 `WHERE` 片段（`?` 只有 1 个：`owner_id`）。
///
/// `created_at` 的窗口写成字面量 interval：窗口是编译期常量
/// （[`TASK_TODO_WINDOW_DAYS`]），不是请求参数，没有注入面；
/// 写成 `?::int * interval '1 day'` 反而会让 planner 对窗口失去常量估计。
fn task_todo_where() -> String {
    format!(
        " WHERE t.owner_id = ?::uuid AND t.status = 'TODO' AND t.deleted_at IS NULL \
           AND t.created_at >= now() - interval '{TASK_TODO_WINDOW_DAYS} days'"
    )
}

/// 待我审批条数。
pub async fn count_flow_todo(user_id: Uuid) -> DbResult<i64> {
    let sql = format!(
        "SELECT count(*) AS todo FROM biz_flow_task t \
           JOIN biz_flow_instance i ON i.id = t.instance_id AND i.deleted_at IS NULL{}",
        flow_todo_where()
    );
    db().scalar_with_params(&sql, vec![Value::from(user_id)]).await
}

/// 待我处理条数（`owner_id = me` ∧ `TODO` ∧ 7 天内创建）。
pub async fn count_task_todo(user_id: Uuid) -> DbResult<i64> {
    let sql = format!("SELECT count(*) AS todo FROM biz_task t{}", task_todo_where());
    db().scalar_with_params(&sql, vec![Value::from(user_id)]).await
}

/// 四个桶一次性取齐（**四条 count 并发**）。
///
/// # 为什么并发
///
/// 四条 SQL 互不依赖，串行是 4 次 round-trip；`tokio::join!` 后总耗时 ≈ 最慢那条。
/// `nik` 的 `Db` 是 `Arc<dyn RawExecutor>`（连接池），`&self` 方法可以直接并发调用，
/// 不需要 clone 连接、也不需要额外事务（读一致性对「徽标」这种弱一致场景没有要求）。
///
/// 公告的受众（角色 / 部门）需要**先**查一次才能构造可见性谓词，所以它在 `join!` 之前。
///
/// # 异常情况
///
/// 任一条查询失败 → [`DbError`]（handler 翻成 500）。**不吞错**：徽标查询失败却返回
/// 全 0 会让用户以为「没有待办」，比报错更糟。
pub async fn load(user_id: Uuid) -> DbResult<BadgeCounts> {
    let audience = announcement::model::load_audience(&user_id).await?;
    let (announcement_unread, notice_unread, flow_todo, task_todo) = tokio::join!(
        announcement::model::count_unread(&audience),
        notice::model::count_unread(&user_id),
        count_flow_todo(user_id),
        count_task_todo(user_id),
    );
    Ok(BadgeCounts {
        announcement_unread: announcement_unread?,
        notice_unread: notice_unread?,
        flow_todo: flow_todo?,
        task_todo: task_todo?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_serialize_with_four_snake_case_keys() {
        let counts = BadgeCounts {
            announcement_unread: 3,
            notice_unread: 2,
            flow_todo: 4,
            task_todo: 2,
        };
        let value = serde_json::to_value(counts).expect("序列化不该失败");
        let object = value.as_object().expect("响应体必须是对象");
        // 键名与前端 store 逐字一致，改名会让徽标静默变 0
        for key in ["announcement_unread", "notice_unread", "flow_todo", "task_todo"] {
            assert!(object.contains_key(key), "缺少字段 {key}");
        }
        assert_eq!(object.len(), 4, "只允许四个桶，多一个前端就用不上");
        assert_eq!(value["announcement_unread"], 3);
        assert_eq!(value["task_todo"], 2);
    }

    #[test]
    fn total_sums_four_buckets() {
        let counts = BadgeCounts {
            announcement_unread: 3,
            notice_unread: 2,
            flow_todo: 4,
            task_todo: 2,
        };
        assert_eq!(counts.total(), 11);
        assert_eq!(BadgeCounts::default().total(), 0);
    }

    #[test]
    fn flow_todo_where_matches_my_todo_list() {
        let sql = flow_todo_where();
        assert!(sql.contains("t.assignee_id = ?::uuid"));
        assert!(sql.contains("t.status IN ('PENDING')"), "状态口径取自 MY_TODO_STATUSES");
        assert!(
            sql.contains("t.task_kind IN ('APPROVAL','RESUBMIT')"),
            "类型口径取自 flow::dto::TASK_KINDS"
        );
        assert_eq!(sql.matches('?').count(), 1, "占位符个数必须等于参数个数");
    }

    #[test]
    fn flow_todo_sql_joins_live_instance_only() {
        let sql = format!(
            "SELECT count(*) AS todo FROM biz_flow_task t \
               JOIN biz_flow_instance i ON i.id = t.instance_id AND i.deleted_at IS NULL{}",
            flow_todo_where()
        );
        // 已软删实例的待办不该进徽标（否则红点数对不上待办列表）
        assert!(sql.contains("i.deleted_at IS NULL"));
    }

    #[test]
    fn task_todo_where_uses_seven_day_window() {
        let sql = task_todo_where();
        assert!(sql.contains("t.owner_id = ?::uuid"));
        assert!(sql.contains("t.status = 'TODO'"));
        assert!(sql.contains("t.deleted_at IS NULL"));
        assert!(
            sql.contains(&format!("interval '{TASK_TODO_WINDOW_DAYS} days'")),
            "窗口必须是 7 天"
        );
        assert_eq!(TASK_TODO_WINDOW_DAYS, 7, "B 方案固定 7 天");
        assert_eq!(sql.matches('?').count(), 1, "占位符个数必须等于参数个数");
    }
}

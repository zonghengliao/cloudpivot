// ============================================================================
// flow_model_inbox.rs —— 三个身份列表与实例可见性：待办/我发起/抄送分页、实例可见性并集、实例分页、在跑计数、我参与过的实例 id
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::flow_model_base::*;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::Value as Json;

/// 实例里还在跑的节点数（`ACTIVE` + 汇聚等待中的 `PENDING`）。
///
/// **0 表示可以判定实例结束了**（[`crate::business::flow::engine::Run::maybe_finish`]）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn count_inflight(ex: &Transaction, instance_id: Uuid) -> DbResult<i64> {
    let rows = ex
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid AND status IN ('ACTIVE','PENDING')",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(0),
        Some(row) => row.get_typed("total"),
    }
}

// ---------- 可见性：我的相关实例 id 集合（架构 §9.1 / D16） ----------

/// 我的相关实例 id（待办 + 抄送，**一次 UNION 查完**）。
///
/// 架构 D16：可见性用「应用层预解析 id 集合 → `id = ANY(?::text::uuid[])`」，
/// 不写相关子查询（实测 0.887 ms vs 227 ms）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn my_instance_ids(user_id: Uuid) -> DbResult<Vec<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT DISTINCT instance_id FROM biz_flow_task WHERE assignee_id = ?::uuid \
             UNION \
             SELECT DISTINCT instance_id FROM biz_flow_cc WHERE receiver_id = ?::uuid",
            vec![Value::from(&user_id), Value::from(&user_id)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(row.get_typed("instance_id")?);
    }
    out.sort();
    Ok(out)
}

/// 实例分页条件。
#[derive(Debug, Clone, Default)]
pub struct InstanceFilter {
    /// 单据号 / 标题关键词。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 流程编码。
    pub flow_code: Option<String>,
    /// 业务类型。
    pub biz_type: Option<String>,
}

/// 实例分页结果。
#[derive(Debug, Clone)]
pub struct InstancePage {
    /// 当前页。
    pub list: Vec<InstanceRow>,
    /// 总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub size: u64,
}

/// 实例可见性目标（`initiator_dept_id` 走数据范围、`initiator_id` 走「仅本人」降级）。
pub const INSTANCE_TARGET: crate::business::shared::scope::ScopeTarget =
    crate::business::shared::scope::ScopeTarget::new(
        "biz_flow_instance",
        "i",
        Some("initiator_dept_id"),
        Some("initiator_id"),
    );

/// 实例可见性片段（并集：**数据范围 ∪ 我发起的 ∪ 我待办/已办 ∪ 抄送我**）。
///
/// `sql` 已去掉 `AND` 前缀，可直接放进 `AND (...)` 或作为并集的第一支；
/// `params` 顺序与 `sql` 里 `?` 出现顺序一致（数据范围参数 → user_id → 预解析 id 数组）。
pub struct VisibilityClause {
    /// 可见性 SQL 片段（带外层括号）。
    pub sql: String,
    /// 绑定参数。
    pub params: Vec<Value>,
}

/// 解析当前用户对审批单的可见性（架构 D16：预解析 id 集合 + `ANY(?::text::uuid[])`）。
///
/// 统计接口与列表接口**共用这一个函数**，避免两套可见性口径漂移（D4b 红线）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn instance_visibility(
    principal: &crate::business::shared::principal::ScopedPrincipal,
) -> DbResult<VisibilityClause> {
    let user_id = principal.user_id();
    let mine = my_instance_ids(user_id).await?;
    let scope = crate::business::shared::scope::filter_for(principal, INSTANCE_TARGET);
    let scope_sql = scope.sql().trim().trim_start_matches("AND ").trim().to_string();
    let mut params: Vec<Value> = scope.params().to_vec();
    params.push(Value::from(&user_id));
    params.push(crate::business::flow::assignee::array_param(&mine));
    Ok(VisibilityClause {
        sql: format!("({scope_sql} OR i.initiator_id = ?::uuid OR i.id = ANY(?::text::uuid[]))"),
        params,
    })
}

/// 实例分页（**可见性过滤**：数据范围 ∪ 我发起的 ∪ 我待办/已办 ∪ 抄送我）。
///
/// 这里刻意不用 `scope::paginate`：可见性是**并集**（`OR`），而 `paginate` 的
/// `{FILTER}` 只能做 `AND` 注入。口径仍是架构 D16 的「预解析 id 集合 + `id = ANY(…)`」，
/// 数据范围片段用 `scope::filter_for` 生成后**拼成并集的第一支**（`AND` 前缀去掉）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn page_instances(
    principal: &crate::business::shared::principal::ScopedPrincipal,
    filter: &InstanceFilter,
    page: u64,
    page_size: u64,
) -> DbResult<InstancePage> {
    let vis = instance_visibility(principal).await?;
    let mut where_sql = format!(" WHERE i.deleted_at IS NULL AND {}", vis.sql);
    let mut params: Vec<Value> = vis.params;

    if let Some(keyword) = filter.keyword.as_deref() {
        where_sql.push_str(" AND (i.instance_no ILIKE ? OR i.title ILIKE ?)");
        let pattern = format!("%{}%", keyword.replace('%', "\\%").replace('_', "\\_"));
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    if let Some(status) = filter.status.as_deref() {
        where_sql.push_str(" AND i.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(flow_code) = filter.flow_code.as_deref() {
        where_sql.push_str(" AND i.flow_code = ?");
        params.push(Value::Text(flow_code.to_string()));
    }
    if let Some(biz_type) = filter.biz_type.as_deref() {
        where_sql.push_str(" AND i.biz_type = ?");
        params.push(Value::Text(biz_type.to_string()));
    }

    let count_sql = format!("SELECT count(*) AS total FROM biz_flow_instance i{where_sql}");
    let total_row = db().find_with_params(&count_sql, params.clone()).await?;
    let total: i64 = match total_row.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };

    let mut sql = String::with_capacity(1024);
    sql.push_str("SELECT ");
    sql.push_str(INSTANCE_COLUMNS);
    sql.push_str(" FROM biz_flow_instance i");
    sql.push_str(&where_sql);
    sql.push_str(" ORDER BY i.started_at DESC, i.id DESC LIMIT ? OFFSET ?");
    let mut page_params = params;
    page_params.push(Value::from(page_size as i64));
    page_params.push(Value::from(((page.saturating_sub(1)) * page_size) as i64));

    let rows = db().find_with_params(&sql, page_params).await?;
    let list = rows.iter().map(InstanceRow::from_row).collect::<DbResult<Vec<_>>>()?;
    Ok(InstancePage {
        list,
        total: total.max(0) as u64,
        page,
        size: page_size,
    })
}

/// 我的待办的唯一状态口径（**红线：只返回 PENDING**）。
pub const MY_TODO_STATUSES: &[&str] = &["PENDING"];

/// 我的已办的状态口径（**我操作过的**：同意 / 驳回 / 转办出去）。
pub const MY_DONE_STATUSES: &[&str] = &["APPROVED", "REJECTED", "TRANSFERRED"];

/// 「我发起的」where 片段（**身份口径 `initiator_id`**，不看数据范围 / 待办 / 抄送）。
pub(super) const MY_INSTANCE_WHERE: &str = " WHERE i.initiator_id = ?::uuid AND i.deleted_at IS NULL";

/// 「抄送我的」where 片段（**身份口径 `biz_flow_cc.receiver_id`**）。
pub(super) const MY_CC_WHERE: &str = " WHERE c.receiver_id = ?::uuid";

/// 我发起的审批单（`initiator_id = me`，按发起时间倒序；架构 §7 API 24）。
///
/// 走 `idx_biz_flow_instance_initiator (initiator_id, started_at DESC)`，
/// 不掺任何可见性并集 —— 「我发起的」是身份口径，不是数据范围口径。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn page_my_instances(user_id: Uuid, page: u64, page_size: u64) -> DbResult<InstancePage> {
    let where_sql = MY_INSTANCE_WHERE;
    let count_sql = format!("SELECT count(*) AS total FROM biz_flow_instance i{where_sql}");
    let total: i64 = match db()
        .find_with_params(&count_sql, vec![Value::from(&user_id)])
        .await?
        .first()
    {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    let sql = format!(
        "SELECT {INSTANCE_COLUMNS} FROM biz_flow_instance i{where_sql} \
          ORDER BY i.started_at DESC, i.id DESC LIMIT ? OFFSET ?"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![
                Value::from(&user_id),
                Value::from(page_size as i64),
                Value::from(((page.saturating_sub(1)) * page_size) as i64),
            ],
        )
        .await?;
    let list = rows.iter().map(InstanceRow::from_row).collect::<DbResult<Vec<_>>>()?;
    Ok(InstancePage {
        list,
        total: total.max(0) as u64,
        page,
        size: page_size,
    })
}

/// 待办 / 已办列表的排序口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyTaskOrder {
    /// 按**到达时间**倒序（我的待办）。
    Arrived,
    /// 按**操作时间**倒序（我的已办，`completed_at`）。
    Operated,
}

impl MyTaskOrder {
    /// 排序片段（列已带表别名，防 JOIN 后二义）。
    ///
    /// `pub(super)`：拆分后单测住在 `flow::model::tests`（不是本文件的子模块），
    /// 需要跨部件调用；拆分前它是同模块私有 fn，可见性语义未变（仍不出 `flow` 模块）。
    pub(super) fn order_sql(self) -> &'static str {
        match self {
            Self::Arrived => "t.created_at DESC, t.id DESC",
            Self::Operated => "t.completed_at DESC, t.id DESC",
        }
    }
}

/// 待办 / 已办列表行：任务行 + 展示期需要的单据 / 流程字段。
///
/// 一次 JOIN 取齐（不再逐行 `find_instance` 的 N+1），D5 缺口 2 的
/// `flow_name` / `initiator_name` / `current_node_name` 就来自这里。
#[derive(Debug, Clone)]
pub struct MyTaskRow {
    /// 任务行。
    pub task: TaskRow,
    /// 单据号。
    pub instance_no: String,
    /// 单据标题。
    pub title: String,
    /// 审批业务类型（`biz_flow_instance.biz_type`；工作台卡片按它映射单据类型与跳转）。
    pub biz_type: String,
    /// 业务单据 id（`None` = 无关联业务单据）。
    pub biz_id: Option<Uuid>,
    /// 业务单据号（`None` = 无业务单号）。
    pub biz_no: Option<String>,
    /// 流程名称。
    pub flow_name: String,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 所属审批实例的状态（P11：前端据此把「实例已结束但待办还 PENDING」的脏行挡在门外）。
    pub instance_status: String,
    /// 当前节点中文名（`biz_flow_instance_node.node_name` 快照）。
    pub node_name: String,
    /// 表单快照（P1-FIX：待办卡片摘要直接用，前端不必再逐单查实例详情）。
    pub form_data: Json,
}

/// 待办 / 已办分页结果。
#[derive(Debug, Clone)]
pub struct MyTaskPage {
    /// 当前页。
    pub list: Vec<MyTaskRow>,
    /// 跨页总数（`count(*)` 现算）。
    pub total: u64,
}

/// 抄送列表行。
#[derive(Debug, Clone)]
pub struct MyCcRow {
    /// 抄送行。
    pub cc: CcRow,
    /// 单据号。
    pub instance_no: String,
    /// 单据标题。
    pub title: String,
    /// 流程名称。
    pub flow_name: String,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 节点中文名（节点行缺失 → 空串）。
    pub node_name: String,
}

/// 抄送分页结果。
#[derive(Debug, Clone)]
pub struct MyCcPage {
    /// 当前页。
    pub list: Vec<MyCcRow>,
    /// 跨页总数（`count(*)` 现算）。
    pub total: u64,
}

/// 我的待办 / 已办（**身份口径：`assignee_id = me`**，与实例可见性无关）。
///
/// - 待办：`status = PENDING`，按到达时间倒序；
/// - 已办：`status IN (APPROVED/REJECTED/TRANSFERRED)`，按**操作时间**（`completed_at`）倒序。
///
/// `biz_flow_task` 没有独立的 `operator_id` 列 —— 「操作人」就是这条任务行的
/// `assignee_id`（审批 / 驳回 / 转办都由该行的审批人本人完成，`ck_biz_flow_task_done`
/// 保证非 PENDING 行必有 `completed_at`）。所以「我操作过的」= `assignee_id = me
/// AND status <> 'PENDING'`，不需要额外列、不需要迁移。
///
/// `statuses` / `kinds` 只由本模块的常量传入（不是用户输入），格式化成 `IN (...)`
/// 字面量是安全的；本仓没有绑定数组的 `IN` 语法糖，`= ANY(...)` 只用于 uuid 数组。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
///
/// `#[allow(clippy::too_many_arguments)]` 的理由：8 个参数是一条「我的任务」查询的
/// 全部筛选轴（用户 / 状态集 / 单据类型集 / 排序 / 分页 / 只看进行中 / 完成时间下界），
/// 收口成 struct 要改全部调用方与测试，属独立重构；见跟进卡「too_many_arguments 参数收口为 struct」。
#[allow(clippy::too_many_arguments)]
pub async fn page_my_tasks(
    user_id: Uuid,
    statuses: &[&str],
    kinds: &[&str],
    order: MyTaskOrder,
    page: u64,
    page_size: u64,
    live_only: bool,
    completed_since: Option<chrono::DateTime<chrono::Utc>>,
) -> DbResult<MyTaskPage> {
    let status_list = statuses
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(",");
    let kind_list = kinds
        .iter()
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(",");
    // P11：「我的待办」只认**实例还在跑**的待办。
    // 后端已经在「任务取消/删除」两条路径上撤审批（`task_bridge::cancel_on_task_cancelled`
    // / `cancel_on_task_deleted`），这里再按实例状态兜一层：历史脏数据（实例已终态、
    // 待办还 PENDING）不该出现在审批人的待办里 —— 那是一条点不动的孤儿（老板报的 TSK-149）。
    // 列表与计数用**同一个**谓词，否则会出现「显示 3 条、total 说 5 条」。
    let live_sql = if live_only { " AND i.status = 'PENDING'" } else { "" };
    // 已办时间下界（首页工作台「今日已办」）：只加在「我的已办」上的可选谓词。
    // 绑文本再 `::text::timestamptz` 是本仓既有写法（`self_approval.rs` / `notice/model.rs`），
    // 直接把 `Value::Text` 交给 `?::timestamptz` 在驱动层不可靠。
    let since_sql = if completed_since.is_some() {
        " AND t.completed_at >= ?::text::timestamptz"
    } else {
        ""
    };
    let where_sql = format!(
        " WHERE t.assignee_id = ?::uuid AND t.status IN ({status_list}) \
           AND t.task_kind IN ({kind_list}){live_sql}{since_sql}"
    );
    let mut params = vec![Value::from(&user_id)];
    if let Some(since) = completed_since {
        params.push(Value::Text(since.to_rfc3339()));
    }
    let count_sql = format!(
        "SELECT count(*) AS total FROM biz_flow_task t \
           JOIN biz_flow_instance i ON i.id = t.instance_id AND i.deleted_at IS NULL{where_sql}"
    );
    let total: i64 = match db().find_with_params(&count_sql, params.clone()).await?.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    let sql = format!(
        "SELECT {TASK_COLUMNS}, i.instance_no AS instance_no, i.title AS title, \
                i.biz_type AS biz_type, i.biz_id AS biz_id, i.biz_no AS biz_no, \
                i.flow_name AS flow_name, i.initiator_name AS initiator_name, \
                i.status AS instance_status, i.form_data::text AS form_data_text, \
                n.node_name AS node_name \
           FROM biz_flow_task t \
           JOIN biz_flow_instance i ON i.id = t.instance_id AND i.deleted_at IS NULL \
           JOIN biz_flow_instance_node n ON n.id = t.instance_node_id{where_sql} \
          ORDER BY {} LIMIT ? OFFSET ?",
        order.order_sql()
    );
    params.push(Value::from(page_size as i64));
    params.push(Value::from(((page.saturating_sub(1)) * page_size) as i64));
    let rows = db().find_with_params(&sql, params).await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        let form_text: Option<String> = row.get_typed("form_data_text")?;
        list.push(MyTaskRow {
            task: TaskRow::from_row(row)?,
            instance_no: row.get_typed("instance_no")?,
            title: row.get_typed("title")?,
            biz_type: row.get_typed("biz_type")?,
            biz_id: row.get_typed("biz_id")?,
            biz_no: row.get_typed("biz_no")?,
            flow_name: row.get_typed("flow_name")?,
            initiator_name: row.get_typed("initiator_name")?,
            instance_status: row.get_typed("instance_status")?,
            node_name: row.get_typed("node_name")?,
            // 解析失败给 `{}`（= 无表单）：待办列表不该因为一条坏 jsonb 整页 500。
            form_data: form_text
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_else(|| serde_json::json!({})),
        });
    }
    Ok(MyTaskPage {
        list,
        total: total.max(0) as u64,
    })
}

/// 我收到的抄送（**身份口径：`biz_flow_cc.receiver_id = me`**，按抄送时间倒序）。
///
/// 走 `idx_biz_flow_cc_mine (receiver_id, created_at DESC)`；刻意不加
/// `is_read` 到排序首位（已读状态不参与排序，否则索引失效）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn page_my_cc(user_id: Uuid, page: u64, page_size: u64) -> DbResult<MyCcPage> {
    let count_sql = format!(
        "SELECT count(*) AS total FROM biz_flow_cc c \
           JOIN biz_flow_instance i ON i.id = c.instance_id AND i.deleted_at IS NULL{MY_CC_WHERE}"
    );
    let total: i64 = match db()
        .find_with_params(&count_sql, vec![Value::from(&user_id)])
        .await?
        .first()
    {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    let sql = format!(
        "SELECT {CC_COLUMNS}, i.instance_no AS instance_no, i.title AS title, \
                i.flow_name AS flow_name, i.initiator_name AS initiator_name, \
                COALESCE(n.node_name, '') AS node_name \
           FROM biz_flow_cc c \
           JOIN biz_flow_instance i ON i.id = c.instance_id AND i.deleted_at IS NULL \
           LEFT JOIN biz_flow_instance_node n \
                  ON n.instance_id = c.instance_id AND n.node_key = c.node_key AND n.round = c.round \
          {MY_CC_WHERE} \
          ORDER BY c.created_at DESC, c.id DESC LIMIT ? OFFSET ?"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![
                Value::from(&user_id),
                Value::from(page_size as i64),
                Value::from(((page.saturating_sub(1)) * page_size) as i64),
            ],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(MyCcRow {
            cc: CcRow::from_row(row)?,
            instance_no: row.get_typed("instance_no")?,
            title: row.get_typed("title")?,
            flow_name: row.get_typed("flow_name")?,
            initiator_name: row.get_typed("initiator_name")?,
            node_name: row.get_typed("node_name")?,
        });
    }
    Ok(MyCcPage {
        list,
        total: total.max(0) as u64,
    })
}

// ---------- 用户小工具 ----------

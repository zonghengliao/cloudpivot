//! 审批流程定义 · 数据访问层（批次 D · D1）。
//!
//! 依据：`docs/批次D-工作流审批-技术架构.md` §3.1（三个表示）、§3.6（版本管理）、§6.1（DDL）。
//!
//! # 三个表示（L1 / L2 / L3）在本模块的落点
//!
//! | 层 | 存哪 | 本模块的函数 |
//! |----|------|--------------|
//! | L1 编辑期行 | `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee` | [`load_graph`] / [`replace_graph`] |
//! | L2 发布快照 | `biz_flow_def_version.graph_snapshot` | [`apply_publish`]（写）+ [`load_snapshot`]（读） |
//! | L3 运行期内存图 | 进程内 LRU（D2 才建） | 不在本卡范围 |
//!
//! # 三条不变式
//!
//! 1. **PUBLISHED 版本的行走不了写路径**：[`replace_graph`] 的调用方（handler）先查
//!    `version_status`，只有 `DRAFT` 才允许覆盖；库层的 `ck_biz_flow_def_version_published`
//!    再兜一道（PUBLISHED 必须有快照 + hash + 发布时间）。
//! 2. **发布只改 `def.current_version_id` + 版本行状态，不动任何已有版本的图**
//!    （架构 D2：在跑实例绑的是自己的 `version_id`）。
//! 3. **`is_current` 与 `def.current_version_id` 同事务维护**：`uk_biz_flow_def_version_current`
//!    是库级「每个 def 至多一个当前版本」的部分唯一索引，两者不一致会立刻被它拦下。
//!
//! # 闸②（`scripts/check.sh`）与数据权限
//!
//! `biz_flow_*` 已登记进闸②清单，本文件在文件头写了 `// SCOPE-EXEMPT:` 说明：
//! **流程定义是全局配置，不参与数据范围裁剪**。理由：流程定义不是「谁的业务数据」，
//! 它是「全平台共用的规则」；若按部门范围过滤，A 部门的人会看不到自己的单据要走哪个流程，
//! 而编辑权限已经由 `business:flow:update` / `publish` 这类权限码隔离。
//! 这与 `biz_task_status_def`（全局字典）的豁免口径一致，与 `biz_task`（有 owner/dept）相反。
//
// SCOPE-EXEMPT: 流程定义 / 版本 / 节点 / 边 / 审批人是**全局配置**（不是谁的业务数据），
// SCOPE-EXEMPT: 可见性由 `business:flow:read` 权限码控制，按部门范围裁剪会导致「看不到自己的流程」。

use alun::prelude::*;
use alun::uuid::Uuid;
use serde_json::Value as Json;

use crate::business::flow::graph::{
    ApprovalMode, AssigneeKind, AssigneeRef, EdgeSpec, FlowGraph, GatewayMode, NodeSpec, NodeType,
};
use crate::shared::error_code;

// ==================== 常量：列清单 ====================

/// 流程定义列清单（含当前版本的节点 / 边数）。
const DEF_COLUMNS: &str = "\
     d.id, d.flow_code, d.flow_name, d.biz_type, d.category, d.description, d.status, \
     d.current_version_id, d.current_version_no, d.is_default, d.allow_auto_approve, \
     d.created_at, d.updated_at, \
     COALESCE(v.node_count, 0) AS cur_node_count, COALESCE(v.edge_count, 0) AS cur_edge_count";

/// 流程定义 FROM 片段（当前版本 LEFT JOIN —— 未发布也有行）。
const DEF_FROM: &str = " FROM biz_flow_def d \
     LEFT JOIN biz_flow_def_version v ON v.id = d.current_version_id";

/// 版本列清单（含发布人姓名）。
const VERSION_COLUMNS: &str = "\
     v.id, v.def_id, v.version_no, v.version_status, v.is_current, v.graph_hash, \
     v.node_count, v.edge_count, v.change_note, v.published_at, v.published_by, \
     v.created_at, v.updated_at, \
     COALESCE(NULLIF(u.real_name, ''), u.username, '') AS published_by_name";

/// 版本 FROM 片段。
const VERSION_FROM: &str = " FROM biz_flow_def_version v \
     LEFT JOIN sys_user u ON u.id = v.published_by";

/// 节点列清单（`approve_ratio` / `config` 在 SQL 里转成好绑定的形态）。
const NODE_COLUMNS: &str = "\
     n.id, n.version_id, n.node_key, n.node_name, n.node_type, n.assignee_type, \
     n.superior_levels, n.form_var_key, n.approval_mode, n.approve_ratio::float8 AS approve_ratio, \
     n.reject_veto, n.empty_assignee, n.self_approval, n.allow_robot, n.join_mode, n.fork_mode, \
     n.timeout_hours, n.timeout_action, n.remind_before_hours, n.max_remind_times, \
     n.cc_on_approved, n.position_x, n.position_y, n.sort_order, n.config::text AS config_text";

// ==================== 参数助手 ====================

fn opt_text(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }
}

fn opt_i32(value: Option<i32>) -> Value {
    match value {
        Some(number) => Value::from(number),
        None => Value::Null,
    }
}

// ==================== 行结构 ====================

/// 流程定义行。
#[derive(Debug, Clone)]
pub struct DefRow {
    /// 主键。
    pub id: Uuid,
    /// 编码。
    pub flow_code: String,
    /// 名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 分类。
    pub category: Option<String>,
    /// 说明。
    pub description: Option<String>,
    /// 状态。
    pub status: String,
    /// 当前发布版本。
    pub current_version_id: Option<Uuid>,
    /// 当前发布版本号。
    pub current_version_no: i32,
    /// 是否默认。
    pub is_default: bool,
    /// 是否允许超时自动通过。
    pub allow_auto_approve: bool,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 当前版本节点数。
    pub cur_node_count: i64,
    /// 当前版本边数。
    pub cur_edge_count: i64,
}

impl DefRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            category: row.get_typed("category")?,
            description: row.get_typed("description")?,
            status: row.get_typed("status")?,
            current_version_id: row.get_typed("current_version_id")?,
            current_version_no: row.get_typed("current_version_no")?,
            is_default: row.get_typed("is_default")?,
            allow_auto_approve: row.get_typed("allow_auto_approve")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            cur_node_count: row.get_typed("cur_node_count")?,
            cur_edge_count: row.get_typed("cur_edge_count")?,
        })
    }
}

/// 版本行。
#[derive(Debug, Clone)]
pub struct VersionRow {
    /// 主键。
    pub id: Uuid,
    /// 所属流程。
    pub def_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 版本状态（`DRAFT` / `PUBLISHED` / `ARCHIVED`）。
    pub version_status: String,
    /// 是否当前版本。
    pub is_current: bool,
    /// 图 hash。
    pub graph_hash: Option<String>,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
    /// 变更备注。
    pub change_note: Option<String>,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人。
    pub published_by: Option<Uuid>,
    /// 发布人姓名。
    pub published_by_name: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl VersionRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            def_id: row.get_typed("def_id")?,
            version_no: row.get_typed("version_no")?,
            version_status: row.get_typed("version_status")?,
            is_current: row.get_typed("is_current")?,
            graph_hash: row.get_typed("graph_hash")?,
            node_count: row.get_typed("node_count")?,
            edge_count: row.get_typed("edge_count")?,
            change_note: row.get_typed("change_note")?,
            published_at: row.get_typed("published_at")?,
            published_by: row.get_typed("published_by")?,
            published_by_name: row.get_typed("published_by_name")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 是否可编辑（只有 DRAFT）。
    pub fn editable(&self) -> bool {
        self.version_status == "DRAFT"
    }
}

// ==================== 读路径 ====================

/// 流程定义分页条件。
#[derive(Debug, Clone, Default)]
pub struct DefFilter {
    /// 编码 / 名称关键词（已归一化）。
    pub keyword: Option<String>,
    /// 状态（已归一化）。
    pub status: Option<String>,
    /// 业务类型（已转大写）。
    pub biz_type: Option<String>,
}

/// 分页结果。
#[derive(Debug, Clone)]
pub struct DefPage {
    /// 当前页。
    pub list: Vec<DefRow>,
    /// 总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub size: u64,
}

/// 流程定义分页（未软删；关键词匹配编码或名称）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn page_defs(filter: &DefFilter, page: u64, page_size: u64) -> DbResult<DefPage> {
    let mut where_sql = String::from(" WHERE d.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(keyword) = filter.keyword.as_deref() {
        where_sql.push_str(" AND (d.flow_code ILIKE ? OR d.flow_name ILIKE ?)");
        let pattern = format!("%{}%", keyword.replace('%', "\\%").replace('_', "\\_"));
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    if let Some(status) = filter.status.as_deref() {
        where_sql.push_str(" AND d.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(biz_type) = filter.biz_type.as_deref() {
        where_sql.push_str(" AND d.biz_type = ?");
        params.push(Value::Text(biz_type.to_string()));
    }

    let count_sql = format!("SELECT count(*) AS total FROM biz_flow_def d{where_sql}");
    let total_row = db().find_with_params(&count_sql, params.clone()).await?;
    let total: i64 = match total_row.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };

    let mut sql = String::with_capacity(512);
    sql.push_str("SELECT ");
    sql.push_str(DEF_COLUMNS);
    sql.push_str(DEF_FROM);
    sql.push_str(&where_sql);
    sql.push_str(" ORDER BY d.updated_at DESC, d.id DESC LIMIT ? OFFSET ?");
    let mut page_params = params;
    page_params.push(Value::from(page_size as i64));
    page_params.push(Value::from(((page.saturating_sub(1)) * page_size) as i64));

    let rows = db().find_with_params(&sql, page_params).await?;
    let list = rows
        .iter()
        .map(DefRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(DefPage {
        list,
        total: total.max(0) as u64,
        page,
        size: page_size,
    })
}

/// 按 id 取流程定义（未软删；不存在 / 已删 → `None`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_def(id: Uuid) -> DbResult<Option<DefRow>> {
    let sql =
        format!("SELECT {DEF_COLUMNS}{DEF_FROM} WHERE d.id = ?::uuid AND d.deleted_at IS NULL");
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(DefRow::from_row).transpose()
}

/// 编码是否已被占用（软删过滤；`exclude` 用于编辑自身时排除）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn flow_code_taken(code: &str, exclude: Option<Uuid>) -> DbResult<bool> {
    let mut sql = String::from(
        "SELECT count(*) AS total FROM biz_flow_def WHERE flow_code = ? AND deleted_at IS NULL",
    );
    let mut params = vec![Value::Text(code.to_string())];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    let rows = db().find_with_params(&sql, params).await?;
    let total: i64 = match rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok(total > 0)
}

/// 按 id 取版本（含发布人姓名）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_version(id: Uuid) -> DbResult<Option<VersionRow>> {
    let sql = format!("SELECT {VERSION_COLUMNS}{VERSION_FROM} WHERE v.id = ?::uuid");
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(VersionRow::from_row).transpose()
}

/// 某流程的全部版本（版本号倒序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn versions_of(def_id: Uuid) -> DbResult<Vec<VersionRow>> {
    let sql = format!(
        "SELECT {VERSION_COLUMNS}{VERSION_FROM} WHERE v.def_id = ?::uuid ORDER BY v.version_no DESC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&def_id)])
        .await?;
    rows.iter().map(VersionRow::from_row).collect()
}

/// 某流程最新的 DRAFT 版本（没有 → `None`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn latest_draft(def_id: Uuid) -> DbResult<Option<VersionRow>> {
    let sql = format!(
        "SELECT {VERSION_COLUMNS}{VERSION_FROM} \
          WHERE v.def_id = ?::uuid AND v.version_status = 'DRAFT' \
          ORDER BY v.version_no DESC LIMIT 1"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&def_id)])
        .await?;
    rows.first().map(VersionRow::from_row).transpose()
}

/// 取某流程的业务类型（编辑 `is_default` 时需要知道同组还有谁）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn biz_type_of(ex: &impl SqlExecutor, def_id: Uuid) -> DbResult<Option<String>> {
    let rows = ex
        .query_sql(
            "SELECT biz_type FROM biz_flow_def WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&def_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(row.get_typed("biz_type")?)),
        None => Ok(None),
    }
}

/// 把同业务类型的其它默认流程清掉（部分唯一索引 `uk_biz_flow_def_default` 的前置动作）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn clear_default_of_biz_type(
    ex: &impl SqlExecutor,
    biz_type: &str,
    keep: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_flow_def SET is_default = FALSE, updated_at = now() \
          WHERE biz_type = ? AND deleted_at IS NULL AND is_default AND id <> ?::uuid"
            .to_string(),
        vec![Value::Text(biz_type.to_string()), Value::from(&keep)],
    )
    .await
}

/// 某流程最大的版本号（没有版本 → 0）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn max_version_no(ex: &impl SqlExecutor, def_id: Uuid) -> DbResult<i32> {
    let rows = ex
        .query_sql(
            "SELECT COALESCE(max(version_no), 0) AS max_no \
               FROM biz_flow_def_version WHERE def_id = ?::uuid"
                .to_string(),
            vec![Value::from(&def_id)],
        )
        .await?;
    let value: i32 = match rows.first() {
        Some(row) => row.get_typed("max_no")?,
        None => 0,
    };
    Ok(value)
}

/// 读整图（L1）：**一次读全图**，不做逐节点查询（架构 §8 证据 D：0.250 ms）。
///
/// 三条 SELECT：节点、边（JOIN 出两端 key）、审批人（JOIN node）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]；节点类型 / 枚举值非法（脏数据）→ [`DbError`]。
pub async fn load_graph(version_id: Uuid) -> DbResult<FlowGraph> {
    let node_sql = format!(
        "SELECT {NODE_COLUMNS} FROM biz_flow_node n WHERE n.version_id = ?::uuid \
         ORDER BY n.sort_order, n.node_key"
    );
    let node_rows = db()
        .find_with_params(&node_sql, vec![Value::from(&version_id)])
        .await?;

    let edge_rows = db()
        .find_with_params(
            "SELECT e.id, e.edge_key, f.node_key AS from_key, t.node_key AS to_key, \
                    e.condition_expr::text AS condition_text, e.condition_label, \
                    e.priority, e.is_default \
               FROM biz_flow_edge e \
               JOIN biz_flow_node f ON f.id = e.from_node_id \
               JOIN biz_flow_node t ON t.id = e.to_node_id \
              WHERE e.version_id = ?::uuid \
              ORDER BY e.edge_key",
            vec![Value::from(&version_id)],
        )
        .await?;

    let assignee_rows = db()
        .find_with_params(
            "SELECT a.node_id, a.assignee_type, a.ref_id, a.sort_order \
               FROM biz_flow_node_assignee a \
               JOIN biz_flow_node n ON n.id = a.node_id \
              WHERE n.version_id = ?::uuid \
              ORDER BY a.assignee_type, a.ref_id",
            vec![Value::from(&version_id)],
        )
        .await?;

    let mut assignees_by_node: std::collections::HashMap<Uuid, Vec<AssigneeRef>> =
        std::collections::HashMap::new();
    for row in &assignee_rows {
        let node_id: Uuid = row.get_typed("node_id")?;
        assignees_by_node
            .entry(node_id)
            .or_default()
            .push(AssigneeRef {
                kind: row.get_typed("assignee_type")?,
                ref_id: row.get_typed::<Uuid>("ref_id")?.to_string(),
                sort_order: row.get_typed("sort_order")?,
            });
    }

    let mut nodes: Vec<NodeSpec> = Vec::with_capacity(node_rows.len());
    for row in &node_rows {
        let id: Uuid = row.get_typed("id")?;
        let node_type_raw: String = row.get_typed("node_type")?;
        let node_type = NodeType::parse(&node_type_raw)
            .ok_or_else(|| DbError::InvalidValue(format!("node_type 非法：{node_type_raw}")))?;
        let assignee_raw: Option<String> = row.get_typed("assignee_type")?;
        let assignee_type = match assignee_raw.as_deref() {
            None => None,
            Some(text) => Some(
                AssigneeKind::parse(text)
                    .ok_or_else(|| DbError::InvalidValue(format!("assignee_type 非法：{text}")))?,
            ),
        };
        let approval_raw: Option<String> = row.get_typed("approval_mode")?;
        let approval_mode = match approval_raw.as_deref() {
            None => None,
            Some(text) => Some(
                ApprovalMode::parse(text)
                    .ok_or_else(|| DbError::InvalidValue(format!("approval_mode 非法：{text}")))?,
            ),
        };
        let join_raw: String = row.get_typed("join_mode")?;
        let fork_raw: String = row.get_typed("fork_mode")?;
        let config_text: String = row.get_typed("config_text")?;
        let config = serde_json::from_str(&config_text).unwrap_or(serde_json::Value::Null);
        let mut assignees = assignees_by_node.remove(&id).unwrap_or_default();
        assignees.sort_by(|left, right| {
            (left.kind.as_str(), left.ref_id.as_str())
                .cmp(&(right.kind.as_str(), right.ref_id.as_str()))
        });
        nodes.push(NodeSpec {
            key: row.get_typed("node_key")?,
            name: row.get_typed("node_name")?,
            node_type,
            assignee_type,
            approval_mode,
            approve_ratio: row.get_typed("approve_ratio")?,
            reject_veto: row.get_typed("reject_veto")?,
            join_mode: GatewayMode::parse(&join_raw)
                .ok_or_else(|| DbError::InvalidValue(format!("join_mode 非法：{join_raw}")))?,
            fork_mode: GatewayMode::parse(&fork_raw)
                .ok_or_else(|| DbError::InvalidValue(format!("fork_mode 非法：{fork_raw}")))?,
            form_var_key: row.get_typed("form_var_key")?,
            superior_levels: row.get_typed("superior_levels")?,
            assignees,
            empty_assignee: row.get_typed("empty_assignee")?,
            self_approval: row.get_typed("self_approval")?,
            allow_robot: row.get_typed("allow_robot")?,
            timeout_hours: row.get_typed("timeout_hours")?,
            timeout_action: row.get_typed("timeout_action")?,
            remind_before_hours: row.get_typed("remind_before_hours")?,
            max_remind_times: row.get_typed("max_remind_times")?,
            cc_on_approved: row.get_typed("cc_on_approved")?,
            x: row.get_typed("position_x")?,
            y: row.get_typed("position_y")?,
            sort_order: row.get_typed("sort_order")?,
            config,
        });
    }

    let mut edges: Vec<EdgeSpec> = Vec::with_capacity(edge_rows.len());
    for row in &edge_rows {
        let condition_text: Option<String> = row.get_typed("condition_text")?;
        let condition =
            match condition_text.as_deref() {
                None => None,
                Some(text) if text.trim().is_empty() || text.trim() == "null" => None,
                Some(text) => Some(serde_json::from_str(text).map_err(|error| {
                    DbError::InvalidValue(format!("condition_expr 非法：{error}"))
                })?),
            };
        edges.push(EdgeSpec {
            key: row.get_typed("edge_key")?,
            from: row.get_typed("from_key")?,
            to: row.get_typed("to_key")?,
            condition,
            label: row.get_typed("condition_label")?,
            priority: row.get_typed("priority")?,
            is_default: row.get_typed("is_default")?,
        });
    }

    Ok(FlowGraph { nodes, edges })
}

/// 读发布快照（L2）。未发布 → `None`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn load_snapshot(version_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT graph_snapshot::text AS snapshot_text \
               FROM biz_flow_def_version WHERE id = ?::uuid",
            vec![Value::from(&version_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => {
            let text: Option<String> = row.get_typed("snapshot_text")?;
            Ok(text.filter(|value| !value.trim().is_empty()))
        }
    }
}

/// 统计版本下的行数（验收 8：`SELECT count(*)` 与 `node_count/edge_count` 一致）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn count_rows(ex: &impl SqlExecutor, version_id: Uuid) -> DbResult<(i64, i64)> {
    let node_rows = ex
        .query_sql(
            "SELECT count(*) AS total FROM biz_flow_node WHERE version_id = ?::uuid".to_string(),
            vec![Value::from(&version_id)],
        )
        .await?;
    let edge_rows = ex
        .query_sql(
            "SELECT count(*) AS total FROM biz_flow_edge WHERE version_id = ?::uuid".to_string(),
            vec![Value::from(&version_id)],
        )
        .await?;
    let nodes: i64 = match node_rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    let edges: i64 = match edge_rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok((nodes, edges))
}

// ==================== 写路径：定义 ====================

/// 新建流程定义入参。
#[derive(Debug, Clone)]
pub struct NewDef {
    /// 主键（应用侧生成）。
    pub id: Uuid,
    /// 编码。
    pub flow_code: String,
    /// 名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 分类。
    pub category: Option<String>,
    /// 说明。
    pub description: Option<String>,
    /// 是否默认。
    pub is_default: bool,
    /// 是否允许超时自动通过。
    pub allow_auto_approve: bool,
}

/// 新建流程定义（事务内）。
///
/// # 异常情况
///
/// 数据库异常（含 `uk_biz_flow_def_code` / `uk_biz_flow_def_default` 冲突）→ [`DbError`]。
pub async fn create_def(ex: &Transaction, new: &NewDef, operator: Uuid) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_def \
             (id, flow_code, flow_name, biz_type, category, description, status, \
              current_version_no, is_default, allow_auto_approve, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, ?, ?, 'DRAFT', 0, ?, ?, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&new.id),
            Value::Text(new.flow_code.clone()),
            Value::Text(new.flow_name.clone()),
            Value::Text(new.biz_type.clone()),
            opt_text(new.category.as_deref()),
            opt_text(new.description.as_deref()),
            Value::from(new.is_default),
            Value::from(new.allow_auto_approve),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(())
}

/// 定义可改字段（`None` = 不改）。
#[derive(Debug, Clone, Default)]
pub struct DefPatch {
    /// 名称。
    pub flow_name: Option<String>,
    /// 业务类型。
    pub biz_type: Option<String>,
    /// 分类（`Some(None)` = 清空）。
    pub category: Option<Option<String>>,
    /// 说明（`Some(None)` = 清空）。
    pub description: Option<Option<String>>,
    /// 是否默认。
    pub is_default: Option<bool>,
    /// 是否允许超时自动通过。
    pub allow_auto_approve: Option<bool>,
}

/// 改流程元信息（只改 `biz_flow_def`，不碰版本）。
///
/// # 异常情况
///
/// 数据库异常（含唯一索引冲突）→ [`DbError`]。
pub async fn update_def(id: Uuid, patch: &DefPatch, operator: Uuid) -> DbResult<u64> {
    let mut sets: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(name) = patch.flow_name.as_deref() {
        sets.push("flow_name = ?");
        params.push(Value::Text(name.to_string()));
    }
    if let Some(biz_type) = patch.biz_type.as_deref() {
        sets.push("biz_type = ?");
        params.push(Value::Text(biz_type.to_string()));
    }
    if let Some(category) = patch.category.as_ref() {
        sets.push("category = ?");
        params.push(opt_text(category.as_deref()));
    }
    if let Some(description) = patch.description.as_ref() {
        sets.push("description = ?");
        params.push(opt_text(description.as_deref()));
    }
    if let Some(is_default) = patch.is_default {
        sets.push("is_default = ?");
        params.push(Value::from(is_default));
    }
    if let Some(allow) = patch.allow_auto_approve {
        sets.push("allow_auto_approve = ?");
        params.push(Value::from(allow));
    }
    if sets.is_empty() {
        return Ok(0);
    }
    sets.push("updated_at = now()");
    sets.push("update_by = ?::uuid");
    params.push(Value::from(&operator));
    params.push(Value::from(&id));
    let sql = format!(
        "UPDATE biz_flow_def SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    db().execute_with_params(&sql, params).await
}

/// 软删流程定义（版本 / 节点 / 边保留 —— 历史实例仍可引用它们）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn soft_delete_def(id: Uuid, operator: Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_flow_def SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(&id)],
    )
    .await
}

/// 停用流程（`status = 'DISABLED'`，不动版本、不动在跑实例）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn mark_disabled(id: Uuid, operator: Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_flow_def SET status = 'DISABLED', updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(&id)],
    )
    .await
}

// ==================== 写路径：版本 ====================

/// 新建版本行（事务内）。
///
/// # 异常情况
///
/// 数据库异常（含 `uk_biz_flow_def_version` 冲突）→ [`DbError`]。
pub async fn create_version(
    ex: &Transaction,
    def_id: Uuid,
    version_no: i32,
    change_note: Option<&str>,
    operator: Uuid,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    ex.execute_with_params(
        "INSERT INTO biz_flow_def_version \
             (id, def_id, version_no, version_status, is_current, node_count, edge_count, \
              change_note, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?::uuid, ?, 'DRAFT', FALSE, 0, 0, ?, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&id),
            Value::from(&def_id),
            Value::from(version_no),
            opt_text(change_note),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(id)
}

/// 更新版本的变更备注与行数（保存整图后回写）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn touch_version(
    ex: &Transaction,
    version_id: Uuid,
    change_note: Option<&str>,
    node_count: i64,
    edge_count: i64,
    operator: Uuid,
) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET change_note = COALESCE(?, change_note), node_count = ?, edge_count = ?, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND version_status = 'DRAFT'",
        vec![
            opt_text(change_note),
            Value::from(node_count),
            Value::from(edge_count),
            Value::from(&operator),
            Value::from(&version_id),
        ],
    )
    .await
}

/// 整图覆盖式保存（L1）：先删该版本全部节点（级联删边与审批人），再逐条插入。
///
/// **调用方必须先确认版本是 `DRAFT`**（`handler::save_graph` 做，红线：PUBLISHED 行只读）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn replace_graph(ex: &Transaction, version_id: Uuid, graph: &FlowGraph) -> DbResult<()> {
    // 删节点即级联删 biz_flow_edge / biz_flow_node_assignee（两条 FK 都是 ON DELETE CASCADE）
    ex.execute_with_params(
        "DELETE FROM biz_flow_node WHERE version_id = ?::uuid",
        vec![Value::from(&version_id)],
    )
    .await?;

    // 节点 id 由应用侧生成并保留映射，边与审批人据此挂接
    let mut node_ids: std::collections::HashMap<&str, Uuid> = std::collections::HashMap::new();
    for node in &graph.nodes {
        let id = Uuid::now_v7();
        node_ids.insert(node.key.as_str(), id);
        ex.execute_with_params(
            "INSERT INTO biz_flow_node \
                 (id, version_id, node_key, node_name, node_type, assignee_type, superior_levels, \
                  form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, \
                  self_approval, allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, \
                  remind_before_hours, max_remind_times, cc_on_approved, \
                  position_x, position_y, sort_order, config) \
             VALUES (?::uuid, ?::uuid, ?, ?, ?, ?, ?, ?, ?, ?::text::numeric, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                     ?, ?, ?, ?::jsonb)",
            vec![
                Value::from(&id),
                Value::from(&version_id),
                Value::Text(node.key.clone()),
                Value::Text(node.name.clone()),
                Value::Text(node.node_type.as_str().to_string()),
                opt_text(node.assignee_type.map(|kind| kind.as_str())),
                Value::I16(node.superior_levels),
                opt_text(node.form_var_key.as_deref()),
                opt_text(node.approval_mode.map(|mode| mode.as_str())),
                match node.approve_ratio {
                    Some(ratio) => Value::Text(format!("{ratio:.3}")),
                    None => Value::Null,
                },
                Value::from(node.reject_veto),
                Value::Text(node.empty_assignee.clone()),
                Value::Text(node.self_approval.clone()),
                Value::from(node.allow_robot),
                Value::Text(node.join_mode.as_str().to_string()),
                Value::Text(node.fork_mode.as_str().to_string()),
                opt_i32(node.timeout_hours),
                Value::Text(node.timeout_action.clone()),
                opt_i32(node.remind_before_hours),
                Value::I16(node.max_remind_times),
                Value::from(node.cc_on_approved),
                Value::from(node.x),
                Value::from(node.y),
                Value::from(node.sort_order),
                Value::Json(node.config.to_string()),
            ],
        )
        .await?;
    }

    for node in &graph.nodes {
        let Some(node_id) = node_ids.get(node.key.as_str()).copied() else {
            continue;
        };
        for assignee in &node.assignees {
            let ref_id = Uuid::parse_str(&assignee.ref_id).map_err(|error| {
                DbError::InvalidValue(format!(
                    "节点 {} 的审批人 ref_id 不是合法 UUID：{}（{error}）",
                    node.key, assignee.ref_id
                ))
            })?;
            ex.execute_with_params(
                "INSERT INTO biz_flow_node_assignee \
                     (node_id, assignee_type, ref_id, sort_order) \
                 VALUES (?::uuid, ?, ?::uuid, ?) ON CONFLICT DO NOTHING",
                vec![
                    Value::from(&node_id),
                    Value::Text(assignee.kind.clone()),
                    Value::from(&ref_id),
                    Value::from(assignee.sort_order),
                ],
            )
            .await?;
        }
    }

    for edge in &graph.edges {
        let (Some(from_id), Some(to_id)) = (
            node_ids.get(edge.from.as_str()),
            node_ids.get(edge.to.as_str()),
        ) else {
            return Err(DbError::InvalidValue(format!(
                "连线 {} 指向不存在的节点（{} → {}）",
                edge.key, edge.from, edge.to
            )));
        };
        ex.execute_with_params(
            "INSERT INTO biz_flow_edge \
                 (id, version_id, edge_key, from_node_id, to_node_id, condition_expr, \
                  condition_label, priority, is_default) \
             VALUES (?::uuid, ?::uuid, ?, ?::uuid, ?::uuid, ?::jsonb, ?, ?, ?)",
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&version_id),
                Value::Text(edge.key.clone()),
                Value::from(from_id),
                Value::from(to_id),
                match edge.condition.as_ref() {
                    Some(condition) => Value::Json(condition.to_string()),
                    None => Value::Null,
                },
                opt_text(edge.label.as_deref()),
                Value::from(edge.priority),
                Value::from(edge.is_default),
            ],
        )
        .await?;
    }

    Ok(())
}

/// 发布写入参数（一次发布的全部字段）。
///
/// 打包成结构体而不是 9 个位置参数：这条路径的参数里有两个 hash / 快照字符串和两个计数，
/// 位置参数一旦顺序搞反（`graph_hash` 与 `graph_snapshot` 互串）编译期发现不了。
#[derive(Debug)]
pub struct PublishWrite<'a> {
    /// 流程 id。
    pub def_id: Uuid,
    /// 要发布的版本 id。
    pub version_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 快照 canonical JSON。
    pub snapshot: &'a str,
    /// 快照 sha256。
    pub graph_hash: &'a str,
    /// 节点数。
    pub node_count: i64,
    /// 边数。
    pub edge_count: i64,
    /// 操作人。
    pub operator: Uuid,
}

/// 发布事务（L1 → L2）：写快照 / hash / 行数，切换 `is_current` 与 `def.current_version_id`。
///
/// # 顺序为什么不能换
///
/// 1. 先把**其它**版本清成 `is_current = FALSE` 并把旧的 PUBLISHED 降为 ARCHIVED
///    （否则 `uk_biz_flow_def_version_current` 这条部分唯一索引会挡下第 2 步）；
/// 2. 再把本版本置 `PUBLISHED + is_current = TRUE` 并写快照；
/// 3. 最后改 `def`（`ck_biz_flow_def_published_has_version` 要求 PUBLISHED 时
///    `current_version_id` 非空 —— 所以必须等第 2 步完成后才能置 PUBLISHED）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn apply_publish(ex: &Transaction, write: &PublishWrite<'_>) -> DbResult<()> {
    let PublishWrite {
        def_id,
        version_id,
        version_no,
        snapshot,
        graph_hash,
        node_count,
        edge_count,
        operator,
    } = *write;
    ex.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET is_current = FALSE, \
                version_status = CASE WHEN version_status = 'PUBLISHED' THEN 'ARCHIVED' \
                                      ELSE version_status END, \
                updated_at = now(), update_by = ?::uuid \
          WHERE def_id = ?::uuid AND id <> ?::uuid AND (is_current OR version_status = 'PUBLISHED')",
        vec![
            Value::from(&operator),
            Value::from(&def_id),
            Value::from(&version_id),
        ],
    )
    .await?;

    ex.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET version_status = 'PUBLISHED', is_current = TRUE, graph_hash = ?, \
                graph_snapshot = ?::jsonb, node_count = ?, edge_count = ?, \
                published_at = now(), published_by = ?::uuid, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND def_id = ?::uuid",
        vec![
            Value::Text(graph_hash.to_string()),
            Value::Json(snapshot.to_string()),
            Value::from(node_count),
            Value::from(edge_count),
            Value::from(&operator),
            Value::from(&operator),
            Value::from(&version_id),
            Value::from(&def_id),
        ],
    )
    .await?;

    ex.execute_with_params(
        "UPDATE biz_flow_def \
            SET status = 'PUBLISHED', current_version_id = ?::uuid, current_version_no = ?, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(&version_id),
            Value::from(version_no),
            Value::from(&operator),
            Value::from(&def_id),
        ],
    )
    .await?;
    Ok(())
}

/// 另存为新草稿：把 `source_version_id` 的节点 / 边 / 审批人整份复制到新版本。
///
/// 三步纯 SQL 复制（不把图读进内存再写回）：节点先复制并按 `node_key` 映射新 id，
/// 审批人与边再按 key JOIN 回新节点 —— 200 节点的图也只是 3 条语句。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn copy_version_to_draft(
    ex: &Transaction,
    def_id: Uuid,
    source_version_id: Uuid,
    new_version_id: Uuid,
    change_note: Option<&str>,
    operator: Uuid,
) -> DbResult<(i64, i64)> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_node \
             (id, version_id, node_key, node_name, node_type, assignee_type, superior_levels, \
              form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, \
              self_approval, allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, \
              remind_before_hours, max_remind_times, cc_on_approved, \
              position_x, position_y, sort_order, config) \
         SELECT gen_random_uuid(), ?::uuid, n.node_key, n.node_name, n.node_type, n.assignee_type, \
                n.superior_levels, n.form_var_key, n.approval_mode, n.approve_ratio, n.reject_veto, \
                n.empty_assignee, n.self_approval, n.allow_robot, n.join_mode, n.fork_mode, \
                n.timeout_hours, n.timeout_action, n.remind_before_hours, n.max_remind_times, \
                n.cc_on_approved, n.position_x, n.position_y, n.sort_order, n.config \
           FROM biz_flow_node n WHERE n.version_id = ?::uuid",
        vec![
            Value::from(&new_version_id),
            Value::from(&source_version_id),
        ],
    )
    .await?;

    ex.execute_with_params(
        "INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order) \
         SELECT target.id, a.assignee_type, a.ref_id, a.sort_order \
           FROM biz_flow_node_assignee a \
           JOIN biz_flow_node source ON source.id = a.node_id \
           JOIN biz_flow_node target \
             ON target.version_id = ?::uuid AND target.node_key = source.node_key \
          WHERE source.version_id = ?::uuid \
         ON CONFLICT DO NOTHING",
        vec![
            Value::from(&new_version_id),
            Value::from(&source_version_id),
        ],
    )
    .await?;

    ex.execute_with_params(
        "INSERT INTO biz_flow_edge \
             (id, version_id, edge_key, from_node_id, to_node_id, condition_expr, \
              condition_label, priority, is_default) \
         SELECT gen_random_uuid(), ?::uuid, e.edge_key, target_from.id, target_to.id, \
                e.condition_expr, e.condition_label, e.priority, e.is_default \
           FROM biz_flow_edge e \
           JOIN biz_flow_node source_from ON source_from.id = e.from_node_id \
           JOIN biz_flow_node source_to ON source_to.id = e.to_node_id \
           JOIN biz_flow_node target_from \
             ON target_from.version_id = ?::uuid AND target_from.node_key = source_from.node_key \
           JOIN biz_flow_node target_to \
             ON target_to.version_id = ?::uuid AND target_to.node_key = source_to.node_key \
          WHERE e.version_id = ?::uuid",
        vec![
            Value::from(&new_version_id),
            Value::from(&new_version_id),
            Value::from(&new_version_id),
            Value::from(&source_version_id),
        ],
    )
    .await?;

    let (node_count, edge_count) = count_rows(ex, new_version_id).await?;
    ex.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET node_count = ?, edge_count = ?, change_note = ?, updated_at = now(), \
                update_by = ?::uuid \
          WHERE id = ?::uuid AND def_id = ?::uuid",
        vec![
            Value::from(node_count),
            Value::from(edge_count),
            opt_text(change_note),
            Value::from(&operator),
            Value::from(&new_version_id),
            Value::from(&def_id),
        ],
    )
    .await?;
    Ok((node_count, edge_count))
}

/// 版本是否已被引用（D2 的 `biz_flow_instance.version_id`）。
///
/// 本卡（D1）范围里实例表还不存在，函数先返回 `false` 并在 D2 落地后改为真实查询。
/// **明确写成一个函数**而不是注释：D2 只需要改这一处，不会漏掉「删流程前先查引用」。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn has_running_instance(def_id: Uuid) -> DbResult<bool> {
    // D2 落地：定义侧删流程前必须查「还有没有在跑的单子」（架构 §7.1 #5 的前置校验）。
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_instance \
              WHERE def_id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL",
            vec![Value::from(&def_id)],
        )
        .await?;
    let total: i64 = match rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok(total > 0)
}

/// 流程错误码转 [`ApiError`]（写路径复用的 404 文案）。
pub fn not_found() -> ApiError {
    ApiError::new(404, error_code::FLOW_NOT_FOUND, "流程不存在或已删除")
}

/// 版本不存在 → 404。
pub fn version_not_found() -> ApiError {
    ApiError::new(404, error_code::FLOW_NOT_FOUND, "流程版本不存在")
}

/// 按 `flow_code` 取流程定义（未软删）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_def_by_code(code: &str) -> DbResult<Option<DefRow>> {
    let sql =
        format!("SELECT {DEF_COLUMNS}{DEF_FROM} WHERE d.flow_code = ? AND d.deleted_at IS NULL");
    let rows = db()
        .find_with_params(&sql, vec![Value::Text(code.to_string())])
        .await?;
    rows.first().map(DefRow::from_row).transpose()
}

/// 取某业务类型的**默认已发布流程**（发起时只给 `biz_type` 用）。
///
/// 排序口径：`is_default` 优先，其次当前版本号大的（最新发布的那个）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn default_def_of_biz_type(biz_type: &str) -> DbResult<Option<DefRow>> {
    let sql = format!(
        "SELECT {DEF_COLUMNS}{DEF_FROM} \
          WHERE d.biz_type = ? AND d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
            AND d.current_version_id IS NOT NULL \
          ORDER BY d.is_default DESC, d.current_version_no DESC, d.updated_at DESC \
          LIMIT 1"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::Text(biz_type.to_string())])
        .await?;
    rows.first().map(DefRow::from_row).transpose()
}

// ---------- 运行侧批注：run-time runtime block (D2) ----------
// ==================== 运行侧：实例 / 节点 / 待办 / 抄送（D2） ====================
//
// 依据：`docs/批次D-工作流审批-技术架构.md` §6.2（4 张表的列清单）、§4.4（一次流转算法）、
// §4.5（并发三件套）、§4.6（退回 round）、§9.1（实例可见性谓词）。
//
// # 本段的三条纪律
//
// 1. **锁顺序固定** `biz_flow_task` → `biz_flow_instance_node` → `biz_flow_instance`
//    （架构 §4.5.1）。所有 `*_for_update` 函数只做 `SELECT … FOR UPDATE`，
//    由 `engine.rs` 按上述顺序调用；这里不隐含任何加锁顺序，避免「函数内部顺手锁了实例」。
// 2. **计票一律现算**：`tally_of_node` 是唯一的票数来源，`approved_count` / `rejected_count`
//    只是 `update_node_counts` 写进去的展示冗余（**没有任何 `+=`**）。
// 3. **`auto_passed_nodes` 也是派生的**：`sync_auto_passed(instance_id)` 用
//    `count(*) WHERE is_auto_passed` 覆写，而不是 `auto_passed_nodes + 1`
//    —— 审计计数一旦能漂移，坑 3 的「本单有 1 个节点自动通过」就不可信了。

// ---------- 列清单 ----------

const INSTANCE_COLUMNS: &str = "\
     i.id, i.instance_no, i.def_id, i.version_id, i.version_no, i.flow_code, i.flow_name, \
     i.biz_type, i.biz_id, i.biz_no, i.title, i.status, i.initiator_id, i.initiator_name, \
     i.initiator_dept_id, i.form_data::text AS form_data_text, i.result_comment, \
     i.auto_passed_nodes, i.round_total, i.prev_instance_id, i.revision, \
     i.started_at, i.ended_at, i.due_at, i.created_at, i.updated_at";

const NODE_RT_COLUMNS: &str = "\
     n.id, n.instance_id, n.node_id, n.node_key, n.node_name, n.node_type, n.round, n.status, \
     n.approval_mode, n.approve_ratio::float8 AS approve_ratio, n.reject_veto, \
     n.approver_count, n.approved_count, n.rejected_count, n.taken_edge_key, n.is_auto_passed, \
     n.activated_at, n.ended_at, n.duration_ms, n.created_at, n.updated_at";

const TASK_COLUMNS: &str = "\
     t.id, t.instance_id, t.instance_node_id, t.node_key, t.round, t.task_kind, t.assignee_id, \
     t.assignee_name, t.assignee_source, t.assignee_source_ref, t.status, t.is_added_sign, \
     t.is_self_approval, t.is_auto, t.comment, t.attachments::text AS attachments_text, \
     t.transferred_from, t.transferred_to, t.due_at, t.remind_count, t.created_at, t.completed_at";

const CC_COLUMNS: &str = "\
     c.id, c.instance_id, c.node_key, c.round, c.receiver_id, c.receiver_name, c.source_type, \
     c.source_ref, c.is_read, c.read_at, c.created_at";

// ---------- 行结构 ----------

/// 实例行。
#[derive(Debug, Clone)]
pub struct InstanceRow {
    /// 主键。
    pub id: Uuid,
    /// 单据号。
    pub instance_no: String,
    /// 流程定义。
    pub def_id: Uuid,
    /// 绑定版本（架构 D2：引擎只读该版本的快照）。
    pub version_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 流程编码（冗余，防展示期 JOIN 定义表）。
    pub flow_code: String,
    /// 流程名称（冗余）。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 业务 id。
    pub biz_id: Option<Uuid>,
    /// 业务单号。
    pub biz_no: Option<String>,
    /// 标题。
    pub title: String,
    /// 状态（`PENDING` / `APPROVED` / `REJECTED` / `CANCELLED` / `WITHDRAWN`）。
    pub status: String,
    /// 发起人。
    pub initiator_id: Uuid,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 发起人部门。
    pub initiator_dept_id: Option<Uuid>,
    /// 表单数据（发起时冻结）。
    pub form_data: Json,
    /// 结束说明。
    pub result_comment: Option<String>,
    /// 自动通过节点数（派生值）。
    pub auto_passed_nodes: i32,
    /// 最大退回轮次。
    pub round_total: i32,
    /// 上一次实例（重新发起时串联）。
    pub prev_instance_id: Option<Uuid>,
    /// CAS 版本号。
    pub revision: i32,
    /// 发起时间。
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 结束时间。
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 截止时间。
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl InstanceRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let form_text: String = row.get_typed("form_data_text")?;
        Ok(Self {
            id: row.get_typed("id")?,
            instance_no: row.get_typed("instance_no")?,
            def_id: row.get_typed("def_id")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            biz_id: row.get_typed("biz_id")?,
            biz_no: row.get_typed("biz_no")?,
            title: row.get_typed("title")?,
            status: row.get_typed("status")?,
            initiator_id: row.get_typed("initiator_id")?,
            initiator_name: row.get_typed("initiator_name")?,
            initiator_dept_id: row.get_typed("initiator_dept_id")?,
            form_data: serde_json::from_str(&form_text)
                .unwrap_or_else(|_| Json::Object(Default::default())),
            result_comment: row.get_typed("result_comment")?,
            auto_passed_nodes: row.get_typed("auto_passed_nodes")?,
            round_total: row.get_typed("round_total")?,
            prev_instance_id: row.get_typed("prev_instance_id")?,
            revision: row.get_typed("revision")?,
            started_at: row.get_typed("started_at")?,
            ended_at: row.get_typed("ended_at")?,
            due_at: row.get_typed("due_at")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 是否进行中。
    pub fn is_pending(&self) -> bool {
        self.status == "PENDING"
    }

    /// 审批人解析上下文（`assignee::InstanceCtx`）。
    pub fn assignee_ctx(&self) -> crate::business::flow::assignee::InstanceCtx {
        crate::business::flow::assignee::InstanceCtx {
            instance_id: self.id,
            initiator_id: self.initiator_id,
            initiator_dept_id: self.initiator_dept_id,
            biz_type: self.biz_type.clone(),
            biz_id: self.biz_id,
            form_data: self.form_data.clone(),
        }
    }
}

/// 实例节点行。
#[derive(Debug, Clone)]
pub struct NodeRow {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 定义侧节点 id（可空，删版本时 `SET NULL`）。
    pub node_id: Option<Uuid>,
    /// 节点 key。
    pub node_key: String,
    /// 节点名。
    pub node_name: String,
    /// 节点类型。
    pub node_type: String,
    /// 退回轮次。
    pub round: i32,
    /// 状态。
    pub status: String,
    /// 会签模式。
    pub approval_mode: Option<String>,
    /// 会签阈值。
    pub approve_ratio: Option<f64>,
    /// 一票否决。
    pub reject_veto: Option<bool>,
    /// 应到人数。
    pub approver_count: i32,
    /// 已同意（展示冗余）。
    pub approved_count: i32,
    /// 已驳回（展示冗余）。
    pub rejected_count: i32,
    /// 条件节点命中的边。
    pub taken_edge_key: Option<String>,
    /// 是否自动通过。
    pub is_auto_passed: bool,
    /// 激活时间。
    pub activated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 结束时间。
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 耗时（毫秒）。
    pub duration_ms: Option<i64>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl NodeRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            instance_id: row.get_typed("instance_id")?,
            node_id: row.get_typed("node_id")?,
            node_key: row.get_typed("node_key")?,
            node_name: row.get_typed("node_name")?,
            node_type: row.get_typed("node_type")?,
            round: row.get_typed("round")?,
            status: row.get_typed("status")?,
            approval_mode: row.get_typed("approval_mode")?,
            approve_ratio: row.get_typed("approve_ratio")?,
            reject_veto: row.get_typed("reject_veto")?,
            approver_count: row.get_typed("approver_count")?,
            approved_count: row.get_typed("approved_count")?,
            rejected_count: row.get_typed("rejected_count")?,
            taken_edge_key: row.get_typed("taken_edge_key")?,
            is_auto_passed: row.get_typed("is_auto_passed")?,
            activated_at: row.get_typed("activated_at")?,
            ended_at: row.get_typed("ended_at")?,
            duration_ms: row.get_typed("duration_ms")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 是否已经结束（终态）。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "APPROVED" | "REJECTED" | "SKIPPED" | "CANCELLED"
        )
    }
}

/// 待办 / 审批记录行。
#[derive(Debug, Clone)]
pub struct TaskRow {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 实例节点。
    pub instance_node_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 待办种类（`APPROVAL` / `RESUBMIT`）。
    pub task_kind: String,
    /// 审批人。
    pub assignee_id: Uuid,
    /// 审批人姓名快照。
    pub assignee_name: Option<String>,
    /// 来源。
    pub assignee_source: String,
    /// 来源引用（角色 / 部门）。
    pub assignee_source_ref: Option<Uuid>,
    /// 状态。
    pub status: String,
    /// 是否加签。
    pub is_added_sign: bool,
    /// 是否自审。
    pub is_self_approval: bool,
    /// 是否系统代点。
    pub is_auto: bool,
    /// 意见。
    pub comment: Option<String>,
    /// 附件（JSON 数组）。
    pub attachments: Json,
    /// 转办来源。
    pub transferred_from: Option<Uuid>,
    /// 转办去向。
    pub transferred_to: Option<Uuid>,
    /// 截止时间。
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 提醒次数。
    pub remind_count: i16,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 完成时间。
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TaskRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let attachments_text: String = row.get_typed("attachments_text")?;
        Ok(Self {
            id: row.get_typed("id")?,
            instance_id: row.get_typed("instance_id")?,
            instance_node_id: row.get_typed("instance_node_id")?,
            node_key: row.get_typed("node_key")?,
            round: row.get_typed("round")?,
            task_kind: row.get_typed("task_kind")?,
            assignee_id: row.get_typed("assignee_id")?,
            assignee_name: row.get_typed("assignee_name")?,
            assignee_source: row.get_typed("assignee_source")?,
            assignee_source_ref: row.get_typed("assignee_source_ref")?,
            status: row.get_typed("status")?,
            is_added_sign: row.get_typed("is_added_sign")?,
            is_self_approval: row.get_typed("is_self_approval")?,
            is_auto: row.get_typed("is_auto")?,
            comment: row.get_typed("comment")?,
            attachments: serde_json::from_str(&attachments_text)
                .unwrap_or_else(|_| Json::Array(Vec::new())),
            transferred_from: row.get_typed("transferred_from")?,
            transferred_to: row.get_typed("transferred_to")?,
            due_at: row.get_typed("due_at")?,
            remind_count: row.get_typed("remind_count")?,
            created_at: row.get_typed("created_at")?,
            completed_at: row.get_typed("completed_at")?,
        })
    }

    /// 是否还在待办里。
    pub fn is_pending(&self) -> bool {
        self.status == "PENDING"
    }
}

/// 抄送行。
#[derive(Debug, Clone)]
pub struct CcRow {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 收件人。
    pub receiver_id: Uuid,
    /// 收件人姓名。
    pub receiver_name: Option<String>,
    /// 来源。
    pub source_type: String,
    /// 来源引用。
    pub source_ref: Option<Uuid>,
    /// 是否已读。
    pub is_read: bool,
    /// 读取时间。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl CcRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            instance_id: row.get_typed("instance_id")?,
            node_key: row.get_typed("node_key")?,
            round: row.get_typed("round")?,
            receiver_id: row.get_typed("receiver_id")?,
            receiver_name: row.get_typed("receiver_name")?,
            source_type: row.get_typed("source_type")?,
            source_ref: row.get_typed("source_ref")?,
            is_read: row.get_typed("is_read")?,
            read_at: row.get_typed("read_at")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 票数（**唯一权威来源是 `COUNT(*)`**，架构 D15）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// 已同意。
    pub approved: i64,
    /// 已驳回。
    pub rejected: i64,
    /// 未表态。
    pub pending: i64,
    /// 应到（在册票数：`PENDING + APPROVED + REJECTED`，**不含** `SKIPPED` / `TRANSFERRED` / `CANCELLED`）。
    pub total: i64,
}

// ---------- 单据号 ----------

/// 生成单据号 `FLW-YYYYMMDD-<uuid 末 8 位>`。
///
/// **用 UUIDv7 的末 8 位而不是前 8 位**：v7 的高位就是时间戳，同一毫秒连发的两条
/// 会拿到相同前缀 → 撞 `uk_biz_flow_instance_no`（skill cailu-dev §9.12-3 实测）。
pub fn new_instance_no(now: chrono::DateTime<chrono::Utc>) -> String {
    let id = Uuid::now_v7().to_string();
    let tail: String = id.chars().filter(|c| *c != '-').collect::<String>();
    let tail = tail
        .get(tail.len().saturating_sub(8)..)
        .unwrap_or("00000000");
    format!("FLW-{}-{}", now.format("%Y%m%d"), tail.to_ascii_uppercase())
}

// ---------- 写：实例 ----------

/// 新建实例入参。
#[derive(Debug, Clone)]
pub struct NewInstance {
    /// 主键。
    pub id: Uuid,
    /// 单据号。
    pub instance_no: String,
    /// 流程定义。
    pub def_id: Uuid,
    /// 绑定版本。
    pub version_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 业务 id。
    pub biz_id: Option<Uuid>,
    /// 业务单号。
    pub biz_no: Option<String>,
    /// 标题。
    pub title: String,
    /// 发起人。
    pub initiator_id: Uuid,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 发起人部门。
    pub initiator_dept_id: Option<Uuid>,
    /// 表单数据。
    pub form_data: Json,
    /// 截止时间。
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 新建实例（事务内）。
///
/// # 异常情况
///
/// `uk_biz_flow_instance_biz_active` / `uk_biz_flow_instance_no` 冲突 → [`DbError`]。
pub async fn create_instance(ex: &Transaction, new: &NewInstance) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_instance \
             (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
              biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name, \
              initiator_dept_id, form_data, auto_passed_nodes, round_total, revision, \
              started_at, due_at, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?, ?, ?, ?, ?::uuid, ?, ?, 'PENDING', \
                 ?::uuid, ?, ?::uuid, ?::jsonb, 0, 1, 0, now(), ?, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&new.id),
            Value::Text(new.instance_no.clone()),
            Value::from(&new.def_id),
            Value::from(&new.version_id),
            Value::from(new.version_no),
            Value::Text(new.flow_code.clone()),
            Value::Text(new.flow_name.clone()),
            Value::Text(new.biz_type.clone()),
            opt_uuid(new.biz_id),
            opt_text(new.biz_no.as_deref()),
            Value::Text(new.title.clone()),
            Value::from(&new.initiator_id),
            opt_text(new.initiator_name.as_deref()),
            opt_uuid(new.initiator_dept_id),
            Value::Json(new.form_data.to_string()),
            opt_ts(new.due_at),
            Value::from(&new.initiator_id),
            Value::from(&new.initiator_id),
        ],
    )
    .await?;
    Ok(())
}

/// 读实例（含软删过滤）。不存在 → `None`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_instance(id: Uuid) -> DbResult<Option<InstanceRow>> {
    let sql = format!(
        "SELECT {INSTANCE_COLUMNS} FROM biz_flow_instance i \
          WHERE i.id = ?::uuid AND i.deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(InstanceRow::from_row).transpose()
}

/// 锁实例行（`SELECT … FOR UPDATE`，锁序第三）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn lock_instance(ex: &Transaction, id: Uuid) -> DbResult<Option<InstanceRow>> {
    let sql = format!(
        "SELECT {INSTANCE_COLUMNS} FROM biz_flow_instance i WHERE i.id = ?::uuid FOR UPDATE"
    );
    let rows = ex.find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(InstanceRow::from_row).transpose()
}

/// 实例 CAS：`revision` 自增并写入状态 / 轮次 / 说明。
///
/// 返回受影响行数：**0 表示 CAS 失败**（别人先推进了同一实例），调用方重新读取后重试一次
/// （架构 §4.5.1 第 ⑦ 步）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn bump_instance(
    ex: &Transaction,
    id: Uuid,
    expected_revision: i32,
    status: Option<&str>,
    result_comment: Option<&str>,
    round_total: i32,
) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance \
            SET status = COALESCE(?, status), \
                result_comment = COALESCE(?, result_comment), \
                round_total = ?, \
                revision = revision + 1, \
                ended_at = CASE WHEN COALESCE(?, status) = 'PENDING' THEN NULL ELSE now() END, \
                updated_at = now() \
          WHERE id = ?::uuid AND revision = ?",
        vec![
            opt_text(status),
            opt_text(result_comment),
            Value::from(round_total),
            opt_text(status),
            Value::from(&id),
            Value::from(expected_revision),
        ],
    )
    .await
}

/// 用 `count(*)` 覆写 `auto_passed_nodes`（**派生值，不是 `+=`**）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn sync_auto_passed(ex: &Transaction, instance_id: Uuid) -> DbResult<i32> {
    let rows = ex
        .find_with_params(
            "UPDATE biz_flow_instance \
                SET auto_passed_nodes = ( \
                        SELECT count(*) FROM biz_flow_instance_node \
                         WHERE instance_id = ?::uuid AND is_auto_passed), \
                    updated_at = now() \
              WHERE id = ?::uuid \
          RETURNING auto_passed_nodes",
            vec![Value::from(&instance_id), Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(0),
        Some(row) => row.get_typed("auto_passed_nodes"),
    }
}

/// 同一业务单据是否还有在跑的实例（`start` 的前置校验 + 并发兜底）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn biz_instance_running(biz_type: &str, biz_id: Uuid) -> DbResult<bool> {
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_instance \
              WHERE biz_type = ? AND biz_id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL",
            vec![Value::Text(biz_type.to_string()), Value::from(&biz_id)],
        )
        .await?;
    let total: i64 = match rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok(total > 0)
}

// ---------- 写：实例节点 ----------

/// 节点「到达」入参。
#[derive(Debug, Clone)]
pub struct NewNode {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 定义侧节点 id。
    pub node_id: Option<Uuid>,
    /// 节点 key。
    pub node_key: String,
    /// 节点名。
    pub node_name: String,
    /// 节点类型。
    pub node_type: String,
    /// 轮次。
    pub round: i32,
    /// 会签模式。
    pub approval_mode: Option<String>,
    /// 会签阈值。
    pub approve_ratio: Option<f64>,
    /// 一票否决。
    pub reject_veto: Option<bool>,
}

/// 幂等地登记「本轮到达过这个节点」。
///
/// `ON CONFLICT (instance_id, node_key, round) DO UPDATE` 让重复激活变成一次无害的
/// `updated_at` 刷新（**幂等**而不报 23505）—— 与 `biz_flow_task` 的插入刻意不同：
/// 那边要求「23505 当幂等成功」的红线走显式错误分支，这边没有外部可观察语义。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn touch_node(ex: &Transaction, new: &NewNode) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_instance_node \
             (id, instance_id, node_id, node_key, node_name, node_type, round, status, \
              approval_mode, approve_ratio, reject_veto, approver_count, approved_count, \
              rejected_count, is_auto_passed, created_at, updated_at) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?, ?, ?, 'PENDING', ?, ?::text::numeric, ?, 0, 0, 0, \
                 FALSE, now(), now()) \
         ON CONFLICT (instance_id, node_key, round) DO UPDATE SET updated_at = now()",
        vec![
            Value::from(&new.id),
            Value::from(&new.instance_id),
            opt_uuid(new.node_id),
            Value::Text(new.node_key.clone()),
            Value::Text(new.node_name.clone()),
            Value::Text(new.node_type.clone()),
            Value::from(new.round),
            opt_text(new.approval_mode.as_deref()),
            new.approve_ratio
                .map(|ratio| Value::Text(format!("{ratio:.3}")))
                .unwrap_or(Value::Null),
            new.reject_veto.map(Value::from).unwrap_or(Value::Null),
        ],
    )
    .await?;
    Ok(())
}

/// 读某轮次的节点行（不加锁）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_node(
    ex: &Transaction,
    instance_id: Uuid,
    node_key: &str,
    round: i32,
) -> DbResult<Option<NodeRow>> {
    let sql = format!(
        "SELECT {NODE_RT_COLUMNS} FROM biz_flow_instance_node n \
          WHERE n.instance_id = ?::uuid AND n.node_key = ? AND n.round = ?"
    );
    let rows = ex
        .find_with_params(
            &sql,
            vec![
                Value::from(&instance_id),
                Value::Text(node_key.to_string()),
                Value::from(round),
            ],
        )
        .await?;
    rows.first().map(NodeRow::from_row).transpose()
}

/// 锁节点行（`SELECT … FOR UPDATE`，锁序第二）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn lock_node(ex: &Transaction, id: Uuid) -> DbResult<Option<NodeRow>> {
    let sql = format!(
        "SELECT {NODE_RT_COLUMNS} FROM biz_flow_instance_node n WHERE n.id = ?::uuid FOR UPDATE"
    );
    let rows = ex.find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(NodeRow::from_row).transpose()
}

/// 把节点置为 `ACTIVE` 并写应到人数。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn activate_node_row(ex: &Transaction, id: Uuid, approver_count: i32) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance_node \
            SET status = 'ACTIVE', approver_count = ?, activated_at = now(), \
                ended_at = NULL, duration_ms = NULL, updated_at = now() \
          WHERE id = ?::uuid",
        vec![Value::from(approver_count), Value::from(&id)],
    )
    .await?;
    Ok(())
}

/// 结束节点：写终态 / 计数 / 结束时间 / 耗时 / 是否自动通过。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn end_node_row(
    ex: &Transaction,
    id: Uuid,
    status: &str,
    tally: Tally,
    taken_edge_key: Option<&str>,
    is_auto_passed: bool,
) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance_node \
            SET status = ?, approved_count = ?, rejected_count = ?, \
                approver_count = CASE WHEN ? > 0 THEN ? ELSE approver_count END, \
                taken_edge_key = COALESCE(?, taken_edge_key), \
                is_auto_passed = ?, \
                activated_at = COALESCE(activated_at, now()), \
                ended_at = now(), \
                duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(activated_at, now()))) * 1000)::bigint, \
                updated_at = now() \
          WHERE id = ?::uuid",
        vec![
            Value::Text(status.to_string()),
            Value::from(tally.approved),
            Value::from(tally.rejected),
            Value::from(tally.total),
            Value::from(tally.total),
            opt_text(taken_edge_key),
            Value::from(is_auto_passed),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}

/// 覆写节点上的票数展示冗余（**绝对值，不是 `+=`**，架构 D15）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn update_node_counts(ex: &Transaction, id: Uuid, tally: Tally) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance_node \
            SET approved_count = ?, rejected_count = ?, \
                approver_count = CASE WHEN ? > 0 THEN ? ELSE approver_count END, \
                updated_at = now() \
          WHERE id = ?::uuid",
        vec![
            Value::from(tally.approved),
            Value::from(tally.rejected),
            Value::from(tally.total),
            Value::from(tally.total),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}

/// 把节点标成「等人齐」（汇聚点 `AND` 未到齐）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn mark_node_waiting(ex: &Transaction, id: Uuid) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_instance_node \
            SET status = CASE WHEN status = 'ACTIVE' THEN status ELSE 'PENDING' END, \
                updated_at = now() \
          WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await?;
    Ok(())
}

/// 实例下所有节点行（按轮次、到达顺序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn nodes_of(instance_id: Uuid) -> DbResult<Vec<NodeRow>> {
    let sql = format!(
        "SELECT {NODE_RT_COLUMNS} FROM biz_flow_instance_node n \
          WHERE n.instance_id = ?::uuid ORDER BY n.round, n.created_at, n.node_key"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&instance_id)])
        .await?;
    rows.iter().map(NodeRow::from_row).collect()
}

/// 掐断实例里所有 `ACTIVE` 节点（退回 / 撤回 / 终止时用）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cancel_active_nodes(
    ex: &Transaction,
    instance_id: Uuid,
    except_node_id: Option<Uuid>,
) -> DbResult<u64> {
    let mut sql = String::from(
        "UPDATE biz_flow_instance_node \
            SET status = 'CANCELLED', ended_at = now(), \
                duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(activated_at, now()))) * 1000)::bigint, \
                updated_at = now() \
          WHERE instance_id = ?::uuid AND status IN ('ACTIVE','PENDING')",
    );
    let mut params = vec![Value::from(&instance_id)];
    if let Some(node_id) = except_node_id {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&node_id));
    }
    ex.execute_with_params(&sql, params).await
}

/// 实例当前最大轮次。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn max_round(ex: &Transaction, instance_id: Uuid) -> DbResult<i32> {
    let rows = ex
        .find_with_params(
            "SELECT COALESCE(max(round), 1) AS max_round FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(1),
        Some(row) => row.get_typed("max_round"),
    }
}

/// 某节点在某轮次的行是否存在且为 `APPROVED`/`SKIPPED`（汇聚判定用，**必须带 round**）。
///
/// 这是坑 4 的核心：不带 `round` 就会读到**上一轮**的通过记录，退回后汇聚点直接放行。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn node_arrived(
    ex: &Transaction,
    instance_id: Uuid,
    node_key: &str,
    round: i32,
) -> DbResult<bool> {
    let rows = ex
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid AND node_key = ? AND round = ? \
                AND status IN ('APPROVED','SKIPPED')",
            vec![
                Value::from(&instance_id),
                Value::Text(node_key.to_string()),
                Value::from(round),
            ],
        )
        .await?;
    let total: i64 = match rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok(total > 0)
}

// ---------- 写：待办 ----------

/// 新建待办入参。
#[derive(Debug, Clone)]
pub struct NewTask {
    /// 主键。
    pub id: Uuid,
    /// 实例。
    pub instance_id: Uuid,
    /// 实例节点。
    pub instance_node_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 种类（`APPROVAL` / `RESUBMIT`）。
    pub task_kind: String,
    /// 审批人。
    pub assignee_id: Uuid,
    /// 审批人姓名快照。
    pub assignee_name: Option<String>,
    /// 来源。
    pub assignee_source: String,
    /// 来源引用。
    pub assignee_source_ref: Option<Uuid>,
    /// 是否加签。
    pub is_added_sign: bool,
    /// 是否自审。
    pub is_self_approval: bool,
    /// 是否系统代点。
    pub is_auto: bool,
    /// 状态（`PENDING` / `SKIPPED`）。
    pub status: String,
    /// 意见。
    pub comment: Option<String>,
    /// 截止时间。
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 转办来源行。
    pub transferred_from: Option<Uuid>,
}

/// 插入一条待办行。
///
/// **不用 `ON CONFLICT DO NOTHING`**：三层去重的第 2 层必须真的能挡住重复
/// （验收 2 要贴 23505），并让应用显式走「把 23505 当幂等成功」这条红线路径。
/// 调用方用 [`is_pending_duplicate`] 判定。
///
/// `SKIPPED` 行没有 `PENDING` 槽位，不受部分唯一索引约束。
///
/// # 异常情况
///
/// `uk_biz_flow_task_pending` 冲突 → [`DbError::UniqueViolation`]；其它 → [`DbError`]。
pub async fn insert_task(ex: &Transaction, new: &NewTask) -> DbResult<()> {
    // `completed_at` 必须与 status 同真同假（`ck_biz_flow_task_done`）：
    // PENDING → NULL；其它（SKIPPED）→ now()。用 CASE 而不是绑时间值，保证与库时钟一致。
    ex.execute_with_params(
        "INSERT INTO biz_flow_task \
             (id, instance_id, instance_node_id, node_key, round, task_kind, assignee_id, \
              assignee_name, assignee_source, assignee_source_ref, status, is_added_sign, \
              is_self_approval, is_auto, comment, attachments, transferred_from, due_at, \
              remind_count, created_at, completed_at) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?, ?::uuid, ?, ?, ?, ?, ?, \
                 '[]'::jsonb, ?::uuid, ?, 0, now(), \
                 CASE WHEN ? = 'PENDING' THEN NULL ELSE now() END)",
        vec![
            Value::from(&new.id),
            Value::from(&new.instance_id),
            Value::from(&new.instance_node_id),
            Value::Text(new.node_key.clone()),
            Value::from(new.round),
            Value::Text(new.task_kind.clone()),
            Value::from(&new.assignee_id),
            opt_text(new.assignee_name.as_deref()),
            Value::Text(new.assignee_source.clone()),
            opt_uuid(new.assignee_source_ref),
            Value::Text(new.status.clone()),
            Value::from(new.is_added_sign),
            Value::from(new.is_self_approval),
            Value::from(new.is_auto),
            opt_text(new.comment.as_deref()),
            opt_uuid(new.transferred_from),
            opt_ts(new.due_at),
            Value::Text(new.status.clone()),
        ],
    )
    .await?;
    Ok(())
}

/// 是否「同一节点同一人已有 PENDING 待办」的唯一约束冲突（三层去重第 2 层）。
pub fn is_pending_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains("uk_biz_flow_task_pending"))
            .unwrap_or(false),
        other => other.to_string().contains("uk_biz_flow_task_pending"),
    }
}

/// 是否「同一实例同一节点同一轮次」的唯一约束冲突（幂等激活）。
pub fn is_node_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains("uk_biz_flow_instance_node"))
            .unwrap_or(false),
        other => other.to_string().contains("uk_biz_flow_instance_node"),
    }
}

/// 锁待办行（`SELECT … FOR UPDATE`，锁序第一）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn lock_task(ex: &Transaction, id: Uuid) -> DbResult<Option<TaskRow>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM biz_flow_task t WHERE t.id = ?::uuid FOR UPDATE");
    let rows = ex.find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(TaskRow::from_row).transpose()
}

/// 读待办（不加锁）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_task(id: Uuid) -> DbResult<Option<TaskRow>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM biz_flow_task t WHERE t.id = ?::uuid");
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(TaskRow::from_row).transpose()
}

/// 结束一条待办（写状态 / 意见 / 完成时间）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn finish_task(
    ex: &Transaction,
    id: Uuid,
    status: &str,
    comment: Option<&str>,
    is_auto: bool,
) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE biz_flow_task \
            SET status = ?, comment = COALESCE(?, comment), is_auto = is_auto OR ?, \
                completed_at = CASE WHEN ? = 'PENDING' THEN NULL ELSE now() END \
          WHERE id = ?::uuid",
        vec![
            Value::Text(status.to_string()),
            opt_text(comment),
            Value::from(is_auto),
            Value::Text(status.to_string()),
            Value::from(&id),
        ],
    )
    .await?;
    Ok(())
}

/// 把某节点其余 `PENDING` 待办置 `CANCELLED`（节点通过 / 驳回 / 退回时）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cancel_pending_of_node(
    ex: &Transaction,
    node_id: Uuid,
    except_task_id: Option<Uuid>,
    comment: &str,
) -> DbResult<u64> {
    let mut sql = String::from(
        "UPDATE biz_flow_task \
            SET status = 'CANCELLED', comment = COALESCE(comment, ?), completed_at = now() \
          WHERE instance_node_id = ?::uuid AND status = 'PENDING'",
    );
    let mut params = vec![Value::Text(comment.to_string()), Value::from(&node_id)];
    if let Some(task_id) = except_task_id {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&task_id));
    }
    ex.execute_with_params(&sql, params).await
}

/// 把整个实例的 `PENDING` 待办置 `CANCELLED`（撤回 / 终止）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cancel_pending_of_instance(
    ex: &Transaction,
    instance_id: Uuid,
    comment: &str,
) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_flow_task \
            SET status = 'CANCELLED', comment = COALESCE(comment, ?), completed_at = now() \
          WHERE instance_id = ?::uuid AND status = 'PENDING'",
        vec![Value::Text(comment.to_string()), Value::from(&instance_id)],
    )
    .await
}

/// 现算票数（**唯一的票数来源**，`COUNT(*) FILTER`，架构 D15 第 3 层）。
///
/// `total` 只统计「在册票」= `PENDING + APPROVED + REJECTED`：
/// `SKIPPED`（自审跳过）与 `TRANSFERRED`（已转办）不是票，`CANCELLED` 是已作废的票。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn tally_of_node(ex: &Transaction, node_id: Uuid, round: i32) -> DbResult<Tally> {
    let rows = ex
        .find_with_params(
            "SELECT count(*) FILTER (WHERE status = 'APPROVED') AS approved, \
                    count(*) FILTER (WHERE status = 'REJECTED') AS rejected, \
                    count(*) FILTER (WHERE status = 'PENDING') AS pending, \
                    count(*) FILTER (WHERE status IN ('PENDING','APPROVED','REJECTED')) AS total \
               FROM biz_flow_task \
              WHERE instance_node_id = ?::uuid AND round = ?",
            vec![Value::from(&node_id), Value::from(round)],
        )
        .await?;
    match rows.first() {
        None => Ok(Tally::default()),
        Some(row) => Ok(Tally {
            approved: row.get_typed("approved")?,
            rejected: row.get_typed("rejected")?,
            pending: row.get_typed("pending")?,
            total: row.get_typed("total")?,
        }),
    }
}

/// 实例里「已经表过态」的票数（撤回的「零人已审」判定，架构 §4.7）。
///
/// **不算** `SKIPPED`（自动跳过的人没审过）与 `TRANSFERRED`（转办出去的人也没审）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn reviewed_count(ex: &Transaction, instance_id: Uuid) -> DbResult<i64> {
    let rows = ex
        .find_with_params(
            "SELECT count(*) AS total FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED')",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(0),
        Some(row) => row.get_typed("total"),
    }
}

/// 实例的审批时间线（全部轮次，含 `SKIPPED` / `TRANSFERRED` / `CANCELLED` 留痕）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn timeline(instance_id: Uuid) -> DbResult<Vec<TaskRow>> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM biz_flow_task t \
          WHERE t.instance_id = ?::uuid ORDER BY t.round, t.created_at, t.id"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&instance_id)])
        .await?;
    rows.iter().map(TaskRow::from_row).collect()
}

/// 我在这个实例上的待办 id（详情页的「可用动作」）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn my_task_of(instance_id: Uuid, user_id: Uuid) -> DbResult<Option<TaskRow>> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM biz_flow_task t \
          WHERE t.instance_id = ?::uuid AND t.assignee_id = ?::uuid AND t.status = 'PENDING' \
          ORDER BY t.round DESC, t.created_at DESC LIMIT 1"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&instance_id), Value::from(&user_id)])
        .await?;
    rows.first().map(TaskRow::from_row).transpose()
}

// ---------- 写：抄送 ----------

/// 新建抄送行入参（参数收成结构体：clippy 的 `too_many_arguments` 阈值是 7）。
#[derive(Debug, Clone)]
pub struct NewCc {
    /// 实例。
    pub instance_id: Uuid,
    /// 节点 key。
    pub node_key: String,
    /// 轮次。
    pub round: i32,
    /// 收件人。
    pub receiver_id: Uuid,
    /// 收件人姓名快照。
    pub receiver_name: Option<String>,
    /// 来源（与 `assignee_source` 同取值域）。
    pub source_type: String,
    /// 来源引用（角色 / 部门 id）。
    pub source_ref: Option<Uuid>,
}

/// 新建抄送行。///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（重复由 `uk_biz_flow_cc` + `ON CONFLICT DO NOTHING` 吞掉）。
pub async fn insert_cc(ex: &Transaction, new: &NewCc) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_cc \
             (id, instance_id, node_key, round, receiver_id, receiver_name, source_type, \
              source_ref, is_read, created_at) \
         VALUES (?::uuid, ?::uuid, ?, ?, ?::uuid, ?, ?, ?::uuid, FALSE, now()) \
         ON CONFLICT (instance_id, node_key, round, receiver_id) DO NOTHING",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&new.instance_id),
            Value::Text(new.node_key.clone()),
            Value::from(new.round),
            Value::from(&new.receiver_id),
            opt_text(new.receiver_name.as_deref()),
            Value::Text(new.source_type.clone()),
            opt_uuid(new.source_ref),
        ],
    )
    .await?;
    Ok(())
}

/// 实例的抄送列表。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cc_of(instance_id: Uuid) -> DbResult<Vec<CcRow>> {
    let sql = format!(
        "SELECT {CC_COLUMNS} FROM biz_flow_cc c \
          WHERE c.instance_id = ?::uuid ORDER BY c.created_at"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&instance_id)])
        .await?;
    rows.iter().map(CcRow::from_row).collect()
}

/// 实例里所有 `PENDING` 待办的审批人（撤回时通知他们）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn pending_assignees(ex: &Transaction, instance_id: Uuid) -> DbResult<Vec<Uuid>> {
    let rows = ex
        .find_with_params(
            "SELECT DISTINCT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance_id)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(row.get_typed("assignee_id")?);
    }
    out.sort();
    Ok(out)
}

/// 某节点某轮次的抄送收件人（抄送节点落库后发通知用）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cc_receivers(
    ex: &Transaction,
    instance_id: Uuid,
    node_key: &str,
    round: i32,
) -> DbResult<Vec<Uuid>> {
    let rows = ex
        .find_with_params(
            "SELECT receiver_id FROM biz_flow_cc \
              WHERE instance_id = ?::uuid AND node_key = ? AND round = ?",
            vec![
                Value::from(&instance_id),
                Value::Text(node_key.to_string()),
                Value::from(round),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(row.get_typed("receiver_id")?);
    }
    out.sort();
    Ok(out)
}

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
    let scope_sql = scope
        .sql()
        .trim()
        .trim_start_matches("AND ")
        .trim()
        .to_string();
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
    let list = rows
        .iter()
        .map(InstanceRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
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
const MY_INSTANCE_WHERE: &str = " WHERE i.initiator_id = ?::uuid AND i.deleted_at IS NULL";

/// 「抄送我的」where 片段（**身份口径 `biz_flow_cc.receiver_id`**）。
const MY_CC_WHERE: &str = " WHERE c.receiver_id = ?::uuid";

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
    let list = rows
        .iter()
        .map(InstanceRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
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
    fn order_sql(self) -> &'static str {
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
    /// 流程名称。
    pub flow_name: String,
    /// 发起人姓名。
    pub initiator_name: Option<String>,
    /// 当前节点中文名（`biz_flow_instance_node.node_name` 快照）。
    pub node_name: String,
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
pub async fn page_my_tasks(
    user_id: Uuid,
    statuses: &[&str],
    kinds: &[&str],
    order: MyTaskOrder,
    page: u64,
    page_size: u64,
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
    let where_sql = format!(
        " WHERE t.assignee_id = ?::uuid AND t.status IN ({status_list}) \
           AND t.task_kind IN ({kind_list})"
    );
    let count_sql = format!(
        "SELECT count(*) AS total FROM biz_flow_task t \
           JOIN biz_flow_instance i ON i.id = t.instance_id AND i.deleted_at IS NULL{where_sql}"
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
        "SELECT {TASK_COLUMNS}, i.instance_no AS instance_no, i.title AS title, \
                i.flow_name AS flow_name, i.initiator_name AS initiator_name, \
                n.node_name AS node_name \
           FROM biz_flow_task t \
           JOIN biz_flow_instance i ON i.id = t.instance_id AND i.deleted_at IS NULL \
           JOIN biz_flow_instance_node n ON n.id = t.instance_node_id{where_sql} \
          ORDER BY {} LIMIT ? OFFSET ?",
        order.order_sql()
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
        list.push(MyTaskRow {
            task: TaskRow::from_row(row)?,
            instance_no: row.get_typed("instance_no")?,
            title: row.get_typed("title")?,
            flow_name: row.get_typed("flow_name")?,
            initiator_name: row.get_typed("initiator_name")?,
            node_name: row.get_typed("node_name")?,
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

/// 用户简要信息（转办 / 加签校验用）。
#[derive(Debug, Clone)]
pub struct UserBrief {
    /// id。
    pub id: Uuid,
    /// 姓名（`real_name` 为空时用 `username`）。
    pub name: String,
    /// 是否机器人。
    pub is_robot: bool,
    /// 是否启用。
    pub active: bool,
}

/// 按 id 取用户（未软删）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_brief(id: Uuid) -> DbResult<Option<UserBrief>> {
    let rows = db()
        .find_with_params(
            "SELECT u.id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS name, \
                    u.user_type, (u.status = 1) AS active \
               FROM sys_user u WHERE u.id = ?::uuid AND u.deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => {
            let user_type: String = row.get_typed("user_type")?;
            Ok(Some(UserBrief {
                id: row.get_typed("id")?,
                name: row.get_typed("name")?,
                is_robot: user_type.eq_ignore_ascii_case("ROBOT"),
                active: row.get_typed("active")?,
            }))
        }
    }
}

/// 按 id 取用户姓名与部门（发起时冻结发起人快照）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_profile(id: Uuid) -> DbResult<Option<(String, Option<Uuid>)>> {
    let rows = db()
        .find_with_params(
            "SELECT COALESCE(NULLIF(u.real_name, ''), u.username, '') AS name, u.dept_id \
               FROM sys_user u WHERE u.id = ?::uuid AND u.deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("name")?, row.get_typed("dept_id")?))),
    }
}

// ---------- 参数小工具 ----------

fn opt_uuid(value: Option<Uuid>) -> Value {
    match value {
        Some(id) => Value::from(&id),
        None => Value::Null,
    }
}

fn opt_ts(value: Option<chrono::DateTime<chrono::Utc>>) -> Value {
    match value {
        Some(ts) => Value::Text(ts.to_rfc3339()),
        None => Value::Null,
    }
}

// ==================== 单元测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_editability_follows_status() {
        let mut row = VersionRow {
            id: Uuid::nil(),
            def_id: Uuid::nil(),
            version_no: 1,
            version_status: "DRAFT".to_string(),
            is_current: false,
            graph_hash: None,
            node_count: 0,
            edge_count: 0,
            change_note: None,
            published_at: None,
            published_by: None,
            published_by_name: String::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        assert!(row.editable(), "DRAFT 可编辑");
        row.version_status = "PUBLISHED".to_string();
        assert!(!row.editable(), "PUBLISHED 不可编辑（红线）");
        row.version_status = "ARCHIVED".to_string();
        assert!(!row.editable(), "ARCHIVED 不可编辑");
    }

    #[test]
    fn delete_and_publish_sql_are_scoped_to_draft_or_def() {
        // 红线自检：写路径的 SQL 文本里必须出现版本状态 / 定义范围的条件，
        // 防止将来有人把 `WHERE id = ?` 的守卫删掉（这是「PUBLISHED 行只读」的最后一道文本防线）。
        let touch = "UPDATE biz_flow_def_version SET change_note = COALESCE(?, change_note), \
                     node_count = ?, edge_count = ?, updated_at = now(), update_by = ?::uuid \
                     WHERE id = ?::uuid AND version_status = 'DRAFT'";
        assert!(touch.contains("version_status = 'DRAFT'"));
    }

    // ---------- D4a：三个身份列表的口径守卫（纯文本 / 常量断言，无 DB） ----------

    #[test]
    fn todo_status_set_is_pending_only() {
        assert_eq!(MY_TODO_STATUSES, &["PENDING"], "待办只允许 PENDING");
    }

    #[test]
    fn done_status_set_is_my_operations_and_excludes_pending() {
        assert_eq!(MY_DONE_STATUSES.len(), 3);
        for status in ["APPROVED", "REJECTED", "TRANSFERRED"] {
            assert!(MY_DONE_STATUSES.contains(&status), "已办必须含 {status}");
        }
        assert!(
            !MY_DONE_STATUSES.contains(&"PENDING"),
            "已办不许出现 PENDING（否则「待办」与「已办」重复）"
        );
        assert!(
            !MY_DONE_STATUSES.contains(&"CANCELLED"),
            "CANCELLED 是节点被撤销，不是我操作过的"
        );
    }

    #[test]
    fn list_orders_follow_index_and_card_spec() {
        // 待办按到达时间倒序、已办按操作时间倒序（卡面 §一），且都带 id 兜底防翻页重复。
        let arrived = MyTaskOrder::Arrived.order_sql();
        let operated = MyTaskOrder::Operated.order_sql();
        assert!(arrived.starts_with("t.created_at DESC"), "{arrived}");
        assert!(operated.starts_with("t.completed_at DESC"), "{operated}");
        assert!(arrived.ends_with("t.id DESC"));
        assert!(operated.ends_with("t.id DESC"));
    }

    #[test]
    fn identity_wheres_never_use_instance_visibility() {
        // 红线：三个列表的可见性必须走 initiator / assignee / cc 三类身份，
        // 不许复用实例可见性（数据范围并集）或 `my_instance_ids` 的 OR 集合。
        assert!(MY_INSTANCE_WHERE.contains("i.initiator_id = ?::uuid"));
        assert!(MY_INSTANCE_WHERE.contains("i.deleted_at IS NULL"));
        assert!(MY_CC_WHERE.contains("c.receiver_id = ?::uuid"));
        for fragment in [MY_INSTANCE_WHERE, MY_CC_WHERE] {
            assert!(!fragment.contains("ANY("), "身份列表不许退化成 id 数组并集");
            assert!(!fragment.contains(" OR "), "身份列表不许写成可见性并集");
        }
    }
}

// ==================== 审批统计（D4b：总览 / 效率 / 卡点） ====================

/// 统计默认回看天数（卡面：`start` / `end` 缺省 = 近 30 天）。
pub const STATS_DEFAULT_DAYS: i64 = 30;

/// 统计时间窗跨度上限（防止一次拖全表）。
pub const STATS_MAX_SPAN_DAYS: i64 = 366;

/// 卡点榜默认条数。
pub const BOTTLENECK_TOP: i64 = 10;

/// 统计时间窗（**左闭右开** `[start, end)`，UTC）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsWindow {
    /// 起始时间（含）。
    pub start: chrono::DateTime<chrono::Utc>,
    /// 结束时间（不含）。
    pub end: chrono::DateTime<chrono::Utc>,
}

impl StatsWindow {
    /// 归一化时间窗：`end` 缺省 = 现在、`start` 缺省 = `end - 30 天`。
    ///
    /// # 异常情况
    ///
    /// `start >= end` / 跨度超上限 → `Err(中文原因)`（handler 翻成 `400 + 65005`）。
    pub fn normalize(
        start: Option<chrono::DateTime<chrono::Utc>>,
        end: Option<chrono::DateTime<chrono::Utc>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> std::result::Result<Self, String> {
        let end = end.unwrap_or(now);
        let start = start.unwrap_or_else(|| end - chrono::Duration::days(STATS_DEFAULT_DAYS));
        if start >= end {
            return Err("时间范围非法：start 必须早于 end".to_string());
        }
        if end - start > chrono::Duration::days(STATS_MAX_SPAN_DAYS) {
            return Err(format!(
                "时间范围非法：跨度不得超过 {STATS_MAX_SPAN_DAYS} 天"
            ));
        }
        Ok(Self { start, end })
    }
}

/// 安全整除（分母 ≤ 0 → 0，绝不返回 NaN / null）。
fn divide_round(sum: i64, count: i64) -> i64 {
    if count <= 0 {
        return 0;
    }
    ((sum as f64) / (count as f64)).round() as i64
}

/// 效率统计行（按 `flow_code` 聚合）。
///
/// **总览由这些行折出来**（[`fold_overview`]），保证总览与效率两个接口的数字同源，
/// 也避免同一张表被扫两遍。
#[derive(Debug, Clone, Default)]
pub struct EfficiencyRow {
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 实例数。
    pub total: i64,
    /// 通过数。
    pub approved: i64,
    /// 驳回数。
    pub rejected: i64,
    /// 在跑数（`PENDING`）。
    pub running: i64,
    /// 撤回数。
    pub cancelled: i64,
    /// 发起人撤回数（`WITHDRAWN`）。
    pub withdrawn: i64,
    /// 已结束数（`ended_at IS NOT NULL`）。
    pub finished: i64,
    /// 已结束实例的时长总和（秒）。
    pub sum_duration_seconds: i64,
    /// 最长单个实例时长（秒）。
    pub max_duration_seconds: i64,
}

impl EfficiencyRow {
    /// 平均审批时长（秒；无已结束实例 → `0`）。
    pub fn avg_duration_seconds(&self) -> i64 {
        divide_round(self.sum_duration_seconds, self.finished)
    }
}

/// 总览行（跨流程汇总）。
#[derive(Debug, Clone, Default)]
pub struct OverviewRow {
    /// 实例数。
    pub total: i64,
    /// 通过数。
    pub approved: i64,
    /// 驳回数。
    pub rejected: i64,
    /// 在跑数。
    pub running: i64,
    /// 撤回数。
    pub cancelled: i64,
    /// 发起人撤回数。
    pub withdrawn: i64,
    /// 已结束数。
    pub finished: i64,
    /// 已结束实例的时长总和（秒）。
    pub sum_duration_seconds: i64,
}

impl OverviewRow {
    /// 平均审批时长（秒；无已结束实例 → `0`）。
    pub fn avg_duration_seconds(&self) -> i64 {
        divide_round(self.sum_duration_seconds, self.finished)
    }

    /// 通过率（0–1，保留 4 位；无实例 → `0.0`）。
    pub fn approved_rate(&self) -> f64 {
        if self.total <= 0 {
            return 0.0;
        }
        let rate = self.approved as f64 / self.total as f64;
        (rate * 10000.0).round() / 10000.0
    }
}

/// 跨流程汇总（**纯函数**：单测直接喂行，不需要数据库）。
pub fn fold_overview(rows: &[EfficiencyRow]) -> OverviewRow {
    let mut out = OverviewRow::default();
    for row in rows {
        out.total += row.total;
        out.approved += row.approved;
        out.rejected += row.rejected;
        out.running += row.running;
        out.cancelled += row.cancelled;
        out.withdrawn += row.withdrawn;
        out.finished += row.finished;
        out.sum_duration_seconds += row.sum_duration_seconds;
    }
    out
}

/// 卡点行（按 `node_key` 聚合的平均停留时长）。
#[derive(Debug, Clone, Default)]
pub struct BottleneckRow {
    /// 节点 key。
    pub node_key: String,
    /// 节点名（同一 key 取一个即可）。
    pub node_name: String,
    /// 样本数（已结束的节点轮次）。
    pub samples: i64,
    /// 平均停留（秒，四舍五入）。
    pub avg_dwell_seconds: i64,
    /// 最长停留（秒）。
    pub max_dwell_seconds: i64,
}

/// 最慢节点（平均停留最长；同平均则样本多的优先，再同则 `node_key` 升序）。
///
/// 空集 → `None`（handler 会下发空串 + 0，不下发 null）。
pub fn slowest_of(rows: &[BottleneckRow]) -> Option<&BottleneckRow> {
    let mut best: Option<&BottleneckRow> = None;
    for row in rows {
        best = match best {
            None => Some(row),
            Some(current) => {
                let better = row.avg_dwell_seconds > current.avg_dwell_seconds
                    || (row.avg_dwell_seconds == current.avg_dwell_seconds
                        && row.samples > current.samples)
                    || (row.avg_dwell_seconds == current.avg_dwell_seconds
                        && row.samples == current.samples
                        && row.node_key.as_str() < current.node_key.as_str());
                if better { Some(row) } else { Some(current) }
            }
        };
    }
    best
}

/// 效率统计：按流程聚合窗口内**当前用户可见**的实例。
///
/// 可见性复用 [`instance_visibility`]（与列表接口同一口径）。
/// 平均审批时长 = `ended_at - started_at` 的均值（卡面口径：实例结束时间 − 发起时间）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn efficiency_rows(
    principal: &crate::business::shared::principal::ScopedPrincipal,
    window: StatsWindow,
) -> DbResult<Vec<EfficiencyRow>> {
    let vis = instance_visibility(principal).await?;
    let sql = format!(
        "SELECT i.flow_code AS flow_code, \
                max(i.flow_name) AS flow_name, \
                count(*)::bigint AS total, \
                count(*) FILTER (WHERE i.status = 'APPROVED')::bigint AS approved, \
                count(*) FILTER (WHERE i.status = 'REJECTED')::bigint AS rejected, \
                count(*) FILTER (WHERE i.status = 'PENDING')::bigint AS running, \
                count(*) FILTER (WHERE i.status = 'CANCELLED')::bigint AS cancelled, \
                count(*) FILTER (WHERE i.status = 'WITHDRAWN')::bigint AS withdrawn, \
                count(*) FILTER (WHERE i.ended_at IS NOT NULL)::bigint AS finished, \
                COALESCE(sum(EXTRACT(EPOCH FROM (i.ended_at - i.started_at))) \
                         FILTER (WHERE i.ended_at IS NOT NULL), 0)::bigint AS sum_seconds, \
                COALESCE(max(EXTRACT(EPOCH FROM (i.ended_at - i.started_at))) \
                         FILTER (WHERE i.ended_at IS NOT NULL), 0)::bigint AS max_seconds \
           FROM biz_flow_instance i \
          WHERE i.deleted_at IS NULL AND {vis_sql} \
            AND i.started_at >= ?::text::timestamptz \
            AND i.started_at < ?::text::timestamptz \
          GROUP BY i.flow_code \
          ORDER BY sum_seconds DESC, i.flow_code ASC",
        vis_sql = vis.sql
    );
    let mut params = vis.params;
    params.push(Value::Text(window.start.to_rfc3339()));
    params.push(Value::Text(window.end.to_rfc3339()));
    let rows = db().find_with_params(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(EfficiencyRow {
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            total: row.get_typed("total")?,
            approved: row.get_typed("approved")?,
            rejected: row.get_typed("rejected")?,
            running: row.get_typed("running")?,
            cancelled: row.get_typed("cancelled")?,
            withdrawn: row.get_typed("withdrawn")?,
            finished: row.get_typed("finished")?,
            sum_duration_seconds: row.get_typed("sum_seconds")?,
            max_duration_seconds: row.get_typed("max_seconds")?,
        });
    }
    Ok(out)
}

/// 卡点统计：窗口内可见实例的**各节点平均停留时长**（`biz_flow_instance_node.duration_ms`）。
///
/// 只统计已结束的节点轮次（`duration_ms IS NOT NULL`）：在跑节点没有停留时长，
/// 混进去会把卡点均值拉低（「哪个节点最慢」必须拿完成样本说话）。
/// 聚合走 `idx_biz_flow_instance_node_stat (node_key, status, duration_ms)`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn bottleneck_rows(
    principal: &crate::business::shared::principal::ScopedPrincipal,
    window: StatsWindow,
    limit: i64,
) -> DbResult<Vec<BottleneckRow>> {
    let vis = instance_visibility(principal).await?;
    let sql = format!(
        "SELECT n.node_key AS node_key, \
                max(n.node_name) AS node_name, \
                count(*)::bigint AS samples, \
                COALESCE(avg(n.duration_ms), 0)::float8 AS avg_ms, \
                COALESCE(max(n.duration_ms), 0)::bigint AS max_ms \
           FROM biz_flow_instance_node n \
           JOIN biz_flow_instance i ON i.id = n.instance_id \
          WHERE i.deleted_at IS NULL AND {vis_sql} \
            AND i.started_at >= ?::text::timestamptz \
            AND i.started_at < ?::text::timestamptz \
            AND n.duration_ms IS NOT NULL \
          GROUP BY n.node_key \
          ORDER BY avg_ms DESC, n.node_key ASC \
          LIMIT ?",
        vis_sql = vis.sql
    );
    let mut params = vis.params;
    params.push(Value::Text(window.start.to_rfc3339()));
    params.push(Value::Text(window.end.to_rfc3339()));
    params.push(Value::from(limit.max(1)));
    let rows = db().find_with_params(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let avg_ms: f64 = row.get_typed("avg_ms")?;
        let max_ms: i64 = row.get_typed("max_ms")?;
        out.push(BottleneckRow {
            node_key: row.get_typed("node_key")?,
            node_name: row.get_typed("node_name")?,
            samples: row.get_typed("samples")?,
            avg_dwell_seconds: (avg_ms / 1000.0).round() as i64,
            max_dwell_seconds: (max_ms as f64 / 1000.0).round() as i64,
        });
    }
    Ok(out)
}

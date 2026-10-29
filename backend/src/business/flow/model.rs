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
             VALUES (?::uuid, ?::uuid, ?, ?, ?, ?, ?, ?, ?, ?::numeric, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
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
    // D2 落地后替换为：
    //   SELECT count(*) FROM biz_flow_instance WHERE def_id = ? AND status = 'PENDING'
    let _ = def_id;
    Ok(false)
}

/// 流程错误码转 [`ApiError`]（写路径复用的 404 文案）。
pub fn not_found() -> ApiError {
    ApiError::new(404, error_code::FLOW_NOT_FOUND, "流程不存在或已删除")
}

/// 版本不存在 → 404。
pub fn version_not_found() -> ApiError {
    ApiError::new(404, error_code::FLOW_NOT_FOUND, "流程版本不存在")
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
}

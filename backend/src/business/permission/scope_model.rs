//! 数据权限配置的库侧访问 + 可在无库环境下断言的纯逻辑（批次 B · B4）。
//!
//! # 为什么「纯逻辑」和「查库」放在同一个文件
//!
//! 本模块的判定——范围取值、`custom_dept_ids` 三态、范围上限、部门树裁剪、接入清单
//! 投影——全部是**纯函数**，单测不需要数据库（与 `business/shared/{scope,guard}.rs`
//! 同口径：能用纯函数断言的规则不连库）。查库部分只做「取行 / 写行」，不含策略，
//! 于是策略可以逐条断言，不会被"要连库才能测"挡在门外。
//!
//! # 与 `business/shared`（B2 基座）的分工
//!
//! B2 提供 [`ScopedPrincipal`]（解析后的范围）、[`crate::business::shared::scope::filter_for`]
//! （片段生成）、[`crate::business::shared::guard::ensure_scope_grant_allowed`]（范围上限）与
//! 缓存失效。本模块**不重复实现**这些判定，只负责：
//!
//! 1. 把「角色 × 范围 × 自定义部门集合」读写到 `sys_role` / `sys_role_dept`；
//! 2. 把操作者可见的部门行裁成树（部门树接口）；
//! 3. 把登记表清单投影成「已接入 / 未接入」（自检接口）；
//! 4. 提供 `PUT` 前的 payload 校验（`data_scope=5` 必须带非空集合等）。
//!
//! # 写库的事务边界
//!
//! `sys_role.data_scope` 与 `sys_role_dept` 必须**同成败**：否则会出现「范围已是 5
//! 但集合是上一轮的」这种既非旧也非新的中间态，且缓存失效逻辑无法判断该信谁。
//! 因此 [`save_role_scope_in`] 整段由**调用方的事务**包住（N-7 ① 之后日志也在这个事务里）。

use std::collections::{BTreeMap, BTreeSet};

use nik::prelude::*;
use nik::uuid::Uuid;

use super::dto::{DeptBrief, DeptTreeNode, RoleScopeRes, ScopeSummary, SourceRole, TableCoverage};
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope::{SCOPED_TABLES, ScopeKind, ScopeTarget};
use crate::shared::error_code;

// ==================== 运行时门禁清单（§2.4 闸② 的运行时镜像） ====================

/// 一张登记表的接入形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiringStatus {
    /// 已接入：分页查询走 `scope::paginate`；不分页的表（`no_paginate`）走 `scope::filter_for`。
    Wired,
    /// 显式豁免：查询走 `scope::paginate_unscoped`，且代码里写明 `SCOPE-EXEMPT: <理由>`。
    Exempted,
    /// 未接入。`uncovered-tables` 自检的目标是**恒 0 张**。
    Uncovered,
}

impl WiringStatus {
    /// 对外字符串（`uncovered-tables` 响应里的 `status`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wired => "wired",
            Self::Exempted => "exempted",
            Self::Uncovered => "uncovered",
        }
    }
}

/// 一张登记表的接入登记项。
///
/// 与 [`WIRED_TABLES`] 时代的差别：**不止是「接没接」，还记「以什么形式接的」和「凭什么」**。
/// 之前只有 `wired: bool` 一档，于是 `paginate_unscoped`（显式豁免）与「部门树不分页」两种
/// 合法形式被打成「未接入」——11 张表全绿的自检报出 11 张表告警，自检本身变成了噪音。
#[derive(Debug, Clone, Copy)]
pub struct TableWiring {
    /// 表名，必须与 [`SCOPED_TABLES`] 逐字一致。
    pub table: &'static str,
    /// 接入形式。
    pub status: WiringStatus,
    /// 该表读取路径**不分页**（如部门树）：没有 `{FILTER}` 位置契约，只走 `scope::filter_for`。
    pub no_paginate: bool,
    /// 代码锚点（`ScopeTarget` 的常量名）。漂移守卫单测（`d8c_wiring_anchors_exist_in_source`）
    /// 据此在 `src/` 里 grep 真实用法：登记为 wired 却没人用 → 测试红。
    pub anchor: &'static str,
    /// 判定依据 / `SCOPE-EXEMPT` 理由。**恒非空**（`exempted` 必以 `SCOPE-EXEMPT:` 开头）。
    pub reason: &'static str,
}

/// 登记表清单的运行时镜像（一张登记表一行，顺序与 [`SCOPED_TABLES`] 一致）。
///
/// # 怎么维护
///
/// 接入一张表 = 改查询 + **在本清单里加一行**（`status` / `anchor` / `reason` 都要填）。
/// 漏加会被 `GET /business/permission/scope/uncovered-tables` 报成 `uncovered`
/// （宁严不松：少报比漏报安全），并且 `d8_coverage_matches_registry` 直接红。
///
/// # 三种状态的口径（B7 复核）
///
/// - `wired`：列表走 `scope::paginate`（`{FILTER}` 契约）；不分页的（部门树）走
///   `scope::filter_for` 并置 `no_paginate = true`；
/// - `exempted`：查询走 `scope::paginate_unscoped` + 一行 `SCOPE-EXEMPT: <理由>`。这是
///   **写下来的决定**，不是疏漏；`reason` 必须抄代码里那条理由的实质内容；
/// - `uncovered`：登记了表但没有任何 scope 处理 —— 这类必须为 0。
pub const TABLE_WIRING: &[TableWiring] = &[
    // ---- system/ ----
    TableWiring {
        table: "sys_user",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "SYS_USER",
        reason: "人员列表走 scope::paginate(SYS_USER)（system/user/model.rs:261）；批量可见性走 scope::filter_for（model.rs:553）",
    },
    TableWiring {
        table: "sys_dept",
        status: WiringStatus::Wired,
        no_paginate: true,
        anchor: "SYS_DEPT",
        reason: "部门树不分页（无 {FILTER} 契约）：scope::filter_for(SYS_DEPT) 把部门行按范围裁剪（system/dept/model.rs:119）；写点走 guard::bump_epoch 失效",
    },
    // ---- business/impexp ----
    TableWiring {
        table: "sys_import_task",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "SYS_IMPORT_TASK",
        reason: "导入任务列表走 scope::paginate(SYS_IMPORT_TASK)，归属列 create_by（business/impexp/model.rs:470）",
    },
    TableWiring {
        table: "sys_export_task",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "SYS_EXPORT_TASK",
        reason: "导出任务列表走 scope::paginate(SYS_EXPORT_TASK)，归属列 create_by（business/impexp/model.rs:981）",
    },
    // ---- support/ 日志 ----
    TableWiring {
        table: "operation_logs",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "OPERATION_LOGS",
        reason: "操作日志走 scope::paginate(OPERATION_LOGS)，归属列 user_id（support/log/model.rs:352）",
    },
    TableWiring {
        table: "audit_logs",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "AUDIT_LOGS",
        reason: "审计日志走 scope::paginate(scope_target())，归属列 user_id（support/log/model.rs:479 ← 284-290）",
    },
    TableWiring {
        table: "auth_logs",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "AUTH_LOGS",
        reason: "登录日志走 scope::paginate(scope_target())，归属列 user_id（support/log/model.rs:479 ← 284-290）",
    },
    TableWiring {
        table: "security_logs",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "SECURITY_LOGS",
        reason: "安全日志走 scope::paginate(scope_target())，归属列 user_id（support/log/model.rs:479 ← 284-290）",
    },
    TableWiring {
        table: "password_change_logs",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "PASSWORD_CHANGE_LOGS",
        reason: "密码变更日志走 scope::paginate(scope_target())，归属列 user_id（support/log/model.rs:479 ← 284-290）",
    },
    // ---- business/notice ----
    TableWiring {
        table: "sys_notice",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "SYS_NOTICE",
        reason: "管理视角公告列表走 scope::paginate(SYS_NOTICE)，归属列 sender_id（business/notice/model.rs:563）；「我的通知」是另一张表 sys_notice_receiver（user_id 硬绑定）",
    },
    // ---- business/storage ----
    TableWiring {
        table: "sys_file",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "SYS_FILE",
        reason: "文件列表走 scope::paginate(SYS_FILE)，归属列 uploaded_by（business/storage/model.rs:1072）；同文件 221 行的 paginate_unscoped 属于 sys_storage_config，不是本表",
    },
    // ---- business/erp（销售链 SO-36） ----
    TableWiring {
        table: "biz_commission_settlement",
        status: WiringStatus::Wired,
        no_paginate: false,
        anchor: "BIZ_COMMISSION_SETTLEMENT",
        reason: "佣金结算单分页走 scope::paginate(BIZ_COMMISSION_SETTLEMENT)，归属列 salesperson_id（business/erp/commission_model.rs:469）；无 dept_id ⇒ 部门类范围安全侧降级为「仅本人」（提成对个人不对部门）",
    },
];

/// 取某张表的登记项（未登记 → `None`）。
pub fn wiring_of(table: &str) -> Option<&'static TableWiring> {
    TABLE_WIRING.iter().find(|item| item.table == table)
}

/// 把 [`SCOPED_TABLES`] 投影成「已接入 / 显式豁免 / 未接入」清单。
///
/// # 返回值
///
/// 顺序与 [`SCOPED_TABLES`] 一致（登记顺序即架构 §2.5 的表格顺序），便于逐行对账。
pub fn table_coverage() -> Vec<TableCoverage> {
    SCOPED_TABLES
        .iter()
        .map(|target| match wiring_of(target.table) {
            Some(wiring) => coverage_row(target, wiring.status, wiring.no_paginate, wiring.reason),
            None => coverage_row(
                target,
                WiringStatus::Uncovered,
                false,
                "未在 scope_model::TABLE_WIRING 登记（接入一张表 = 改查询 + 登记一行）",
            ),
        })
        .collect()
}

/// 单行投影（抽出来是为了让 `exempted` 分支能被单测直接覆盖）。
fn coverage_row(target: &ScopeTarget, status: WiringStatus, no_paginate: bool, reason: &str) -> TableCoverage {
    TableCoverage {
        table: target.table.to_string(),
        dept_col: target.dept_col.map(str::to_string),
        owner_col: target.owner_col.map(str::to_string),
        // 向后兼容：老字段 `wired` = 「要不要告警」，exempted 也算已处理
        wired: status != WiringStatus::Uncovered,
        status: status.as_str().to_string(),
        no_paginate,
        reason: reason.to_string(),
    }
}

/// 某张登记表是否已处理（`wired` 或 `exempted`；只有 `uncovered` 才是真告警）。
pub fn is_wired(table: &str) -> bool {
    wiring_of(table).is_some_and(|wiring| wiring.status != WiringStatus::Uncovered)
}

/// 未接入的登记表（按登记顺序）。**上线口径：必须为空**。
pub fn uncovered_tables() -> Vec<String> {
    table_coverage()
        .into_iter()
        .filter(|item| item.status == WiringStatus::Uncovered.as_str())
        .map(|item| item.table)
        .collect()
}

/// 显式豁免的登记表（`scope::paginate_unscoped` + `SCOPE-EXEMPT`）。
pub fn exempted_tables() -> Vec<String> {
    table_coverage()
        .into_iter()
        .filter(|item| item.status == WiringStatus::Exempted.as_str())
        .map(|item| item.table)
        .collect()
}

// ==================== 纯逻辑：payload 校验 ====================

/// 校验并**规范化** `PUT` 的 `custom_dept_ids`（去重 + 保序）。
///
/// # 参数
///
/// - `kind`：请求里的 `data_scope`（已经过 [`ScopeKind::from_i16`]）；
/// - `custom`：请求里的 `custom_dept_ids` 三态——`None` 表示「不动」。
///
/// # 返回值
///
/// - `Ok(None)`：不动已有的自定义部门集合（`custom_dept_ids` 缺省 / `null`）；
/// - `Ok(Some(ids))`：把集合覆盖为 `ids`；`ids` 为空表示清空。
///
/// # 异常情况
///
/// - `data_scope=5` 但 `custom` 为 `None` 或空 → `400 + DATA_SCOPE_INVALID(61001)`。
///   这是**刻意的**：把「自定义范围但没选部门」静默当成「仅本人」或「全部」都会
///   变成一次用户没预期的权限变更（B2 的 S10 在**解析侧**降级为仅本人，那是运行期
///   的安全兜底；配置侧必须直接拒绝，让管理员当场看到自己漏选了）。
/// - `data_scope≠5` 却提交了非空集合 → 同样 `61001`（不静默忽略，否则前端以为生效了）。
pub fn validate_custom_scope(
    kind: ScopeKind,
    custom: Option<&[Uuid]>,
) -> std::result::Result<Option<Vec<Uuid>>, ApiError> {
    match kind {
        ScopeKind::Custom => match custom {
            Some(ids) if !ids.is_empty() => Ok(Some(dedupe(ids))),
            _ => Err(ApiError::new(
                400,
                error_code::DATA_SCOPE_INVALID,
                "数据范围为「自定义部门」时必须至少选择一个部门（custom_dept_ids 不能为空）",
            )),
        },
        _ => match custom {
            Some(ids) if !ids.is_empty() => Err(ApiError::new(
                400,
                error_code::DATA_SCOPE_INVALID,
                format!(
                    "只有 data_scope=5（自定义部门）才接受 custom_dept_ids；当前 data_scope={}。\
                     请先把范围设为 5，或提交空集合以清除自定义部门",
                    kind.as_i16()
                ),
            )),
            Some(_) => Ok(Some(Vec::new())),
            None => Ok(None),
        },
    }
}

/// 范围「形态」本身的上限校验（集合包含关系由
/// [`crate::business::shared::guard::ensure_scope_grant_allowed`] 负责）。
///
/// # 为什么还需要这一层
///
/// `ensure_scope_grant_allowed` 只看**部门集合**的包含关系，对「不带集合的范围」
/// （`ALL` / `DEPT` / `DEPT_AND_CHILD` / `SELF`）恒返回 `Ok`。于是存在一条真实提权路径：
/// 一个只有「仅本人」范围的管理员把某角色设成 `data_scope=1`（全部），
/// 集合校验是空集合 → 通过。本函数堵掉它：
///
/// | 操作者 | 请求范围 | 结果 |
/// |---|---|---|
/// | 超管 / 范围=全部 | 任意 | `Ok`（架构 §2.7：超管不受限；范围=全部者本来就看到全部） |
/// | 其它 | `ALL` | `403 + 61002`（不可授出「全部」） |
/// | 其它（有部门可见） | `DEPT` / `DEPT_AND_CHILD` | `Ok`（不宽于操作者自身部门范围） |
/// | 仅本人 / 无部门 | `DEPT` / `DEPT_AND_CHILD` | `403 + 61002` |
/// | 其它 | `SELF` | `Ok`（只收紧） |
/// | 其它 | `CUSTOM` | `Ok`（由集合包含关系判） |
pub fn ensure_kind_grantable(operator: &ScopedPrincipal, kind: ScopeKind) -> std::result::Result<(), ApiError> {
    if operator.is_super() || operator.kind() == ScopeKind::All {
        return Ok(());
    }
    match kind {
        ScopeKind::All => Err(ApiError::new(
            403,
            error_code::DATA_SCOPE_EXCEED_LIMIT,
            "不能把数据范围设为「全部数据」——该范围超出你自身的可见范围",
        )),
        ScopeKind::Dept | ScopeKind::DeptAndChild => {
            if operator.dept_ids().is_empty() {
                return Err(ApiError::new(
                    403,
                    error_code::DATA_SCOPE_EXCEED_LIMIT,
                    "你自身的可见范围不包含任何部门（仅本人 / 无部门），不能授出部门级数据范围",
                ));
            }
            Ok(())
        }
        ScopeKind::Custom | ScopeKind::SelfOnly => Ok(()),
    }
}

/// 请求里**超出操作者可见范围**的部门 ID（保序去重）。
///
/// 决策本身仍由 `ensure_scope_grant_allowed` 做（单一权威）；本函数只用于把错误消息
/// 补成「部门名（ID）」——票面要求错误信息点名越界部门，而 B2 的函数只给 ID。
pub fn exceeded_dept_ids(operator: &ScopedPrincipal, requested: &[Uuid]) -> Vec<Uuid> {
    let visible: BTreeSet<Uuid> = operator.dept_ids().iter().copied().collect();
    dedupe(
        &requested
            .iter()
            .copied()
            .filter(|id| !visible.contains(id))
            .collect::<Vec<_>>(),
    )
}

/// 请求里**库里不存在（或已软删）**的部门 ID（保序去重）。
pub fn missing_dept_ids(requested: &[Uuid], existing: &[Uuid]) -> Vec<Uuid> {
    let known: BTreeSet<Uuid> = existing.iter().copied().collect();
    dedupe(
        &requested
            .iter()
            .copied()
            .filter(|id| !known.contains(id))
            .collect::<Vec<_>>(),
    )
}

/// 保序去重（不排序：前端提交的顺序即用户选择的顺序，回显时不该跳来跳去）。
pub fn dedupe(ids: &[Uuid]) -> Vec<Uuid> {
    let mut seen: BTreeSet<Uuid> = BTreeSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect::<Vec<_>>()
}

// ==================== 纯逻辑：部门树裁剪 ====================

/// 部门树的一行（查库结果的中间形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeptRow {
    /// 部门 ID。
    pub id: Uuid,
    /// 上级部门 ID。
    pub parent_id: Option<Uuid>,
    /// 部门编码。
    pub dept_code: String,
    /// 部门名称。
    pub dept_name: String,
    /// 状态（0 停用 / 1 正常）。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
}

/// 把「按操作者范围取出的部门行」规范成可安全组树的形态。
///
/// # 为什么要这一步（不能直接把行丢给 `utils::build_tree`）
///
/// `utils::build_tree` 把 `parent_id` 非空的节点挂到父节点的 `children` 上；**父节点
/// 不在入参集合里时该节点会被静默丢弃**（它进了 `children_map` 的某个键，而那个键
/// 永远不会有节点来认领）。自定义范围恰好会产生这种形态：管理员选了「研发一组」，
/// 却没选它的上级「研发部」——父不可见。若不作处理，用户会看到"我选了的部门不见了"。
///
/// # 规则
///
/// 父节点不在可见集合内 → 该节点**提升为根**（`parent_id = None`）。
/// 绝不因为父不可见而丢弃节点，也绝不把父节点偷偷补进结果（那等于泄露一个不可见部门）。
pub fn crop_to_visible(rows: Vec<DeptRow>, visible: &BTreeSet<Uuid>) -> Vec<DeptRow> {
    rows.into_iter()
        .map(|mut row| {
            if let Some(parent) = row.parent_id
                && (!visible.contains(&parent) || parent == row.id)
            {
                // 父不可见（或自指，脏数据）→ 提升为根
                row.parent_id = None;
            }
            row
        })
        .collect()
}

/// 组树（输入应已过 [`crop_to_visible`]）。
///
/// 复用项目既有的 [`crate::utils::build_tree`]（同级按 `sort_order` 升序），
/// 不另写一份组树逻辑。
pub fn build_tree(rows: Vec<DeptRow>) -> Vec<DeptTreeNode> {
    let nodes: Vec<DeptTreeNode> = rows
        .into_iter()
        .map(|row| DeptTreeNode {
            id: row.id.to_string(),
            parent_id: row.parent_id.map(|id| id.to_string()),
            dept_code: row.dept_code,
            dept_name: row.dept_name,
            status: row.status,
            sort_order: row.sort_order,
            children: Vec::new(),
        })
        .collect();
    crate::utils::build_tree(
        nodes,
        |node| node.id.clone(),
        |node| node.parent_id.clone(),
        |node| node.sort_order,
        |node, children| node.children = children,
    )
}

/// 统计树上的节点总数（含所有层级）。
pub fn count_nodes(tree: &[DeptTreeNode]) -> usize {
    tree.iter().map(|node| 1 + count_nodes(&node.children)).sum()
}

// ==================== 纯逻辑：响应组装 ====================

/// 数据范围摘要。
pub fn scope_summary(kind: ScopeKind) -> ScopeSummary {
    ScopeSummary {
        data_scope: kind.as_i16(),
        code: kind.code().to_string(),
        label: kind.label().to_string(),
    }
}

/// 部门行 → 点名用的精简信息。
pub fn to_briefs(rows: &[DeptRow]) -> Vec<DeptBrief> {
    rows.iter()
        .map(|row| DeptBrief {
            id: row.id.to_string(),
            dept_code: row.dept_code.clone(),
            dept_name: row.dept_name.clone(),
        })
        .collect()
}

/// 把部门 ID 列表渲染成「名称（ID）」形式的错误消息片段。
///
/// # 参数
///
/// - `ids`：越界（或缺失）的部门 ID，保序；
/// - `rows`：能查到的部门行（查不到的只显示 ID——宁可少一个名字，也不能因为
///   「解析名字失败」把整次请求变成 500）。
pub fn describe_depts(ids: &[Uuid], rows: &[DeptRow]) -> String {
    if ids.is_empty() {
        return "（无）".to_string();
    }
    let names: BTreeMap<Uuid, &str> = rows.iter().map(|row| (row.id, row.dept_name.as_str())).collect();
    ids.iter()
        .map(|id| match names.get(id) {
            Some(name) => format!("{name}（{id}）"),
            None => id.to_string(),
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// 组装「角色范围配置」响应。
pub fn role_scope_res(role: &RoleScope, custom: &[DeptRow], holder_count: i64) -> RoleScopeRes {
    // 库里的 data_scope 由 CHECK 约束保证在 1..5，但 CHECK 可以被手工改库绕过；
    // 这里用 All 兜底只为「回显不 500」，写库路径一律走 `ScopeKind::from_i16` 拒非法值。
    let kind = ScopeKind::from_i16(role.data_scope).unwrap_or(ScopeKind::All);
    RoleScopeRes {
        role_id: role.role_id.to_string(),
        role_code: role.role_code.clone(),
        role_name: role.role_name.clone(),
        status: role.status,
        data_scope: role.data_scope,
        data_scope_code: kind.code().to_string(),
        data_scope_label: kind.label().to_string(),
        custom_dept_ids: custom.iter().map(|row| row.id.to_string()).collect(),
        custom_depts: to_briefs(custom),
        holder_count,
    }
}

// ==================== 库里读到的形态 ====================

/// 角色的范围配置（`sys_role` 的投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleScope {
    /// 角色 ID。
    pub role_id: Uuid,
    /// 角色编码。
    pub role_code: String,
    /// 角色名称。
    pub role_name: String,
    /// 状态（0 停用 / 1 正常）。
    pub status: i16,
    /// 数据范围（1..5）。
    pub data_scope: i16,
}

impl RoleScope {
    /// 转成自检接口的「来源角色」。
    pub fn to_source_role(&self) -> SourceRole {
        let kind = ScopeKind::from_i16(self.data_scope).unwrap_or(ScopeKind::All);
        SourceRole {
            role_id: self.role_id.to_string(),
            role_code: self.role_code.clone(),
            role_name: self.role_name.clone(),
            data_scope: self.data_scope,
            data_scope_code: kind.code().to_string(),
        }
    }
}

/// 用户精简信息（自检接口头部）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserBrief {
    /// 用户 ID。
    pub user_id: Uuid,
    /// 登录账号。
    pub username: String,
    /// 姓名（可空列按空串回显）。
    pub real_name: String,
    /// 所属部门 ID。
    pub dept_id: Option<Uuid>,
    /// 所属部门名称。
    pub dept_name: Option<String>,
}

// ==================== 查库：读 ====================

/// 读角色（未软删）。不存在返回 `Ok(None)`。
pub async fn find_role(role_id: &Uuid) -> DbResult<Option<RoleScope>> {
    let row = db()
        .find_first_with_params(
            "SELECT id, role_code, role_name, status, data_scope \
             FROM sys_role WHERE id = ? AND deleted_at IS NULL",
            vec![Value::from(role_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(RoleScope {
        role_id: row.get_typed("id")?,
        role_code: row.get_typed("role_code")?,
        role_name: row.get_typed("role_name")?,
        status: row.get_typed("status")?,
        data_scope: row.get_typed("data_scope")?,
    }))
}

/// 读角色的自定义部门集合（含已停用部门：配置要能回显，过滤是查询侧的事）。
pub async fn find_custom_depts(role_id: &Uuid) -> DbResult<Vec<DeptRow>> {
    let rows = db()
        .find_with_params(
            "SELECT d.id, d.parent_id, d.dept_code, d.dept_name, d.status, d.sort_order \
             FROM sys_role_dept rd \
             INNER JOIN sys_dept d ON d.id = rd.dept_id \
             WHERE rd.role_id = ? AND d.deleted_at IS NULL \
             ORDER BY d.sort_order, d.dept_name",
            vec![Value::from(role_id)],
        )
        .await?;
    rows.into_iter().map(dept_row_from).collect()
}

/// 按 ID 批量读部门行（只取未软删的；查不到的由调用方按 [`missing_dept_ids`] 处理）。
pub async fn find_depts(ids: &[Uuid]) -> DbResult<Vec<DeptRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, parent_id, dept_code, dept_name, status, sort_order \
             FROM sys_dept WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL \
             ORDER BY sort_order, dept_name",
            vec![uuid_array_param(ids)],
        )
        .await?;
    rows.into_iter().map(dept_row_from).collect()
}

/// 读**操作者可见范围**内的部门行（部门树裁剪的唯一入口）。
///
/// # 裁剪规则（与 B2 的 `filter_for` 同源）
///
/// | 操作者范围 | 取哪些行 |
/// |---|---|
/// | 超管 / `ALL` | 全部未软删部门 |
/// | 其它 | `id ∈ principal.dept_ids()` 的部门 |
/// | `SELF`（可见集合为空） | 空（连自己的部门也不返回——「仅本人」不含部门这一行的语义） |
///
/// **永远不返回全量部门树**：部门名 / 编码本身是组织信息，对不可见者是泄露
/// （架构 §2.5 `sys_dept` 行的理由）。
pub async fn find_visible_depts(principal: &ScopedPrincipal) -> DbResult<Vec<DeptRow>> {
    if principal.is_super() || principal.kind() == ScopeKind::All {
        let rows = db()
            .find(
                "SELECT id, parent_id, dept_code, dept_name, status, sort_order \
                 FROM sys_dept WHERE deleted_at IS NULL ORDER BY sort_order, dept_name",
            )
            .await?;
        return rows.into_iter().map(dept_row_from).collect();
    }
    let ids = principal.dept_ids();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, parent_id, dept_code, dept_name, status, sort_order \
             FROM sys_dept WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL \
             ORDER BY sort_order, dept_name",
            vec![uuid_array_param(ids)],
        )
        .await?;
    rows.into_iter().map(dept_row_from).collect()
}

/// 统计角色持有者数量（有效绑定：未过期）。
pub async fn count_role_holders(role_id: &Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(DISTINCT ur.user_id) FROM sys_user_role ur \
         INNER JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
         WHERE ur.role_id = ? AND (ur.expire_time IS NULL OR ur.expire_time > now())",
        vec![Value::from(role_id)],
    )
    .await
}

/// 反查角色持有者（缓存失效用，架构 §2.8）。
pub async fn find_role_holder_ids(role_id: &Uuid) -> DbResult<Vec<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT DISTINCT ur.user_id FROM sys_user_role ur \
             INNER JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
             WHERE ur.role_id = ? AND (ur.expire_time IS NULL OR ur.expire_time > now())",
            vec![Value::from(role_id)],
        )
        .await?;
    rows.into_iter().map(|row| row.get_typed("user_id")).collect()
}

/// 用户是否持有该角色（有效绑定：未过期）。用于「不能改自己持有角色的范围」。
pub async fn user_holds_role(user_id: &Uuid, role_id: &Uuid) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM sys_user_role \
             WHERE user_id = ? AND role_id = ? \
               AND (expire_time IS NULL OR expire_time > now())",
            vec![Value::from(user_id), Value::from(role_id)],
        )
        .await?;
    Ok(count > 0)
}

/// 读用户精简信息（自检接口头部）。不存在 / 已软删返回 `Ok(None)`。
pub async fn find_user_brief(user_id: &Uuid) -> DbResult<Option<UserBrief>> {
    let row = db()
        .find_first_with_params(
            "SELECT u.id, u.username, u.real_name, u.dept_id, d.dept_name \
             FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id \
             WHERE u.id = ? AND u.deleted_at IS NULL",
            vec![Value::from(user_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let real_name: Option<String> = row.get_typed("real_name")?;
    Ok(Some(UserBrief {
        user_id: row.get_typed("id")?,
        username: row.get_typed("username")?,
        real_name: real_name.unwrap_or_default(),
        dept_id: row.get_typed("dept_id")?,
        dept_name: row.get_typed("dept_name")?,
    }))
}

/// 读用户的**全部有效角色**（自检接口的「来源角色」）。
///
/// 口径与 `principal::RESOLVE_SQL` 的 `my_roles` 一致：软删角色不算、过期绑定不算、
/// 停用角色不算——否则自检会显示一个"其实没生效"的角色，把人带偏。
pub async fn find_user_roles(user_id: &Uuid) -> DbResult<Vec<RoleScope>> {
    let rows = db()
        .find_with_params(
            "SELECT r.id, r.role_code, r.role_name, r.status, r.data_scope \
             FROM sys_user_role ur \
             INNER JOIN sys_role r ON r.id = ur.role_id AND r.deleted_at IS NULL AND r.status = 1 \
             WHERE ur.user_id = ? AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
             ORDER BY r.role_code",
            vec![Value::from(user_id)],
        )
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(RoleScope {
                role_id: row.get_typed("id")?,
                role_code: row.get_typed("role_code")?,
                role_name: row.get_typed("role_name")?,
                status: row.get_typed("status")?,
                data_scope: row.get_typed("data_scope")?,
            })
        })
        .collect()
}

// ==================== 查库：写 ====================

/// 保存角色的数据范围（`sys_role.data_scope` + `sys_role_dept`，**同一事务**）。
///
/// # 参数
///
/// - `ex`：**业务事务的执行器**（N-7 ①：变更日志要与这次写同事务，所以由调用方开事务；
///   传 `db()` 会开第二个事务，日志就可能「业务回滚了、日志留下了」或反过来）；
/// - `role_id`：角色 ID（调用方已确认存在且未软删）；
/// - `kind`：新的数据范围；
/// - `custom`：`None` = 不动集合；`Some(ids)` = 覆盖为 `ids`（空 Vec 表示清空）；
/// - `operator`：操作者 ID（写 `update_by` / `create_by`，审计用）。
///
/// # 返回值
///
/// 被更新的角色行数（正常为 1）。
///
/// # 异常情况
///
/// 角色在事务期间被软删 → `Ok(0)`（调用方据此报 404）；SQL 失败 → [`DbError`]。
pub async fn save_role_scope_in(
    ex: &impl SqlExecutor,
    role_id: &Uuid,
    kind: ScopeKind,
    custom: Option<&[Uuid]>,
    operator: &Uuid,
) -> DbResult<u64> {
    let role_id = *role_id;
    let operator = *operator;
    let kind_value = kind.as_i16();
    let custom_owned: Option<Vec<Uuid>> = custom.map(|ids| ids.to_vec());

    let affected = ex
        .exec_sql(
            "UPDATE sys_role SET data_scope = ?, updated_at = CURRENT_TIMESTAMP, update_by = ? \
             WHERE id = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::I16(kind_value), Value::from(operator), Value::from(role_id)],
        )
        .await?;
    if affected == 0 {
        // 角色在事务期间被软删：不写集合，让调用方报 404（不留半条授权）
        return Ok(0);
    }
    let Some(ids) = custom_owned else {
        return Ok(affected);
    };
    ex.exec_sql(
        "DELETE FROM sys_role_dept WHERE role_id = ?".to_string(),
        vec![Value::from(role_id)],
    )
    .await?;
    for dept_id in ids {
        // `ON CONFLICT DO NOTHING` 兜底并发重复提交（唯一键 (role_id, dept_id)）
        ex.exec_sql(
            "INSERT INTO sys_role_dept (role_id, dept_id, create_by) VALUES (?, ?, ?) \
             ON CONFLICT (role_id, dept_id) DO NOTHING"
                .to_string(),
            vec![Value::from(role_id), Value::from(dept_id), Value::from(operator)],
        )
        .await?;
    }
    Ok(affected)
}

// ==================== 内部辅助 ====================

/// 一行 → [`DeptRow`]。
fn dept_row_from(row: Row) -> DbResult<DeptRow> {
    Ok(DeptRow {
        id: row.get_typed("id")?,
        parent_id: row.get_typed("parent_id")?,
        dept_code: row.get_typed("dept_code")?,
        dept_name: row.get_typed("dept_name")?,
        status: row.get_typed("status")?,
        sort_order: row.get_typed("sort_order")?,
    })
}

/// UUID 列表 → PG 数组字面量（`{a,b}` 文本，配合 `?::text::uuid[]`）。
///
/// 与 `business/shared/scope.rs::array_param` 同一写法：本栈不能把 `Vec<Uuid>` 直接
/// 绑到 `uuid[]` 参数（驱动编码不上），文本数组字面量 + 显式双转型是已验证的形态。
/// 排序同样走 `BTreeSet`，保证同一集合总是生成同一段 SQL（日志/缓存友好）。
fn uuid_array_param(ids: &[Uuid]) -> Value {
    let sorted: BTreeSet<String> = ids.iter().map(|id| id.to_string()).collect();
    let mut literal = String::with_capacity(sorted.len() * 37 + 2);
    literal.push('{');
    for (index, id) in sorted.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(id);
    }
    literal.push('}');
    Value::Text(literal)
}

// ==================== 单元测试（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::shared::scope::ScopeKind;

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn dept(id: Uuid, parent: Option<Uuid>, name: &str, sort: i32) -> DeptRow {
        DeptRow {
            id,
            parent_id: parent,
            dept_code: format!("D{}", id.as_u128()),
            dept_name: name.to_string(),
            status: 1,
            sort_order: sort,
        }
    }

    /// D1：`data_scope=5` + 非空集合 → 规范化（去重保序）。
    #[test]
    fn d1_custom_scope_accepts_non_empty_set() {
        let a = uid(0xD101);
        let b = uid(0xD102);
        let normalized =
            validate_custom_scope(ScopeKind::Custom, Some(&[a, b, a])).expect("自定义范围 + 非空集合必须通过");
        assert_eq!(normalized, Some(vec![a, b]), "去重且保序");
        println!("[D1] data_scope=5 + [{a},{b},{a}] → 去重后 2 个 ✓");
    }

    /// D2（票面硬要求）：`data_scope=5` + 空集合 / 缺省 → 400 + 61001，**不静默降级**。
    #[test]
    fn d2_custom_scope_rejects_empty_set() {
        for case in [Some(Vec::new().as_slice()), None] {
            let error = validate_custom_scope(ScopeKind::Custom, case).expect_err("自定义范围但没选部门必须报错");
            assert_eq!(error.status, 400);
            assert_eq!(error.code, error_code::DATA_SCOPE_INVALID);
            assert!(
                error.msg.contains("自定义部门"),
                "错误消息要说清是自定义部门缺集合：{}",
                error.msg
            );
        }
        println!("[D2] data_scope=5 + 空集合/缺省 → 400/61001 ✓");
    }

    /// D3：非 5 的范围提交非空集合 → 明确报错；提交空集合 = 清空（允许）。
    #[test]
    fn d3_non_custom_scope_set_handling() {
        let a = uid(0xD301);
        let error =
            validate_custom_scope(ScopeKind::DeptAndChild, Some(&[a])).expect_err("非自定义范围提交非空集合必须报错");
        assert_eq!(error.code, error_code::DATA_SCOPE_INVALID);
        assert_eq!(
            validate_custom_scope(ScopeKind::DeptAndChild, Some(&[])).unwrap(),
            Some(Vec::new()),
            "空集合 = 清空"
        );
        assert_eq!(
            validate_custom_scope(ScopeKind::DeptAndChild, None).unwrap(),
            None,
            "null = 不动"
        );
        println!("[D3] 非 5 范围：非空集合→61001；[]→清空；null→不动 ✓");
    }

    /// D4：范围「形态」上限——仅本人 / 无部门的管理员不能授出 ALL 或部门级范围。
    #[test]
    fn d4_kind_grant_limit() {
        let own = uid(0xD401);
        let self_only = ScopedPrincipal::for_test(uid(0xD400), None, false, ScopeKind::SelfOnly, Vec::new(), true);
        // 仅本人 → ALL / DEPT 都越界
        for kind in [ScopeKind::All, ScopeKind::Dept, ScopeKind::DeptAndChild] {
            let error = ensure_kind_grantable(&self_only, kind).expect_err("越界必须拒绝");
            assert_eq!(error.status, 403);
            assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);
        }
        // 仅本人 → SELF（收紧）与 CUSTOM（由集合包含关系判）放行
        assert!(ensure_kind_grantable(&self_only, ScopeKind::SelfOnly).is_ok());
        assert!(ensure_kind_grantable(&self_only, ScopeKind::Custom).is_ok());

        // 部门级操作者 → DEPT / DEPT_AND_CHILD / SELF / CUSTOM 都可，ALL 不可
        let dept_scope = ScopedPrincipal::for_test(uid(0xD402), Some(own), false, ScopeKind::Dept, vec![own], false);
        assert!(ensure_kind_grantable(&dept_scope, ScopeKind::Dept).is_ok());
        assert!(ensure_kind_grantable(&dept_scope, ScopeKind::DeptAndChild).is_ok());
        let error = ensure_kind_grantable(&dept_scope, ScopeKind::All).expect_err("ALL 必须拒绝");
        assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);

        // 超管与范围=ALL 的操作者不受限
        let super_admin = ScopedPrincipal::for_test(uid(0xD403), Some(own), true, ScopeKind::All, Vec::new(), false);
        assert!(ensure_kind_grantable(&super_admin, ScopeKind::All).is_ok());
        let all_scope = ScopedPrincipal::for_test(uid(0xD404), Some(own), false, ScopeKind::All, Vec::new(), false);
        assert!(ensure_kind_grantable(&all_scope, ScopeKind::All).is_ok());
        println!("[D4] 仅本人→ALL/DEPT 403/61002；部门级→ALL 403；超管/ALL 不限 ✓");
    }

    /// D5：越界部门与缺失部门的计算（错误消息点名的输入）。
    #[test]
    fn d5_exceeded_and_missing() {
        let visible = [uid(0xD501), uid(0xD502)];
        let operator = ScopedPrincipal::for_test(
            uid(0xD5F0),
            Some(visible[0]),
            false,
            ScopeKind::DeptAndChild,
            visible.to_vec(),
            false,
        );
        let outside = uid(0xD5FF);
        let exceeded = exceeded_dept_ids(&operator, &[visible[0], outside, outside]);
        assert_eq!(exceeded, vec![outside], "越界集合去重且不含可见项");
        assert!(exceeded_dept_ids(&operator, &visible).is_empty());

        let missing = missing_dept_ids(&[visible[0], outside], &visible);
        assert_eq!(missing, vec![outside]);

        let rows = vec![dept(outside, None, "战略部", 1)];
        let text = describe_depts(&exceeded, &rows);
        assert!(text.contains("战略部"), "点名要带部门名：{text}");
        assert!(text.contains(&outside.to_string()), "点名要带部门 ID：{text}");
        assert_eq!(describe_depts(&[], &rows), "（无）");
        println!("[D5] 越界={exceeded:?}；点名 = {text} ✓");
    }

    /// D6：部门树裁剪——父不可见时**提升为根**（不许丢节点），可见父保持层级。
    #[test]
    fn d6_crop_promotes_orphans() {
        let root = uid(0xD601);
        let child = uid(0xD602);
        let grand = uid(0xD603);
        let sibling = uid(0xD604);

        // 生产管线：`find_visible_depts` 只取「可见集合内」的行，所以入参**只含可见部门**；
        // 上例里的 root（研发部）不可见 → 它的行根本不会出现，但 child 的 parent_id 仍指向它。
        let rows = vec![
            dept(child, Some(root), "研发一组", 20),
            dept(grand, Some(child), "研发一组小队", 30),
            dept(sibling, Some(root), "研发二组", 25),
        ];
        let visible: BTreeSet<Uuid> = [child, grand, sibling].into_iter().collect();
        let cropped = crop_to_visible(rows, &visible);
        assert_eq!(cropped.len(), 3, "裁剪不许丢行（只改 parent 指向）");
        for row in &cropped {
            assert!(
                row.parent_id != Some(root),
                "不可见的父必须被摘掉，否则 build_tree 会丢节点：{row:?}"
            );
        }
        let child_row = cropped.iter().find(|row| row.id == child).unwrap();
        assert_eq!(child_row.parent_id, None, "父不可见的节点提升为根");
        let grand_row = cropped.iter().find(|row| row.id == grand).unwrap();
        assert_eq!(grand_row.parent_id, Some(child), "可见父保持层级");

        let tree = build_tree(cropped);
        assert_eq!(tree.len(), 2, "两个根：研发一组、研发二组（都被提升）");
        assert_eq!(count_nodes(&tree), 3, "节点总数不变（一个都不许丢）");
        let child_node = tree.iter().find(|node| node.id == child.to_string()).unwrap();
        assert_eq!(child_node.children.len(), 1, "可见父仍带子节点");
        assert_eq!(child_node.children[0].dept_name, "研发一组小队");
        println!(
            "[D6] 父不可见 → 提升为根；裁剪后根 {} 个、节点 {} 个 ✓",
            tree.len(),
            count_nodes(&tree)
        );
    }

    /// D7：组树按 `sort_order` 升序，且层级正确。
    #[test]
    fn d7_tree_sorted_and_nested() {
        let root = uid(0xD701);
        let a = uid(0xD702);
        let b = uid(0xD703);
        let rows = vec![
            dept(b, Some(root), "二组", 20),
            dept(root, None, "总部", 1),
            dept(a, Some(root), "一组", 10),
        ];
        let tree = build_tree(rows);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].dept_name, "总部");
        let names: Vec<&str> = tree[0].children.iter().map(|node| node.dept_name.as_str()).collect();
        assert_eq!(names, vec!["一组", "二组"], "同级按 sort_order 升序");
        println!("[D7] 组树：{} → {:?} ✓", tree[0].dept_name, names);
    }

    /// D8：接入清单与登记表清单同源（不漂移），且 `uncovered` 必须为 0。
    #[test]
    fn d8_coverage_matches_registry() {
        let coverage = table_coverage();
        assert_eq!(coverage.len(), SCOPED_TABLES.len(), "投影必须覆盖全部登记表");
        for (item, target) in coverage.iter().zip(SCOPED_TABLES.iter()) {
            assert_eq!(item.table, target.table);
            assert_eq!(item.dept_col.as_deref(), target.dept_col);
            assert_eq!(item.owner_col.as_deref(), target.owner_col);
            assert!(
                matches!(item.status.as_str(), "wired" | "exempted" | "uncovered"),
                "{} 的 status 只能是三种之一，实际 {}",
                item.table,
                item.status
            );
            assert!(
                !item.reason.trim().is_empty(),
                "{} 的 reason 不能为空（每条豁免/接入都必须站得住）",
                item.table
            );
            assert_eq!(
                item.wired,
                item.status != "uncovered",
                "{} 的旧字段 wired 必须与 status 一致",
                item.table
            );
        }
        // 登记项与登记表一一对应：漏登记 → uncovered（红）；多登记 → 表名拼错（红）
        assert_eq!(
            TABLE_WIRING.len(),
            SCOPED_TABLES.len(),
            "TABLE_WIRING 行数必须等于 SCOPED_TABLES 行数"
        );
        for wiring in TABLE_WIRING {
            assert!(
                SCOPED_TABLES.iter().any(|target| target.table == wiring.table),
                "TABLE_WIRING 里的 {} 不在 SCOPED_TABLES 登记表中",
                wiring.table
            );
            if wiring.status == WiringStatus::Exempted {
                assert!(
                    wiring.reason.starts_with("SCOPE-EXEMPT:"),
                    "{} 是 exempted，reason 必须以 `SCOPE-EXEMPT:` 开头",
                    wiring.table
                );
            }
            if wiring.no_paginate {
                assert!(
                    wiring.reason.contains("不分页"),
                    "{} 标了 no_paginate，reason 里要写清为什么不分页",
                    wiring.table
                );
            }
        }
        let uncovered = uncovered_tables();
        assert!(uncovered.is_empty(), "登记表还有未接入的：{uncovered:?}");
        let exempted = exempted_tables();
        println!(
            "[D8] 登记 {} 张：wired={} exempted={exempted:?} uncovered={} ✓",
            coverage.len(),
            coverage.len() - exempted.len(),
            uncovered.len()
        );
    }

    /// D8b：`exempted` 分支投影（合成登记项，不依赖真实数据里恰好有豁免表）。
    #[test]
    fn d8b_exempted_row_projection() {
        let target = ScopeTarget::unscoped("synthetic_tbl");
        let row = coverage_row(&target, WiringStatus::Exempted, false, "SCOPE-EXEMPT: 合成用例");
        assert_eq!(row.status, "exempted");
        assert!(row.wired, "exempted 视为已处理，否则会被误报成 uncovered");
        assert_eq!(row.reason, "SCOPE-EXEMPT: 合成用例");
        assert!(row.dept_col.is_none() && row.owner_col.is_none());
        println!("[D8b] exempted 投影：status=exempted wired=true ✓");
    }

    /// D8c：漂移守卫 —— 登记为 `wired` 的表，其锚点常量必须在某个**含 scope:: 调用**的源文件里真实出现。
    #[test]
    fn d8c_wiring_anchors_exist_in_source() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(std::path::Path::new("src"), &mut files);
        assert!(!files.is_empty(), "找不到 src/ 下的 rs 文件（单测须以 crate 根为 CWD）");
        let wired: Vec<&TableWiring> = TABLE_WIRING
            .iter()
            .filter(|wiring| wiring.status == WiringStatus::Wired)
            .collect();
        for wiring in &wired {
            let anchor = format!("ScopeTarget::{}", wiring.anchor);
            let found = files.iter().any(|path| {
                let Ok(text) = std::fs::read_to_string(path) else {
                    return false;
                };
                text.contains(&anchor)
                    && (text.contains("scope::paginate(")
                        || text.contains("scope::filter_for(")
                        || text.contains("scope::paginate_unscoped("))
            });
            assert!(
                found,
                "{} 登记为 wired，但 src/ 里没有含 scope:: 调用的文件出现 {anchor}（登记表已与代码漂移）",
                wiring.table
            );
        }
        println!("[D8c] {} 张 wired 表的锚点在源码中可查 ✓", wired.len());
    }

    /// D9：角色范围响应组装（含范围编码 / 标签 / 部门明细）。
    #[test]
    fn d9_role_scope_res() {
        let role = RoleScope {
            role_id: uid(0xD901),
            role_code: "DEPT_LEAD".to_string(),
            role_name: "部门负责人".to_string(),
            status: 1,
            data_scope: 5,
        };
        let custom = vec![dept(uid(0xD902), None, "研发部", 1)];
        let res = role_scope_res(&role, &custom, 3);
        assert_eq!(res.data_scope_code, "CUSTOM");
        assert_eq!(res.data_scope_label, "自定义部门");
        assert_eq!(res.custom_dept_ids.len(), 1);
        assert_eq!(res.custom_depts[0].dept_name, "研发部");
        assert_eq!(res.holder_count, 3);

        // 库里的非法值（CHECK 被手工绕过）→ 回显兜底为 ALL，不让 GET 500
        let broken = RoleScope { data_scope: 9, ..role };
        assert_eq!(role_scope_res(&broken, &[], 0).data_scope_code, "ALL");
        println!("[D9] CUSTOM 回显 = {res:?} ✓；脏值兜底 ✓");
    }

    /// D10：`dedupe` 保序；`scope_summary` 与 `ScopeKind` 一一对应。
    #[test]
    fn d10_dedupe_and_summary() {
        let (a, b) = (uid(0xDA01), uid(0xDA02));
        assert_eq!(dedupe(&[b, a, b, a]), vec![b, a]);
        for kind in [
            ScopeKind::All,
            ScopeKind::Dept,
            ScopeKind::DeptAndChild,
            ScopeKind::SelfOnly,
            ScopeKind::Custom,
        ] {
            let summary = scope_summary(kind);
            assert_eq!(summary.data_scope, kind.as_i16());
            assert_eq!(summary.code, kind.code());
            assert_eq!(summary.label, kind.label());
        }
        println!("[D10] dedupe 保序；5 种范围摘要与 ScopeKind 一致 ✓");
    }
}

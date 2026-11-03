//! 审批流「未配置」的显式拒绝 + 「显式跳过」+ 流程配置自检（ARCH-3 §5 **DC-01** / §6 R-P0-2）。
//!
//! # 改造前是什么样（审计实测的 6 处静默降级）
//!
//! 采购单提交 / 收货单过账 / 收货单作废 / 应付付款 / 应付作废 / 凭证红冲 六个入口，
//! 「没有已发布的审批流程」时一律**降级为直接生效**，只留一条 `tracing::warn!`：
//!
//! | # | 入口 | 改造前 |
//! |---|------|--------|
//! | 1 | [`crate::business::erp::purchase_handler::submit_approval`] | `DRAFT → SUBMITTED`，无审批单 |
//! | 2 | [`crate::business::erp::stockin_handler::submit_post`] | 直接过账（三本账 + 凭证 + 应付） |
//! | 3 | [`crate::business::erp::stockin_handler::change_stock_in_status`]（`POSTED → CANCELLED`） | 直接作废并回滚六本账 |
//! | 4 | [`crate::business::erp::payable_payment::submit_payment`] | 直接付款并出凭证 |
//! | 5 | [`crate::business::erp::payable_payment::submit_cancel`] | 直接作废（联动红冲凭证） |
//! | 6 | [`crate::business::erp::voucher_write::submit_reverse`] | 直接红冲 |
//!
//! 后果是「内控形同虚设」且**用户无从知晓**：同一次点击，配置齐全的客户走两道复核，
//! 新建客户（或流程被误下线）的客户一个人说了算，界面上看不出任何差别。
//!
//! # 改造后：默认拒绝 + 显式跳过
//!
//! ```text
//! 有已发布流程 ──→ 走审批（行为不变）；若同时请求了 skip_approval → 400（不许绕过已配置的内控）
//! 没有已发布流程 ─┬─→ 没请求 skip_approval            → 409 + APPROVAL_FLOW_NOT_CONFIGURED（点名 flow_code）
//!                ├─→ 请求了但没有 business:erp:skip-approval → 403（跳过审批不能自助申请）
//!                ├─→ 有权限但理由 < 10 字              → 400（理由要进 biz_status_log 供审计追溯）
//!                └─→ 有权限 + 理由齐全                → 放行，理由写进 biz_status_log
//! ```
//!
//! 三条判据都在**同一个函数**里（[`authorize_missing_flow`]），六个入口只是调用点 ——
//! 这样「跳过审批」的口径不可能在六处漂移（与 P10 铁律③「状态变更只有一个入口」同一个理由）。
//!
//! # 为什么「默认拒绝」而不是「默认放行 + 提示」
//!
//! 审批流是**内控**：它的价值在于「不配置的时候业务走不通」，配置齐全才放行。
//! 默认放行等于「谁都可以靠删流程来绕过审批」—— 一条 `UPDATE biz_flow_def SET status`
//! 就能让所有单据免复核，而且事后只有一行日志。所以这里必须 fail-closed，
//! 把「这次没有复核」变成一个**有意为之、留了理由、留了权限**的动作。
//!
//! # 为什么「跳过」要单独一个权限码，而不是复用发起权限
//!
//! 发起单据（`business:erp:purchase-order:status` 等）是**经办人**的日常动作；
//! 跳过复核是**内控例外**，必须由管理员显式授予（[`PERM_SKIP_APPROVAL`]），
//! 且只授予「确实需要在这种单据类型上免复核」的角色。
//!
//! # 自检端点
//!
//! [`health`] 是 `GET /business/erp/approval-flow/health` 的实现：8 个 `biz_type` 逐个返回
//! 「是否有已发布版本 / 当前版本号 / 图形校验是否通过」。**图形校验复用
//! [`crate::business::flow::graph::FlowGraph::validate_with`]**（R1–R21 同一份实现），
//! 不另写一套——否则会出现「编辑器说能发、自检说不行」的双真相。
//!
//! # 边界（刻意不做的事）
//!
//! - **不自动补流程**：种子流程由迁移保证（`V0202__erp_approval_flow_health`），
//!   客户自己删掉/停用流程属配置变更，自检只如实报告，不替客户决定；
//! - **不改状态机边表**：跳过审批走的仍是原状态机（`DRAFT → SUBMITTED` 等），
//!   只是「不用先过审批」—— 松的是审批这一层，不是状态机；
//! - **调拨单（`STOCK_TRANSFER`）不在此列**：它的「没配流程」是 `debug` 日志 +
//!   允许人工推 `APPROVED`（一次需要 `business:erp:transfer:status` 权限码、
//!   会写 `biz_status_log` 的显式动作），不构成「静默直接生效」，
//!   与审计 §5 的 6 处不是同一形态（已在交付说明里注明）。

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;
use crate::utils;

/// 「跳过审批」的权限码。
///
/// 与 `sys_menu.auth_code` 一一对应（`V0202__erp_approval_flow_health.up.sql` 种入）；
/// `SUPER_ADMIN` 由 `TokenClaims::has_permission` 的通配短路放行。
pub const PERM_SKIP_APPROVAL: &str = "business:erp:skip-approval";

/// 自检端点的权限码（与 [`health`] 的 `#[nik::permission]` 注解同值）。
pub const PERM_FLOW_HEALTH: &str = "business:erp:approval-flow:health";

/// 「跳过审批」理由的最小字数（按字符数，中文一个字算一个）。
pub const MIN_SKIP_REASON_CHARS: usize = 10;

/// 写进 `biz_status_log.reason` 的合成上限。
///
/// 三个入口的 `MAX_REASON_LEN` 分别是 200（序列号）/ 255（采购），这里取更保守的 200：
/// 超长时截断**跳过理由**（保留前缀「跳过审批（未配置 x 流程）：」与开头文字），
/// 不截断调用方自己的业务理由（合成时业务理由在前）。
pub const MAX_MERGED_REASON_CHARS: usize = 200;

// ==================== 请求片段 ====================

/// 「显式跳过审批」的请求片段。
///
/// 以 `#[serde(flatten)]` 混进各入口原有的请求体（采购 / 收货 / 付款 / 作废 / 红冲），
/// 所以**老客户端不传这两个字段时行为完全不变**（`skip_approval` 缺省 `false`）。
///
/// ```json
/// { "status": "POSTED", "skip_approval": true, "skip_reason": "月底关账，供应商已现场验收" }
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkipApprovalReq {
    /// 是否显式跳过审批。**必须为 `true` 才算请求跳过**（缺省/`false` = 没请求）。
    #[serde(default)]
    pub skip_approval: bool,
    /// 跳过审批的理由（≥ [`MIN_SKIP_REASON_CHARS`] 字）。只有真的走跳过时才校验。
    #[serde(default)]
    pub skip_reason: Option<String>,
}

impl SkipApprovalReq {
    /// 跳过理由（去空白；空串 → `None`）。
    pub fn reason_text(&self) -> Option<&str> {
        self.skip_reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }

    /// 本次是否**请求了**跳过（`skip_approval = true`）。
    pub fn requested(&self) -> bool {
        self.skip_approval
    }
}

/// 「没有请求跳过」的静态占位（系统内部路径用，见 [`ApprovalGate::none`]）。
static NO_SKIP: SkipApprovalReq = SkipApprovalReq {
    skip_approval: false,
    skip_reason: None,
};

/// 一次审批前置检查的上下文：**跳过请求 + 操作者的权限声明**。
///
/// 为什么把两者打包：判据需要「这个人能不能跳过」，而权限只能在 `TokenClaims` 上判
/// （`has_permission` 含 `SUPER_ADMIN` 通配短路，与路由层 `#[nik::permission]` 同口径）。
/// 打包成一个参数，六个入口的调用点写法一致，也不会出现「传了 skip 忘了传 claims」。
///
/// **系统内部路径**（审批回调 / 单测走查 / 定时任务）用 [`ApprovalGate::none`]：
/// 它**没有**跳过能力 —— 走到「没配流程」分支一律 409，这正是 fail-closed 想要的。
pub struct ApprovalGate<'a> {
    skip: &'a SkipApprovalReq,
    claims: Option<&'a TokenClaims>,
}

impl<'a> ApprovalGate<'a> {
    /// 系统内部路径：不请求跳过、也没有任何权限可声明（→ 没配流程时一律 409）。
    pub fn none() -> Self {
        Self {
            skip: &NO_SKIP,
            claims: None,
        }
    }

    /// HTTP 请求路径：请求体里的跳过片段 + 当前登录人的 claims。
    pub fn new(skip: &'a SkipApprovalReq, claims: &'a TokenClaims) -> Self {
        Self {
            skip,
            claims: Some(claims),
        }
    }

    /// 本次是否请求了跳过审批。
    pub fn requested(&self) -> bool {
        self.skip.requested()
    }

    /// 当前操作者是否持有 [`PERM_SKIP_APPROVAL`]。
    pub fn may_skip(&self) -> bool {
        self.claims
            .map(|claims| claims.has_permission(PERM_SKIP_APPROVAL))
            .unwrap_or(false)
    }
}

// ==================== 判据 ====================

/// 400：有流程却请求了跳过。
fn flow_is_published(flow_code: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::PARAM_INVALID,
        format!(
            "审批流程 `{flow_code}` 已发布：有流程时必须走审批，不能跳过。\
             如果这类单据确实不该审批，请先让管理员**下线**该流程（配置变更要留痕），\
             再在请求里显式传 skip_approval + skip_reason"
        ),
    )
}

/// 403：请求了跳过但没有权限码。
fn skip_forbidden() -> ApiError {
    ApiError::new(
        403,
        error_code::PERMISSION_DENIED,
        format!(
            "跳过审批需要权限码 `{PERM_SKIP_APPROVAL}`（当前账号未持有）：\
             「跳过审批」意味着这次单据不经复核直接生效，属内控例外，\
             只能由管理员在「系统管理 · 菜单权限」里授予，不能自助开启"
        ),
    )
}

/// 400：请求了跳过但理由不达标。
fn skip_reason_too_short(actual: usize) -> ApiError {
    ApiError::new(
        400,
        error_code::PARAM_INVALID,
        format!(
            "跳过审批必须写清理由（≥{MIN_SKIP_REASON_CHARS} 字，当前 {actual} 字）：\
             理由会写进单据状态日志（biz_status_log），财务 / 审计据此追溯\
             「为什么这一次没有复核」——不写理由的跳过等于没有复核记录"
        ),
    )
}

/// 409：该单据类型没有已发布的审批流程（**默认拒绝**）。
fn flow_not_configured(biz_type: &str, flow_code: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::APPROVAL_FLOW_NOT_CONFIGURED,
        format!(
            "没有配置已发布的审批流程 `{flow_code}`（业务类型 {biz_type}）：\
             为避免「没有审批就直接生效」，本次操作已被拒绝。\
             两条出路：① 让管理员在「审批配置」里发布 `{flow_code}`；\
             ② 如果这类单据暂时不走审批，请给对应角色授予 `{PERM_SKIP_APPROVAL}`，\
             并在请求里显式传 skip_approval=true + skip_reason（≥{MIN_SKIP_REASON_CHARS} 字）"
        ),
    )
}

/// 流程**已发布**时的闸：请求了跳过 → 400（不许绕过已配置的内控）。
///
/// 六个入口在 `Some(flow)` 分支里调它。**不静默忽略**：忽略等于「以为跳过了、其实走了审批」，
/// 用户会因为「提交后单据卡在审批中」而困惑（这正是本次改造要消灭的那类静默行为）。
pub fn ensure_not_skipped(
    flow_code: &str,
    gate: &ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    if gate.requested() {
        return Err(flow_is_published(flow_code));
    }
    Ok(())
}

/// 流程**未发布**时的闸：默认 409，只有「有权限 + 理由齐全」才放行。
///
/// # 返回
///
/// 放行时返回**要写进 `biz_status_log.reason` 的合成理由**，形如
/// `跳过审批（未配置 stock_in_post 流程）：<用户填的理由>`。
/// 调用方把它连同业务自身的理由一起交给 [`merge_reason`]。
///
/// # 错误
///
/// * 409 + [`error_code::APPROVAL_FLOW_NOT_CONFIGURED`]：没请求跳过（**默认拒绝**）；
/// * 403 + [`error_code::PERMISSION_DENIED`]：请求了跳过但没有 [`PERM_SKIP_APPROVAL`]；
/// * 400 + [`error_code::PARAM_INVALID`]：有权限但理由缺失 / 不足 [`MIN_SKIP_REASON_CHARS`] 字。
pub fn authorize_missing_flow(
    biz_type: &str,
    flow_code: &str,
    gate: &ApprovalGate<'_>,
) -> std::result::Result<String, ApiError> {
    if !gate.requested() {
        return Err(flow_not_configured(biz_type, flow_code));
    }
    if !gate.may_skip() {
        return Err(skip_forbidden());
    }
    let reason = gate.skip.reason_text().unwrap_or_default();
    let chars = reason.chars().count();
    if chars < MIN_SKIP_REASON_CHARS {
        return Err(skip_reason_too_short(chars));
    }
    Ok(format!(
        "跳过审批（未配置 {flow_code} 流程）：{}",
        reason
            .chars()
            .take(MAX_MERGED_REASON_CHARS)
            .collect::<String>()
    ))
}

/// 把「业务自己的理由」与「跳过审批的理由」合成一条写进状态日志。
///
/// 顺序：业务理由在前（那是这次动作的业务含义），跳过标记在后（那是内控例外）。
/// 总体截断到 [`MAX_MERGED_REASON_CHARS`] 字：`biz_status_log.reason` 的写入口
/// （采购 / 收货）会把超长理由判 400，而这里截断的只是「日志文本」，
/// 不影响任何业务判据 —— 用户填的跳过理由前 200 字已经足够追溯。
pub fn merge_reason(own: Option<&str>, skip_reason: &str) -> String {
    let merged = match own.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => format!("{text}｜{skip_reason}"),
        None => skip_reason.to_string(),
    };
    merged.chars().take(MAX_MERGED_REASON_CHARS).collect()
}

// ==================== 自检端点 ====================

/// 需要自检的 8 个业务类型（`(biz_type, flow_code)`）。
///
/// 与 `V0202__erp_approval_flow_health.up.sql` 的自检块逐字一致：
/// 卡面点名的 8 个流程全部在内，顺序对人友好（按业务链路：采购 → 收货 → 付款 → 凭证 → 调拨 → 验收）。
pub const FLOW_TYPES: [(&str, &str); 8] = [
    ("PO_APPROVE", "po_approve"),
    ("STOCK_IN_POST", "stock_in_post"),
    ("STOCK_IN_CANCEL", "stock_in_cancel"),
    ("PAYABLE_PAY", "payable_pay"),
    ("PAYABLE_CANCEL", "payable_cancel"),
    ("VOUCHER_REVERSE", "voucher_reverse"),
    ("STOCK_TRANSFER", "stock_transfer"),
    ("TASK_ACCEPT", "task_accept"),
];

/// 一个业务类型的自检结果。
#[derive(Debug, Clone, Serialize)]
pub struct FlowHealthItem {
    /// 业务类型（`biz_flow_def.biz_type`，大写）。
    pub biz_type: String,
    /// 流程编码（`biz_flow_def.flow_code`，小写）。
    pub flow_code: String,
    /// **是否有已发布版本**（`status = 'PUBLISHED'` 且 `current_version_id` 非空、未软删）。
    pub published: bool,
    /// 命中的流程定义 id（没配 = `None`）。
    pub def_id: Option<String>,
    /// 流程名称（没配 = `None`）。
    pub flow_name: Option<String>,
    /// 定义状态（`PUBLISHED` / `DISABLED` / …；没配 = `None`）。**停用的流程也会显示**，
    /// 便于运维看出「流程在、只是被下线了」与「从来没有」的区别。
    pub def_status: Option<String>,
    /// **当前版本号**（`current_version_no`）。
    pub version_no: Option<i32>,
    /// 当前版本的状态（`version_status`）。
    pub version_status: Option<String>,
    /// 当前版本是否带发布快照（`graph_snapshot` 非空 —— 引擎只认快照）。
    pub has_snapshot: bool,
    /// **图形校验是否通过**（复用 `FlowGraph::validate_with`，`published && issues.is_empty()`）。
    pub graph_ok: bool,
    /// 图形校验不通过项（`R1 流程必须有且仅有一个开始节点 …`），通过时为空。
    pub graph_issues: Vec<String>,
}

/// 自检汇总。
#[derive(Debug, Clone, Serialize)]
pub struct FlowHealthRes {
    /// 检查时刻（RFC 3339，UTC）。
    pub checked_at: String,
    /// 检查的业务类型个数（应恒为 [`FLOW_TYPES`] 的长度 = 8）。
    pub total: usize,
    /// 其中「已发布且图形校验通过」的个数。
    pub healthy: usize,
    /// 需要关注（没有发布版本 / 图形校验不过）的业务类型个数 = `total - healthy`。
    pub unhealthy: usize,
    /// 逐项结果。
    pub items: Vec<FlowHealthItem>,
}

/// 查一个业务类型的流程现状（**任意状态**都返回，便于区分「停用」与「没有」）。
///
/// 排序口径与 `stockin_query::published_flow_of` 一致（`biz_type` 精确命中优先 →
/// `is_default` → 版本号 → 更新时间），保证自检看到的行与业务实际选中的行是同一行。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
// SCOPE-EXEMPT: biz_flow_def 是全局流程配置（无 dept_id / 归属人列），自检必须看到 8 个 biz_type「运行时实际会选中的那一行」，注入部门范围会把别部门已发布的流程判成「没有配置」→ 上线体检假红（错的是告警而不是数据）；权限在端点：business/erp/approval_gate.rs:519 approval_flow_health（:516 #[nik::permission] business:erp:approval-flow:health，管理员/实施专用）。
async fn inspect(biz_type: &str, flow_code: &str) -> DbResult<FlowHealthItem> {
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.status, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no, \
                    v.version_status, (v.graph_snapshot IS NOT NULL) AS has_snapshot \
               FROM biz_flow_def d \
               LEFT JOIN biz_flow_def_version v ON v.id = d.current_version_id \
              WHERE d.deleted_at IS NULL AND (d.biz_type = ? OR lower(d.flow_code) = ?) \
              ORDER BY (d.biz_type = ?) DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC",
            vec![
                Value::Text(biz_type.to_string()),
                Value::Text(flow_code.to_string()),
                Value::Text(biz_type.to_string()),
            ],
        )
        .await?;

    let Some(row) = rows.first() else {
        return Ok(FlowHealthItem {
            biz_type: biz_type.to_string(),
            flow_code: flow_code.to_string(),
            published: false,
            def_id: None,
            flow_name: None,
            def_status: None,
            version_no: None,
            version_status: None,
            has_snapshot: false,
            graph_ok: false,
            graph_issues: vec![format!(
                "没有配置任何 `{flow_code}` 流程（biz_type {biz_type}）：这类单据提交时会 409"
            )],
        });
    };

    let def_id: Uuid = row.get_typed("def_id")?;
    let def_status: String = row.get_typed("status")?;
    let flow_name: String = row.get_typed("flow_name")?;
    let version_id: Option<Uuid> = row.get_typed("version_id")?;
    let version_no: Option<i32> = row.get_typed("version_no")?;
    let version_status: Option<String> = row.get_typed("version_status")?;
    let has_snapshot: bool = row.get_typed("has_snapshot")?;
    let published = def_status == "PUBLISHED" && version_id.is_some();

    // 图形校验：复用图的唯一校验实现（R1–R21），不另写一套判据。
    let (graph_ok, graph_issues) = if published && has_snapshot {
        match version_id {
            Some(version_id) => match crate::business::flow::model::load_graph(version_id).await {
                Ok(graph) => {
                    let issues: Vec<String> = graph
                        .validate_with(Some(biz_type))
                        .into_iter()
                        .map(|issue| format!("{} {}", issue.rule, issue.message))
                        .collect();
                    (issues.is_empty(), issues)
                }
                // 行数据读不出来（脏数据 / 版本被物理删）→ 自检报「不过」，不把错误往上抛：
                // 自检的价值就在于「一张表看完所有毛病」，不能因为一条坏行整页 500。
                Err(error) => (false, vec![format!("读取流程版本图失败：{error}")]),
            },
            None => (false, vec!["current_version_id 为空".to_string()]),
        }
    } else if published {
        (
            false,
            vec!["当前版本没有发布快照（graph_snapshot 为空）：引擎无法按版本运行".to_string()],
        )
    } else {
        (
            false,
            vec![format!(
                "流程状态是 `{def_status}`（不是 PUBLISHED）：这类单据提交时会 409"
            )],
        )
    };

    Ok(FlowHealthItem {
        biz_type: biz_type.to_string(),
        flow_code: flow_code.to_string(),
        published,
        def_id: Some(def_id.to_string()),
        flow_name: Some(flow_name),
        def_status: Some(def_status),
        version_no,
        version_status,
        has_snapshot,
        graph_ok,
        graph_issues,
    })
}

/// 审批流配置自检：8 个 `biz_type` 逐个返回「是否有已发布版本 / 当前版本号 / 图形校验是否通过」。
///
/// 发布前的检查清单就靠它（`GET /business/erp/approval-flow/health` 应返回
/// `unhealthy = 0`）——本版本改造后「没配流程」会挡住业务，所以上线前必须确认 8 个流程都在。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（`500`）。单个流程的图读不出来**不**抛错，
/// 而是记进该项的 `graph_issues`（自检要能一次看完所有问题）。
pub async fn health() -> DbResult<FlowHealthRes> {
    let mut items = Vec::with_capacity(FLOW_TYPES.len());
    for (biz_type, flow_code) in FLOW_TYPES {
        items.push(inspect(biz_type, flow_code).await?);
    }
    let total = items.len();
    let healthy = items
        .iter()
        .filter(|item| item.published && item.graph_ok)
        .count();
    Ok(FlowHealthRes {
        checked_at: Utc::now().to_rfc3339(),
        total,
        healthy,
        unhealthy: total - healthy,
        items,
    })
}

// ==================== 自检端点 ====================

/// 审批流配置自检：8 个业务类型逐个返回「是否有已发布版本 / 当前版本号 / 图形校验是否通过」。
///
/// # 为什么需要这个端点
///
/// ARCH-3 DC-01 之后「没配流程」会**挡住业务**（409），所以上线前必须能一眼确认
/// 8 个流程都在、且图都校验得过。这个端点就是那张体检单 —— 它读的是
/// **业务运行时选中的同一行**（排序口径与 `stockin_query::published_flow_of` 一致），
/// 图校验复用 `FlowGraph::validate_with`（R1–R21 同一份实现）。
///
/// # 出参
///
/// `total` = 8、`healthy` = 已发布且图形校验通过的个数、`unhealthy` = 其余；
/// `items` 逐项给出 `published` / `version_no` / `graph_ok` / `graph_issues`。
///
/// # 权限
///
/// `business:erp:approval-flow:health`（与 `V0202__erp_approval_flow_health.up.sql` 同值）：
/// 这是「配置体检」，给管理员与实施同学用，不开放给普通业务角色。
///
/// # 异常情况
///
/// * 401：未登录；
/// * 403：缺 `business:erp:approval-flow:health`；
/// * 500 + `10004`：数据库异常（单张流程图读不出来不算错，记进该项的 `graph_issues`）。
#[nik::permission(
    path = "/business/erp/approval-flow/health",
    method = "GET",
    permission = "business:erp:approval-flow:health"
)]
#[nik::get("/business/erp/approval-flow/health")]
pub async fn approval_flow_health() -> ResResult<FlowHealthRes> {
    Ok(Res::ok(health().await.map_err(|error| {
        utils::db_error(error, "审批流配置自检")
    })?))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一份 claims（只关心 `permissions`）。
    fn claims_with(permissions: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: Uuid::now_v7().to_string(),
            username: Some("dc01_test".to_string()),
            roles: Vec::new(),
            permissions: permissions.iter().map(|item| item.to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    fn skip_req(flag: bool, reason: Option<&str>) -> SkipApprovalReq {
        SkipApprovalReq {
            skip_approval: flag,
            skip_reason: reason.map(str::to_string),
        }
    }

    /// **DC-01 的判据全部在这条用例里钉死**（纯逻辑，不需要数据库）：
    ///
    /// | 场景 | 期望 |
    /// |------|------|
    /// | 没请求跳过（default deny） | 409 + 66033，报文含 flow_code |
    /// | 请求了但没有 `business:erp:skip-approval` | 403 + 30001 |
    /// | 有权限但完全没写理由 | 400 + 10001 |
    /// | 有权限但理由不足 10 字 | 400 + 10001，报文给出实际字数 |
    /// | 有权限 + 理由齐全 | 放行，理由带「跳过审批（未配置 x 流程）」前缀 |
    /// | 超管（`*` 通配） | 视同有权限 |
    /// | 流程已发布时请求跳过 | 400（不许绕过已配置的内控） |
    /// | `ApprovalGate::none()`（系统路径）请求跳过也跳不了 | 409 |
    #[test]
    fn dc01_gate_denies_by_default_and_requires_permission_and_reason() {
        // ① 默认拒绝：没请求跳过 → 409 + 新错误码，报文点名 flow_code
        let nobody = claims_with(&["business:erp:purchase-order:status"]);
        let req = skip_req(false, None);
        let gate = ApprovalGate::new(&req, &nobody);
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate)
            .expect_err("没配流程且没请求跳过必须 409");
        assert_eq!(error.status, 409, "默认拒绝必须是 409（冲突）");
        assert_eq!(
            error.code,
            error_code::APPROVAL_FLOW_NOT_CONFIGURED,
            "必须用新码 66033，不能复用 RESOURCE_CONFLICT"
        );
        assert!(
            error.msg.contains("po_approve"),
            "报文必须点名缺哪个 flow_code：{}",
            error.msg
        );
        assert!(
            error
                .msg
                .contains(crate::business::erp::approval_gate::PERM_SKIP_APPROVAL),
            "报文要给出「显式跳过」这条出路：{}",
            error.msg
        );

        // ② 请求了跳过但没有权限码 → 403（不给自助开启的机会）
        let req = skip_req(true, Some("月底关账，供应商已现场验收，先放行"));
        let gate = ApprovalGate::new(&req, &nobody);
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate)
            .expect_err("无权限码必须 403");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, error_code::PERMISSION_DENIED);
        assert!(
            error.msg.contains("business:erp:skip-approval"),
            "403 报文要点名缺哪个权限码：{}",
            error.msg
        );

        // ③ 有权限但完全没写理由 → 400
        let holder = claims_with(&[PERM_SKIP_APPROVAL]);
        let req = skip_req(true, None);
        let gate = ApprovalGate::new(&req, &holder);
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate)
            .expect_err("有权限没理由必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);

        // ④ 有权限但理由不足 10 字 → 400，报文给出实际字数
        let req = skip_req(true, Some("  先放行  "));
        let gate = ApprovalGate::new(&req, &holder);
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate)
            .expect_err("理由不足 10 字必须 400");
        assert_eq!(error.status, 400);
        assert!(
            error.msg.contains("当前 3 字"),
            "报文要给出实际字数（去空白后 3 字）：{}",
            error.msg
        );

        // ⑤ 有权限 + 理由齐全 → 放行，返回写日志用的合成理由
        let req = skip_req(true, Some("供应商现场验收完成，月底关账前先入库"));
        let gate = ApprovalGate::new(&req, &holder);
        let reason = authorize_missing_flow("STOCK_IN_POST", "stock_in_post", &gate)
            .expect("有权限且理由齐全必须放行");
        assert!(
            reason.starts_with("跳过审批（未配置 stock_in_post 流程）："),
            "合成理由要能一眼看出「这是跳过审批」：{reason}"
        );
        assert!(reason.contains("供应商现场验收完成"), "用户理由要原样保留");

        // ⑥ 超管（`*` 通配）视同持有权限码
        let root = claims_with(&["*"]);
        let gate = ApprovalGate::new(&req, &root);
        assert!(
            approve(&gate),
            "SUPER_ADMIN 的通配权限必须短路放行（与路由层同口径）"
        );

        // ⑦ 流程已发布时请求跳过 → 400（不许绕过已配置的内控）
        let error = ensure_not_skipped("po_approve", &gate).expect_err("已发布流程不许跳过");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("po_approve"));

        // ⑧ 系统内部路径（`ApprovalGate::none()`）：即便「请求了」也跳不了 → 409
        let system = ApprovalGate::none();
        assert!(!system.requested(), "系统路径不带跳过请求");
        assert!(!system.may_skip(), "系统路径没有权限声明");
        let error = authorize_missing_flow("VOUCHER_REVERSE", "voucher_reverse", &system)
            .expect_err("系统路径必须 409");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::APPROVAL_FLOW_NOT_CONFIGURED);
    }

    /// 「放行」的小助手：把 `authorize_missing_flow` 的成功与否压成 bool（断言可读性）。
    fn approve(gate: &ApprovalGate<'_>) -> bool {
        authorize_missing_flow("PO_APPROVE", "po_approve", gate).is_ok()
    }

    /// 理由合成：业务理由在前、跳过标记在后，总长截断到 200 字。
    #[test]
    fn dc01_merge_reason_keeps_business_reason_first_and_clamps() {
        let merged = merge_reason(
            Some("供应商召回"),
            "跳过审批（未配置 stock_in_cancel 流程）：现场已清点",
        );
        assert!(
            merged.starts_with("供应商召回｜跳过审批"),
            "业务理由在前：{merged}"
        );
        let merged = merge_reason(None, "跳过审批（未配置 x 流程）：理由");
        assert_eq!(merged, "跳过审批（未配置 x 流程）：理由");
        let long = merge_reason(Some(&"甲".repeat(300)), "跳过审批");
        assert_eq!(
            long.chars().count(),
            MAX_MERGED_REASON_CHARS,
            "必须截断到 200 字"
        );
    }

    /// 8 个业务类型都在自检清单里（卡面点名的 8 个，一个不能少）。
    #[test]
    fn dc01_flow_types_cover_the_eight_biz_types() {
        let expected = [
            "PO_APPROVE",
            "STOCK_IN_POST",
            "STOCK_IN_CANCEL",
            "PAYABLE_PAY",
            "PAYABLE_CANCEL",
            "VOUCHER_REVERSE",
            "STOCK_TRANSFER",
            "TASK_ACCEPT",
        ];
        let actual: Vec<&str> = FLOW_TYPES.iter().map(|(biz, _)| *biz).collect();
        assert_eq!(actual, expected, "自检清单必须覆盖 8 个业务类型且顺序一致");
        for (biz_type, flow_code) in FLOW_TYPES {
            assert_eq!(
                flow_code,
                biz_type.to_lowercase(),
                "flow_code 与 biz_type 的大小写口径（{biz_type} → {flow_code}）"
            );
        }
    }

    // ---------- 真库：自检端点（`ERP_DB_URL`，与其它 ERP 用例同口径） ----------

    /// 连库（进程内只做一次）。`false` = 没配 `ERP_DB_URL`，真库用例直接跳过。
    ///
    /// ⚠️ 不设 `ERP_DB_URL` 时这些用例是 **SKIP 假绿**：交付前必须
    /// `ERP_DB_URL=… cargo test --lib business::erp::approval_gate` 真跑一遍。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// **自检端点**：8 个业务类型逐个 `published = true`、`version_no` 有值、
    /// `graph_ok = true`（图形校验复用 `FlowGraph::validate_with`）。
    ///
    /// 这条用例就是「上线前排查」的自动化版本：库里 8 个流程缺一个、或某个流程图改坏了，
    /// 它都会红。
    #[tokio::test]
    async fn dc01_health_reports_eight_published_flows() {
        if !ensure_db().await {
            return;
        }
        let report = health().await.expect("审批流自检查询失败");

        assert_eq!(report.total, 8, "自检必须覆盖 8 个业务类型");
        assert_eq!(
            report.items.len(),
            8,
            "items 必须与 FLOW_TYPES 一样多（实际 {:#?}）",
            report.items
        );
        for item in &report.items {
            assert!(
                item.published,
                "{}（{}）必须已发布，实际 def_status={:?}：{:?}",
                item.biz_type, item.flow_code, item.def_status, item.graph_issues
            );
            assert!(
                item.version_no.is_some(),
                "{} 必须有当前版本号",
                item.biz_type
            );
            assert!(
                item.has_snapshot,
                "{} 的当前版本必须有发布快照",
                item.biz_type
            );
            assert!(
                item.graph_ok,
                "{} 的图形校验必须通过：{:?}",
                item.biz_type, item.graph_issues
            );
        }
        assert_eq!(
            report.healthy, 8,
            "8 个流程必须全部「已发布 + 图形校验通过」"
        );
        assert_eq!(report.unhealthy, 0);
        println!(
            "[DC-01] 审批流自检 {} / {} 健康（checked_at={}）",
            report.healthy, report.total, report.checked_at
        );
    }
}

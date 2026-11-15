//! 审批「未配置流程」的显式拒绝 + 「显式跳过」的判据（P2-34 从 `erp::approval_gate` 搬来）。
//!
//! # 为什么在 `flow`
//!
//! 这套判据回答的是**审批这件事本身**的问题：「这类单据有没有已发布的流程」「没有的话谁能跳过、
//! 要写什么理由」。它不碰任何业务表 —— 三个入口（`erp` 的采购 / 收货 / 付款 / 作废 / 红冲、
//! `flow::edge_admin` 的状态机配置审批、`hr` 的请假 / 薪资）读的是同一份口径。
//! 放在 `erp` 里，另外两个域就得直连 ERP；放在这里，三个域都只依赖 `flow`。
//!
//! # 判据（fail-closed）
//!
//! ```text
//! 有已发布流程 ──→ 走审批（行为不变）；若同时请求了 skip_approval → 400（不许绕过已配置的内控）
//! 没有已发布流程 ─┬─→ 没请求 skip_approval            → 409 + APPROVAL_FLOW_NOT_CONFIGURED（点名 flow_code）
//!                ├─→ 请求了但没有跳过审批的权限码      → 403（跳过审批不能自助申请）
//!                ├─→ 有权限但理由 < 10 字              → 400（理由要进 biz_status_log 供审计追溯）
//!                └─→ 有权限 + 理由齐全                → 放行，理由写进 biz_status_log
//! ```
//!
//! 判据全部在 [`authorize_missing_flow`] 一个函数里，调用点只负责把
//! [`ApprovalGate`]（跳过请求 + 权限声明）交进来 —— 口径不可能在各入口之间漂移。
//!
//! # 权限码 `business:erp:skip-approval` 为什么带 `erp` 前缀
//!
//! 历史命名：它由 ERP 的迁移（`V0202`）种进 `sys_menu`，键名不能改（存量授权会丢）。
//! 语义是「允许在不配流程的单据上免复核」，与它挂在哪棵菜单树下无关。

use nik::prelude::*;
use serde::Deserialize;

use crate::shared::error_code;

/// 「跳过审批」的权限码。
///
/// 与 `sys_menu.auth_code` 一一对应（`V0202__erp_approval_flow_health.up.sql` 种入）；
/// `SUPER_ADMIN` 由 `TokenClaims::has_permission` 的通配短路放行。
pub const PERM_SKIP_APPROVAL: &str = "business:erp:skip-approval";

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
pub fn ensure_not_skipped(flow_code: &str, gate: &ApprovalGate<'_>) -> std::result::Result<(), ApiError> {
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
        reason.chars().take(MAX_MERGED_REASON_CHARS).collect::<String>()
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

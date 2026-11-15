//! 审批流「未配置」的显式拒绝 + 「显式跳过」+ 流程配置自检（ARCH-3 §5 **DC-01** / §6 R-P0-2）。
//!
//! # 改造前是什么样（审计实测的 6 处静默降级）
//!
//! 采购单提交 / 收货单过账 / 收货单作废 / 应付付款 / 应付作废 / 凭证红冲 六个入口，
//! 「没有已发布的审批流程」时一律**降级为直接生效**，只留一条 `tracing::warn!`：
//!
//! | # | 入口 | 改造前 |
//! |---|------|--------|
//! | 1 | [`crate::business::erp::purchase::purchase_handler::submit_approval`] | `DRAFT → SUBMITTED`，无审批单 |
//! | 2 | [`crate::business::erp::stockin::stockin_handler::submit_post`] | 直接过账（三本账 + 凭证 + 应付） |
//! | 3 | [`crate::business::erp::stockin::stockin_handler::change_stock_in_status`]（`POSTED → CANCELLED`） | 直接作废并回滚六本账 |
//! | 4 | [`crate::business::erp::payable::payable_payment::submit_payment`] | 直接付款并出凭证 |
//! | 5 | [`crate::business::erp::payable::payable_payment::submit_cancel`] | 直接作废（联动红冲凭证） |
//! | 6 | [`crate::business::erp::voucher::voucher_write::submit_reverse`] | 直接红冲 |
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
use serde::Serialize;

use cloudpivot_kernel::utils;

// ==================== 通用闸门（P2-34 从 ERP 搬来） ====================

// 「没有已发布流程」+「显式跳过」这套判据不是 ERP 专属：`flow::edge_admin`（状态机配置审批）、
// `hr` 的请假 / 薪资入口也用同一份口径。留在 `erp` 里，那两个域就得直连 ERP —— 那正是 P2-B 要摘的边。
// 搬走后 ERP 侧用 `pub use` 再导出（35 个调用点一行不改），判据只有这一份实现。
pub use crate::business::flow::approval_gate::{
    ApprovalGate, MAX_MERGED_REASON_CHARS, MIN_SKIP_REASON_CHARS, PERM_SKIP_APPROVAL, SkipApprovalReq,
    authorize_missing_flow, ensure_not_skipped, merge_reason,
};

// ==================== 自检端点 ====================

/// 自检端点的权限码（与 [`approval_flow_health`] 的 `#[nik::permission]` 注解同值）。
pub const PERM_FLOW_HEALTH: &str = "business:erp:approval-flow:health";

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
    /// **测试残留提示**（`t_61dca492`）：库里还剩多少 `E2E_*` / `D2_SMOKE` 的流程定义 / 实例。
    ///
    /// 为什么放在这个体检单里：e2e 脚本被指到生产库时，最先被污染的正是这两张表
    /// （2026-10-30/31 实测 37 条实例 + 85 条定义），而这张单子是运维/实施上线必看的那一页。
    /// **不影响 `healthy` / `unhealthy` 的判定**（残留不是「流程没配」），只是多一条可见的提示。
    pub test_residue: TestResidueRes,
}

/// 测试残留提示（`biz_type` = `E2E_*` / `D2_SMOKE` 且未软删）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TestResidueRes {
    /// 未软删的测试残留流程定义行数（应为 0）。
    pub defs: u64,
    /// 未软删的测试残留流程实例行数（应为 0）。
    pub instances: u64,
    /// 人话提示（0 行时是「干净」的文案；> 0 时点名数量 + 清理入口）。
    pub message: String,
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
    let healthy = items.iter().filter(|item| item.published && item.graph_ok).count();
    Ok(FlowHealthRes {
        checked_at: Utc::now().to_rfc3339(),
        total,
        healthy,
        unhealthy: total - healthy,
        items,
        test_residue: inspect_test_residue().await?,
    })
}

/// 查测试残留（`biz_type` = `E2E_*` / `D2_SMOKE` 且未软删）。
///
/// 判据**不在这里重写**：用 [`cloudpivot_doc::TEST_RESIDUE_SQL`]，
/// 与 `biz_type_domain.py --strict`、[`cloudpivot_kernel::shared::startup_guard::count_test_residue`]
/// 是同一个口径（否则「巡检红了但 /health 说干净」这种对不上会反复出现）。
///
/// 只读，两条走 `deleted_at` 的 count 查询，毫秒级。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（500）。
// SCOPE-EXEMPT: 体检是全局计数（跨全部流程定义/实例），注入数据范围会漏检。
async fn inspect_test_residue() -> DbResult<TestResidueRes> {
    use cloudpivot_doc::TEST_RESIDUE_SQL;

    let defs: i64 = db()
        .scalar_with_params(
            &format!("SELECT count(*) FROM biz_flow_def WHERE deleted_at IS NULL AND {TEST_RESIDUE_SQL}"),
            vec![],
        )
        .await?;
    let instances: i64 = db()
        .scalar_with_params(
            &format!("SELECT count(*) FROM biz_flow_instance WHERE deleted_at IS NULL AND {TEST_RESIDUE_SQL}"),
            vec![],
        )
        .await?;
    let residue = cloudpivot_kernel::shared::startup_guard::TestResidue {
        defs: defs.max(0) as u64,
        instances: instances.max(0) as u64,
    };
    let message = match cloudpivot_kernel::shared::startup_guard::test_residue_warning(residue) {
        Some(message) => message,
        None => "测试残留 0 行：biz_flow_def / biz_flow_instance 里没有 E2E_* / D2_SMOKE 活行".to_string(),
    };
    Ok(TestResidueRes {
        defs: residue.defs,
        instances: residue.instances,
        message,
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
    Ok(Res::ok(
        health()
            .await
            .map_err(|error| utils::db_error(error, "审批流配置自检"))?,
    ))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use cloudpivot_kernel::shared::error_code;

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
        let error =
            authorize_missing_flow("PO_APPROVE", "po_approve", &gate).expect_err("没配流程且没请求跳过必须 409");
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
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate).expect_err("无权限码必须 403");
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
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate).expect_err("有权限没理由必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);

        // ④ 有权限但理由不足 10 字 → 400，报文给出实际字数
        let req = skip_req(true, Some("  先放行  "));
        let gate = ApprovalGate::new(&req, &holder);
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate).expect_err("理由不足 10 字必须 400");
        assert_eq!(error.status, 400);
        assert!(
            error.msg.contains("当前 3 字"),
            "报文要给出实际字数（去空白后 3 字）：{}",
            error.msg
        );

        // ⑤ 有权限 + 理由齐全 → 放行，返回写日志用的合成理由
        let req = skip_req(true, Some("供应商现场验收完成，月底关账前先入库"));
        let gate = ApprovalGate::new(&req, &holder);
        let reason = authorize_missing_flow("STOCK_IN_POST", "stock_in_post", &gate).expect("有权限且理由齐全必须放行");
        assert!(
            reason.starts_with("跳过审批（未配置 stock_in_post 流程）："),
            "合成理由要能一眼看出「这是跳过审批」：{reason}"
        );
        assert!(reason.contains("供应商现场验收完成"), "用户理由要原样保留");

        // ⑥ 超管（`*` 通配）视同持有权限码
        let root = claims_with(&["*"]);
        let gate = ApprovalGate::new(&req, &root);
        assert!(approve(&gate), "SUPER_ADMIN 的通配权限必须短路放行（与路由层同口径）");

        // ⑦ 流程已发布时请求跳过 → 400（不许绕过已配置的内控）
        let error = ensure_not_skipped("po_approve", &gate).expect_err("已发布流程不许跳过");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("po_approve"));

        // ⑧ 系统内部路径（`ApprovalGate::none()`）：即便「请求了」也跳不了 → 409
        let system = ApprovalGate::none();
        assert!(!system.requested(), "系统路径不带跳过请求");
        assert!(!system.may_skip(), "系统路径没有权限声明");
        let error =
            authorize_missing_flow("VOUCHER_REVERSE", "voucher_reverse", &system).expect_err("系统路径必须 409");
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
        assert!(merged.starts_with("供应商召回｜跳过审批"), "业务理由在前：{merged}");
        let merged = merge_reason(None, "跳过审批（未配置 x 流程）：理由");
        assert_eq!(merged, "跳过审批（未配置 x 流程）：理由");
        let long = merge_reason(Some(&"甲".repeat(300)), "跳过审批");
        assert_eq!(long.chars().count(), MAX_MERGED_REASON_CHARS, "必须截断到 200 字");
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
            assert!(item.version_no.is_some(), "{} 必须有当前版本号", item.biz_type);
            assert!(item.has_snapshot, "{} 的当前版本必须有发布快照", item.biz_type);
            assert!(
                item.graph_ok,
                "{} 的图形校验必须通过：{:?}",
                item.biz_type, item.graph_issues
            );
        }
        assert_eq!(report.healthy, 8, "8 个流程必须全部「已发布 + 图形校验通过」");
        assert_eq!(report.unhealthy, 0);
        println!(
            "[DC-01] 审批流自检 {} / {} 健康（checked_at={}）",
            report.healthy, report.total, report.checked_at
        );
    }
}

// ==================== 补测（P2-3A）：审批进度 / 审批闸门的判据边界 ====================
//
// 评审实测：审批相关只有 `approval_gate.rs` 的 4 条 `#[test]`，而它是**六个业务入口
// 共用的一道闸**（采购提交 / 收货过账 / 收货作废 / 应付付款 / 应付作废 / 凭证红冲）。
// 判据写松一格 = 内控被静默绕过；写严一格 = 正常业务提交不了。两边都是生产事故。
//
// 已有 4 条覆盖「主路径」，本模块补**边界与契约**：
// - 理由字数的 9 / 10 字边界（按**字符**计，中文一字算一字）；
// - 合成理由的截断口径（200 字）与前缀可读性；
// - 四个错误出口的 status / code 与「报文里必须出现的东西」；
// - `ApprovalGate::none()`（系统内部路径）不能变成后门。
//
// 全部为纯逻辑用例（不连库），跑法：
//
// ```bash
// cargo test --lib business::erp::approval_gate
// ```
#[cfg(test)]
mod gate_boundary_tests {
    use super::*;
    use cloudpivot_kernel::shared::error_code;

    /// 造一份 claims（只关心 `permissions`）。
    fn claims_with(permissions: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: Uuid::now_v7().to_string(),
            username: Some("p23a_approval".to_string()),
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

    /// 持有跳过权限码的操作者。
    fn holder() -> TokenClaims {
        claims_with(&[PERM_SKIP_APPROVAL])
    }

    // ---------- ① 跳过请求片段 ----------

    /// 缺省（老客户端不传）= 没请求跳过；只有显式 `true` 才算请求。
    #[test]
    fn skip_req_default_is_no_request() {
        let default = SkipApprovalReq::default();
        assert!(!default.requested(), "缺省必须是「没请求」");
        assert_eq!(default.reason_text(), None);
        assert!(
            !skip_req(false, Some("理由写满了也一样")).requested(),
            "flag=false 不生效"
        );
        assert!(skip_req(true, None).requested());
    }

    /// 理由去空白；空串 / 纯空白 → `None`（不能算「写了理由」）。
    #[test]
    fn skip_req_reason_text_trims_and_blank_is_none() {
        assert_eq!(skip_req(true, Some("  现场已验收  ")).reason_text(), Some("现场已验收"));
        assert_eq!(skip_req(true, Some("")).reason_text(), None);
        assert_eq!(skip_req(true, Some("   \n\t ")).reason_text(), None);
        assert_eq!(skip_req(true, None).reason_text(), None);
    }

    // ---------- ② 闸门的能力声明 ----------

    /// 系统内部路径（`ApprovalGate::none()`）：不请求跳过、也没有任何权限 → 只能走 409。
    #[test]
    fn gate_none_has_no_skip_capability() {
        let gate = ApprovalGate::none();
        assert!(!gate.requested(), "系统路径不带跳过请求");
        assert!(!gate.may_skip(), "系统路径没有任何权限可声明");
        let error = authorize_missing_flow("PAYABLE_PAY", "payable_pay", &gate)
            .expect_err("系统路径必须 409（fail-closed 的核心）");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::APPROVAL_FLOW_NOT_CONFIGURED);
    }

    /// HTTP 路径：能力完全由 claims 决定（无权限码 → false，`*` 通配 → true）。
    #[test]
    fn gate_new_derives_capability_from_claims() {
        let req = skip_req(true, Some("月底关账，供应商已现场验收"));
        let nobody = claims_with(&["business:erp:purchase-order:status"]);
        let gate = ApprovalGate::new(&req, &nobody);
        assert!(gate.requested(), "请求了跳过");
        assert!(!gate.may_skip(), "没有权限码不能跳");

        let root = claims_with(&["*"]);
        let gate = ApprovalGate::new(&req, &root);
        assert!(gate.may_skip(), "SUPER_ADMIN 通配必须短路放行");
    }

    // ---------- ③ 理由字数边界（9 字拒 / 10 字放行） ----------

    /// 边界：9 字拒（报文给出「当前 9 字」），10 字放行 —— 差一个字就是能不能过内控。
    #[test]
    fn authorize_missing_flow_enforces_nine_vs_ten_chars() {
        let claims = holder();

        let nine = "一二三四五六七八九";
        assert_eq!(nine.chars().count(), MIN_SKIP_REASON_CHARS - 1);
        let req = skip_req(true, Some(nine));
        let gate = ApprovalGate::new(&req, &claims);
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate).expect_err("9 字必须拒");
        assert_eq!((error.status, error.code), (400, error_code::PARAM_INVALID));
        assert!(error.msg.contains("当前 9 字"), "报文必须给出实际字数：{}", error.msg);
        assert!(
            error.msg.contains(&MIN_SKIP_REASON_CHARS.to_string()),
            "报文必须给出下限：{}",
            error.msg
        );

        let ten = "一二三四五六七八九十";
        assert_eq!(ten.chars().count(), MIN_SKIP_REASON_CHARS);
        let req = skip_req(true, Some(ten));
        let gate = ApprovalGate::new(&req, &claims);
        let reason = authorize_missing_flow("PO_APPROVE", "po_approve", &gate).expect("10 字必须放行");
        assert!(reason.contains(ten), "理由原样带出：{reason}");
    }

    /// 字数按**字符**计：11 个汉字 = 11 字（不是 33 字节），带空白的不算数。
    #[test]
    fn authorize_missing_flow_counts_chars_not_bytes() {
        let claims = holder();
        // 「供」+ 9 个字符 = 10 字；字节数 20，若按字节判会被误拒。
        let ten_cjk = "供应商已受12345";
        assert_eq!(ten_cjk.chars().count(), 10);
        assert!(ten_cjk.len() > 10, "字节数必须大于 10 才有区分度");
        let req = skip_req(true, Some(ten_cjk));
        let gate = ApprovalGate::new(&req, &claims);
        assert!(
            authorize_missing_flow("PO_APPROVE", "po_approve", &gate).is_ok(),
            "10 个字符必须放行（按字符不按字节）"
        );

        // 前后空白不算：`"  一二三四五六七八九  "` 仍是 9 字。
        let req = skip_req(true, Some("  一二三四五六七八九  "));
        let gate = ApprovalGate::new(&req, &claims);
        let error = authorize_missing_flow("PO_APPROVE", "po_approve", &gate).expect_err("去空白后只有 9 字必须拒");
        assert!(error.msg.contains("当前 9 字"), "{}", error.msg);
    }

    /// 超长理由截断到 [`MAX_MERGED_REASON_CHARS`] 字，前缀（「跳过审批（未配置 x 流程）：」）完整保留。
    #[test]
    fn authorize_missing_flow_clamps_reason_to_limit() {
        let claims = holder();
        let long = "甲".repeat(MAX_MERGED_REASON_CHARS + 80);
        let req = skip_req(true, Some(&long));
        let gate = ApprovalGate::new(&req, &claims);
        let reason = authorize_missing_flow("VOUCHER_REVERSE", "voucher_reverse", &gate)
            .expect("超长理由应放行（截断而不是拒）");
        let tail = reason
            .split("）：")
            .nth(1)
            .expect("合成理由必须带「）：」分隔，日志才读得懂");
        assert_eq!(
            tail.chars().count(),
            MAX_MERGED_REASON_CHARS,
            "截断后必须恰好 {MAX_MERGED_REASON_CHARS} 字"
        );
        assert!(
            reason.starts_with("跳过审批（未配置 voucher_reverse 流程）："),
            "前缀必须保留：{reason}"
        );
    }

    // ---------- ④ 四条错误出口的报文契约 ----------

    /// 默认拒绝（409）的报文必须：点名 flow_code、说明「默认拒绝」、给出两条出路。
    #[test]
    fn flow_not_configured_message_offers_two_ways_out() {
        let req = skip_req(false, None);
        let nobody = claims_with(&[]);
        let gate = ApprovalGate::new(&req, &nobody);
        let error = authorize_missing_flow("STOCK_IN_POST", "stock_in_post", &gate).expect_err("没请求跳过必须 409");
        assert_eq!(
            (error.status, error.code),
            (409, error_code::APPROVAL_FLOW_NOT_CONFIGURED)
        );
        assert!(error.msg.contains("stock_in_post"), "{}", error.msg);
        assert!(error.msg.contains("STOCK_IN_POST"), "业务类型也要点名：{}", error.msg);
        assert!(error.msg.contains("①"), "出路①（发布流程）：{}", error.msg);
        assert!(error.msg.contains("②"), "出路②（授权跳过）：{}", error.msg);
        assert!(error.msg.contains(PERM_SKIP_APPROVAL), "{}", error.msg);
    }

    /// 403 的报文必须点名缺哪个权限码，并说明「不能自助开启」。
    #[test]
    fn skip_forbidden_message_names_the_permission_code() {
        let req = skip_req(true, Some("月底关账，供应商已现场验收"));
        let nobody = claims_with(&["business:erp:payable-payment:status"]);
        let gate = ApprovalGate::new(&req, &nobody);
        let error = authorize_missing_flow("PAYABLE_PAY", "payable_pay", &gate).expect_err("无权限码必须 403");
        assert_eq!((error.status, error.code), (403, error_code::PERMISSION_DENIED));
        assert!(error.msg.contains(PERM_SKIP_APPROVAL), "{}", error.msg);
        assert!(error.msg.contains("不能自助"), "{}", error.msg);
    }

    /// 已发布流程上请求跳过 → 400，且报文告诉用户「要跳过先下线流程」。
    #[test]
    fn ensure_not_skipped_explains_disabling_the_flow() {
        let claims = holder();
        let req = skip_req(true, Some("月底关账，供应商已现场验收"));
        let gate = ApprovalGate::new(&req, &claims);
        let error = ensure_not_skipped("stock_in_post", &gate).expect_err("已发布不许跳过");
        assert_eq!((error.status, error.code), (400, error_code::PARAM_INVALID));
        assert!(error.msg.contains("stock_in_post"), "{}", error.msg);
        assert!(error.msg.contains("下线"), "要指明明线：{}", error.msg);
    }

    /// 没请求跳过时，已发布流程的闸必须放行（这是六个入口的正常路径）。
    #[test]
    fn ensure_not_skipped_allows_normal_requests() {
        let req = skip_req(false, None);
        let nobody = claims_with(&[]);
        let gate = ApprovalGate::new(&req, &nobody);
        assert!(ensure_not_skipped("po_approve", &gate).is_ok());
        assert!(ensure_not_skipped("voucher_reverse", &gate).is_ok());
    }

    // ---------- ⑤ 理由合成 ----------

    /// 业务理由为空 / 纯空白时只留跳过标记（不能出现「｜跳过审批」这种残句）。
    #[test]
    fn merge_reason_ignores_blank_own_reason() {
        let skip = "跳过审批（未配置 po_approve 流程）：现场已验收";
        assert_eq!(merge_reason(None, skip), skip);
        assert_eq!(merge_reason(Some("   "), skip), skip);
        assert_eq!(merge_reason(Some(""), skip), skip);
        assert_eq!(
            merge_reason(Some("  供应商召回  "), skip),
            format!("供应商召回｜{skip}"),
            "业务理由要去空白且排在前"
        );
    }

    /// 合成后总长恰好截到 200 字（`biz_status_log.reason` 的写入口会判 400）。
    #[test]
    fn merge_reason_clamps_total_length() {
        let merged = merge_reason(Some(&"业".repeat(150)), &format!("跳过审批：{}", "由".repeat(150)));
        assert_eq!(
            merged.chars().count(),
            MAX_MERGED_REASON_CHARS,
            "合成理由必须恰好截到 {MAX_MERGED_REASON_CHARS} 字"
        );
        assert!(merged.starts_with("业业"), "业务理由在前：{merged}");
        let short = merge_reason(Some("理由"), "跳过审批");
        assert_eq!(short, "理由｜跳过审批");
    }

    // ---------- ⑥ 常量 / 清单契约 ----------

    /// 权限码是 `模块:资源:动作` 三段式，且两个码不能相同（授权/自检分开管）。
    #[test]
    fn permission_codes_are_scoped_and_distinct() {
        for code in [PERM_SKIP_APPROVAL, PERM_FLOW_HEALTH] {
            let parts: Vec<&str> = code.split(':').collect();
            assert!(parts.len() >= 3, "权限码必须是 {{模块}}:{{资源}}:{{动作}}：{code}");
            assert_eq!(parts[0], "business", "业务侧权限码前缀：{code}");
            assert_eq!(parts[1], "erp", "{code}");
        }
        assert_ne!(PERM_SKIP_APPROVAL, PERM_FLOW_HEALTH, "跳过审批与配置自检必须是两个码");
        assert_eq!(MIN_SKIP_REASON_CHARS, 10, "理由下限是 10 字（卡面契约）");
        assert_eq!(MAX_MERGED_REASON_CHARS, 200, "合成理由上限 200 字");
    }

    /// 8 个自检业务类型的 flow_code 不能重复（重复会让体检单漏报一个流程）。
    #[test]
    fn flow_type_codes_are_unique() {
        let mut biz_types: Vec<&str> = FLOW_TYPES.iter().map(|(biz, _)| *biz).collect();
        let mut flow_codes: Vec<&str> = FLOW_TYPES.iter().map(|(_, code)| *code).collect();
        assert_eq!(biz_types.len(), 8);
        biz_types.sort_unstable();
        biz_types.dedup();
        assert_eq!(biz_types.len(), 8, "biz_type 不能重复");
        flow_codes.sort_unstable();
        flow_codes.dedup();
        assert_eq!(flow_codes.len(), 8, "flow_code 不能重复");
        for (biz_type, flow_code) in FLOW_TYPES {
            assert!(
                !flow_code.chars().any(|c| c.is_ascii_uppercase()),
                "flow_code 一律小写（{biz_type} → {flow_code}）"
            );
        }
    }
}

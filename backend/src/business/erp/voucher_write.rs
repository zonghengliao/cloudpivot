//! 凭证红冲 + 手工新增凭证（批次 2 · P7）：把「只有自动凭证」的凭证域变成**可写**。
//!
//! # 为什么单开一个模块
//!
//! * [`crate::business::erp::voucher`]（P2-C1）是**自动凭证**的数据层：过账时生成、作废时红冲，
//!   红冲实现（`reverse_posted`）已经在那里 —— 本卡**复用**它，不重写（硬红线 6）；
//! * [`crate::business::erp::voucher_query`]（P2-D1）是只读查询层，文件头明写「只读」；
//!
//! 本卡新增的「手工凭证」与「红冲开关」属于凭证台账的**第二个写入口**，与 P5（`transfer_*` /
//! `task_bridge` / `error_code`）、P6（`stockin_model`）并行开工，新文件不产生冲突。
//!
//! # 接口（卡面 §二 ② + P0-FIX-4A）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/voucher/{id}/reverse` | `business:erp:voucher:reverse` | 红冲一张已记账凭证 |
//! | POST | `/business/erp/voucher` | `business:erp:voucher:create` | 手工新增凭证（落 **DRAFT**） |
//! | POST | `/business/erp/voucher/{id}/audit` | `business:erp:voucher:audit` | 审核 `DRAFT → AUDITED` |
//! | POST | `/business/erp/voucher/{id}/un-audit` | `business:erp:voucher:un-audit` | 反审核 `AUDITED → DRAFT`（reason 必填） |
//! | POST | `/business/erp/voucher/{id}/post` | `business:erp:voucher:post` | 记账 `AUDITED → POSTED` |
//!
//! 后三条是 P0-FIX-4A 的「双人复核链」，前端契约见
//! `frontend/src/views/business/erp/voucher/voucher-audit-api.ts`（P0-FIX-4B 先落）。
//! 全部回 [`VoucherDetailRes`]，前端拿它直接刷抽屉。
//!
//! # 红冲语义（与 P2-C1 同口径，复用 `voucher::reverse_posted`）
//!
//! ```text
//! ① 只能红冲 POSTED 且 is_red = 0 且 reversed_by IS NULL 的凭证
//! ② 本身就是红字凭证（is_red = 1）的不能再红冲
//! ③ 期间已结账（biz_accounting_period.status = CLOSED）的不能红冲
//! 动作：生成红字凭证（is_red=1、reverse_of=原凭证、每条分录借贷互换、独立号段 PZR）
//!       原凭证 status 保持 POSTED，只打 reversed_by
//! ```
//!
//! # 手工凭证语义
//!
//! ```text
//! ① 借贷必平衡（400，文案点名差额）
//! ② 每条分录 debit / credit 不能同时非零（也不能同时为 0）
//! ③ 摘要必填（分录摘要留空则继承单头摘要）
//! ④ 期间必须是 OPEN（期间不存在 / 已关闭都拒绝）
//! ⑤ 至少 2 条分录
//! 动作：status 落 **DRAFT**（P0-FIX-4A 起）—— 制单只是「写了一张单」，
//!       还要别人审核（AUDITED）再记账（POSTED）才进总账。
//!       DRAFT 凭证的借贷仍然必须平衡：库里的 CHECK 与 A1/A2 巡检对**所有状态**生效。
//! ```
//!
//! # 为什么改掉「制单即记账」（P0-FIX-4A）
//!
//! 改造前手工凭证落库即 `POSTED`，等于**制单人可以一个人把账做进总账**：
//! 没有复核、没有第二双眼睛，与 PRD E4-09（审核 / 反审核）E4-10（记账）两条 P0 正面冲突，
//! 也是审计必问的内控缺口（实测 21 张生产凭证全是 POSTED，一张草稿都没有）。
//! 现在制单（DRAFT）→ 审核（AUDITED）→ 记账（POSTED）三步分离，审核那一步由
//! [`check_self_audit`] 挡住「自己审自己」。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_voucher / biz_voucher_entry / biz_account / biz_accounting_period 是会计台账
//! 与全局字典（V0149 建表，无 dept_id / 归属人列），数据范围表达不出来；
//! 可见性由 business:erp:voucher:reverse / business:erp:voucher:create 权限码控制。
//! ```

use axum::Json;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Deserialize;

use crate::business::erp::approval_gate;
use crate::business::erp::separation;
use crate::business::erp::voucher::{self, AccountRef, EntryLine, ReversalInput};
use crate::business::erp::voucher_bridge;
use crate::business::erp::voucher_linkage;
use crate::business::erp::voucher_query::{self, VoucherDetailRes};
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 入参 ====================

/// 手工凭证的一条分录。
#[derive(Debug, Clone, Deserialize)]
pub struct ManualEntryReq {
    /// 科目 id（`biz_account.id`）。
    pub account_id: Option<String>,
    /// 借方金额（缺省 0）。
    pub debit: Option<Decimal>,
    /// 贷方金额（缺省 0）。
    pub credit: Option<Decimal>,
    /// 分录摘要（留空则继承单头摘要）。
    pub summary: Option<String>,
}

/// 手工新增凭证入参。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateVoucherReq {
    /// 凭证日期（缺省 = 今天，CST）。
    pub voucher_date: Option<NaiveDate>,
    /// 凭证摘要（必填）。
    pub summary: Option<String>,
    /// 分录（至少 2 条）。
    pub entries: Option<Vec<ManualEntryReq>>,
    /// 备注。
    pub remark: Option<String>,
}

/// 审核入参（P0-FIX-4A）：`remark` 是**可选**审核意见，写进凭证状态日志。
///
/// 与反审核的 `reason`（必填）区分开：审核是流程内的正常推进，意见是加分项；
/// 反审核是撤回已复核的结论，必须说明为什么。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditVoucherReq {
    /// 审核意见（可空）。
    pub remark: Option<String>,
}

/// 反审核入参（P0-FIX-4A）：`reason` **必填**（缺失 → 400）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UnAuditVoucherReq {
    /// 反审核原因（必填）。
    pub reason: Option<String>,
}

/// 记账入参（P0-FIX-4A）：契约是 `{}`，没有业务字段。
///
/// 之所以还要有结构体而不是「无 body」：前端 `post(url, {})` 会带 `Content-Type: application/json`
/// 与空对象，框架按 `Json<T>` 解析；用 `Option<Json<T>>` 兼容「完全不带 body」的调用方。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PostAuditedVoucherReq {}

/// 红冲入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReverseVoucherReq {
    /// 红冲原因（必填）。
    pub reason: Option<String>,
    /// 「显式跳过审批」片段（ARCH-3 DC-01）：`skip_approval` + `skip_reason`。
    #[serde(flatten, default)]
    pub approval: crate::business::erp::approval_gate::SkipApprovalReq,
}

// ==================== 错误收口 ====================

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：状态冲突（已红冲 / 红字凭证 / 期间已结账）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404：凭证不存在。
fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "凭证不存在或已删除".to_string())
}

/// 提交审批撞 `uk_biz_flow_instance_biz_active` → `409 + 本单号`（不是「单号重复、去查 biz_seq」）。
///
/// 判据与文案收在 [`flow_model::is_biz_active_duplicate`] / [`flow_model::approval_in_progress`]，
/// 与调拨单等兄弟模块同一份实现。只在**拿得到单号的入口**（发起凭证红冲审批）用它；
/// 其余出口走 [`write_error`] 的兜底分支（文案退化成「该凭证」）。
fn submit_conflict(err: DbError, ctx: &str, voucher_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(
            context = ctx,
            voucher_no,
            error = %err,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress("凭证", Some(voucher_no));
    }
    write_error(err, ctx)
}

/// 写路径的数据库错误收口。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    // ⚠️ 顺序要紧：审批实例的唯一索引冲突**先**认出来。
    // 落到下面的 `UniqueViolation` 分支会被说成「凭证号重复（唯一约束
    // uk_biz_flow_instance_biz_active）…联系运维排查 biz_seq」—— 用户按提示去查取号器，
    // 方向完全错（真正原因是这张凭证已经有一个在跑的红冲审批单）。
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(context = ctx, error = %err, "撞审批实例唯一索引：已有在跑实例，翻 409");
        return flow_model::approval_in_progress("凭证", None);
    }
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "凭证号重复（唯一约束{}）：取号器与库里的号段不一致，请联系运维排查 biz_seq",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足库约束 {}（借贷必须平衡；分录单边；分录行数 ≥ 2；红字凭证必须指向原凭证）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        DbError::InvalidValue(message) => bad_request(message),
        other => utils::db_error(other, ctx),
    }
}

/// 路径里的 UUID：非法 → `400 + 66006`。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

/// 操作人。
fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

// ==================== ① 红冲 ====================

/// 红冲一张已记账凭证（handler 与单测共用）。
///
/// 校验全部在**行锁内**做（`SELECT ... FOR UPDATE`）：并发两次红冲只有一次能过，
/// 第二次看到的是 `reversed_by` 已回写的那一行。
pub async fn reverse_voucher(
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<VoucherDetailRes, ApiError> {
    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    let Some(reason) = reason else {
        return Err(bad_request(
            "reason 必填：请写明红冲原因（红字凭证的分录摘要会带这段文字）",
        ));
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启红冲事务"))?;
    let result = reverse_in_tx(&tx, id, reason, operator, now).await;
    let (red_id, change) = match result {
        Ok(pair) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交红冲事务"))?;
            pair
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    // 提交后动作（通知 / 建任务）：永不回滚业务
    voucher_linkage::run_deferred_after_commit(change).await;
    voucher_query::voucher_detail(red_id).await
}

/// 红冲的事务体（返回红字凭证 id + 联动结果）。
async fn reverse_in_tx(
    tx: &Transaction,
    id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<(Uuid, voucher_linkage::VoucherStatusChange), ApiError> {
    let original = voucher::find_voucher_locked(tx, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定凭证"))?
        .ok_or_else(not_found)?;

    if original.is_red == voucher::IS_RED_YES {
        return Err(conflict(format!(
            "凭证 {} 本身是红字凭证，红字凭证不能再红冲（要更正请直接补一张蓝字凭证）",
            original.voucher_no
        )));
    }
    if original.status != voucher::STATUS_POSTED {
        return Err(conflict(format!(
            "凭证 {} 当前状态是 {}，只有已记账（POSTED）的凭证能红冲",
            original.voucher_no, original.status
        )));
    }
    if let Some(red_id) = original.reversed_by {
        let red_no = voucher::find_voucher(tx, red_id)
            .await
            .map_err(|error| utils::db_error(error, "查询已存在的红字凭证"))?
            .map(|row| row.voucher_no)
            .unwrap_or_else(|| red_id.to_string());
        return Err(conflict(format!(
            "该凭证 {} 已经红冲（红字凭证 {red_no}），不能重复红冲",
            original.voucher_no
        )));
    }
    if voucher::is_period_closed(tx, &original.period)
        .await
        .map_err(|error| utils::db_error(error, "查询会计期间状态"))?
    {
        return Err(conflict(format!(
            "凭证 {} 所属期间 {} 已结账，不能再红冲（请先反结账）",
            original.voucher_no, original.period
        )));
    }

    let red_id = voucher::reverse_posted(
        tx,
        &original,
        ReversalInput {
            reason: Some(reason),
            voucher_date: voucher::today_cst(now),
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "红冲凭证"))?;

    // ② 联动（LNK-06）：红字凭证落库即 `POSTED`，同样是一次「凭证过账」——
    //    `VOUCHER/POSTED` 上的规则（通知 / 建任务）不能因为它是红字就静默不响。
    let red = voucher::find_voucher(tx, red_id)
        .await
        .map_err(|error| utils::db_error(error, "读取红字凭证"))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("红字凭证 {red_id} 刚落库却读不到（同一事务内）"),
            )
        })?;
    let change = voucher_linkage::run_in_tx(tx, &red, voucher::STATUS_POSTED, operator).await?;
    Ok((red_id, change))
}

// ==================== ③ 改凭证摘要（P10 入账后只读）====================

/// 改凭证摘要（**入账后只读闸**的凭证侧落点）。
///
/// 卡面 §二 ③ 把 `VOUCHER: POSTED` 列入「入账态」：已记账凭证的摘要属于会计档案的
/// 描述字段，改了它账证虽不差钱，但会与已出的报表 / 归档 PDF 不一致 ——
/// 统一按「先红冲再重开」处理（**409**，文案点「已入账」）。
///
/// 只有 `DRAFT` 状态的凭证允许改摘要（`AUDITED` 已复核、`POSTED` 已入账，都锁）：
/// 审核之后改摘要 = 复核人看的是另一份数据（审 A 改 B），记账之后改 = 与已出报表不一致，
/// 统一按「先反审核 / 先红冲，再改」处理（**409 + 入账态文案**，由 `ensure_writable` 给出）。
pub async fn update_summary(
    id: Uuid,
    summary: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let summary = summary.map(str::trim).filter(|text| !text.is_empty());
    let Some(summary) = summary else {
        return Err(bad_request("summary 不能为空：凭证摘要是会计档案的一部分"));
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改凭证事务"))?;
    let row = voucher::find_voucher_locked(&tx, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定凭证"))?
        .ok_or_else(not_found)?;
    // 入账后只读：POSTED → 409（文案点「已入账」）
    state_machine::ensure_writable(state_machine::ENTITY_VOUCHER, &row.status, &row.voucher_no)?;
    let now = Utc::now();
    tx.exec_sql(
        "UPDATE biz_voucher SET summary = ?, updated_at = ?, \
            update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(summary.to_string()),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "改凭证摘要"))?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交改凭证事务"))?;
    Ok(())
}

// ==================== ④ 审核链（P0-FIX-4A）：DRAFT → AUDITED → POSTED ====================
//
// 三个动作（审核 / 反审核 / 记账）**只走这一条路**：行锁取凭证 → 状态核对 →
// 状态机 `transition()`（校验边 + reason + 写 `biz_status_log`）→ 改 `biz_voucher.status`。
// 没有第二个写状态的地方 —— 谁想「顺手 UPDATE 一下状态」，会立刻被
// `erp_invariants` 的「状态日志与凭证状态对不上」抓出来。

/// 审核链一次状态变更的入参（三条边共用的事务体入参）。
struct VoucherTransition<'a> {
    /// 凭证 id。
    id: Uuid,
    /// 期望的当前状态（不是它 → 409，文案说明该先做什么）。
    expect: &'a str,
    /// 目标状态（必须是一条注册过的边，否则状态机自己会 409）。
    to: &'a str,
    /// 操作人。
    operator: Option<Uuid>,
    /// 原因（反审核必填；审核时是可选意见）。
    reason: Option<&'a str>,
    /// 中文动作名（拼错误文案）。
    action: &'a str,
    /// 这个动作的前提（拼错误文案）。
    precondition: &'a str,
}

/// 状态的中文名（错误文案用；未知值原样返回，不假装认识）。
fn status_label(status: &str) -> &str {
    match status {
        voucher::STATUS_DRAFT => "草稿",
        voucher::STATUS_AUDITED => "已审核",
        voucher::STATUS_POSTED => "已记账",
        other => other,
    }
}

/// 「当前能做什么」的提示（从状态机注册表现算，不写死 —— 边表改了文案自动跟着走）。
fn next_actions(status: &str) -> String {
    let next = state_machine::Entity::Voucher.next_statuses(status);
    if next.is_empty() {
        return "没有可选动作（该状态是终态，要更正请红冲后重开）".to_string();
    }
    next.iter()
        .map(|to| {
            format!(
                "{}（{status} → {to}）",
                match *to {
                    voucher::STATUS_AUDITED => "审核",
                    voucher::STATUS_DRAFT => "反审核",
                    voucher::STATUS_POSTED => "记账",
                    other => other,
                }
            )
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 自审闸（**纯函数**，便于单测钉死）：制单人不能审核自己制的凭证 → `403 + 65019`。
///
/// # 为什么是 403 而不是 409
///
/// 与仓里所有「自审」闸同口径（`purchase_bridge::guard_self_approval` /
/// `payable_bridge::guard_self_approval` / `flow::assignee` 的 `FORBID`）：
/// 这是**权限不足**（你没有资格审这一张），不是「参数冲突」。
///
/// # 为什么仍然受 `erp.separation_of_duties.approve` 开关约束
///
/// 复用的是 ERP-B2 的既有内控设施（默认 **开** = 拦），不是另造一套：
/// 开关在「系统参数 / ERP 内控」里可见可审计，只有管理员显式关掉才放行本人自审
/// （一人财务岗的小团队确实需要，这也是那个开关存在的原因）。
/// 关掉这件事会留在参数变更日志里，比藏一个 code 里的常量更可追溯。
fn check_self_audit(
    switch_on: bool,
    creator: Option<Uuid>,
    operator: Option<Uuid>,
    voucher_no: &str,
) -> std::result::Result<(), ApiError> {
    if !switch_on {
        return Ok(());
    }
    // 操作人未知（定时任务 / 无令牌）→ 没有「自己」可言
    let Some(operator) = operator else {
        return Ok(());
    };
    if creator != Some(operator) {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
        format!(
            "不能审核自己制作的凭证：凭证 {voucher_no} 由你本人制作（内控要求制单与审核分离），\
             请交给其他有审核权限的账号处理{}",
            separation::approval_hint(separation::Duty::Approve)
        ),
    ))
}

/// 审核链的事务体：行锁 → 状态核对 → 内控闸 → 状态机 → 改状态列 → 联动（LNK-06）。
async fn transition_voucher_in_tx(
    tx: &Transaction,
    input: VoucherTransition<'_>,
) -> std::result::Result<voucher_linkage::VoucherStatusChange, ApiError> {
    let row = voucher::find_voucher_locked(tx, input.id)
        .await
        .map_err(|error| utils::db_error(error, "锁定凭证"))?
        .ok_or_else(not_found)?;

    // ① 当前状态必须是这条边的起点（否则 409，并告诉用户现在到底能做什么）
    if row.status != input.expect {
        return Err(conflict(format!(
            "凭证 {} 当前是「{}」，不能{}：{}。当前可选动作：{}",
            row.voucher_no,
            status_label(&row.status),
            input.action,
            input.precondition,
            next_actions(&row.status)
        )));
    }

    // ② 内控闸（只对「审核」）：制单人不能审核自己的凭证
    if input.to == voucher::STATUS_AUDITED {
        let switch_on = separation::enabled(separation::Duty::Approve).await;
        let creator = voucher::creator_of(tx, row.id)
            .await
            .map_err(|error| utils::db_error(error, "查询凭证制单人"))?;
        check_self_audit(switch_on, creator, input.operator, &row.voucher_no)?;
    }

    // ③ 记账前再查一次期间：制单时开着、审核后才封账的月份不许再记账
    if input.to == voucher::STATUS_POSTED
        && voucher::is_period_closed(tx, &row.period)
            .await
            .map_err(|error| utils::db_error(error, "查询会计期间状态"))?
    {
        return Err(conflict(format!(
            "凭证 {} 所属期间 {} 已结账，不能再记账：已封账的月份只读，\
             请先反结账（或在下一个未结账期间重开这张凭证）",
            row.voucher_no, row.period
        )));
    }

    // ④ 唯一状态变更入口：校验边（含配置层收窄）、校验必填 reason、写 biz_status_log
    state_machine::transition(
        tx,
        state_machine::ENTITY_VOUCHER,
        row.id,
        &row.voucher_no,
        &voucher::LEGAL_EDGES,
        input.expect,
        input.to,
        input.operator,
        input.reason,
        None,
    )
    .await?;

    // ⑤ 改状态列。记账时同时盖「记账时间 / 记账人」戳；反审核把戳清掉
    //    （回到草稿的凭证不该留着「已记账」的痕迹），审核不动这两个戳。
    //
    // 三条分支各自拼 SQL + 参数（**同序**），不做「先拼一条再 insert 参数」——那种写法
    // 一旦有人加字段就会错位，而且错位后是「静默写错值」而不是编译错。
    let now = Utc::now();
    let operator_text = input.operator.map(|v| v.to_string()).unwrap_or_default();
    let (sql, params) = match input.to {
        voucher::STATUS_POSTED => (
            "UPDATE biz_voucher SET status = ?, posted_at = CURRENT_TIMESTAMP, \
                posted_by = NULLIF(?::text,'')::uuid, updated_at = ?, \
                update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(input.to.to_string()),
                Value::Text(operator_text),
                Value::from(now),
                Value::Text(input.operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&row.id),
            ],
        ),
        voucher::STATUS_DRAFT => (
            "UPDATE biz_voucher SET status = ?, posted_at = NULL, posted_by = NULL, \
                updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(input.to.to_string()),
                Value::from(now),
                Value::Text(operator_text),
                Value::from(&row.id),
            ],
        ),
        // AUDITED：既不盖戳也不清戳（从 DRAFT 审核上来时本来就没有戳）
        _ => (
            "UPDATE biz_voucher SET status = ?, updated_at = ?, \
                update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(input.to.to_string()),
                Value::from(now),
                Value::Text(operator_text),
                Value::from(&row.id),
            ],
        ),
    };
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    tx.exec_sql(sql, params)
        .await
        .map_err(|error| utils::db_error(error, "回写凭证状态"))?;

    // ⑥ 联动（LNK-06）：这里是凭证状态列的**唯一写入点**，所以审核（DRAFT → AUDITED）、
    //    反审核（AUDITED → DRAFT）、记账（AUDITED → POSTED）三条边共用这一处接线。
    //    排除清单见 `spec::VOUCHER_EXCLUDED_ACTIONS`：在凭证上跑 GENERATE_VOUCHER 是
    //    「凭证生凭证」的死循环（运行时排除 + 配置期 400 双闸）。
    voucher_linkage::run_in_tx(tx, &row, input.to, input.operator).await
}

/// 审核（`DRAFT → AUDITED`，PRD E4-09）。
///
/// # 异常情况
///
/// * 404 + `10002`：凭证不存在 / 已删除；
/// * 409 + `10003`：不是草稿（已审核 / 已记账）；
/// * 403 + `65019`：制单人自己来审核（内控：制单 ≠ 审核）。
pub async fn audit_voucher(
    id: Uuid,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<VoucherDetailRes, ApiError> {
    let reason = remark.map(str::trim).filter(|text| !text.is_empty());
    run_voucher_transition(VoucherTransition {
        id,
        expect: voucher::STATUS_DRAFT,
        to: voucher::STATUS_AUDITED,
        operator,
        reason,
        action: "审核",
        precondition: "只有草稿（DRAFT）状态的凭证能审核",
    })
    .await
}

/// 反审核（`AUDITED → DRAFT`，PRD E4-09）：**原因必填**，写进 `biz_status_log`。
///
/// # 异常情况
///
/// * 400 + `10001`：没给原因（状态机的必填 reason 边拦下）；
/// * 404 + `10002`：凭证不存在 / 已删除；
/// * 409 + `10003`：不是已审核（草稿 / 已记账都拦 —— 已记账只能红冲）。
pub async fn un_audit_voucher(
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<VoucherDetailRes, ApiError> {
    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    let Some(reason) = reason else {
        return Err(bad_request(
            "reason 必填：请写明反审核原因（会写进凭证状态日志，审计要能查到为什么把已审核打回草稿）",
        ));
    };
    run_voucher_transition(VoucherTransition {
        id,
        expect: voucher::STATUS_AUDITED,
        to: voucher::STATUS_DRAFT,
        operator,
        reason: Some(reason),
        action: "反审核",
        precondition: "只有已审核（AUDITED）的凭证能反审核；已记账的只能红冲",
    })
    .await
}

/// 记账（`AUDITED → POSTED`，PRD E4-10）：记完进总账 / 报表，**不可再反审核**。
///
/// # 异常情况
///
/// * 404 + `10002`：凭证不存在 / 已删除；
/// * 409 + `10003`：不是已审核（草稿要先审核；已记账无需再记）；
/// * 409 + `10003`：所属期间已结账。
pub async fn post_audited_voucher(id: Uuid, operator: Option<Uuid>) -> std::result::Result<VoucherDetailRes, ApiError> {
    run_voucher_transition(VoucherTransition {
        id,
        expect: voucher::STATUS_AUDITED,
        to: voucher::STATUS_POSTED,
        operator,
        reason: None,
        action: "记账",
        precondition: "只有已审核（AUDITED）的凭证能记账；草稿要先审核（制单 ≠ 审核）",
    })
    .await
}

/// 审核链的公共外壳：开事务 → 事务体 → 提交 → 提交后联动（通知 / 建任务）→ 回详情。
async fn run_voucher_transition(input: VoucherTransition<'_>) -> std::result::Result<VoucherDetailRes, ApiError> {
    let id = input.id;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启凭证状态变更事务"))?;
    let change = match transition_voucher_in_tx(&tx, input).await {
        Ok(change) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交凭证状态变更事务"))?;
            change
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    // 提交后动作（通知 / 建任务）：永不回滚业务（账已落定，一封信失败不该把过账退回去）
    voucher_linkage::run_deferred_after_commit(change).await;
    voucher_query::voucher_detail(id).await
}

// ==================== ② 手工新增凭证 ====================

/// 手工新增凭证（handler 与单测共用）：校验 → 科目快照 → 落库（**DRAFT，待复核**）。
///
/// 落 `DRAFT` 而不是 `POSTED`（P0-FIX-4A）：手工凭证要经过「审核 → 记账」两步才进总账，
/// 制单人一个人推不进账本。自动凭证（收货 / 付款 / 红冲 / 年结）仍由 `post_voucher`
/// 直接落 `POSTED`，它们的复核在各自的审批链上。
pub async fn create_manual_voucher(
    req: &CreateVoucherReq,
    operator: Option<Uuid>,
) -> std::result::Result<VoucherDetailRes, ApiError> {
    let summary = req
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("summary 必填：凭证摘要"))?
        .to_string();
    let entries = req
        .entries
        .as_deref()
        .ok_or_else(|| bad_request("entries 必填：手工凭证至少要有 2 条分录"))?;
    if entries.len() < 2 {
        return Err(bad_request(format!(
            "手工凭证至少要 2 条分录（当前 {} 条），会计上不构成一张凭证",
            entries.len()
        )));
    }

    // ① 纯逻辑校验（金额符号 / 单边 / 合计平衡）—— 不碰库，先拦住绝大多数脏输入
    let (raw, debit_total) = check_entries(entries, &summary)?;

    let now = Utc::now();
    let voucher_date = req.voucher_date.unwrap_or_else(|| voucher::today_cst(now));

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启手工凭证事务"))?;
    let result = manual_in_tx(
        &tx,
        ManualVoucher {
            raw,
            debit_total,
            voucher_date,
            summary,
            remark: req.remark.as_deref(),
            operator,
            now,
        },
    )
    .await;
    let (voucher_id, change) = match result {
        Ok(pair) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交手工凭证事务"))?;
            pair
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    // 提交后动作（通知 / 建任务）：永不回滚业务
    voucher_linkage::run_deferred_after_commit(change).await;
    voucher_query::voucher_detail(voucher_id).await
}

/// 一条「待补科目快照」的分录（校验后、查库前）。
#[derive(Debug, Clone)]
struct RawLine {
    /// 借方金额。
    debit: Decimal,
    /// 贷方金额。
    credit: Decimal,
    /// 分录摘要（已继承单头摘要）。
    summary: String,
    /// 科目 id。
    account_id: String,
}

/// 纯逻辑校验：单边 / 非负 / ≥2 条 / 借贷平衡。返回分录与借方合计。
fn check_entries(
    entries: &[ManualEntryReq],
    voucher_summary: &str,
) -> std::result::Result<(Vec<RawLine>, Decimal), ApiError> {
    if entries.len() < 2 {
        return Err(bad_request(format!(
            "手工凭证至少要 2 条分录（当前 {} 条），会计上不构成一张凭证",
            entries.len()
        )));
    }
    let mut lines = Vec::with_capacity(entries.len());
    let mut debit_total = Decimal::ZERO;
    let mut credit_total = Decimal::ZERO;
    for (index, entry) in entries.iter().enumerate() {
        let debit = entry.debit.unwrap_or(Decimal::ZERO).round_dp(4);
        let credit = entry.credit.unwrap_or(Decimal::ZERO).round_dp(4);
        if debit < Decimal::ZERO || credit < Decimal::ZERO {
            return Err(bad_request(format!(
                "entries[{index}] 金额不能是负数（借 {debit} / 贷 {credit}）：\
                 红冲请走「红冲」按钮，不要在分录里写负数"
            )));
        }
        if debit > Decimal::ZERO && credit > Decimal::ZERO {
            return Err(bad_request(format!(
                "entries[{index}] 借贷不能同时有值（借 {debit} / 贷 {credit}）：一条分录只能是单边"
            )));
        }
        if debit == Decimal::ZERO && credit == Decimal::ZERO {
            return Err(bad_request(format!(
                "entries[{index}] 借贷金额都是 0：空分录不允许（后端也会被 \
                 ck_biz_voucher_entry_side 拦住）"
            )));
        }
        let account_id = entry
            .account_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| bad_request(format!("entries[{index}].account_id 必填")))?;
        let line_summary = entry
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(voucher_summary)
            .to_string();
        debit_total += debit;
        credit_total += credit;
        lines.push(RawLine {
            debit,
            credit,
            summary: line_summary,
            account_id: account_id.to_string(),
        });
    }

    let debit_total = debit_total.round_dp(4);
    let credit_total = credit_total.round_dp(4);
    if debit_total != credit_total {
        let diff = (debit_total - credit_total).round_dp(4);
        return Err(bad_request(format!(
            "借贷不平衡：借方合计 {debit_total} / 贷方合计 {credit_total}，差额 {diff}"
        )));
    }
    if debit_total <= Decimal::ZERO {
        return Err(bad_request("借贷合计不能为 0"));
    }
    Ok((lines, debit_total))
}

/// 手工凭证事务的全部入参（结构体化，避开 `clippy::too_many_arguments`）。
struct ManualVoucher<'a> {
    /// 待补科目快照的分录。
    raw: Vec<RawLine>,
    /// 借方合计（= 贷方合计）。
    debit_total: Decimal,
    /// 凭证日期。
    voucher_date: NaiveDate,
    /// 单头摘要。
    summary: String,
    /// 备注。
    remark: Option<&'a str>,
    /// 操作人。
    operator: Option<Uuid>,
    /// 业务时间。
    now: nik::prelude::chrono::DateTime<Utc>,
}

/// 手工凭证的事务体：期间校验 → 科目快照 → 落库 → 联动（LNK-06）。
async fn manual_in_tx(
    tx: &Transaction,
    input: ManualVoucher<'_>,
) -> std::result::Result<(Uuid, voucher_linkage::VoucherStatusChange), ApiError> {
    let ManualVoucher {
        raw,
        debit_total,
        voucher_date,
        summary,
        remark,
        operator,
        now,
    } = input;
    let period = voucher::period_of(tx, voucher_date)
        .await
        .map_err(|error| utils::db_error(error, "查询会计期间"))?;
    match voucher::period_status(tx, &period)
        .await
        .map_err(|error| utils::db_error(error, "查询会计期间状态"))?
        .as_deref()
    {
        Some(voucher::PERIOD_OPEN) => {}
        // ERP-P1（卡面 §一 ②）：已结账期间记账是 **400**「期间已结账」。
        // 与 `voucher::post_voucher` 里那道闸文案一致 —— 这里提前一步拦，
        // 是为了在「还没开始拼分录」时就给出同一个 400，而不是让调用方看到
        // 一条「状态是 CLOSED，只能记到已开启期间」的 409（语义偏成「参数冲突」）。
        Some(voucher::PERIOD_CLOSED) => {
            return Err(bad_request(format!(
                "会计期间 {period} 已结账，不能再生成凭证（凭证日期 {voucher_date}）：\
                 已封账的月份只读，请在下一个未结账的期间做调整（或先反结账）"
            )));
        }
        Some(other) => {
            return Err(conflict(format!(
                "会计期间 {period} 当前是 {other}，手工凭证只能记到「已开启（OPEN）」的期间：\
                 请先在「会计期间」页开启该期间"
            )));
        }
        None => {
            return Err(conflict(format!(
                "会计期间 {period} 不存在（凭证日期 {voucher_date}），手工凭证只能记到「已开启（OPEN）」\
                 的期间：请先在「会计期间」页创建并开启该期间"
            )));
        }
    }

    let mut lines = Vec::with_capacity(raw.len());
    for (index, line) in raw.iter().enumerate() {
        let account_id = parse_uuid(&line.account_id, &format!("entries[{index}].account_id"))?;
        let account: AccountRef = voucher::account_by_id(tx, account_id)
            .await
            .map_err(|error| utils::db_error(error, "查询科目"))?
            .ok_or_else(|| {
                ApiError::new(
                    400,
                    error_code::ERP_REFERENCE_MISSING,
                    format!("entries[{index}].account_id 科目不存在 / 已停用 / 已删除（{account_id}）"),
                )
            })?;
        lines.push(EntryLine {
            line_no: i32::try_from(index + 1).unwrap_or(0),
            summary: line.summary.clone(),
            account,
            debit: line.debit,
            credit: line.credit,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }

    let source_no = format!("手工凭证 {debit_total}");
    let voucher_id = voucher::post_voucher(
        tx,
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_MANUAL,
            source_type: voucher::SOURCE_MANUAL,
            source_id: None,
            source_no: Some(&source_no),
            source_doc_no: None,
            voucher_date,
            summary,
            lines,
            remark: remark.map(str::trim).filter(|text| !text.is_empty()),
            red_of: None,
            status: crate::business::erp::voucher::STATUS_DRAFT,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成手工凭证"))?;

    // ② 联动（LNK-06）：手工凭证落 `DRAFT` 也是一次「状态推进」—— `DRAFT` 是能走到的状态
    //    （反审核 / 手工新增都会到它），不接这一条 `VOUCHER/DRAFT` 上的规则就成了
    //    「能存能启永远不触发」（P1-FIX-5C 的原病）。
    let created = voucher::find_voucher(tx, voucher_id)
        .await
        .map_err(|error| utils::db_error(error, "读取新建的手工凭证"))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "手工凭证刚落库却读不到（同一事务内）".to_string(),
            )
        })?;
    let change = voucher_linkage::run_in_tx(tx, &created, voucher::STATUS_DRAFT, operator).await?;
    Ok((voucher_id, change))
}

// ==================== 接口层 ====================

// ==================== P-FLOW-3B：凭证红冲审批 ====================

/// 红冲审批的业务类型（与 V0191 种子 / `voucher_bridge::BIZ_TYPE` 同口径）。
pub const REVERSE_BIZ_TYPE: &str = "VOUCHER_REVERSE";
/// 红冲审批的流程编码。
pub const REVERSE_FLOW_CODE: &str = "voucher_reverse";

/// 红冲提交的 outcome：已送审（凭证仍是 `POSTED`，只挂 pending）。
pub const OUTCOME_REVERSE_PENDING: &str = "PENDING_APPROVAL";
/// 红冲提交的 outcome：未配流程，按老行为**直接红冲**。
pub const OUTCOME_REVERSED: &str = "REVERSED";

/// 红冲提交的出参。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReverseSubmitRes {
    /// `PENDING_APPROVAL`（已送审，账未动） / `REVERSED`（未配流程，直接红冲）。
    pub outcome: String,
    /// 给用户看的一句话。
    pub message: String,
    /// 审批实例 id（未配流程时为 `None`）。
    pub approval_instance_id: Option<String>,
    /// 凭证最新详情（送审时是**原凭证**，直接红冲时是**红字凭证**）。
    #[serde(flatten)]
    pub detail: VoucherDetailRes,
}

/// 提交凭证红冲（**唯一对外入口**，P-FLOW-3B + ARCH-3 DC-01）。
///
/// 配了已发布的 `voucher_reverse` 流程 → 先送审（**不改状态**，只挂
/// `pending_reverse_instance_id`；报表口径不变）；**没配流程 → 409 +
/// `APPROVAL_FLOW_NOT_CONFIGURED`（默认拒绝）** —— 只有持 `business:erp:skip-approval`
/// 且写明理由（≥10 字）的账号才能显式跳过，跳过理由与红冲原因合并后：
/// ① 进红字凭证的分录摘要 / 备注；② 进红字凭证的 `biz_status_log` 建单留痕
/// （`红字冲销凭证｜跳过审批（未配置 voucher_reverse 流程）：…`）。
///
/// 为什么入口收在这里、而不是让「红冲」和「送审」各开一个接口：
/// 那样审批可以被换一个 URL 绕过（与 `stockin_handler::submit_post` 同一个理由）。
pub async fn submit_reverse(
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    gate: &approval_gate::ApprovalGate<'_>,
) -> std::result::Result<ReverseSubmitRes, ApiError> {
    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    let Some(reason) = reason else {
        return Err(bad_request(
            "reason 必填：请写明红冲原因（红字凭证的分录摘要会带这段文字）",
        ));
    };
    match crate::business::erp::voucher_bridge::published_reverse_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询凭证红冲审批流程"))?
    {
        Some(flow) => {
            approval_gate::ensure_not_skipped(voucher_bridge::FLOW_CODE, gate)?;
            // 送审前先把「能不能红冲」的账务闸全部走一遍（别把注定失败的单送给审批人）
            ensure_reversible(id).await?;
            submit_reverse_for_approval(id, reason, operator, &flow).await
        }
        None => {
            // ARCH-3 DC-01：**不再静默直接红冲**。
            let skip_reason =
                approval_gate::authorize_missing_flow(voucher_bridge::BIZ_TYPE, voucher_bridge::FLOW_CODE, gate)?;
            tracing::warn!(
                voucher_id = %id,
                %skip_reason,
                "凭证红冲：没有已发布的 voucher_reverse 流程，按「显式跳过审批」直接红冲"
            );
            let merged = approval_gate::merge_reason(Some(reason), &skip_reason);
            let detail = reverse_voucher(id, Some(&merged), operator).await?;
            Ok(ReverseSubmitRes {
                outcome: OUTCOME_REVERSED.to_string(),
                message: "未配置凭证红冲审批流程，本次红冲已按「显式跳过审批」直接生效".to_string(),
                approval_instance_id: None,
                detail,
            })
        }
    }
}

/// 红冲前的账务闸（与 [`reverse_in_tx`] 逐条一致，只是**不加锁**、不写库）。
///
/// 送审时先跑一遍：注定会被拒的单（红字凭证 / 已红冲 / 期间已结账）不该占用财务经理的待办。
async fn ensure_reversible(id: Uuid) -> std::result::Result<(), ApiError> {
    let original = voucher::find_voucher(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询凭证"))?
        .ok_or_else(not_found)?;
    if original.is_red == voucher::IS_RED_YES {
        return Err(conflict(format!(
            "凭证 {} 本身是红字凭证，红字凭证不能再红冲（要更正请直接补一张蓝字凭证）",
            original.voucher_no
        )));
    }
    if original.status != voucher::STATUS_POSTED {
        return Err(conflict(format!(
            "凭证 {} 当前状态是 {}，只有已记账（POSTED）的凭证能红冲",
            original.voucher_no, original.status
        )));
    }
    if let Some(red_id) = original.reversed_by {
        let red_no = voucher::find_voucher(db(), red_id)
            .await
            .map_err(|error| utils::db_error(error, "查询已存在的红字凭证"))?
            .map(|row| row.voucher_no)
            .unwrap_or_else(|| red_id.to_string());
        return Err(conflict(format!(
            "该凭证 {} 已经红冲（红字凭证 {red_no}），不能重复红冲",
            original.voucher_no
        )));
    }
    if let Some(pending) = original.pending_reverse_instance_id {
        let instance_no = instance_no_of(pending).await?;
        return Err(conflict(format!(
            "凭证 {} 已在红冲审批中（{instance_no}）：请等审批结束，或先撤回审批",
            original.voucher_no
        )));
    }
    if voucher::is_period_closed(db(), &original.period)
        .await
        .map_err(|error| utils::db_error(error, "查询会计期间状态"))?
    {
        return Err(conflict(format!(
            "凭证 {} 所属期间 {} 已结账，不能再红冲（请先反结账）",
            original.voucher_no, original.period
        )));
    }
    Ok(())
}

/// 发起红冲审批（**不改凭证状态**，只挂 `pending_reverse_instance_id`）。
async fn submit_reverse_for_approval(
    id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    flow: &crate::business::erp::voucher_bridge::VoucherFlow,
) -> std::result::Result<ReverseSubmitRes, ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交红冲审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let original = voucher::find_voucher(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询凭证"))?
        .ok_or_else(not_found)?;
    // 幂等：同一凭证已有在跑的红冲审批 → 409（先查，别等库层
    // uk_biz_flow_instance_biz_active）
    if flow_model::biz_instance_running(REVERSE_BIZ_TYPE, original.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = instance_no_of(original.pending_reverse_instance_id.unwrap_or_else(Uuid::nil)).await?;
        return Err(conflict(format!(
            "凭证 {} 已在红冲审批中（{instance_no}）：请等审批结束，或从审批中心撤回后再提交",
            original.voucher_no
        )));
    }
    let Some((initiator_name, initiator_dept_id)) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
    else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "发起凭证红冲审批失败：当前用户不存在或已停用",
        ));
    };
    let entries = voucher::entries_of(db(), original.id)
        .await
        .map_err(|error| utils::db_error(error, "查询凭证分录"))?;
    if entries.is_empty() {
        return Err(conflict(format!(
            "凭证 {} 没有任何分录，不能提交红冲审批",
            original.voucher_no
        )));
    }
    let instance_id = Uuid::now_v7();
    let title = format!("{} 凭证红冲", original.voucher_no)
        .chars()
        .take(200)
        .collect::<String>();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(original.id),
        biz_no: Some(original.voucher_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: reverse_form_data(&original, reason, &entries, operator_id),
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启红冲审批事务"))?;
    let outcome = submit_reverse_tx(&tx, &original, reason, operator_id, &start, now).await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交红冲审批事务"))?;
            tracing::info!(
                voucher_id = %original.id,
                voucher_no = %original.voucher_no,
                %instance_id,
                flow_code = %start.flow_code,
                "凭证已提交红冲审批（状态仍是 POSTED，只挂 pending）"
            );
            let detail = voucher_query::voucher_detail(original.id).await?;
            Ok(ReverseSubmitRes {
                outcome: OUTCOME_REVERSE_PENDING.to_string(),
                message: format!(
                    "已提交红冲审批（凭证 {}，借贷合计 {}），等待复核；审批通过后才会生成红字凭证",
                    original.voucher_no, original.total_debit
                ),
                approval_instance_id: Some(instance_id.to_string()),
                detail,
            })
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// [`submit_reverse_for_approval`] 的事务体：发起审批单 → CAS 回写 pending。
///
/// **故意不写状态机**：原凭证 `status` 保持 `POSTED`（卡面硬红线 3），
/// 「审批中」由 `pending_reverse_instance_id` 表达；审批单本身就是这次的审计留痕
/// （`biz_flow_instance.biz_id = 凭证 id`）。库侧 `ck_biz_voucher_pending_reverse`
/// 保证 pending 只可能挂在「已记账且未红冲」的凭证上。
async fn submit_reverse_tx(
    tx: &Transaction,
    original: &voucher::VoucherRow,
    reason: &str,
    operator: Uuid,
    start: &engine::StartReq,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let mut out = engine::start(tx, start)
        .await
        .map_err(|error| submit_conflict(error, "发起凭证红冲审批单", &original.voucher_no))?;
    if let Some(rejected) = out.rejected.take() {
        // FIX-1：用 `appended` 而不是 `ApiError::new(..)` 重建 —— 后者会把 65018 的
        // 结构化载荷（前端「指派审批人」引导框）连同 `internal_detail` 一起丢掉。
        return Err(rejected
            .appended("（内控要求：红冲复核人不能是发起人本人。请由他人提交，或让另一个账号具备复核权限后再审）"));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    let affected = tx
        .exec_sql(
            "UPDATE biz_voucher \
                SET pending_reverse_instance_id = ?::uuid, reverse_reason = ?, \
                    updated_at = ?, update_by = ?::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = ? \
                AND reversed_by IS NULL AND pending_reverse_instance_id IS NULL"
                .to_string(),
            vec![
                Value::from(&instance_id),
                Value::Text(reason.to_string()),
                Value::from(now),
                Value::from(&operator),
                Value::from(&original.id),
                Value::Text(voucher::STATUS_POSTED.to_string()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "凭证进入红冲审批中"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "凭证 {} 的状态在本次提交期间已被改动（期望 POSTED 且未红冲），请刷新后重试",
            original.voucher_no
        )));
    }
    Ok(())
}

/// 红冲审批的**表单变量**（金额分级条件边读的 `form.amount` 就是这里）。
///
/// `amount` 取**借贷合计**（红冲一笔 1 万的凭证，冲的就是 1 万）——
/// 用贷方或借方单边会漏掉「一借多贷」的凭证。
fn reverse_form_data(
    original: &voucher::VoucherRow,
    reason: &str,
    entries: &[voucher::EntryRow],
    operator: Uuid,
) -> serde_json::Value {
    serde_json::json!({
        "voucher_id": original.id.to_string(),
        "voucher_no": original.voucher_no,
        "period": original.period,
        "voucher_date": original.voucher_date.to_string(),
        "summary": original.summary,
        "amount": amount_number(original.total_debit),
        "amount_text": original.total_debit.to_string(),
        "debit_total": amount_number(original.total_debit),
        "credit_total": amount_number(original.total_credit),
        "entry_count": entries.len(),
        "reverse_reason": reason,
        // 内控留痕：审批人 ≠ 制单人（真正的拦截在 voucher_bridge::guard_self_approval）
        "create_by": operator.to_string(),
    })
}

/// 金额 → JSON 数字（`Decimal` 直接序列化成字符串会让条件节点 `cmp` 的类型比对失败）。
fn amount_number(value: Decimal) -> serde_json::Value {
    let as_float = value.to_string().parse::<f64>().unwrap_or(0.0);
    serde_json::Number::from_f64(as_float)
        .map(serde_json::Value::Number)
        .unwrap_or_else(|| serde_json::Value::Number(serde_json::Number::from(0)))
}

/// 审批单号（用于 409 文案；取不到就退回 id 文本）。
async fn instance_no_of(instance_id: Uuid) -> std::result::Result<String, ApiError> {
    if instance_id.is_nil() {
        return Ok("审批中".to_string());
    }
    let rows = db()
        .find_with_params(
            "SELECT instance_no FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询审批单号"))?;
    Ok(rows
        .first()
        .and_then(|row| row.get_typed::<String>("instance_no").ok())
        .unwrap_or_else(|| instance_id.to_string()))
}

/// 撤回在跑的红冲审批（发起人自己撤 → 只清 pending，凭证一直是 `POSTED`）。
pub async fn withdraw_reverse(
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<VoucherDetailRes, ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "撤回凭证红冲审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let original = voucher::find_voucher(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询凭证"))?
        .ok_or_else(not_found)?;
    let Some(instance_id) = original.pending_reverse_instance_id else {
        return Err(conflict(format!(
            "凭证 {} 没有在跑的红冲审批可撤回",
            original.voucher_no
        )));
    };
    let mut out = db()
        .transaction(|tx| async move { engine::withdraw(&tx, instance_id, operator_id).await })
        .await
        .map_err(|error| utils::db_error(error, "撤回红冲审批"))?;
    if let Some(error) = out.rejected.take() {
        return Err(ApiError::new(error.status, error.code, error.msg));
    }
    // 与审批中心撤回同口径：审批结论回调（只清 pending，账一分没动）
    crate::business::erp::voucher_bridge::on_reverse_settled(Some(instance_id), "WITHDRAWN").await;
    tracing::info!(
        voucher_id = %original.id,
        voucher_no = %original.voucher_no,
        %instance_id,
        reason = reason.unwrap_or("(未填)"),
        "凭证红冲审批已撤回（pending 已清，凭证仍是 POSTED）"
    );
    voucher_query::voucher_detail(id).await
}

/// 红冲审批通过后的**执行体**（`voucher_bridge::on_reverse_settled` 调）。
///
/// 复用 P7 的 [`voucher::reverse_posted`]（借贷互换 + 独立号段 PZR + 回写 `reversed_by`），
/// 一行记账逻辑都没重写。
///
/// ⚠️ 顺序很关键：**先清 pending，再红冲**。
/// 库侧 `ck_biz_voucher_pending_reverse` 要求「pending 非空 ⟹ `reversed_by IS NULL`」，
/// 反过来的话 `reverse_posted` 刚写 `reversed_by` 就会 23514。
pub async fn reverse_after_approval(id: Uuid, operator: Option<Uuid>) -> std::result::Result<Uuid, ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启红冲执行事务"))?;
    let result = async {
        let original = voucher::find_voucher_locked(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定凭证"))?
            .ok_or_else(not_found)?;
        if original.pending_reverse_instance_id.is_none() {
            return Err(conflict(format!(
                "凭证 {} 不在红冲审批中（pending 已清），拒绝执行红冲",
                original.voucher_no
            )));
        }
        let reason = original
            .reverse_reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("红冲审批通过")
            .to_string();
        // ① 先清 pending（否则下一步写 reversed_by 会撞 ck_biz_voucher_pending_reverse）
        // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
        tx.exec_sql(
            "UPDATE biz_voucher SET pending_reverse_instance_id = NULL, updated_at = ? \
              WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(now), Value::from(&original.id)],
        )
        .await
        .map_err(|error| write_error(error, "清理红冲 pending"))?;
        // ② 生成红字凭证 + 回写 reversed_by（P7 的实现，一行没重写）
        let red_id = voucher::reverse_posted(
            &tx,
            &original,
            ReversalInput {
                reason: Some(&reason),
                voucher_date: voucher::today_cst(now),
            },
            operator,
            now,
        )
        .await
        .map_err(|error| write_error(error, "红冲凭证"))?;
        // ③ 联动（LNK-06）：红字凭证落库即 POSTED（与手工红冲 `reverse_in_tx` 同一口径）
        let red = voucher::find_voucher(&tx, red_id)
            .await
            .map_err(|error| utils::db_error(error, "读取红字凭证"))?
            .ok_or_else(|| {
                ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!("红字凭证 {red_id} 刚落库却读不到（同一事务内）"),
                )
            })?;
        let change = voucher_linkage::run_in_tx(&tx, &red, voucher::STATUS_POSTED, operator).await?;
        Ok::<(Uuid, voucher_linkage::VoucherStatusChange), ApiError>((red_id, change))
    }
    .await;
    match result {
        Ok((red_id, change)) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交红冲执行事务"))?;
            // 提交后动作（通知 / 建任务）：永不回滚业务
            voucher_linkage::run_deferred_after_commit(change).await;
            Ok(red_id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 红冲审批**驳回 / 撤回**后的回位体：只清 pending，凭证一直是 `POSTED`（账一分没动）。
pub async fn restore_after_reverse_rejection(
    id: Uuid,
    verdict: crate::business::erp::voucher_bridge::ReverseVerdict,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    db().exec_sql(
        "UPDATE biz_voucher \
            SET pending_reverse_instance_id = NULL, updated_at = ?, \
                update_by = COALESCE(?::uuid, update_by) \
          WHERE id = ?::uuid AND pending_reverse_instance_id IS NOT NULL"
            .to_string(),
        vec![
            Value::from(now),
            operator.map(Value::from).unwrap_or(Value::Null),
            Value::from(&id),
        ],
    )
    .await
    .map_err(|error| {
        write_error(
            error,
            &format!("凭证 {id} 红冲审批未通过后清 pending（{}）", verdict.as_str()),
        )
    })?;
    Ok(())
}

/// 红冲一张已记账凭证。
///
/// **P-FLOW-3B 起走审批**：配了已发布的 `voucher_reverse` 流程 → 凭证只挂 pending、
/// `status` 仍 `POSTED`（`outcome = PENDING_APPROVAL`）；没配流程 → 按老行为直接红冲
/// （`outcome = REVERSED`）。
///
/// 为什么老入口也改成送审：否则审批可以被换一个 URL 绕过（与 `stockin_handler::submit_post`
/// 把 `PATCH /status` 和 `POST /submit-post` 收到同一个函数是同一个理由）。
#[nik::permission(
    path = "/business/erp/voucher/{id}/reverse",
    method = "POST",
    permission = "business:erp:voucher:reverse"
)]
#[nik::post("/business/erp/voucher/{id}/reverse")]
pub async fn voucher_reverse_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ReverseVoucherReq>,
) -> ResResult<ReverseSubmitRes> {
    let id = parse_uuid(&id, "id")?;
    let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
    Ok(Res::ok(
        submit_reverse(id, req.reason.as_deref(), operator_of(&claims), &gate).await?,
    ))
}

/// 提交凭证红冲审批（P-FLOW-3B 的显式入口，与 `POST /voucher/{id}/reverse` 同一函数）。
///
/// # 异常情况
///
/// * 400 + `10001`：`reason` 为空；
/// * 404 + `10002`：凭证不存在 / 已删除；
/// * 409 + `10003`：红字凭证 / 非 POSTED / 已红冲 / 已在红冲审批中 / 期间已结账；
/// * 422 + `65019`：流程侧内控拒绝（复核人 = 发起人）。
#[nik::permission(
    path = "/business/erp/voucher/{id}/submit-reverse",
    method = "POST",
    permission = "business:erp:voucher:submit-reverse"
)]
#[nik::post("/business/erp/voucher/{id}/submit-reverse")]
pub async fn voucher_submit_reverse_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ReverseVoucherReq>,
) -> ResResult<ReverseSubmitRes> {
    let id = parse_uuid(&id, "id")?;
    let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
    Ok(Res::ok(
        submit_reverse(id, req.reason.as_deref(), operator_of(&claims), &gate).await?,
    ))
}

/// 撤回在跑的红冲审批（发起人自己撤 → 只清 pending，凭证一直是 `POSTED`）。
#[nik::permission(
    path = "/business/erp/voucher/{id}/withdraw-reverse",
    method = "POST",
    permission = "business:erp:voucher:withdraw-reverse"
)]
#[nik::post("/business/erp/voucher/{id}/withdraw-reverse")]
pub async fn voucher_withdraw_reverse_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<ReverseVoucherReq>>,
) -> ResResult<VoucherDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let reason = body.and_then(|Json(req)| req.reason);
    Ok(Res::ok(
        withdraw_reverse(id, reason.as_deref(), operator_of(&claims)).await?,
    ))
}

/// 手工新增凭证（分录手工录入，落 **DRAFT**，P0-FIX-4A 起）。
#[nik::permission(
    path = "/business/erp/voucher",
    method = "POST",
    permission = "business:erp:voucher:create"
)]
#[nik::post("/business/erp/voucher")]
pub async fn voucher_create_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateVoucherReq>,
) -> ResResult<VoucherDetailRes> {
    Ok(Res::ok(create_manual_voucher(&req, operator_of(&claims)).await?))
}

// ==================== P0-FIX-4A：审核链三个入口 ====================
//
// 与 `voucher-audit-api.ts`（P0-FIX-4B 前端契约）逐字对齐：
//
// ```text
// POST /business/erp/voucher/{id}/audit      body { remark? }  DRAFT   → AUDITED
// POST /business/erp/voucher/{id}/un-audit   body { reason }   AUDITED → DRAFT（reason 必填）
// POST /business/erp/voucher/{id}/post       body {}           AUDITED → POSTED
// ```
//
// 三条都回 `VoucherDetailRes`：前端拿它直接刷新抽屉，不用再补一次 `GET /voucher/{id}`。

/// 审核（`DRAFT → AUDITED`）。
///
/// # 异常情况
///
/// * 404 + `10002`：凭证不存在 / 已删除；
/// * 409 + `10003`：当前不是草稿（文案给出「当前可选动作」）；
/// * 403 + `65019`：制单人自己来审核（内控：制单 ≠ 审核）。
#[nik::permission(
    path = "/business/erp/voucher/{id}/audit",
    method = "POST",
    permission = "business:erp:voucher:audit"
)]
#[nik::post("/business/erp/voucher/{id}/audit")]
pub async fn voucher_audit_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<AuditVoucherReq>>,
) -> ResResult<VoucherDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let remark = body.and_then(|Json(req)| req.remark);
    Ok(Res::ok(
        audit_voucher(id, remark.as_deref(), operator_of(&claims)).await?,
    ))
}

/// 反审核（`AUDITED → DRAFT`，**原因必填**）。
///
/// # 异常情况
///
/// * 400 + `10001`：`reason` 为空；
/// * 404 + `10002`：凭证不存在 / 已删除；
/// * 409 + `10003`：当前不是已审核（已记账的只能红冲）。
#[nik::permission(
    path = "/business/erp/voucher/{id}/un-audit",
    method = "POST",
    permission = "business:erp:voucher:un-audit"
)]
#[nik::post("/business/erp/voucher/{id}/un-audit")]
pub async fn voucher_un_audit_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<UnAuditVoucherReq>>,
) -> ResResult<VoucherDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let reason = body.and_then(|Json(req)| req.reason);
    Ok(Res::ok(
        un_audit_voucher(id, reason.as_deref(), operator_of(&claims)).await?,
    ))
}

/// 记账（`AUDITED → POSTED`）：记完进总账 / 报表，之后只能红冲。
///
/// # 异常情况
///
/// * 404 + `10002`：凭证不存在 / 已删除；
/// * 409 + `10003`：当前不是已审核 / 所属期间已结账。
#[nik::permission(
    path = "/business/erp/voucher/{id}/post",
    method = "POST",
    permission = "business:erp:voucher:post"
)]
#[nik::post("/business/erp/voucher/{id}/post")]
pub async fn voucher_post_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<PostAuditedVoucherReq>>,
) -> ResResult<VoucherDetailRes> {
    let _ = body;
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(post_audited_voucher(id, operator_of(&claims)).await?))
}

// ==================== 单测 ====================
//
// 真库用例（需要 `ERP_DB_URL` 指向「已应用 V0149 + V0158 的隔离库」，缺省则全部 SKIP）：
//
// ```bash
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p7'
// cargo test --lib p7_
// ```
//
// 夹具前缀 `P7V-`（单头摘要）与 `voucher_type = 'MANUAL'`（手工凭证只由本模块的单测产生），
// 与 `payable_payment` 的单测（`PM-` / `P7PAY-` 前缀）互不删除对方的行。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::fixture_hygiene;
    use crate::business::erp::ledger;
    use crate::business::erp::voucher::{self, entries_of, find_voucher};

    // ---------- 审批实例唯一索引冲突：409 + 自家单号（不是「单号重复、去查 biz_seq」） ----------

    /// 撞 `uk_biz_flow_instance_biz_active` → **409 + 本凭证号 + 「已有审批中实例」**。
    ///
    /// 两条分支都钉住：驱动给了约束名（`UniqueViolation`）与退化形态（`DbError::InvalidValue`，
    /// 文案里只剩 PG 原文）—— 后者正是真库里掉到「联系运维排查 biz_seq」的那条路。
    #[test]
    fn biz_active_conflict_maps_to_409_with_voucher_no() {
        let variant = || DbError::UniqueViolation {
            column: None,
            constraint: Some(flow_model::BIZ_ACTIVE_INDEX.to_string()),
            source: None,
        };
        let by_text = || {
            DbError::InvalidValue(format!(
                "db error: ERROR: duplicate key value violates unique constraint \"{}\"",
                flow_model::BIZ_ACTIVE_INDEX
            ))
        };
        // 别的唯一索引不能被误判成审批冲突
        let other = DbError::UniqueViolation {
            column: None,
            constraint: Some("uk_biz_voucher_no".to_string()),
            source: None,
        };
        assert!(!flow_model::is_biz_active_duplicate(&other));

        for err in [variant(), by_text()] {
            let api = submit_conflict(err, "发起凭证红冲审批单", "PZ-20261102-000123");
            assert_eq!(api.status, 409, "{}", api.msg);
            assert_eq!(api.code, error_code::RESOURCE_CONFLICT);
            assert!(api.msg.contains("PZ-20261102-000123"), "{}", api.msg);
            assert!(api.msg.contains("已有审批中实例"), "{}", api.msg);
            assert!(!api.msg.contains("biz_seq"), "不能把用户指到取号器：{}", api.msg);
            assert!(!api.msg.contains("单号重复"), "{}", api.msg);
        }

        // 拿不到单号的通用出口（write_error）也必须 409 + 可执行文案
        let generic = write_error(variant(), "凭证状态流转");
        assert_eq!(generic.status, 409);
        assert_eq!(generic.code, error_code::RESOURCE_CONFLICT);
        assert!(generic.msg.contains("该凭证"), "{}", generic.msg);
        assert!(!generic.msg.contains("biz_seq"), "{}", generic.msg);

        // 真的号段冲突不能被新分支吞掉：仍然 409 + 取号器提示
        let genuine = write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_voucher_no".to_string()),
                source: None,
            },
            "生成手工凭证",
        );
        assert_eq!(genuine.status, 409);
        assert!(genuine.msg.contains("biz_seq"), "{}", genuine.msg);
    }

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    fn dec(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("测试金额字面量非法")
    }

    /// 清掉本模块夹具的**全部凭证痕迹**（状态日志 → 分录 → 凭证）。
    ///
    /// 两处调用：每个用例开头的 `setup()`，以及模块里名字排最后的
    /// `zz_teardown_purges_voucher_status_logs`（libtest 按名排序，`zz_` 必然收尾）。
    ///
    /// ⚠️ 状态日志必须**在删凭证之前**按 id 清：`biz_status_log` 没有外键，凭证一删就
    /// 再也查不到那些日志的 `entity_id`（本卡实测：漏了这一步每跑一遍多 9 条孤儿）。
    async fn cleanup_fixtures() {
        // 本模块夹具的凭证定位口径（手工凭证 + P7V 摘要），三条清场语句共用
        const P7V_WHERE: &str = "voucher_type = 'MANUAL' OR summary LIKE 'P7V-%'";
        for sql in [
            // 状态日志（无外键）：必须在删凭证之前按 id 清，否则每轮留一批孤儿（`fixture_hygiene`）
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' \
                  AND entity_id IN (SELECT id FROM biz_voucher WHERE {P7V_WHERE})"
            ),
            format!(
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                 (SELECT id FROM biz_voucher WHERE {P7V_WHERE})"
            ),
            format!("DELETE FROM biz_voucher WHERE {P7V_WHERE}"),
        ] {
            exec(&sql).await;
        }
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup_fixtures().await;
        exec(
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                                                start_date, end_date, status) \
             VALUES (2026, 10, '2026-10', DATE '2026-10-01', DATE '2026-10-31', 'OPEN') \
             ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN'",
        )
        .await;
        // **当月期间也必须开**：`create_manual_voucher` 不给 `voucher_date` 时按「今天」
        // 归期（`voucher::period_of` 查不到就退化成 `yyyy-MM`），只种死 2026-10 会让
        // 整个文件在跨月第一天变红（2026-11-01 实测 7 条：10003 会计期间 2026-11 不存在）。
        exec(
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                                                start_date, end_date, status) \
             SELECT EXTRACT(YEAR FROM current_date)::smallint, \
                    EXTRACT(MONTH FROM current_date)::smallint, \
                    to_char(current_date, 'YYYY-MM'), \
                    date_trunc('month', current_date)::date, \
                    (date_trunc('month', current_date) + INTERVAL '1 month - 1 day')::date, \
                    'OPEN' \
             ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN'",
        )
        .await;
        exec("UPDATE biz_account SET status = 1 WHERE account_code IN ('1405', '2202', '1002')").await;
        Some(guard)
    }

    /// 科目 id（按编码取，V0137/V0158 的种子科目）。
    async fn account_id(code: &str) -> String {
        voucher::account_required(db(), code, "单测科目")
            .await
            .unwrap()
            .id
            .to_string()
    }

    /// 一条分录。
    fn entry(account_id: &str, debit: &str, credit: &str, summary: &str) -> ManualEntryReq {
        ManualEntryReq {
            account_id: Some(account_id.to_string()),
            debit: Some(dec(debit)),
            credit: Some(dec(credit)),
            summary: Some(summary.to_string()),
        }
    }

    /// 夹具操作人：**制单人**与**复核人**必须是两个不同 id（P0-FIX-4A 的内控：
    /// 制单人不能审核自己的凭证）。这里用固定 UUID、不建 `sys_user` 行 ——
    /// `biz_voucher.create_by` / `biz_status_log.operator_id` 都没有外键。
    const CREATOR: Uuid = Uuid::from_u128(0x4a00_0000_0000_0000_0000_0000_0000_0001);
    const AUDITOR: Uuid = Uuid::from_u128(0x4a00_0000_0000_0000_0000_0000_0000_0002);

    /// 建一张平衡的**草稿**手工凭证（借 1405 100 / 贷 2202 100），制单人 = [`CREATOR`]。
    async fn new_draft_voucher() -> VoucherDetailRes {
        let inventory = account_id("1405").await;
        let payable_account = account_id("2202").await;
        let req = CreateVoucherReq {
            voucher_date: None,
            summary: Some("P7V 期初余额调整".to_string()),
            entries: Some(vec![
                entry(&inventory, "100", "0", "P7V 借 存货"),
                entry(&payable_account, "0", "100", "P7V 贷 应付"),
            ]),
            remark: Some("P7V 单测夹具".to_string()),
        };
        create_manual_voucher(&req, Some(CREATOR))
            .await
            .unwrap_or_else(|err| panic!("造手工凭证失败：{} {}", err.code, err.msg))
    }

    /// 建一张**已审核**凭证（制单 [`CREATOR`] → 审核 [`AUDITOR`]）。
    async fn new_audited_voucher() -> VoucherDetailRes {
        let draft = new_draft_voucher().await;
        let id = Uuid::parse_str(&draft.voucher.id).expect("凭证 id 非法");
        audit_voucher(id, Some("P7V 复核通过"), Some(AUDITOR))
            .await
            .unwrap_or_else(|err| panic!("审核失败：{} {}", err.code, err.msg))
    }

    /// 建一张**已记账**凭证（草稿 → 审核 → 记账），返回凭证 id。
    async fn new_posted_voucher() -> Uuid {
        let audited = new_audited_voucher().await;
        let id = Uuid::parse_str(&audited.voucher.id).expect("凭证 id 非法");
        let posted = post_audited_voucher(id, Some(AUDITOR))
            .await
            .unwrap_or_else(|err| panic!("记账失败：{} {}", err.code, err.msg));
        assert_eq!(posted.voucher.status, voucher::STATUS_POSTED);
        id
    }

    /// 某张凭证在**总账**里占几行 —— 走生产取数口径 `ledger::ledger_page`（只算 POSTED）。
    ///
    /// 这里刻意不复写 `WHERE status='POSTED'`：要证明的是「生产总账不认 DRAFT / AUDITED」，
    /// 自己在测试里写一遍那个条件等于自己验自己。
    async fn ledger_lines_of(voucher_id: Uuid, account_code: &str) -> Vec<ledger::LedgerEntryRow> {
        let account = voucher::account_required(db(), account_code, "单测科目").await.unwrap();
        let (rows, _total) = ledger::ledger_page(&ledger::LedgerFilter {
            account_id: account.id,
            date_from: None,
            date_to: None,
            page: 1,
            page_size: 500,
        })
        .await
        .expect("总账取数失败");
        rows.into_iter().filter(|row| row.voucher_id == voucher_id).collect()
    }

    /// 凭证余额表里某科目「本期借方发生额」—— 生产报表口径（只算 POSTED）。
    async fn trial_period_debit(period: &str, account_code: &str) -> Decimal {
        let res = ledger::trial_balance(period).await.expect("余额表取数失败");
        res.rows
            .iter()
            .find(|row| row.account_code == account_code)
            .map(|row| row.period_debit)
            .unwrap_or(Decimal::ZERO)
    }

    /// 凭证余额表里某科目「本期贷方发生额」（同上口径）。
    async fn trial_period_credit(period: &str, account_code: &str) -> Decimal {
        let res = ledger::trial_balance(period).await.expect("余额表取数失败");
        res.rows
            .iter()
            .find(|row| row.account_code == account_code)
            .map(|row| row.period_credit)
            .unwrap_or(Decimal::ZERO)
    }

    // ---------- ⑨ 红冲生成红字凭证 ----------

    /// ⑨ 红冲 `POSTED` 凭证 → 生成红字凭证：`is_red=1`、`reverse_of` 指向原凭证、
    /// 金额等额、分录借贷互换、走独立号段 `PZR-`。
    #[tokio::test]
    async fn p7_reverse_creates_red_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let original_id = new_posted_voucher().await;
        let red = reverse_voucher(original_id, Some("录错"), None)
            .await
            .unwrap_or_else(|err| panic!("红冲失败：{} {}", err.code, err.msg));

        assert_eq!(red.voucher.status, voucher::STATUS_POSTED);
        assert!(red.voucher.is_red, "红字凭证必须 is_red = true");
        assert_eq!(
            red.voucher.reverse_of,
            Some(original_id.to_string()),
            "红字凭证必须指向原凭证"
        );
        assert!(
            red.voucher.voucher_no.starts_with(voucher::RED_NO_PREFIX),
            "红字凭证号必须走独立号段：{}",
            red.voucher.voucher_no
        );
        assert_eq!(red.voucher.total_debit, dec("100"));
        assert_eq!(red.voucher.total_credit, dec("100"));
        assert_eq!(red.voucher.entry_count, 2);

        // 分录借贷互换：原「借 1405」在红字里是「贷 1405」
        let red_id = Uuid::parse_str(&red.voucher.id).unwrap();
        let entries = entries_of(db(), red_id).await.unwrap();
        let inventory = entries
            .iter()
            .find(|row| row.account.code == "1405")
            .expect("红字凭证缺 1405 行");
        assert_eq!(inventory.debit, Decimal::ZERO);
        assert_eq!(inventory.credit, dec("100"));
        assert!(
            inventory.summary.contains("红冲") && inventory.summary.contains("录错"),
            "红冲原因要进分录摘要：{}",
            inventory.summary
        );
    }

    // ---------- ⑩ 原凭证回写 ----------

    /// ⑩ 原凭证 `reversed_by` 被回写，`status` 仍 `POSTED`（卡面 §二 ②）。
    #[tokio::test]
    async fn p7_reverse_writes_back_reversed_by_and_keeps_posted() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let original_id = new_posted_voucher().await;
        let red = reverse_voucher(original_id, Some("金额录错"), None).await.unwrap();
        let original = find_voucher(db(), original_id).await.unwrap().unwrap();
        assert_eq!(
            original.status,
            voucher::STATUS_POSTED,
            "原凭证状态保持 POSTED，只打 reversed_by"
        );
        assert_eq!(original.reversed_by, Some(Uuid::parse_str(&red.voucher.id).unwrap()));
        assert_eq!(original.reverse_of, None, "原凭证不是红字凭证");
    }

    // ---------- ⑪ 重复红冲 ----------

    /// ⑪ 重复红冲同一张凭证 → 409，文案提到「已经红冲」并点名红字凭证号。
    #[tokio::test]
    async fn p7_reverse_twice_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let original_id = new_posted_voucher().await;
        let red = reverse_voucher(original_id, Some("第一次"), None).await.unwrap();

        let err = reverse_voucher(original_id, Some("第二次"), None)
            .await
            .expect_err("重复红冲必须被拒");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已经红冲"), "{}", err.msg);
        assert!(
            err.msg.contains(&red.voucher.voucher_no),
            "文案要点名红字凭证号 {}：{}",
            red.voucher.voucher_no,
            err.msg
        );
        // 红字凭证只允许有一张
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_voucher WHERE reverse_of = ?::uuid",
                vec![Value::from(&original_id)],
            )
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    // ---------- ⑫ 红冲红字凭证 ----------

    /// ⑫ 红冲一张红字凭证 → 409（红字凭证不能再红冲）。
    #[tokio::test]
    async fn p7_reverse_red_voucher_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let original_id = new_posted_voucher().await;
        let red = reverse_voucher(original_id, Some("录错"), None).await.unwrap();
        let red_id = Uuid::parse_str(&red.voucher.id).unwrap();

        let err = reverse_voucher(red_id, Some("再冲一次"), None)
            .await
            .expect_err("红字凭证不能再红冲");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("红字凭证"), "{}", err.msg);
    }

    // ---------- ⑬ 手工凭证不平衡 ----------

    /// ⑬ 手工凭证借贷不平衡 → 400，文案点名差额。
    #[tokio::test]
    async fn p7_manual_voucher_unbalanced_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let inventory = account_id("1405").await;
        let payable_account = account_id("2202").await;
        let req = CreateVoucherReq {
            voucher_date: None,
            summary: Some("P7V 不平衡".to_string()),
            entries: Some(vec![
                entry(&inventory, "100", "0", "借 100"),
                entry(&payable_account, "0", "99.5", "贷 99.5"),
            ]),
            remark: None,
        };
        let err = create_manual_voucher(&req, None).await.expect_err("借贷不平衡必须被拒");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("借贷不平衡"), "{}", err.msg);
        assert!(err.msg.contains("差额 0.5"), "文案要点名差额：{}", err.msg);

        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_voucher WHERE summary = ?",
                vec![Value::Text("P7V 不平衡".to_string())],
            )
            .await
            .unwrap();
        assert_eq!(count, 0, "不平衡的凭证一条都不能落库");
    }

    // ---------- ⑭ 手工凭证平衡 ----------

    /// ⑭ 手工凭证借贷平衡 → 成功：`status = DRAFT`（P0-FIX-4A：制单只是「写了一张单」）、
    /// `voucher_type = MANUAL`、`entry_count` 与分录行数一致、分录科目快照正确、
    /// 记账时间 / 记账人**留空**（草稿还没有记账这回事）。
    #[tokio::test]
    async fn p7_manual_voucher_balanced_lands_draft() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let inventory = account_id("1405").await;
        let payable_account = account_id("2202").await;
        let req = CreateVoucherReq {
            voucher_date: None,
            summary: Some("P7V 手工计提".to_string()),
            entries: Some(vec![
                entry(&inventory, "123.45", "0", "P7V 借 存货"),
                entry(&payable_account, "0", "100", "P7V 贷 应付 1"),
                entry(&payable_account, "0", "23.45", "P7V 贷 应付 2"),
            ]),
            remark: Some("P7V 三道分录".to_string()),
        };
        let detail = create_manual_voucher(&req, Some(CREATOR))
            .await
            .unwrap_or_else(|err| panic!("手工凭证失败：{} {}", err.code, err.msg));

        assert_eq!(
            detail.voucher.status,
            voucher::STATUS_DRAFT,
            "手工凭证落草稿，等别人审核（P0-FIX-4A）"
        );
        assert_eq!(detail.voucher.status_label, "草稿");
        assert!(
            detail.voucher.posted_at.is_none() && detail.voucher.posted_by.is_none(),
            "草稿不许有记账时间 / 记账人：{:?} / {:?}",
            detail.voucher.posted_at,
            detail.voucher.posted_by
        );
        assert_eq!(detail.voucher.voucher_type, voucher::VOUCHER_TYPE_MANUAL);
        assert_eq!(detail.voucher.source_type, voucher::SOURCE_MANUAL);
        assert!(!detail.voucher.is_red, "手工凭证是蓝字凭证");
        assert_eq!(detail.voucher.entry_count, 3);
        assert_eq!(detail.entries.len(), 3);
        assert_eq!(detail.voucher.total_debit, dec("123.45"));
        assert_eq!(detail.voucher.total_credit, dec("123.45"));
        assert!(detail.balanced, "详情必须自报平衡");
        assert!(
            detail.voucher.voucher_no.starts_with(voucher::NO_PREFIX),
            "手工凭证走蓝字号段：{}",
            detail.voucher.voucher_no
        );
        assert_eq!(detail.entries[0].account_code, "1405", "分录要落科目编码快照");
    }

    // ---------- 补充：原因必填 / 已结账期间 ----------

    /// 红冲原因必填 → 400（不写红字凭证）。
    #[tokio::test]
    async fn p7_reverse_requires_reason() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let original_id = new_posted_voucher().await;
        for reason in [None, Some("")] {
            let err = reverse_voucher(original_id, reason, None)
                .await
                .expect_err("缺原因必须被拒");
            assert_eq!(err.status, 400, "{}", err.msg);
            assert!(err.msg.contains("reason 必填"), "{}", err.msg);
        }
        let original = find_voucher(db(), original_id).await.unwrap().unwrap();
        assert_eq!(original.reversed_by, None);
    }

    /// 期间已结账 → 红冲 409（结账后不允许改账）。
    #[tokio::test]
    async fn p7_reverse_closed_period_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let original_id = new_posted_voucher().await;
        // 关的是**原凭证自己所在**的期间，不写死 '2026-10'：
        // 夹具凭证按「今天」归期，写死某一月会让本用例在跨月后失去意义（关掉的不是同一期间）。
        let period = find_voucher(db(), original_id)
            .await
            .unwrap()
            .expect("夹具凭证应存在")
            .period;
        exec(&format!(
            "UPDATE biz_accounting_period SET status = 'CLOSED' WHERE period_code = '{period}'"
        ))
        .await;
        let result = reverse_voucher(original_id, Some("结账后红冲"), None).await;
        // 无论成败，先把期间复位，避免污染后续用例
        exec(&format!(
            "UPDATE biz_accounting_period SET status = 'OPEN' WHERE period_code = '{period}'"
        ))
        .await;
        let err = result.expect_err("已结账期间不能红冲");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已结账"), "{}", err.msg);
    }

    // ==================== P0-FIX-4A：三态（DRAFT → AUDITED → POSTED）====================
    //
    // 真库用例：口径是「生产入口进、生产取数出」——
    //   写：`create_manual_voucher` / `audit_voucher` / `un_audit_voucher` / `post_audited_voucher`
    //   读：`ledger::ledger_page`（总账流水）、`ledger::trial_balance`（科目余额表）
    // 不自己复写 `WHERE status='POSTED'`：要证的是**生产报表不认草稿**，自己写一遍等于自己验自己。

    /// 状态日志（走生产查询 `trail_of_no`）。
    async fn trail(voucher_no: &str) -> Vec<crate::business::shared::state_machine::StatusLogRow> {
        state_machine::trail_of_no(db(), voucher_no)
            .await
            .expect("查凭证状态日志失败")
    }

    /// ⑴ 建单落 DRAFT，并留一条 `NULL → DRAFT` 日志；草稿**不进总账**。
    #[tokio::test]
    async fn p4a_create_lands_draft_with_creation_log() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let detail = new_draft_voucher().await;
        let id = Uuid::parse_str(&detail.voucher.id).expect("凭证 id 非法");
        assert_eq!(detail.voucher.status, voucher::STATUS_DRAFT);

        let logs = trail(&detail.voucher.voucher_no).await;
        assert_eq!(logs.len(), 1, "建单只写一条日志：{logs:?}");
        assert_eq!(logs[0].from_status, None, "建单日志的 from_status 必须是 NULL");
        assert_eq!(logs[0].to_status, voucher::STATUS_DRAFT);
        assert_eq!(logs[0].operator_id, Some(CREATOR), "日志要记制单人");

        assert!(
            ledger_lines_of(id, "1405").await.is_empty(),
            "草稿凭证的分录一条都不许进总账"
        );
    }

    /// ⑵ 自审被拒：制单人审核自己的凭证 → **403 + 65019**，状态与日志都不动。
    #[tokio::test]
    async fn p4a_self_audit_is_forbidden() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let draft = new_draft_voucher().await;
        let id = Uuid::parse_str(&draft.voucher.id).expect("凭证 id 非法");

        let err = audit_voucher(id, Some("我自己审"), Some(CREATOR))
            .await
            .expect_err("制单人自审必须被拒");
        assert_eq!(err.status, 403, "{}", err.msg);
        assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(
            err.msg.contains("不能审核自己制作的凭证"),
            "文案要说清为什么：{}",
            err.msg
        );
        assert!(
            err.msg.contains(&draft.voucher.voucher_no),
            "文案要点名单号：{}",
            err.msg
        );
        assert!(
            err.msg.contains("erp.separation_of_duties.approve"),
            "文案要带开关键，管理员才知道去哪改：{}",
            err.msg
        );

        // 状态没变、日志没多（失败不许留半条轨迹）
        let row = find_voucher(db(), id).await.unwrap().unwrap();
        assert_eq!(row.status, voucher::STATUS_DRAFT);
        assert_eq!(trail(&row.voucher_no).await.len(), 1, "自审失败不许写日志");
    }

    /// ⑶ 换人审核 → AUDITED：写 `DRAFT → AUDITED` 日志（reason = 审核意见），
    /// 但**仍不进总账**（审核 ≠ 记账）。
    #[tokio::test]
    async fn p4a_audit_by_other_lands_audited() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let draft = new_draft_voucher().await;
        let id = Uuid::parse_str(&draft.voucher.id).expect("凭证 id 非法");

        let audited = audit_voucher(id, Some("复核无误"), Some(AUDITOR))
            .await
            .unwrap_or_else(|err| panic!("审核失败：{} {}", err.code, err.msg));
        assert_eq!(audited.voucher.status, voucher::STATUS_AUDITED);
        assert_eq!(audited.voucher.status_label, "已审核");
        assert!(audited.voucher.posted_at.is_none(), "审核不是记账：不许有记账时间");

        let logs = trail(&audited.voucher.voucher_no).await;
        assert_eq!(logs.len(), 2, "建单 + 审核两条：{logs:?}");
        let last = logs.last().expect("审核日志");
        assert_eq!(last.from_status.as_deref(), Some(voucher::STATUS_DRAFT));
        assert_eq!(last.to_status, voucher::STATUS_AUDITED);
        assert_eq!(last.reason.as_deref(), Some("复核无误"));
        assert_eq!(last.operator_id, Some(AUDITOR), "日志要记复核人");

        assert!(
            ledger_lines_of(id, "1405").await.is_empty(),
            "已审核未记账的凭证也不进总账"
        );
    }

    /// ⑷ 反审核原因必填 → 400；凭证仍是 AUDITED。
    #[tokio::test]
    async fn p4a_un_audit_requires_reason() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let audited = new_audited_voucher().await;
        let id = Uuid::parse_str(&audited.voucher.id).expect("凭证 id 非法");

        for reason in [None, Some(""), Some("   ")] {
            let err = un_audit_voucher(id, reason, Some(AUDITOR))
                .await
                .expect_err("反审核缺原因必须被拒");
            assert_eq!(err.status, 400, "{:?} → {}", reason, err.msg);
            assert!(err.msg.contains("reason 必填"), "{}", err.msg);
        }
        let row = find_voucher(db(), id).await.unwrap().unwrap();
        assert_eq!(row.status, voucher::STATUS_AUDITED, "失败不许改状态");
        assert_eq!(trail(&row.voucher_no).await.len(), 2, "失败不许写日志");
    }

    /// ⑸ 反审核带原因 → 回 DRAFT，日志里留原因（审计要能查「为什么拆封条」）。
    #[tokio::test]
    async fn p4a_un_audit_back_to_draft_keeps_reason() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let audited = new_audited_voucher().await;
        let id = Uuid::parse_str(&audited.voucher.id).expect("凭证 id 非法");

        let back = un_audit_voucher(id, Some("科目选错了，退回改"), Some(AUDITOR))
            .await
            .unwrap_or_else(|err| panic!("反审核失败：{} {}", err.code, err.msg));
        assert_eq!(back.voucher.status, voucher::STATUS_DRAFT);
        assert_eq!(back.voucher.status_label, "草稿");

        let logs = trail(&back.voucher.voucher_no).await;
        assert_eq!(logs.len(), 3, "建单 + 审核 + 反审核：{logs:?}");
        let last = logs.last().expect("反审核日志");
        assert_eq!(last.from_status.as_deref(), Some(voucher::STATUS_AUDITED));
        assert_eq!(last.to_status, voucher::STATUS_DRAFT);
        assert_eq!(
            last.reason.as_deref(),
            Some("科目选错了，退回改"),
            "反审核原因必须进日志"
        );
        assert_eq!(last.operator_id, Some(AUDITOR));
    }

    /// ⑹ 草稿不能直接记账（**没有 DRAFT → POSTED 这条边**）→ 409 + 提示先审核。
    #[tokio::test]
    async fn p4a_post_requires_audited() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let draft = new_draft_voucher().await;
        let id = Uuid::parse_str(&draft.voucher.id).expect("凭证 id 非法");

        let err = post_audited_voucher(id, Some(AUDITOR))
            .await
            .expect_err("草稿不许直接记账");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("不能记账") && err.msg.contains("先审核"),
            "文案要说清该先做什么：{}",
            err.msg
        );
        assert!(err.msg.contains("审核"), "文案要给出当前可选动作：{}", err.msg);
        let row = find_voucher(db(), id).await.unwrap().unwrap();
        assert_eq!(row.status, voucher::STATUS_DRAFT);
        assert!(row.posted_at.is_none(), "记账失败不许盖戳");
    }

    /// ⑺ **总账口径**：草稿 / 已审核都不进余额，记账后才进（同一张凭证的前后对比）。
    #[tokio::test]
    async fn p4a_only_posted_enters_ledger_and_trial_balance() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let draft = new_draft_voucher().await;
        let id = Uuid::parse_str(&draft.voucher.id).expect("凭证 id 非法");
        let period = draft.voucher.period.clone();
        // 基线：本用例造的那张凭证还没进账（setup 已清掉本模块夹具）
        let base_ledger_1405 = trial_period_debit(&period, "1405").await;
        let base_ledger_2202 = trial_period_credit(&period, "2202").await;

        // ① DRAFT：总账 0 行，余额表一分没变
        assert!(ledger_lines_of(id, "1405").await.is_empty(), "草稿不进总账");
        assert_eq!(
            trial_period_debit(&period, "1405").await,
            base_ledger_1405,
            "草稿不进余额表（借方）"
        );
        assert_eq!(
            trial_period_credit(&period, "2202").await,
            base_ledger_2202,
            "草稿不进余额表（贷方）"
        );

        // ② AUDITED：仍然 0 行
        audit_voucher(id, None, Some(AUDITOR))
            .await
            .unwrap_or_else(|err| panic!("审核失败：{} {}", err.code, err.msg));
        assert!(ledger_lines_of(id, "1405").await.is_empty(), "已审核未记账也不进总账");
        assert_eq!(
            trial_period_debit(&period, "1405").await,
            base_ledger_1405,
            "已审核不进余额表"
        );

        // ③ POSTED：进总账（1405 一行借方 100 / 2202 一行贷方 100），余额表 +100
        let posted = post_audited_voucher(id, Some(AUDITOR))
            .await
            .unwrap_or_else(|err| panic!("记账失败：{} {}", err.code, err.msg));
        assert_eq!(posted.voucher.status, voucher::STATUS_POSTED);
        assert!(posted.voucher.posted_at.is_some(), "记账要盖时间戳");
        assert_eq!(
            posted.voucher.posted_by.as_deref(),
            Some(AUDITOR.to_string().as_str()),
            "记账要盖记账人"
        );

        let lines_1405 = ledger_lines_of(id, "1405").await;
        assert_eq!(lines_1405.len(), 1, "记账后 1405 应有一行流水");
        assert_eq!(lines_1405[0].debit_amount, dec("100"));
        assert_eq!(lines_1405[0].voucher_no, posted.voucher.voucher_no);
        assert_eq!(
            trial_period_debit(&period, "1405").await,
            base_ledger_1405 + dec("100"),
            "记账后余额表本期借方 +100"
        );
        assert_eq!(
            trial_period_credit(&period, "2202").await,
            base_ledger_2202 + dec("100"),
            "记账后余额表本期贷方 +100"
        );
    }

    /// ⑻ `POSTED` 是终态：不许再审核、不许反审核（更正只能红冲）。
    #[tokio::test]
    async fn p4a_posted_is_terminal() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_posted_voucher().await;

        let audit_err = audit_voucher(id, None, Some(AUDITOR))
            .await
            .expect_err("已记账不能再审核");
        assert_eq!(audit_err.status, 409, "{}", audit_err.msg);
        assert!(audit_err.msg.contains("已记账"), "{}", audit_err.msg);

        let un_audit_err = un_audit_voucher(id, Some("想退回"), Some(AUDITOR))
            .await
            .expect_err("已记账不能反审核");
        assert_eq!(un_audit_err.status, 409, "{}", un_audit_err.msg);
        assert!(
            un_audit_err.msg.contains("红冲"),
            "文案要把人指到红冲：{}",
            un_audit_err.msg
        );

        let row = find_voucher(db(), id).await.unwrap().unwrap();
        assert_eq!(row.status, voucher::STATUS_POSTED, "状态必须不动");
        assert_eq!(trail(&row.voucher_no).await.len(), 3, "建单+审核+记账");
    }

    /// ⑼ 已审核的凭证摘要锁死（审 A 改 B 不许发生）→ 409。
    #[tokio::test]
    async fn p4a_audited_voucher_summary_is_locked() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let audited = new_audited_voucher().await;
        let id = Uuid::parse_str(&audited.voucher.id).expect("凭证 id 非法");

        let err = update_summary(id, Some("改一改"), Some(AUDITOR))
            .await
            .expect_err("已审核的凭证不许改摘要");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已入账"), "{}", err.msg);

        // 回到草稿就能改（反审核之后）
        un_audit_voucher(id, Some("要改摘要"), Some(AUDITOR)).await.unwrap();
        update_summary(id, Some("草稿可以改"), Some(CREATOR))
            .await
            .expect("草稿必须能改摘要");
        let row = find_voucher(db(), id).await.unwrap().unwrap();
        assert_eq!(row.summary, "草稿可以改");
    }

    /// ⑽ 落库状态白名单：`AUDITED` 不能直接落库（没人审却显示已审核 = 掏空复核）。
    #[tokio::test]
    async fn p4a_initial_audited_status_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let inventory = voucher::account_required(db(), "1405", "单测科目").await.unwrap();
        let payable = voucher::account_required(db(), "2202", "单测科目").await.unwrap();
        let line = |line_no: i32, account: AccountRef, debit: &str, credit: &str| EntryLine {
            line_no,
            summary: "P4A 非法初始状态".to_string(),
            account,
            debit: dec(debit),
            credit: dec(credit),
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        };
        let err = voucher::post_voucher(
            db(),
            &voucher::PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_MANUAL,
                source_type: voucher::SOURCE_MANUAL,
                source_id: None,
                source_no: Some("P4A 非法状态"),
                source_doc_no: None,
                voucher_date: voucher::today_cst(Utc::now()),
                summary: "P4A 非法初始状态".to_string(),
                lines: vec![line(1, inventory, "1", "0"), line(2, payable, "0", "1")],
                remark: None,
                red_of: None,
                status: voucher::STATUS_AUDITED,
            },
            Some(CREATOR),
            Utc::now(),
        )
        .await
        .expect_err("AUDITED 不许直接落库");
        let DbError::InvalidValue(message) = err else {
            panic!("应当是参数错，实际 {err:?}");
        };
        assert!(message.contains("落库状态只能是"), "{message}");

        let left: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_voucher WHERE summary = ?",
                vec![Value::Text("P4A 非法初始状态".to_string())],
            )
            .await
            .unwrap();
        assert_eq!(left, 0, "被拒的凭证一条都不许落库");
    }

    /// ⑾ 自动凭证（收货 / 付款 / 红冲）仍直接落 `POSTED` 并写 `NULL → POSTED` 日志，
    ///     不受「三态」影响（它们的复核在上游单据的审批链上）。
    #[tokio::test]
    async fn p4a_auto_voucher_still_posts_directly() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let inventory = voucher::account_required(db(), "1405", "单测科目").await.unwrap();
        let payable = voucher::account_required(db(), "2202", "单测科目").await.unwrap();
        let line = |line_no: i32, account: AccountRef, debit: &str, credit: &str| EntryLine {
            line_no,
            summary: "P4A 自动凭证".to_string(),
            account,
            debit: dec(debit),
            credit: dec(credit),
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        };
        let id = voucher::post_voucher(
            db(),
            &voucher::PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_AUTO,
                source_type: "STOCK_IN",
                source_id: None,
                source_no: Some("P4A 自动凭证"),
                source_doc_no: None,
                voucher_date: voucher::today_cst(Utc::now()),
                summary: "P4A 自动凭证".to_string(),
                lines: vec![line(1, inventory, "5", "0"), line(2, payable, "0", "5")],
                remark: None,
                red_of: None,
                status: voucher::STATUS_POSTED,
            },
            Some(CREATOR),
            Utc::now(),
        )
        .await
        .expect("自动凭证必须能落库");

        let row = find_voucher(db(), id).await.unwrap().unwrap();
        assert_eq!(row.status, voucher::STATUS_POSTED);
        assert!(row.posted_at.is_some(), "自动凭证落库即记账");
        let logs = trail(&row.voucher_no).await;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].to_status, voucher::STATUS_POSTED);
        assert_eq!(logs[0].from_status, None);
        // 自动凭证进了总账（对照组：证明⑺的「不进」是状态造成的，不是查询写坏了）
        assert_eq!(ledger_lines_of(id, "1405").await.len(), 1);
    }

    /// ⑿ 状态机注册表（纯逻辑）：凭证三条边，一条不多一条不少；`DRAFT → POSTED` 不存在。
    #[test]
    fn p4a_voucher_state_machine_registry() {
        use crate::business::shared::state_machine::Entity;
        assert_eq!(Entity::Voucher.edges(), &voucher::LEGAL_EDGES[..]);
        assert_eq!(Entity::Voucher.edges().len(), 3, "凭证只认三条边");

        for (from, to) in [("DRAFT", "AUDITED"), ("AUDITED", "DRAFT"), ("AUDITED", "POSTED")] {
            assert!(Entity::Voucher.can_transition(from, to), "{from} → {to} 必须是合法边");
        }
        for (from, to) in [
            ("DRAFT", "POSTED"),   // 制单 ≠ 审核：故意没有这条捷径
            ("POSTED", "DRAFT"),   // 已记账只能红冲
            ("POSTED", "AUDITED"), // 同上
            ("DRAFT", "DRAFT"),    // 自环不是状态变更
            ("AUDITED", "AUDITED"),
            ("POSTED", "POSTED"),
            ("CANCELLED", "DRAFT"), // 作废不是凭证的态（库里 ck 允许但状态机不认）
        ] {
            assert!(
                !Entity::Voucher.can_transition(from, to),
                "{from} → {to} 必须被拒（非法边）"
            );
        }
        // 终态：POSTED 没有出边
        assert!(Entity::Voucher.next_statuses("POSTED").is_empty());
        assert_eq!(
            Entity::Voucher.next_statuses("AUDITED").len(),
            2,
            "已审核只能反审核或记账"
        );
    }

    /// ⒀ 只有反审核需要 reason（审核的 remark 可空、记账不需要原因）。
    #[test]
    fn p4a_only_un_audit_needs_reason() {
        use crate::business::shared::state_machine::Entity;
        assert!(Entity::Voucher.needs_reason("AUDITED", "DRAFT"));
        assert!(!Entity::Voucher.needs_reason("DRAFT", "AUDITED"));
        assert!(!Entity::Voucher.needs_reason("AUDITED", "POSTED"));
        // BASE 与注册表逐条对齐（状态机自己也有全局用例，这里钉住凭证这一行）
        for (index, edge) in Entity::Voucher.base_edges().iter().enumerate() {
            assert_eq!(
                (edge.from, edge.to),
                voucher::LEGAL_EDGES[index],
                "VOUCHER_BASE 与 LEGAL_EDGES 第 {index} 条不一致"
            );
            assert!(edge.meta.mandatory, "凭证的三条边都必须 mandatory（内控动作不可关）");
        }
        assert_eq!(
            Entity::Voucher
                .base_edge("AUDITED", "DRAFT")
                .map(|e| e.meta.requires_reason),
            Some(true),
            "反审核必须要求原因"
        );
    }

    /// ⒁ 入账态：POSTED / AUDITED 锁，DRAFT 可编辑。
    #[test]
    fn p4a_locked_statuses_are_posted_and_audited() {
        use crate::business::shared::state_machine::Entity;
        assert!(Entity::Voucher.is_locked("POSTED"));
        assert!(Entity::Voucher.is_locked("AUDITED"), "审 A 改 B 必须被锁");
        assert!(!Entity::Voucher.is_locked("DRAFT"), "草稿必须能改");
    }

    /// ⒂ 自审闸矩阵（纯逻辑）：同人拦、异人放、无制单人放、开关关掉放。
    #[test]
    fn p4a_self_audit_gate_matrix() {
        // 开关开（默认）+ 同人 → 403 + 65019
        let err =
            check_self_audit(true, Some(CREATOR), Some(CREATOR), "PZ-20261105-000001").expect_err("同人自审必须被拦");
        assert_eq!(err.status, 403);
        assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(err.msg.contains("PZ-20261105-000001"), "{}", err.msg);
        assert!(err.msg.contains("制单与审核分离"), "{}", err.msg);
        assert!(
            err.msg.contains("erp.separation_of_duties.approve"),
            "给出放行办法：{}",
            err.msg
        );

        // 换人 → 放行
        assert!(check_self_audit(true, Some(CREATOR), Some(AUDITOR), "PZ-1").is_ok());
        // 没有制单人（历史行 / 系统生成）→ 放行
        assert!(check_self_audit(true, None, Some(AUDITOR), "PZ-1").is_ok());
        // 操作人未知（定时任务）→ 放行
        assert!(check_self_audit(true, Some(CREATOR), None, "PZ-1").is_ok());
        // 管理员显式关掉内控开关 → 放行（一人财务岗的既有出口）
        assert!(check_self_audit(false, Some(CREATOR), Some(CREATOR), "PZ-1").is_ok());
    }

    // ---------- 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------

    /// 本模块跑完**不许留** `VOUCHER` 状态日志（含孤儿行）。
    ///
    /// `setup()` 只在每个用例开头跑，模块最后一个用例的产物没人收；而 `biz_status_log`
    /// 没有外键 ⇒ 清场里漏掉「删凭证前先删日志」，每跑一遍全量就多 9 条孤儿（本卡实测）。
    /// libtest 按名字排序，`zz_` 必然最后跑，是这一块的兜底收口。
    #[tokio::test]
    async fn zz_teardown_purges_voucher_status_logs() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // ① 自造一张会写状态日志的凭证（手工凭证走真实 `post_voucher` 收口）
        let id = new_posted_voucher().await;
        let logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'VOUCHER' AND entity_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("查清场前日志")
            .unwrap_or(0);
        assert!(logs > 0, "凭证落地必须写 VOUCHER 状态日志，否则这条用例证明不了什么");

        // ② 走模块真实的清场
        cleanup_fixtures().await;

        // ③ 凭证与日志按 id 全为 0
        assert!(
            find_voucher(db(), id).await.expect("查凭证").is_none(),
            "清场后手工凭证必须不存在"
        );
        let left: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'VOUCHER' AND entity_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("查清场后日志")
            .unwrap_or(0);
        assert_eq!(left, 0, "清场后 VOUCHER 状态日志必须为 0（日志无外键，必须按 id 先删）");

        // ④ 兜底：整库不再有 PAYABLE / VOUCHER 的孤儿行
        fixture_hygiene::purge_orphans(fixture_hygiene::FINANCE).await;
        assert_eq!(
            fixture_hygiene::orphans(fixture_hygiene::FINANCE).await,
            0,
            "整库不许留 PAYABLE / VOUCHER 的孤儿状态日志"
        );
    }
}

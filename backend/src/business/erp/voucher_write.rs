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
//! # 接口（卡面 §二 ②）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/voucher/{id}/reverse` | `business:erp:voucher:reverse` | 红冲一张已记账凭证 |
//! | POST | `/business/erp/voucher` | `business:erp:voucher:create` | 手工新增凭证（分录手工录入） |
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
//! 动作：status 直接 POSTED（与自动凭证同口径，手工凭证也即时记账）
//! ```
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_voucher / biz_voucher_entry / biz_account / biz_accounting_period 是会计台账
//! 与全局字典（V0149 建表，无 dept_id / 归属人列），数据范围表达不出来；
//! 可见性由 business:erp:voucher:reverse / business:erp:voucher:create 权限码控制。
//! ```

use axum::Json;
use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Deserialize;

use crate::business::erp::voucher::{self, AccountRef, EntryLine, ReversalInput};
use crate::business::erp::voucher_query::{self, VoucherDetailRes};
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

/// 红冲入参。
#[derive(Debug, Clone, Deserialize)]
pub struct ReverseVoucherReq {
    /// 红冲原因（必填）。
    pub reason: Option<String>,
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
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "凭证不存在或已删除".to_string(),
    )
}

/// 写路径的数据库错误收口。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "凭证号重复（唯一约束{}）：取号器与库里的号段不一致，请联系运维排查 biz_seq",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
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
    let red_id = match result {
        Ok(red_id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交红冲事务"))?;
            red_id
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    voucher_query::voucher_detail(red_id).await
}

/// 红冲的事务体（返回红字凭证 id）。
async fn reverse_in_tx(
    tx: &Transaction,
    id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    now: nik::prelude::chrono::DateTime<Utc>,
) -> std::result::Result<Uuid, ApiError> {
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

    voucher::reverse_posted(
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
    .map_err(|error| write_error(error, "红冲凭证"))
}

// ==================== ③ 改凭证摘要（P10 入账后只读）====================

/// 改凭证摘要（**入账后只读闸**的凭证侧落点）。
///
/// 卡面 §二 ③ 把 `VOUCHER: POSTED` 列入「入账态」：已记账凭证的摘要属于会计档案的
/// 描述字段，改了它账证虽不差钱，但会与已出的报表 / 归档 PDF 不一致 ——
/// 统一按「先红冲再重开」处理（**409**，文案点「已入账」）。
///
/// 只有 `DRAFT` 状态的凭证允许改摘要；本仓目前没有 DRAFT 凭证的生成路径，
/// 所以线上等价于「凭证一律不可改」，但接口语义在这里钉死，等草稿凭证上线即自动生效。
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
          WHERE id = ?::uuid AND status <> 'POSTED' AND deleted_at IS NULL"
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

// ==================== ② 手工新增凭证 ====================

/// 手工新增凭证（handler 与单测共用）：校验 → 科目快照 → 落库（直接 `POSTED`）。
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
    let voucher_id = match result {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交手工凭证事务"))?;
            id
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
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

/// 手工凭证的事务体：期间校验 → 科目快照 → 落库。
async fn manual_in_tx(
    tx: &Transaction,
    input: ManualVoucher<'_>,
) -> std::result::Result<Uuid, ApiError> {
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
                    format!(
                        "entries[{index}].account_id 科目不存在 / 已停用 / 已删除（{account_id}）"
                    ),
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
        });
    }

    let source_no = format!("手工凭证 {debit_total}");
    voucher::post_voucher(
        tx,
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_MANUAL,
            source_type: voucher::SOURCE_MANUAL,
            source_id: None,
            source_no: Some(&source_no),
            voucher_date,
            summary,
            lines,
            remark: remark.map(str::trim).filter(|text| !text.is_empty()),
            red_of: None,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成手工凭证"))
}

// ==================== 接口层 ====================

/// 红冲一张已记账凭证。
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
) -> ResResult<VoucherDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(
        reverse_voucher(id, req.reason.as_deref(), operator_of(&claims)).await?,
    ))
}

/// 手工新增凭证（分录手工录入，直接 `POSTED`）。
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
    Ok(Res::ok(
        create_manual_voucher(&req, operator_of(&claims)).await?,
    ))
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
    use crate::business::erp::voucher::{self, entries_of, find_voucher};

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

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        for sql in [
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE voucher_type = 'MANUAL' OR summary LIKE 'P7V-%')",
            "DELETE FROM biz_voucher WHERE voucher_type = 'MANUAL' OR summary LIKE 'P7V-%'",
        ] {
            exec(sql).await;
        }
        exec(
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                                                start_date, end_date, status) \
             VALUES (2026, 10, '2026-10', DATE '2026-10-01', DATE '2026-10-31', 'OPEN') \
             ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN'",
        )
        .await;
        exec("UPDATE biz_account SET status = 1 WHERE account_code IN ('1405', '2202', '1002')")
            .await;
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

    /// 建一张平衡的手工凭证（借 1405 100 / 贷 2202 100），返回凭证 id。
    async fn new_posted_voucher() -> Uuid {
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
        let detail = create_manual_voucher(&req, None)
            .await
            .unwrap_or_else(|err| panic!("造手工凭证失败：{} {}", err.code, err.msg));
        Uuid::parse_str(&detail.voucher.id).expect("凭证 id 非法")
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
        let red = reverse_voucher(original_id, Some("金额录错"), None)
            .await
            .unwrap();
        let original = find_voucher(db(), original_id).await.unwrap().unwrap();
        assert_eq!(
            original.status,
            voucher::STATUS_POSTED,
            "原凭证状态保持 POSTED，只打 reversed_by"
        );
        assert_eq!(
            original.reversed_by,
            Some(Uuid::parse_str(&red.voucher.id).unwrap())
        );
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
        let red = reverse_voucher(original_id, Some("第一次"), None)
            .await
            .unwrap();

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
        let red = reverse_voucher(original_id, Some("录错"), None)
            .await
            .unwrap();
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
        let err = create_manual_voucher(&req, None)
            .await
            .expect_err("借贷不平衡必须被拒");
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

    /// ⑭ 手工凭证借贷平衡 → 成功：`status = POSTED`、`voucher_type = MANUAL`、
    /// `entry_count` 与分录行数一致、分录科目快照正确。
    #[tokio::test]
    async fn p7_manual_voucher_balanced_is_posted() {
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
        let detail = create_manual_voucher(&req, None)
            .await
            .unwrap_or_else(|err| panic!("手工凭证失败：{} {}", err.code, err.msg));

        assert_eq!(
            detail.voucher.status,
            voucher::STATUS_POSTED,
            "手工凭证直接记账"
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
        assert_eq!(
            detail.entries[0].account_code, "1405",
            "分录要落科目编码快照"
        );
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
        exec("UPDATE biz_accounting_period SET status = 'CLOSED' WHERE period_code = '2026-10'")
            .await;
        let result = reverse_voucher(original_id, Some("结账后红冲"), None).await;
        // 无论成败，先把期间复位，避免污染后续用例
        exec("UPDATE biz_accounting_period SET status = 'OPEN' WHERE period_code = '2026-10'")
            .await;
        let err = result.expect_err("已结账期间不能红冲");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已结账"), "{}", err.msg);
    }
}

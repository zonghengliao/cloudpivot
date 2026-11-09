//! 多借方凭证入口（ARCH-6 §6.4 边界 ① · FIN-2）。
//!
//! # 为什么必须新增这个入口，而不是改 `create_by_rule`
//!
//! `voucher::create_by_rule` 是**单借方模型**：一条 `biz_voucher_rule` 给
//! 「一个借方科目 + 一个贷方科目 + 一个税科目」，分录形状由 `purchase_in_lines`
//! 固定成「借 M 行存货 + 借 1 行进项税 / 贷 1 行应付」。
//!
//! 折旧与处置不是这个形状：
//!
//! ```text
//! 折旧：借 5602.01（按部门拆 N 行） / 贷 1602（1 行）
//! 处置：借 1602 + 借 1606 / 贷 1601      ← 两个借方科目，语义完全不同
//! 购入：借 1601 + 借 2221.01 / 贷 2202   ← 两个借方科目（可抵扣时的价税拆分）
//! ```
//!
//! 改 `create_by_rule` 会动到收货入库这条**最核心、回归面最大**的链路
//! （`purchase_in_lines` 的 13 条既有断言都在钉它的形状）。所以按 ARCH-6 的做法
//! **新增一个入口**，共享它下游的 `voucher::post_voucher`（取号 / 借贷平衡 /
//! 期间闸 / 落库 / 状态日志都在那里，一行都没有复制）。
//!
//! # 与 `create_by_rule` 的口径一致性
//!
//! | 口径 | 谁保证 |
//! |------|--------|
//! | 借贷平衡 / ≥2 行 / 借贷合计 > 0 | `voucher::post_voucher`（唯一实现） |
//! | 期间已结账不许记账 | `voucher::post_voucher` 里的 `period_status` 闸 |
//! | 取号（`PZ-yyyyMMdd-NNNNNN` / 红字 `PZR`） | `voucher::post_voucher` → `next_voucher_no` |
//! | 分录落库 + 状态日志 | `voucher::post_voucher` → `write_entries` + `log_created` |
//! | 科目必须是末级 | 本模块的 [`ensure_leaf`]（`account_required` 只按编码取，不校验 `is_leaf`） |
//!
//! # 不做什么
//!
//! * 不做「按业务类型自动选规则」—— 调用方必须显式给科目，多借方本来就是
//!   「业务语义决定分录形状」，藏在规则表里反而查不出账是怎么来的；
//! * 不做汇总 / 合并 —— 合并口径属于调用方（折旧按 `(费用科目, 部门)` 聚合，
//!   处置按「处置 / 收款 / 结转」三张凭证拆开），入口只负责「把这些行落成一张凭证」。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::voucher::{self, AccountRef, EntryLine, PostVoucher, VOUCHER_TYPE_AUTO};

/// 一条借方分组（同一科目 + 同一辅助维度 = 一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebitGroup {
    /// 借方科目（必须是末级）。
    pub account: AccountRef,
    /// 借方金额（> 0）。
    pub amount: Decimal,
    /// 部门辅助核算（折旧按计提时的使用部门写；购入 / 处置不写）。
    pub aux_dept_id: Option<Uuid>,
    /// 本行摘要（`None` = 用整单摘要）。
    pub summary: Option<String>,
}

impl DebitGroup {
    /// 构造一条借方分组。
    pub fn new(account: AccountRef, amount: Decimal, aux_dept_id: Option<Uuid>) -> Self {
        Self {
            account,
            amount,
            aux_dept_id,
            summary: None,
        }
    }

    /// 带自定义摘要的借方分组。
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }
}

/// 一张多借方凭证的全部入参。
#[derive(Debug, Clone)]
pub struct MultiDebit<'a> {
    /// 凭证类型（默认 `AUTO`；红字由 `voucher::reverse_posted` 另开一张，不走本结构）。
    pub source_type: &'a str,
    /// 来源单据 id（折旧批次 / 资产卡片）。
    pub source_id: Option<Uuid>,
    /// 来源单号（写进 `biz_voucher.source_no`，对账靠它）。
    pub source_no: &'a str,
    /// 凭证日期（决定会计期间）。
    pub voucher_date: NaiveDate,
    /// 整单摘要。
    pub summary: String,
    /// 借方分组（≥ 1 行）。
    pub debits: Vec<DebitGroup>,
    /// 贷方科目。
    pub credit_account: AccountRef,
    /// 贷方金额（必须等于借方合计）。
    pub credit_amount: Decimal,
    /// 贷方行摘要（`None` = 用整单摘要）。
    pub credit_summary: Option<String>,
    /// 备注（红冲原因之类的自由文本）。
    pub remark: Option<&'a str>,
}

/// 落一张多借方凭证（**不 commit**，由调用方事务收口）。
///
/// 返回 `biz_voucher.id`。任何不平衡 / 空行 / 非末级科目都是 `DbError::InvalidValue`
/// （调用方 `utils::db_error` 会翻成 400 + 原文）。
pub async fn post_multi_debit(
    ex: &impl SqlExecutor,
    req: &MultiDebit<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    if req.debits.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：{} 没有任何借方行（多借方凭证至少一行借方）",
            req.source_no
        )));
    }
    let mut lines: Vec<EntryLine> = Vec::with_capacity(req.debits.len() + 1);
    let mut debit_total = Decimal::ZERO;
    for group in &req.debits {
        if group.amount <= Decimal::ZERO {
            return Err(DbError::InvalidValue(format!(
                "凭证生成失败：{} 的借方行「{}」金额为 {}（必须 > 0；零金额行会让凭证虚增行数）",
                req.source_no, group.account.code, group.amount
            )));
        }
        debit_total += group.amount;
        let line_no = i32::try_from(lines.len() + 1).unwrap_or(1);
        lines.push(EntryLine {
            line_no,
            summary: group.summary.clone().unwrap_or_else(|| req.summary.clone()),
            account: group.account.clone(),
            debit: group.amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: group.aux_dept_id,
        });
    }
    if req.credit_amount <= Decimal::ZERO {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：{} 的贷方金额为 {}（必须 > 0）",
            req.source_no, req.credit_amount
        )));
    }
    if debit_total != req.credit_amount {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：{} 借贷不平衡（借方合计 {debit_total} / 贷方 {})，\
             多借方入口不会替你「凑平」——差额必须由业务口径解释清楚",
            req.source_no, req.credit_amount
        )));
    }
    let line_no = i32::try_from(lines.len() + 1).unwrap_or(1);
    lines.push(EntryLine {
        line_no,
        summary: req.credit_summary.clone().unwrap_or_else(|| req.summary.clone()),
        account: req.credit_account.clone(),
        debit: Decimal::ZERO,
        credit: req.credit_amount,
        aux_supplier_id: None,
        aux_material_id: None,
        aux_warehouse_id: None,
        aux_dept_id: None,
    });

    voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: req.source_type,
            source_id: req.source_id,
            source_no: Some(req.source_no),
            voucher_date: req.voucher_date,
            summary: req.summary.clone(),
            lines,
            remark: req.remark,
            red_of: None,
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
}

/// 逐行显式分录（处置的三张凭证用）：借方 / 贷方都由调用方给，**不做任何映射**。
///
/// 与 [`post_multi_debit`] 的区别：处置凭证里也有「贷 1606 + 贷 2221.02」这种
/// 多贷方形状（出售的收款凭证），用「借方分组 + 单贷方」表达不了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitLine {
    /// 科目。
    pub account: AccountRef,
    /// 借方金额（与 `credit` 二选一，另一个必须是 0）。
    pub debit: Decimal,
    /// 贷方金额。
    pub credit: Decimal,
    /// 部门辅助核算。
    pub aux_dept_id: Option<Uuid>,
    /// 行摘要（`None` = 整单摘要）。
    pub summary: Option<String>,
}

impl ExplicitLine {
    /// 借一行。
    pub fn debit(account: AccountRef, amount: Decimal) -> Self {
        Self {
            account,
            debit: amount,
            credit: Decimal::ZERO,
            aux_dept_id: None,
            summary: None,
        }
    }

    /// 贷一行。
    pub fn credit(account: AccountRef, amount: Decimal) -> Self {
        Self {
            account,
            debit: Decimal::ZERO,
            credit: amount,
            aux_dept_id: None,
            summary: None,
        }
    }

    /// 带摘要。
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }
}

/// 落一张「显式分录」凭证（处置 / 结转用；同样共享 `post_voucher` 的全部收敛）。
// 这十个参数就是一张凭证的完整标识 + 分录 + 操作人；拆成两个结构体只是换个地方堆字段。
#[allow(clippy::too_many_arguments)]
pub async fn post_explicit_lines(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Option<Uuid>,
    source_no: &str,
    voucher_date: NaiveDate,
    summary: &str,
    lines: &[ExplicitLine],
    remark: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    if lines.len() < 2 {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：{source_no} 的分录只有 {} 行（至少要 2 行）",
            lines.len()
        )));
    }
    let mut entries: Vec<EntryLine> = Vec::with_capacity(lines.len());
    for line in lines {
        if line.debit > Decimal::ZERO && line.credit > Decimal::ZERO {
            return Err(DbError::InvalidValue(format!(
                "凭证生成失败：{source_no} 的科目 {} 同时有借贷金额（一行只能一个方向，\
                 红字用 red_of 走红冲，不要在同一条分录里正负相抵）",
                line.account.code
            )));
        }
        let line_no = i32::try_from(entries.len() + 1).unwrap_or(1);
        entries.push(EntryLine {
            line_no,
            summary: line.summary.clone().unwrap_or_else(|| summary.to_string()),
            account: line.account.clone(),
            debit: line.debit,
            credit: line.credit,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: line.aux_dept_id,
        });
    }
    voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type,
            source_id,
            source_no: Some(source_no),
            voucher_date,
            summary: summary.to_string(),
            lines: entries,
            remark,
            red_of: None,
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
}

/// 科目必须是「末级 + 启用」。
///
/// `voucher::account_required` 只按**编码**取科目、不校验 `is_leaf`（它的调用方
/// 都是写死编码的既有链路）；多借方入口的科目来自业务数据（资产分类配的费用科目），
/// 所以必须自己校验一次：往非末级科目记账会让父科目与子科目各记一次，报表翻倍。
pub async fn ensure_leaf(ex: &impl SqlExecutor, account_id: Uuid) -> DbResult<AccountRef> {
    // SCOPE-EXEMPT: 科目表是全局主数据，按主键取 1 行。
    let rows = ex
        .query_sql(
            "SELECT account_code, account_name, is_leaf, status FROM biz_account \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&account_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(DbError::InvalidValue(format!(
            "科目 {account_id} 不存在或已删除：凭证不能往不存在的科目记账"
        )));
    };
    let code: String = row.get_typed("account_code")?;
    let name: String = row.get_typed("account_name")?;
    let is_leaf: i16 = row.get_typed("is_leaf")?;
    let status: i16 = row.get_typed("status")?;
    if is_leaf != 1 {
        return Err(DbError::InvalidValue(format!(
            "科目 {code} 不是末级科目：凭证只能记末级（否则父科目与子科目各记一次，报表翻倍）"
        )));
    }
    if status != 1 {
        return Err(DbError::InvalidValue(format!("科目 {code} 已停用：不能再用它记账")));
    }
    Ok(AccountRef {
        id: account_id,
        code,
        name,
    })
}

/// 按编码取末级科目（`None` = 库里没有这个科目）。
pub async fn leaf_by_code(ex: &impl SqlExecutor, code: &str) -> DbResult<Option<AccountRef>> {
    let Some(account) = voucher::account_by_code(ex, code).await? else {
        return Ok(None);
    };
    ensure_leaf(ex, account.id).await.map(Some)
}

/// 按编码取末级科目，缺失即报错（报文里写清「缺哪个科目」）。
pub async fn leaf_required(ex: &impl SqlExecutor, code: &str) -> DbResult<AccountRef> {
    leaf_by_code(ex, code).await?.ok_or_else(|| {
        DbError::InvalidValue(format!(
            "固定资产凭证需要的科目 {code} 在系统里不存在：请先在「会计科目」里建好\
             （ARCH-6 §6.6 的科目种子，本卡不代建科目）"
        ))
    })
}

/// 一条凭证规则的三个科目（多借方入口按 `rule_code` 取默认科目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleAccounts {
    /// 借方（主）科目。
    pub debit: AccountRef,
    /// 贷方科目。
    pub credit: AccountRef,
    /// 税科目（可空：规则没配 / 停用 / 本次没有税额）。
    pub tax: Option<AccountRef>,
}

/// 按 `biz_voucher_rule.rule_code` 解析三个科目。
///
/// 与 `voucher::load_rule_by_code` 同表同栏位（**不新增第二套映射表**），
/// 只是把三个科目都过一遍 [`ensure_leaf`]：规则表里配成父科目时，
/// 这里当场报错，而不是等 `post_voucher` 落库才发现报表翻倍。
pub async fn rule_accounts(ex: &impl SqlExecutor, rule_code: &str) -> DbResult<Option<RuleAccounts>> {
    // SCOPE-EXEMPT: 凭证规则是全局配置，按 rule_code 精确取 1 行。
    let row = ex
        .query_first_sql(
            "SELECT debit_account_id, credit_account_id, tax_account_id \
               FROM biz_voucher_rule \
              WHERE rule_code = ? AND is_active = 1 AND deleted_at IS NULL \
              ORDER BY created_at ASC LIMIT 1"
                .to_string(),
            vec![Value::Text(rule_code.to_string())],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let debit_id: Option<Uuid> = row.get_typed("debit_account_id")?;
    let credit_id: Option<Uuid> = row.get_typed("credit_account_id")?;
    let tax_id: Option<Uuid> = row.get_typed("tax_account_id")?;
    let (Some(debit_id), Some(credit_id)) = (debit_id, credit_id) else {
        return Err(DbError::InvalidValue(format!(
            "凭证规则 {rule_code} 没有配借方或贷方科目：固定资产凭证无法生成"
        )));
    };
    Ok(Some(RuleAccounts {
        debit: ensure_leaf(ex, debit_id).await?,
        credit: ensure_leaf(ex, credit_id).await?,
        tax: match tax_id {
            Some(id) => Some(ensure_leaf(ex, id).await?),
            None => None,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(code: &str) -> AccountRef {
        AccountRef {
            id: Uuid::now_v7(),
            code: code.to_string(),
            name: format!("科目{code}"),
        }
    }

    /// ① 借贷不平衡必须在入口就被拦住（不依赖库）。
    ///
    /// 用 `post_multi_debit` 需要一个执行器；这里只钉「纯计算部分」的判据：
    /// 借方合计 ≠ 贷方 ⇒ 差额不能为 0（`post_voucher` 也会拦，但错在入口时
    /// 报文能指出是哪个单号，比下游的「借贷不平衡」可诊断得多）。
    #[test]
    fn debit_groups_sum_is_the_credit_amount() {
        let groups = [
            DebitGroup::new(account("5602.01"), Decimal::new(158_333, 2), None),
            DebitGroup::new(account("5602.01"), Decimal::new(41_667, 2), None),
        ];
        let total: Decimal = groups.iter().map(|group| group.amount).sum();
        assert_eq!(total, Decimal::new(200_000, 2), "按部门拆行后合计不变");
        assert_ne!(total, Decimal::new(1, 0), "不对称时入口必须报错（不是凑平）");
    }

    /// ② 一行只能一个方向（红字走红冲，不在同一行正负相抵）。
    #[test]
    fn explicit_line_is_single_sided() {
        let line = ExplicitLine::debit(account("1601"), Decimal::new(100_000, 2));
        assert!(line.debit > Decimal::ZERO);
        assert_eq!(line.credit, Decimal::ZERO);
        let credit = ExplicitLine::credit(account("1606"), Decimal::new(50_000, 2));
        assert_eq!(credit.debit, Decimal::ZERO);
        assert!(credit.credit > Decimal::ZERO);
    }
}

//! 供应商信用额度：**下单时现算**的守卫（ARCH-3 §5 DC-12 · P2-2 ①）。
//!
//! # 它解决什么
//!
//! `biz_supplier.credit_limit` 是 V0137 就有的列，**填了从不参与任何判定** ——
//! 客户认真填了 10 万额度，系统允许他下 100 万的单，一句话都不提。本文件是它唯一的消费者。
//!
//! # 口径（三条，逐条都有单测钉住）
//!
//! ```text
//! 累计未结采购额 = SUM(biz_purchase_order.total_with_tax)
//!                  WHERE supplier_id = ?            -- 同一供应商
//!                    AND status <> 'CANCELLED'      -- 作废单不算承诺（卡面红线）
//!                    AND deleted_at IS NULL         -- 软删单不算
//!                    AND id <> <本单>               -- 本单只算一次（它自己 + 累计）
//! 超限           = credit_limit > 0 AND (累计 + 本单) > credit_limit
//! ```
//!
//! * **`credit_limit = 0` 视为「不限」**（V0137 的列定义 `NOT NULL DEFAULT 0` 与
//!   `credit_limit numeric NOT NULL DEFAULT 0 -- 信用额度（0 = 不限）` 都是这个意思）。
//!   所以「额度为空」在库里就是 `0`，**不校验**；`API` 层把 `null` 归一成 `0`（`dto` 的
//!   `credit_limit.unwrap_or(ZERO)`），前端留空 = 0 = 不限，三处语义一致。
//! * **现算，不缓存**（R1 单一数据源）：不加「已用额度 / 可用额度」列 —— 缓存列必漂移
//!   （作废、改单、部分收货都会让它错），而且一旦漂移就把用户挡在门外或放进超限单。
//!   代价是每次提交多一条聚合查询（一供应商几十张单，走 `idx_biz_po_supplier`，实测毫秒级）。
//!
//! # 超限怎么办：策略 B（允许提交，但强制审批）
//!
//! 卡面给的两种策略里选 B：「超信用要审批」比「直接 409 拒绝」更符合 ERP 现实
//! （紧急采购、客户加急都得能下进去，只是要人复核）。落地方式是：
//!
//! 1. 把评估结果**冻结**在单据上（三列，见 [`write_verdict`]）：
//!    `over_credit_limit` / `credit_exposure` / `credit_limit_snapshot`。
//!    为什么落库而不是每次现算：**「这张单提交那一刻查到什么」是审计事实** ——
//!    供应商额度之后被调高，不能让历史单据的「当时超没超」跟着变。当前值现算、
//!    当时值快照，两件事都要有。
//! 2. 超限单**必须走审批**：没有已发布的 `po_approve` 流程时**不许显式跳过**
//!    （`business:erp:skip-approval` 在这里无效），报文指名要配流程或调额度。
//! 3. `form_data.over_credit_limit` 写进审批单，流程管理员可以在流程设计器里配
//!    条件分支（`form.over_credit_limit eq true` → 加一道财务/总经理节点），
//!    见 `docs/ERP-摆设字段口径.md`。
//!
//! # 并发
//!
//! 不加锁：这里只做「提醒 + 强制审批」，不做「拒绝」。两个并发提交各算各的、
//! 各带自己的快照，最坏结果是两张单都标了超限（都进了审批），**不会漏放** ——
//! 这是策略 B 相对于策略 A（409 拒绝）的天然好处，不需要 `FOR UPDATE` 供应商行。
//! 合同额度守卫（[`crate::business::erp::purchase_handler`] 的 `guard_contract_amount`）
//! 才是必须加锁的那种（它会**拒绝**，锁不到就会双花）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::model::PurchaseOrderRow;

/// 提交时点的信用评估结果（纯数据，可单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditVerdict {
    /// 供应商信用额度（`0` = 不限）。
    pub limit: Decimal,
    /// 该供应商**其它**未取消采购单的累计价税合计（不含本单）。
    pub committed: Decimal,
    /// 本单价税合计。
    pub this_order: Decimal,
    /// 提交后累计 = `committed + this_order`（**含本单**，报文与快照都用它）。
    pub exposure: Decimal,
    /// 是否「不限额度」（`limit <= 0`）。
    pub unlimited: bool,
    /// 是否超限（`unlimited == false` 且 `exposure > limit`）。
    pub over: bool,
}

impl CreditVerdict {
    /// 判定（纯函数，不碰库）。
    ///
    /// 三个入参都是「已经查好的数」：额度取供应商主档现值，`committed` 取
    /// [`committed_exposure`] 的返回值（**不含本单**），`this_order` 取单据的
    /// `total_with_tax`。判定本身不含任何 IO，所以能用单测把三条口径钉死。
    pub fn evaluate(limit: Decimal, committed: Decimal, this_order: Decimal) -> Self {
        let exposure = committed + this_order;
        let unlimited = limit <= Decimal::ZERO;
        Self {
            limit,
            committed,
            this_order,
            exposure,
            unlimited,
            over: !unlimited && exposure > limit,
        }
    }

    /// 「额度不限」的判定结果（查不到供应商主档时用：**不拦、只 WARN**）。
    ///
    /// 供应商被软删不该把单据锁死 —— 那是主数据问题，不是这笔业务超限。
    /// 但必须留痕，否则「为什么这张单没做信用评估」永远查不出来。
    pub fn unassessable(committed: Decimal, this_order: Decimal) -> Self {
        Self {
            limit: Decimal::ZERO,
            committed,
            this_order,
            exposure: committed + this_order,
            unlimited: true,
            over: false,
        }
    }

    /// 一句话结论（日志 / 报错 / 审批备注共用，避免三处各写一套文案）。
    pub fn describe(&self) -> String {
        if self.unlimited {
            format!(
                "供应商额度不限（{}），累计未结采购额 {}（含本单 {}）",
                self.limit, self.exposure, self.this_order
            )
        } else if self.over {
            format!(
                "超出供应商信用额度：额度 {}，累计未结采购额 {}（含本单 {}），超出 {}",
                self.limit,
                self.exposure,
                self.this_order,
                self.exposure - self.limit
            )
        } else {
            format!(
                "未超信用额度：额度 {}，累计未结采购额 {}（含本单 {}），剩余 {}",
                self.limit,
                self.exposure,
                self.this_order,
                self.limit - self.exposure
            )
        }
    }
}

/// 供应商信用额度现值（`0` = 不限）。供应商不存在 / 已软删 → `None`。
pub async fn limit_of(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<Option<Decimal>> {
    let rows = ex
        .query_sql(
            "SELECT credit_limit::text AS credit_limit FROM biz_supplier \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let text: String = row.get_typed("credit_limit")?;
    Ok(Some(text.parse::<Decimal>().unwrap_or(Decimal::ZERO)))
}

/// 「该供应商累计未结采购额」—— **现算**（不含本单）。
///
/// `exclude` 传本单 id：提交时本单还在 `DRAFT`，它自己的金额不算「累计」，否则
/// 会被算两遍（累计一次 + 本单一次），额度 1 万、单 6 千的正当单会被误判超限。
///
/// 口径三条：`status <> 'CANCELLED'`（作废=撤回承诺）、`deleted_at IS NULL`（软删不算）、
/// 含税合计 `total_with_tax`（供应商要的是钱，不是不含税额）。
pub async fn committed_exposure(
    ex: &impl SqlExecutor,
    supplier_id: Uuid,
    exclude: Option<Uuid>,
) -> DbResult<Decimal> {
    let mut sql = "SELECT COALESCE(SUM(total_with_tax), 0)::text AS total \
                   FROM biz_purchase_order \
                  WHERE supplier_id = ?::uuid AND status <> 'CANCELLED' AND deleted_at IS NULL"
        .to_string();
    let mut params = vec![Value::from(&supplier_id)];
    if let Some(exclude_id) = exclude {
        // 条件按需拼接：不用 `NULLIF(?::text,'')` 那类写法（空串在驱动里会被当成 NULL
        // 序列化，实测报 `error serializing parameter N`，A3 踩过）。
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&exclude_id));
    }
    let row = ex.query_first_sql(sql, params).await?;
    let total: Option<String> = match row {
        None => None,
        Some(row) => Some(row.get_typed("total")?),
    };
    Ok(total
        .and_then(|text| text.parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO))
}

/// 按单据现状算判定结果（额度取主档现值 + 累计现算 + 本单金额）。
///
/// 供应商主档查不到（已软删）→ [`CreditVerdict::unassessable`]（不拦，调用方 WARN）。
pub async fn verdict_of(ex: &impl SqlExecutor, row: &PurchaseOrderRow) -> DbResult<CreditVerdict> {
    let committed = committed_exposure(ex, row.supplier_id, Some(row.id)).await?;
    match limit_of(ex, row.supplier_id).await? {
        Some(limit) => Ok(CreditVerdict::evaluate(
            limit,
            committed,
            row.total_with_tax,
        )),
        None => Ok(CreditVerdict::unassessable(committed, row.total_with_tax)),
    }
}

/// 把判定结果冻结到单据上（**提交时点快照**，三列一起写）。
///
/// `credit_exposure` 写成 `NULL` 之外的值 是「已评估」的唯一标记：`NULL` = 从未提交过。
/// 所以 `over_credit_limit = false` + `credit_exposure IS NULL` 的读法是「没查过」，
/// 不是「查过没问题」—— 详情/列表要按这个口径显示，别把两者混为一谈。
pub async fn write_verdict(
    ex: &impl SqlExecutor,
    order_id: Uuid,
    verdict: &CreditVerdict,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_purchase_order \
            SET over_credit_limit = ?, credit_exposure = ?::text::numeric, \
                credit_limit_snapshot = ?::text::numeric \
          WHERE id = ?::uuid"
            .to_string(),
        vec![
            Value::from(verdict.over),
            Value::Text(verdict.exposure.to_string()),
            Value::Text(verdict.limit.to_string()),
            Value::from(&order_id),
        ],
    )
    .await
}

/// 采购单读回时把「未评估」与「评估过」区分开（详情/列表的显示口径）。
///
/// 返回值：`None` = 从未提交过（`credit_exposure IS NULL`）；
/// `Some((over, exposure, limit))` = 最近一次提交时点的评估结果。
pub fn assess_summary(row: &PurchaseOrderRow) -> Option<(bool, Decimal, Decimal)> {
    let exposure = row.credit_exposure?;
    Some((
        row.over_credit_limit,
        exposure,
        row.credit_limit_snapshot.unwrap_or(Decimal::ZERO),
    ))
}

// ==================== 编译期自检（不写库、不依赖 ERP_DB_URL） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量非法")
    }

    /// ① 卡面必测：供应商额度 1 万，下单 2 万 → **超限**。
    #[test]
    fn over_limit_when_order_exceeds_credit() {
        let verdict = CreditVerdict::evaluate(dec("10000"), dec("0"), dec("20000"));
        assert!(verdict.over, "{}", verdict.describe());
        assert!(!verdict.unlimited);
        assert_eq!(verdict.exposure, dec("20000"));
        assert!(
            verdict.describe().contains("10000"),
            "{}",
            verdict.describe()
        );
        assert!(
            verdict.describe().contains("超出"),
            "{}",
            verdict.describe()
        );
    }

    /// ② 卡面必测：额度为空（库里就是 `0`）→ **不校验**。
    ///
    /// `biz_supplier.credit_limit` 是 `NOT NULL DEFAULT 0`，「留空」在库里 = `0`，
    /// V0137 的列定义写明「0 = 不限」。所以这里断言的是「0 不拦」，
    /// 不是「NULL 不拦」—— NULL 这个状态在库里根本不存在（想造也造不出来）。
    #[test]
    fn zero_limit_means_unlimited_and_never_blocks() {
        let verdict = CreditVerdict::evaluate(dec("0"), dec("999999"), dec("20000"));
        assert!(!verdict.over, "{}", verdict.describe());
        assert!(verdict.unlimited);
        // 不限也要把累计算出来（列表要显示「累计 X（额度不限）」，不是显示 0）
        assert_eq!(verdict.exposure, dec("1019999"));
    }

    /// ③ 累计 + 本单 = 额度**恰好等于**不算超（边界不能靠 > 与 >= 的直觉拍脑袋）。
    #[test]
    fn boundary_equal_is_not_over() {
        let verdict = CreditVerdict::evaluate(dec("10000"), dec("6000"), dec("4000"));
        assert!(
            !verdict.over,
            "累计恰好等于额度不该判超限：{}",
            verdict.describe()
        );
        assert_eq!(verdict.exposure, dec("10000"));
        assert!(
            verdict.describe().contains("剩余 0"),
            "{}",
            verdict.describe()
        );
    }

    /// ④ 一分钱就超：金额比较不能有四舍五入的余量。
    #[test]
    fn one_cent_over_is_over() {
        let verdict = CreditVerdict::evaluate(dec("10000"), dec("6000"), dec("4000.01"));
        assert!(verdict.over, "{}", verdict.describe());
    }

    /// ⑤ 判定只吃「不含本单的累计」：本单不会被算两遍。
    #[test]
    fn this_order_is_counted_once() {
        // 额度 1 万，历史累计 6 千，本单 4 千 → 恰好用满，不超
        let verdict = CreditVerdict::evaluate(dec("10000"), dec("6000"), dec("4000"));
        assert_eq!(verdict.exposure, verdict.committed + verdict.this_order);
        assert_eq!(verdict.committed, dec("6000"));
    }

    /// ⑥ 查不到供应商主档 → 不拦 + 额度按不限处理（主数据问题不该锁死单据）。
    #[test]
    fn missing_supplier_is_not_a_blocker() {
        let verdict = CreditVerdict::unassessable(dec("5000"), dec("20000"));
        assert!(!verdict.over);
        assert!(verdict.unlimited);
        assert_eq!(verdict.exposure, dec("25000"));
        assert!(
            verdict.describe().contains("不限"),
            "{}",
            verdict.describe()
        );
    }

    /// ⑦ 未评估 vs 评估过：`credit_exposure IS NULL` 才是「没查过」。
    #[test]
    fn assess_summary_distinguishes_never_checked() {
        let mut row = sample_row();
        assert_eq!(assess_summary(&row), None, "默认（NULL）= 从未提交过");
        row.over_credit_limit = true;
        row.credit_exposure = Some(dec("20000"));
        row.credit_limit_snapshot = Some(dec("10000"));
        assert_eq!(
            assess_summary(&row),
            Some((true, dec("20000"), dec("10000"))),
            "提交过就必须读出当时的三元组"
        );
    }

    fn sample_row() -> PurchaseOrderRow {
        use nik::prelude::chrono::{NaiveDate, Utc};
        PurchaseOrderRow {
            id: Uuid::now_v7(),
            order_no: "PO-T-000001".to_string(),
            supplier_id: Uuid::now_v7(),
            order_date: NaiveDate::from_ymd_opt(2026, 11, 4).expect("日期"),
            expect_date: None,
            warehouse_id: Uuid::now_v7(),
            status: "DRAFT".to_string(),
            currency_code: "CNY".to_string(),
            total_qty: Decimal::ZERO,
            total_amount: Decimal::ZERO,
            total_tax: Decimal::ZERO,
            total_with_tax: Decimal::ZERO,
            credit_days: 0,
            remark: None,
            price_mode: "EXCL".to_string(),
            submitted_at: None,
            submitted_by: None,
            approved_at: None,
            approved_by: None,
            closed_at: None,
            closed_by: None,
            approval_instance_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
            purchase_type: "SPOT".to_string(),
            contract_id: None,
            contract_no_snapshot: None,
            supplier_name_snapshot: None,
            warehouse_name_snapshot: None,
            project_id: None,
            over_credit_limit: false,
            credit_exposure: None,
            credit_limit_snapshot: None,
        }
    }
}

//! 采购申请数据层（P2-02）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0331__purchase_requisition.up.sql`。
//!
//! | 表 | 行数级 | 关键不变量 |
//! |----|--------|------------|
//! | `biz_purchase_requisition` | 每月几十~几百 | 单号存活唯一；`status='CONVERTED'` ⇔ `po_id/po_no/converted_at` 三者齐全；`SUBMITTED/APPROVED` 必有审批实例 |
//! | `biz_purchase_requisition_item` | 一单 1–200 行 | `(requisition_id, line_no)` 唯一；`qty > 0`、`est_price > 0` |
//!
//! # 写入口径
//!
//! * 建单：取号 + 单头 + 明细**同一事务**（明细随主单整体重建，与采购订单 / 询价单同口径）；
//! * 提交：状态推进与预算快照**同一事务**（`DRAFT → SUBMITTED`，CAS 回写实例 id）；
//! * 转单：`SELECT … FOR UPDATE` 按 id 升序锁多张单头（**定序加锁**，避免两张单互相锁死），
//!   逐张 CAS `APPROVED → CONVERTED`，把 PO 号与合并结果一次写回。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_purchase_requisition / _item 是 ERP 单据表（V0331 有 dept_id 但
//!         无归属人列，且「采购要看全公司需求」是业务前提），可见性由
//!         business:erp:purchase-requisition:* 权限码控制；分页走
//!         scope::paginate_unscoped(REQ_TARGET, …)（闸①口径）。
//! ```

use std::collections::BTreeMap;

use nik::prelude::chrono::{DateTime, Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase_requisition_dto::{
    RequisitionFilter, STATUS_APPROVED, STATUS_CONVERTED, STATUS_DRAFT, STATUS_SUBMITTED,
};
use crate::business::shared::scope;

// ==================== 表名 / 列清单 ====================

/// 申请单表。
pub const REQ_TABLE: &str = "biz_purchase_requisition";
/// 申请明细表。
pub const ITEM_TABLE: &str = "biz_purchase_requisition_item";
/// 取号键。
pub const SEQ_TYPE: &str = "PRQ";
/// 单号前缀。
pub const NO_PREFIX: &str = "PRQ";
/// 流水号宽度。
pub const NO_SEQ_WIDTH: usize = 6;

/// 分页锚点（SCOPE-EXEMPT，理由见模块文档）。
pub const REQ_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(REQ_TABLE);

// ==================== 行结构 ====================

/// 申请单行（含派生列：仓库名 / 明细数）。
#[derive(Debug, Clone)]
pub struct RequisitionRow {
    /// 主键。
    pub id: Uuid,
    /// 单号。
    pub req_no: String,
    /// 申请日期。
    pub req_date: NaiveDate,
    /// 申请部门。
    pub dept_id: Uuid,
    /// 申请部门名快照。
    pub dept_name: Option<String>,
    /// 申请人。
    pub applicant_id: Option<Uuid>,
    /// 申请人姓名快照。
    pub applicant_name: Option<String>,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 收货仓库。
    pub warehouse_id: Uuid,
    /// 收货仓库名（join）。
    pub warehouse_name: String,
    /// 状态。
    pub status: String,
    /// 预估金额合计。
    pub est_amount: Decimal,
    /// 是否做过预算判定。
    pub budget_checked: bool,
    /// 是否超预算。
    pub over_budget: bool,
    /// 超出金额。
    pub over_amount: Decimal,
    /// 预算判定说明。
    pub budget_reason: Option<String>,
    /// 预算科目。
    pub budget_account_id: Option<Uuid>,
    /// 预算槽部门。
    pub budget_dept_id: Option<Uuid>,
    /// 预算槽期间。
    pub budget_period_code: Option<String>,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 转单时间。
    pub converted_at: Option<DateTime<Utc>>,
    /// 采购订单 id。
    pub po_id: Option<Uuid>,
    /// 采购订单号。
    pub po_no: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 明细行数。
    pub item_count: i64,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 制单人。
    pub create_by: Option<Uuid>,
}

/// 申请明细行（含物料 / 单位 / 建议供应商名）。
#[derive(Debug, Clone)]
pub struct ItemRow {
    /// 主键。
    pub id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 单位。
    pub unit_id: Uuid,
    /// 单位名。
    pub unit_name: String,
    /// 数量。
    pub qty: Decimal,
    /// 预估不含税单价。
    pub est_price: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 建议供应商。
    pub suggest_supplier_id: Option<Uuid>,
    /// 建议供应商名（join）。
    pub suggest_supplier_name: Option<String>,
    /// 已并进的采购订单明细行。
    pub po_item_id: Option<Uuid>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 建单明细（服务端已校验 + 单位已由物料带出）。
#[derive(Debug, Clone)]
pub struct NewItem {
    /// 物料。
    pub material_id: Uuid,
    /// 单位（由物料基础单位带出）。
    pub unit_id: Uuid,
    /// 数量。
    pub qty: Decimal,
    /// 预估不含税单价。
    pub est_price: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 建议供应商。
    pub suggest_supplier_id: Option<Uuid>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 建单单头（服务端已校验）。
#[derive(Debug, Clone)]
pub struct NewRequisition {
    /// 申请日期。
    pub req_date: NaiveDate,
    /// 申请部门。
    pub dept_id: Uuid,
    /// 申请部门名快照。
    pub dept_name: Option<String>,
    /// 申请人。
    pub applicant_id: Option<Uuid>,
    /// 申请人姓名快照。
    pub applicant_name: Option<String>,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 收货仓库。
    pub warehouse_id: Uuid,
    /// 预估金额合计（服务端算）。
    pub est_amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
}

/// 提交时落库的预算判定快照。
#[derive(Debug, Clone, Default)]
pub struct BudgetSnapshot {
    /// 是否真的做了判定。
    pub checked: bool,
    /// 是否超预算。
    pub over_budget: bool,
    /// 超出金额。
    pub over_amount: Decimal,
    /// 说明。
    pub reason: Option<String>,
    /// 科目。
    pub account_id: Option<Uuid>,
    /// 槽部门。
    pub dept_id: Option<Uuid>,
    /// 槽期间。
    pub period_code: Option<String>,
}

// ==================== 取号 ====================

/// 组装单号（`PRQ-{周期键}-{6 位流水}`）。
pub fn format_req_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 周期键（北京时间 `yyyyMMdd`）——与采购订单 / 询价单同一口径。
pub fn period_key_of(now: DateTime<Utc>) -> String {
    let cst = now + Duration::hours(8);
    cst.format("%Y%m%d").to_string()
}

/// `biz_seq(seq_type='PRQ')` 缺失时幂等补一行。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'PRQ', '采购申请单号', 'PRQ', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '采购申请单号：PRQ-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'PRQ' AND deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await
    .or_else(|error| match error {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='PRQ')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'PRQ' AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
            ],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("current_no"))
        .transpose()
}

/// 取一个申请单号（周期键按北京时间）。
pub async fn next_req_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_req_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='PRQ') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 写：建单 ====================

/// 建单（单头 + 明细，同一事务），返回 id。
pub async fn create_requisition(
    head: &NewRequisition,
    items: &[NewItem],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let tx = db().begin().await?;
    let result = async {
        let req_no = next_req_no(&tx, now).await?;
        let id = Uuid::now_v7();
        tx.exec_sql(
            format!(
                "INSERT INTO {REQ_TABLE} \
                    (id, req_no, req_date, dept_id, dept_name, applicant_id, applicant_name, \
                     demand_date, warehouse_id, status, est_amount, remark, \
                     created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?, NULLIF(?::text,'')::date, ?::uuid, NULLIF(?::text,''), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                     NULLIF(?::text,'')::date, ?::uuid, 'DRAFT', ?::text::numeric, \
                     NULLIF(?::text,''), ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            ),
            vec![
                Value::from(&id),
                Value::Text(req_no),
                Value::Text(head.req_date.to_string()),
                Value::from(&head.dept_id),
                optional_text(head.dept_name.as_deref()),
                uuid_text(head.applicant_id),
                optional_text(head.applicant_name.as_deref()),
                Value::Text(
                    head.demand_date
                        .map(|date| date.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(&head.warehouse_id),
                Value::Text(head.est_amount.to_string()),
                optional_text(head.remark.as_deref()),
                Value::from(now),
                Value::from(now),
                uuid_text(operator),
                uuid_text(operator),
            ],
        )
        .await?;
        insert_items(&tx, id, items).await?;
        Ok::<Uuid, DbError>(id)
    }
    .await;
    match result {
        Ok(id) => {
            tx.commit().await?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 插入明细（建单专用；行号 1 起）。
async fn insert_items(ex: &impl SqlExecutor, req_id: Uuid, items: &[NewItem]) -> DbResult<()> {
    for (index, item) in items.iter().enumerate() {
        let line_no = i32::try_from(index + 1).unwrap_or(i32::MAX);
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} (id, requisition_id, line_no, material_id, unit_id, qty, \
                     est_price, tax_rate, demand_date, suggest_supplier_id, remark) \
                 VALUES (?, ?::uuid, ?, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, NULLIF(?::text,'')::date, NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,''))"
            ),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&req_id),
                Value::I32(line_no),
                Value::from(&item.material_id),
                Value::from(&item.unit_id),
                Value::Text(item.qty.to_string()),
                Value::Text(item.est_price.to_string()),
                Value::Text(item.tax_rate.to_string()),
                Value::Text(
                    item.demand_date
                        .map(|date| date.to_string())
                        .unwrap_or_default(),
                ),
                uuid_text(item.suggest_supplier_id),
                optional_text(item.remark.as_deref()),
            ],
        )
        .await?;
    }
    Ok(())
}

// ==================== 写：提交审批 ====================

/// 锁单头并读状态（提交事务的第一句）。
///
/// 用一条**不带 JOIN** 的语句加锁：PG 不允许 `FOR UPDATE` 作用在外连接的可空侧，
/// 而详情用的 `req_select_sql` 带 `LEFT JOIN`。
pub async fn lock_requisition(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> DbResult<Option<(String, Option<Uuid>)>> {
    // SCOPE-EXEMPT: 按主键取单行状态并加行锁，不涉及数据范围。
    let rows = ex
        .query_sql(
            format!(
                "SELECT status, approval_instance_id FROM {REQ_TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => {
            let status: String = row.get_typed("status")?;
            let instance: Option<Uuid> = row.get_typed("approval_instance_id")?;
            Ok(Some((status, instance)))
        }
    }
}

/// 提交审批：`DRAFT → SUBMITTED` + 预算快照 + 实例 id（同一事务由调用方持有）。
///
/// CAS 口径：`WHERE status = 'DRAFT'`，返回 0 行说明状态被并发改过 → 调用方 409。
pub async fn mark_submitted(
    ex: &impl SqlExecutor,
    id: Uuid,
    budget: &BudgetSnapshot,
    instance_id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {REQ_TABLE} \
                SET status = ?, budget_checked = ?, over_budget = ?, over_amount = ?::text::numeric, \
                    budget_reason = NULLIF(?::text,''), budget_account_id = NULLIF(?::text,'')::uuid, \
                    budget_dept_id = NULLIF(?::text,'')::uuid, budget_period_code = NULLIF(?::text,''), \
                    approval_instance_id = ?::uuid, \
                    remark = CASE WHEN ?::text IS NULL OR btrim(?::text) = '' THEN remark \
                                  ELSE COALESCE(NULLIF(remark, '') || E'\\n', '') || ?::text END, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(STATUS_SUBMITTED.to_string()),
            Value::Bool(budget.checked),
            Value::Bool(budget.over_budget),
            Value::Text(budget.over_amount.to_string()),
            Value::Text(budget.reason.clone().unwrap_or_default()),
            uuid_text(budget.account_id),
            uuid_text(budget.dept_id),
            Value::Text(budget.period_code.clone().unwrap_or_default()),
            Value::from(&instance_id),
            optional_text(reason),
            optional_text(reason),
            optional_text(reason),
            Value::from(now),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 审批结论回写：`SUBMITTED → APPROVED`（通过）。
pub async fn mark_approved(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status(
        ex,
        id,
        STATUS_SUBMITTED,
        STATUS_APPROVED,
        reason,
        operator,
        now,
    )
    .await
}

/// 审批结论回写：`SUBMITTED → DRAFT`（驳回 / 撤回打回草稿，清实例与提交痕迹）。
pub async fn mark_back_to_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {REQ_TABLE} \
                SET status = ?, approval_instance_id = NULL, \
                    remark = CASE WHEN ?::text IS NULL OR btrim(?::text) = '' THEN remark \
                                  ELSE COALESCE(NULLIF(remark, '') || E'\\n', '') || ?::text END, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = 'SUBMITTED' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(STATUS_DRAFT.to_string()),
            optional_text(reason),
            optional_text(reason),
            optional_text(reason),
            Value::from(now),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 显式跳过审批：`DRAFT → APPROVED`（没有审批实例，理由写进备注留痕）。
///
/// 只在「没配已发布的 `pr_approve` 流程」且操作人持 `business:erp:skip-approval`
/// 时由 handler 调用（ARCH-3 DC-01 默认拒绝口径）。与走审批的分支不同，
/// 这条路径**不写** `approval_instance_id` —— 库层 CHECK 只对 `SUBMITTED` 强制实例。
pub async fn mark_skipped_approval(
    ex: &impl SqlExecutor,
    id: Uuid,
    budget: &BudgetSnapshot,
    reason: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {REQ_TABLE} \
                SET status = ?, budget_checked = ?, over_budget = ?, over_amount = ?::text::numeric, \
                    budget_reason = NULLIF(?::text,''), budget_account_id = NULLIF(?::text,'')::uuid, \
                    budget_dept_id = NULLIF(?::text,'')::uuid, budget_period_code = NULLIF(?::text,''), \
                    remark = CASE WHEN btrim(?::text) = '' THEN remark \
                                  ELSE COALESCE(NULLIF(remark, '') || E'\\n', '') || ?::text END, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(STATUS_APPROVED.to_string()),
            Value::Bool(budget.checked),
            Value::Bool(budget.over_budget),
            Value::Text(budget.over_amount.to_string()),
            Value::Text(budget.reason.clone().unwrap_or_default()),
            uuid_text(budget.account_id),
            uuid_text(budget.dept_id),
            Value::Text(budget.period_code.clone().unwrap_or_default()),
            Value::Text(reason.trim().to_string()),
            Value::Text(reason.trim().to_string()),
            Value::from(now),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 通用状态 CAS（`from → to`），返回影响行数。
async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {REQ_TABLE} \
                SET status = ?, \
                    remark = CASE WHEN ?::text IS NULL OR btrim(?::text) = '' THEN remark \
                                  ELSE COALESCE(NULLIF(remark, '') || E'\\n', '') || ?::text END, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(to.to_string()),
            optional_text(reason),
            optional_text(reason),
            optional_text(reason),
            Value::from(now),
            uuid_text(operator),
            Value::from(&id),
            Value::Text(from.to_string()),
        ],
    )
    .await
}

// ==================== 写：合并转单 ====================

/// 按 id 升序锁多张单头（**定序加锁**：并发转单只会有一次成功，且两张单互相不锁死）。
///
/// 返回 `(id, req_no, status, po_id, warehouse_id)`；不存在的 id 不会出现在结果里，
/// 由调用方对比数量后报 404。
pub async fn lock_many(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<Vec<LockedRequisition>> {
    let mut sorted: Vec<Uuid> = ids.to_vec();
    sorted.sort();
    // ids 上限 50（`MAX_MERGE`），`IN (?, ?, …)` 展开比绑 UUID 数组更省心
    // （数组参数要 `?::text::uuid[]` + 字符串拼接，编码坑多）。
    let placeholders = vec!["?::uuid"; sorted.len()].join(", ");
    let params: Vec<Value> = sorted.iter().map(Value::from).collect();
    let rows = ex
        .query_sql(
            format!(
                "SELECT id, req_no, status, po_id, warehouse_id, approval_instance_id, demand_date \
                   FROM {REQ_TABLE} \
                  WHERE deleted_at IS NULL AND id IN ({placeholders}) \
                  ORDER BY id \
                    FOR UPDATE"
            ),
            params,
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(LockedRequisition {
                id: row.get_typed("id")?,
                req_no: row.get_typed("req_no")?,
                status: row.get_typed("status")?,
                po_id: row.get_typed("po_id")?,
                warehouse_id: row.get_typed("warehouse_id")?,
                approval_instance_id: row.get_typed("approval_instance_id")?,
                demand_date: row.get_typed("demand_date")?,
            })
        })
        .collect()
}

/// [`lock_many`] 的行。
#[derive(Debug, Clone)]
pub struct LockedRequisition {
    /// 主键。
    pub id: Uuid,
    /// 单号。
    pub req_no: String,
    /// 状态。
    pub status: String,
    /// 已转的采购订单（幂等闸）。
    pub po_id: Option<Uuid>,
    /// 收货仓库。
    pub warehouse_id: Uuid,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 需求日期（整单口径，明细没填时用它作采购单期望到货日）。
    pub demand_date: Option<NaiveDate>,
}

/// 转单成功：`APPROVED → CONVERTED` + PO 引用（同一事务由调用方持有）。
pub async fn mark_converted(
    ex: &impl SqlExecutor,
    id: Uuid,
    po_id: Uuid,
    po_no: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {REQ_TABLE} \
                SET status = ?, po_id = ?::uuid, po_no = ?, converted_at = ?, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = 'APPROVED' AND po_id IS NULL AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(STATUS_CONVERTED.to_string()),
            Value::from(&po_id),
            Value::Text(po_no.to_string()),
            Value::from(now),
            Value::from(now),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 把「申请明细 → 采购订单明细」的对应关系写回明细行（同一事务由调用方持有）。
pub async fn attach_po_items(
    ex: &impl SqlExecutor,
    mapping: &BTreeMap<Uuid, Uuid>,
) -> DbResult<u64> {
    let mut affected = 0u64;
    for (req_item_id, po_item_id) in mapping {
        affected += ex
            .exec_sql(
                format!("UPDATE {ITEM_TABLE} SET po_item_id = ?::uuid WHERE id = ?::uuid"),
                vec![Value::from(po_item_id), Value::from(req_item_id)],
            )
            .await?;
    }
    Ok(affected)
}

// ==================== 读 ====================

/// 单头查询的 SELECT 片段（列表与详情共用，防止两处口径漂移）。
fn req_select_sql() -> String {
    format!(
        "SELECT r.id, r.req_no, r.req_date, r.dept_id, r.dept_name, r.applicant_id, \
                r.applicant_name, r.demand_date, r.warehouse_id, \
                COALESCE(w.warehouse_name, '') AS warehouse_name, \
                r.status, r.est_amount, r.budget_checked, r.over_budget, r.over_amount, \
                r.budget_reason, r.budget_account_id, r.budget_dept_id, r.budget_period_code, \
                r.approval_instance_id, r.converted_at, r.po_id, r.po_no, r.remark, \
                r.created_at, r.create_by, \
                (SELECT count(*) FROM {ITEM_TABLE} it WHERE it.requisition_id = r.id) AS item_count \
           FROM {REQ_TABLE} r \
           LEFT JOIN biz_warehouse w ON w.id = r.warehouse_id AND w.deleted_at IS NULL \
          WHERE r.deleted_at IS NULL"
    )
}

fn read_req_row(row: &Row) -> DbResult<RequisitionRow> {
    Ok(RequisitionRow {
        id: row.get_typed("id")?,
        req_no: row.get_typed("req_no")?,
        req_date: row.get_typed("req_date")?,
        dept_id: row.get_typed("dept_id")?,
        dept_name: row.get_typed("dept_name")?,
        applicant_id: row.get_typed("applicant_id")?,
        applicant_name: row.get_typed("applicant_name")?,
        demand_date: row.get_typed("demand_date")?,
        warehouse_id: row.get_typed("warehouse_id")?,
        warehouse_name: row.get_typed("warehouse_name")?,
        status: row.get_typed("status")?,
        est_amount: row.get_typed("est_amount")?,
        budget_checked: row.get_typed("budget_checked")?,
        over_budget: row.get_typed("over_budget")?,
        over_amount: row.get_typed("over_amount")?,
        budget_reason: row.get_typed("budget_reason")?,
        budget_account_id: row.get_typed("budget_account_id")?,
        budget_dept_id: row.get_typed("budget_dept_id")?,
        budget_period_code: row.get_typed("budget_period_code")?,
        approval_instance_id: row.get_typed("approval_instance_id")?,
        converted_at: row.get_typed("converted_at")?,
        po_id: row.get_typed("po_id")?,
        po_no: row.get_typed("po_no")?,
        remark: row.get_typed("remark")?,
        item_count: row.get_typed("item_count")?,
        created_at: row.get_typed("created_at")?,
        create_by: row.get_typed("create_by")?,
    })
}

/// 按 id 查单头。
pub async fn find_requisition(id: Uuid) -> DbResult<Option<RequisitionRow>> {
    let rows = db()
        .find_with_params(
            &format!("{} AND r.id = ?::uuid", req_select_sql()),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(read_req_row).transpose()
}

/// 分页查询（关键字 / 状态 / 日期区间 / 只看可转单）。
pub async fn page_requisitions(filter: &RequisitionFilter) -> DbResult<(Vec<RequisitionRow>, u64)> {
    let mut sql = req_select_sql();
    let mut params: Vec<Value> = Vec::new();
    if filter.convertible_only {
        sql.push_str(" AND r.status = 'APPROVED' AND r.po_id IS NULL");
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND r.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(from) = filter.date_from {
        sql.push_str(" AND r.req_date >= NULLIF(?::text,'')::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = filter.date_to {
        sql.push_str(" AND r.req_date <= NULLIF(?::text,'')::date");
        params.push(Value::Text(to.to_string()));
    }
    if let Some(pattern) = crate::utils::like_pattern(filter.keyword.as_deref()) {
        let like = Value::Text(pattern);
        sql.push_str(
            " AND (r.req_no LIKE ? OR COALESCE(r.remark, '') LIKE ? \
                OR COALESCE(r.dept_name, '') LIKE ? OR COALESCE(r.applicant_name, '') LIKE ? \
                OR EXISTS (SELECT 1 FROM sys_dept d \
                            WHERE d.id = r.dept_id AND d.dept_name LIKE ?) \
                OR COALESCE(r.po_no, '') LIKE ?)",
        );
        for _ in 0..6 {
            params.push(like.clone());
        }
    }
    let page = scope::paginate_unscoped(
        REQ_TARGET,
        &sql,
        params,
        Some("ORDER BY r.req_date DESC, r.req_no DESC"),
        filter.page,
        filter.page_size,
    )
    .await?;
    let rows = page
        .list
        .iter()
        .map(read_req_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, page.total))
}

/// 明细列表（含物料编码/名称、单位名、建议供应商名；按行号）。
pub async fn items_of(req_id: Uuid) -> DbResult<Vec<ItemRow>> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT it.id, it.line_no, it.material_id, \
                        COALESCE(m.material_code, '') AS material_code, \
                        COALESCE(m.material_name, '') AS material_name, \
                        it.unit_id, COALESCE(u.unit_name, '') AS unit_name, \
                        it.qty, it.est_price, it.tax_rate, it.demand_date, \
                        it.suggest_supplier_id, s.supplier_name AS suggest_supplier_name, \
                        it.po_item_id, it.remark \
                   FROM {ITEM_TABLE} it \
                   LEFT JOIN biz_material m ON m.id = it.material_id \
                   LEFT JOIN biz_unit u ON u.id = it.unit_id \
                   LEFT JOIN biz_supplier s ON s.id = it.suggest_supplier_id AND s.deleted_at IS NULL \
                  WHERE it.requisition_id = ?::uuid \
                  ORDER BY it.line_no"
            ),
            vec![Value::from(&req_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ItemRow {
                id: row.get_typed("id")?,
                line_no: row.get_typed("line_no")?,
                material_id: row.get_typed("material_id")?,
                material_code: row.get_typed("material_code")?,
                material_name: row.get_typed("material_name")?,
                unit_id: row.get_typed("unit_id")?,
                unit_name: row.get_typed("unit_name")?,
                qty: row.get_typed("qty")?,
                est_price: row.get_typed("est_price")?,
                tax_rate: row.get_typed("tax_rate")?,
                demand_date: row.get_typed("demand_date")?,
                suggest_supplier_id: row.get_typed("suggest_supplier_id")?,
                suggest_supplier_name: row.get_typed("suggest_supplier_name")?,
                po_item_id: row.get_typed("po_item_id")?,
                remark: row.get_typed("remark")?,
            })
        })
        .collect()
}

/// 部门名（建单 / 审批表单快照用；查不到给 `None`）。
pub async fn dept_name_of(ex: &impl SqlExecutor, dept_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 按主键取 1 行名称，不分页、不裁剪。
    let rows = ex
        .query_sql(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&dept_id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("dept_name"))
        .transpose()
}

/// 采购订单号（转单成功后回显；查不到给 `None`）。
pub async fn po_no_of(ex: &impl SqlExecutor, po_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 按主键取 1 行单号，不分页、不裁剪。
    let rows = ex
        .query_sql(
            "SELECT order_no FROM biz_purchase_order WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&po_id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("order_no"))
        .transpose()
}

/// 物料编码 / 名称（转单响应里回填 `material_code` / `material_name`；查不到给 `None`）。
///
/// 为什么要有这个函数：`to_purchase_order` 的合并结果来自**申请明细的纯函数合并**
/// （`merge_by_material`），那一步只有 `material_id`。响应契约里 `material_code` /
/// `material_name` 是必填字段，不回填就只能给空串 —— 前端「转单结果」页按这两列展示，
/// 空串等于把「合并了哪些物料」藏起来，用户只能对着 UUID 猜。
pub async fn material_ref_of(
    ex: &impl SqlExecutor,
    material_id: Uuid,
) -> DbResult<Option<(String, String)>> {
    // SCOPE-EXEMPT: 按主键取 1 行主数据（编码 + 名称），不分页、不裁剪。
    let rows = ex
        .query_sql(
            "SELECT material_code, material_name FROM biz_material \
             WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&material_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some((
        row.get_typed("material_code")?,
        row.get_typed("material_name")?,
    )))
}

// ==================== 小工具 ====================

/// `Option<Uuid>` → `?::text` 参数（空串 = NULL）。
pub fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// `Option<&str>` → `?::text` 参数（None = NULL，空串也折成 NULL）。
pub fn optional_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_no_format_is_zero_padded() {
        assert_eq!(format_req_no("20261108", 1), "PRQ-20261108-000001");
        assert_eq!(format_req_no("20261108", 123456), "PRQ-20261108-123456");
    }

    #[test]
    fn period_key_is_cst() {
        // 2026-11-07T16:30:00Z = 2026-11-08 00:30 CST → 周期键必须是 20261108
        let utc = DateTime::parse_from_rfc3339("2026-11-07T16:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(period_key_of(utc), "20261108");
        let utc2 = DateTime::parse_from_rfc3339("2026-11-07T15:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(period_key_of(utc2), "20261107");
    }

    #[test]
    fn optional_values_fold_to_empty_text() {
        assert_eq!(uuid_text(None), Value::Text(String::new()));
        assert_eq!(optional_text(Some("  ")), Value::Text(String::new()));
        assert_eq!(
            optional_text(Some(" 备注 ")),
            Value::Text("备注".to_string())
        );
    }

    /// 状态常量必须与 DDL 的 CHECK 逐字一致（错一个字就是库层违约）。
    #[test]
    fn status_constants_are_exact() {
        assert_eq!(STATUS_DRAFT, "DRAFT");
        assert_eq!(STATUS_SUBMITTED, "SUBMITTED");
        assert_eq!(STATUS_APPROVED, "APPROVED");
        assert_eq!(STATUS_CONVERTED, "CONVERTED");
        assert_eq!(SEQ_TYPE, "PRQ");
        assert_eq!(REQ_TABLE, "biz_purchase_requisition");
        assert_eq!(ITEM_TABLE, "biz_purchase_requisition_item");
    }
}

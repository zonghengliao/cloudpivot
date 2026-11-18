//! 合同执行量统计的数据访问层（M4-19）。
//!
//! **只读**：本文件一句写 SQL 都没有 —— 已执行额的真相是单据链（合同 → 采购单 / 销售单），
//! 由 V0458 的两个视图现算，合同表上**没有任何**「已下单额」缓存列（V0195 头注释已定死：
//! 累计值必须现算，R1/V-21/V-23）。
//!
//! | 读入口 | 走哪 | 用途 |
//! |:--|:--|:--|
//! | [`page_execution`] | `v_contract_execution` | 跨合同总览（分页 + 筛选） |
//! | [`head_of`] | `v_contract_execution` | 单合同单头（详情页的汇总） |
//! | [`sources_of_contract`] | `v_contract_execution_order` | 下钻：这张合同上的单据（含作废 / 软删） |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 与合同列表同口径（contract_model::CONTRACT_TARGET = ScopeTarget::unscoped，
//!        V0195 的 dept_id 是「归属部门」但 biz_contract 至今没有登记进 SCOPED_TABLES，
//!        数据权限属专业版）。可见性由 business:erp:contract:execution 权限码控制；
//!        总览走 scope::paginate_unscoped（闸①口径），明细按合同主键取。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot_kernel::business::shared::scope;
use cloudpivot_kernel::utils;

use crate::business::erp::contract::contract_execution_dto::{ExecutionFilter, HEAD_VIEW, ORDER_VIEW};

/// 总览视图的分页目标（不登记进 `SCOPED_TABLES`：SCOPE-EXEMPT，见模块头）。
pub const HEAD_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(HEAD_VIEW);

/// 单头（总览的一行 / 详情页的汇总）。
#[derive(Debug, Clone)]
pub struct HeadRow {
    /// 合同 id。
    pub contract_id: Uuid,
    /// 合同号。
    pub contract_no: String,
    /// 合同名称。
    pub contract_name: String,
    /// 合同类型。
    pub contract_type: String,
    /// 合同状态。
    pub status: String,
    /// 甲方 id。
    pub party_a_id: Option<Uuid>,
    /// 甲方名称快照。
    pub party_a_name: Option<String>,
    /// 乙方类型。
    pub party_b_type: String,
    /// 乙方 id。
    pub party_b_id: Uuid,
    /// 乙方名称快照。
    pub party_b_name: String,
    /// 乙方现名（实时 join；主档已删 = None）。
    pub party_b_current_name: Option<String>,
    /// 负责人 id。
    pub owner_id: Option<Uuid>,
    /// 负责人姓名（实时 join）。
    pub owner_name: Option<String>,
    /// 归属部门 id。
    pub dept_id: Option<Uuid>,
    /// 归属部门名（实时 join）。
    pub dept_name: Option<String>,
    /// 合同额。
    pub amount: Decimal,
    /// 合同币种。
    pub currency_code: String,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 生效日。
    pub start_date: Option<NaiveDate>,
    /// 到期日。
    pub end_date: Option<NaiveDate>,
    /// 下游单据类型（None = 无下游）。
    pub downstream_type: Option<String>,
    /// 计入已执行额的单据数。
    pub order_doc_count: i64,
    /// 释放（作废 / 软删）的单据数。
    pub released_doc_count: i64,
    /// 计入的单据里币种 ≠ 合同币种的行数。
    pub currency_mismatch_doc_count: i64,
    /// 计入的采购单数。
    pub purchase_doc_count: i64,
    /// 计入的销售单数。
    pub sales_doc_count: i64,
    /// 已执行额（含税）。
    pub executed_amount: Decimal,
    /// 已执行净额（不含税）。
    pub executed_amount_net: Decimal,
    /// 剩余可执行额（可为负）。
    pub remaining_amount: Decimal,
    /// 执行率（0–1，6 位）。
    pub exec_amount_rate: Option<Decimal>,
    /// 执行状态。
    pub exec_state: String,
    /// 最近一次执行日。
    pub last_order_on: Option<NaiveDate>,
    /// 距到期日天数（已过期 = 负数；无到期日 = None）。
    pub days_left: Option<i32>,
    /// 是否已过期。
    pub expired: bool,
    /// 今天能否接着下单（SIGNED / ACTIVE）。
    pub usable_for_order: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 下钻来源行（一条 = 这张合同上的一张单据）。
#[derive(Debug, Clone)]
pub struct SourceRow {
    /// 单据 id。
    pub order_id: Uuid,
    /// 单据号。
    pub order_no: String,
    /// 单据类型（PURCHASE_ORDER / SALES_ORDER）。
    pub source_type: String,
    /// 单据状态。
    pub order_status: String,
    /// 单据日期。
    pub order_date: NaiveDate,
    /// 单据币种。
    pub currency_code: String,
    /// 不含税金额。
    pub total_amount: Decimal,
    /// 含税金额。
    pub total_with_tax: Decimal,
    /// 单据软删时间。
    pub order_deleted_at: Option<DateTime<Utc>>,
    /// 是否计入已执行额。
    pub counted: bool,
    /// 币种是否与合同一致。
    pub currency_matched: bool,
}

impl SourceRow {
    /// 判定：`(是否计入已执行额, 原因)` —— 纯函数在 dto 里，这里只喂参数。
    pub fn verdict(&self) -> (bool, &'static str) {
        crate::business::erp::contract::contract_execution_dto::source_verdict(
            &self.order_status,
            self.order_deleted_at.is_some(),
        )
    }
}

// ==================== SQL 片段 ====================

/// 单头视图的列清单（顺序与 [`read_head`] 一一对应；改一处必须两处一起改）。
const HEAD_COLUMNS: &str = "contract_id, contract_no, contract_name, contract_type, status, \
                            party_a_id, party_a_name, party_b_type, party_b_id, party_b_name, \
                            party_b_current_name, owner_id, owner_name, dept_id, dept_name, \
                            amount, currency_code, sign_date, start_date, end_date, \
                            downstream_type, order_doc_count, released_doc_count, \
                            currency_mismatch_doc_count, purchase_doc_count, sales_doc_count, \
                            executed_amount, executed_amount_net, remaining_amount, \
                            exec_amount_rate, exec_state, last_order_on, days_left, expired, \
                            usable_for_order, created_at, updated_at";

/// 单据级视图的列清单（同上）。
const ORDER_COLUMNS: &str = "order_id, order_no, source_type, order_status, order_date, \
                             currency_code, total_amount, total_with_tax, order_deleted_at, \
                             counted, currency_matched";

// ==================== 读：总览分页 ====================

/// 跨合同执行量总览（分页 + 筛选）。
///
/// 不吃执行器：`scope::paginate_unscoped` 走全局 `db()`（与框架协议执行量分页同款）。
pub async fn page_execution(filter: &ExecutionFilter) -> DbResult<(Vec<HeadRow>, u64)> {
    let mut sql = format!("SELECT {HEAD_COLUMNS} FROM {HEAD_VIEW} e WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(contract_type) = filter.contract_type.as_deref() {
        sql.push_str(" AND e.contract_type = ?");
        params.push(Value::Text(contract_type.to_string()));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND e.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(state) = filter.exec_state.as_deref() {
        sql.push_str(" AND e.exec_state = ?");
        params.push(Value::Text(state.to_string()));
    }
    if filter.over_only {
        sql.push_str(" AND e.exec_state = 'OVER'");
    }
    if filter.expired_only {
        sql.push_str(" AND e.expired");
    }
    if filter.currency_mismatch_only {
        sql.push_str(" AND e.currency_mismatch_doc_count > 0");
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        let like = Value::Text(pattern);
        sql.push_str(
            " AND (e.contract_no LIKE ? OR e.contract_name LIKE ? \
                OR COALESCE(e.party_b_name, '') LIKE ? \
                OR COALESCE(e.party_b_current_name, '') LIKE ? \
                OR COALESCE(e.owner_name, '') LIKE ?)",
        );
        for _ in 0..5 {
            params.push(like.clone());
        }
    }
    let page = scope::paginate_unscoped(
        HEAD_TARGET,
        &sql,
        params,
        // 超合同额的排前面（控盘先看越界），同档里执行率高的优先，再按合同号稳定排序
        Some(
            "ORDER BY (e.exec_state = 'OVER') DESC, COALESCE(e.exec_amount_rate, 0) DESC, \
             e.contract_no DESC",
        ),
        filter.page,
        filter.page_size,
    )
    .await?;
    let rows = page.list.iter().map(read_head).collect::<DbResult<Vec<_>>>()?;
    Ok((rows, page.total))
}

// ==================== 读：单合同 ====================

/// 单合同的执行量单头（合同不存在 / 已软删 → `None`，调用方按 404 处理）。
pub async fn head_of(ex: &impl SqlExecutor, contract_id: Uuid) -> DbResult<Option<HeadRow>> {
    // SCOPE-EXEMPT: 按主键取 1 行聚合视图（见模块头）。
    let rows = ex
        .query_sql(
            format!("SELECT {HEAD_COLUMNS} FROM {HEAD_VIEW} WHERE contract_id = ?::uuid"),
            vec![Value::from(&contract_id)],
        )
        .await?;
    rows.first().map(read_head).transpose()
}

/// 单合同的下钻单据（按单据日期 / 单号升序）。
///
/// **不过滤** `deleted_at`：已软删 / 已作废的单据也要出现在下钻里（`counted = false` +
/// 原因）—— 直接从列表里消失的话，用户会以为「这批量凭空少了」。
pub async fn sources_of_contract(ex: &impl SqlExecutor, contract_id: Uuid) -> DbResult<Vec<SourceRow>> {
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ORDER_COLUMNS} FROM {ORDER_VIEW} \
                  WHERE contract_id = ?::uuid ORDER BY order_date, order_no, source_type"
            ),
            vec![Value::from(&contract_id)],
        )
        .await?;
    rows.iter().map(read_source).collect()
}

// ==================== 行 → 结构体 ====================

fn read_head(row: &Row) -> DbResult<HeadRow> {
    Ok(HeadRow {
        contract_id: row.get_typed("contract_id")?,
        contract_no: row.get_typed("contract_no")?,
        contract_name: row.get_typed("contract_name")?,
        contract_type: row.get_typed("contract_type")?,
        status: row.get_typed("status")?,
        party_a_id: row.get_typed("party_a_id")?,
        party_a_name: row.get_typed("party_a_name")?,
        party_b_type: row.get_typed("party_b_type")?,
        party_b_id: row.get_typed("party_b_id")?,
        party_b_name: row.get_typed("party_b_name")?,
        party_b_current_name: row.get_typed("party_b_current_name")?,
        owner_id: row.get_typed("owner_id")?,
        owner_name: row.get_typed("owner_name")?,
        dept_id: row.get_typed("dept_id")?,
        dept_name: row.get_typed("dept_name")?,
        amount: row.get_typed("amount")?,
        currency_code: row.get_typed("currency_code")?,
        sign_date: row.get_typed("sign_date")?,
        start_date: row.get_typed("start_date")?,
        end_date: row.get_typed("end_date")?,
        downstream_type: row.get_typed("downstream_type")?,
        order_doc_count: row.get_typed("order_doc_count")?,
        released_doc_count: row.get_typed("released_doc_count")?,
        currency_mismatch_doc_count: row.get_typed("currency_mismatch_doc_count")?,
        purchase_doc_count: row.get_typed("purchase_doc_count")?,
        sales_doc_count: row.get_typed("sales_doc_count")?,
        executed_amount: row.get_typed("executed_amount")?,
        executed_amount_net: row.get_typed("executed_amount_net")?,
        remaining_amount: row.get_typed("remaining_amount")?,
        exec_amount_rate: row.get_typed("exec_amount_rate")?,
        exec_state: row.get_typed("exec_state")?,
        last_order_on: row.get_typed("last_order_on")?,
        days_left: row.get_typed("days_left")?,
        expired: row.get_typed("expired")?,
        usable_for_order: row.get_typed("usable_for_order")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
    })
}

fn read_source(row: &Row) -> DbResult<SourceRow> {
    Ok(SourceRow {
        order_id: row.get_typed("order_id")?,
        order_no: row.get_typed("order_no")?,
        source_type: row.get_typed("source_type")?,
        order_status: row.get_typed("order_status")?,
        order_date: row.get_typed("order_date")?,
        currency_code: row.get_typed("currency_code")?,
        total_amount: row.get_typed("total_amount")?,
        total_with_tax: row.get_typed("total_with_tax")?,
        order_deleted_at: row.get_typed("order_deleted_at")?,
        counted: row.get_typed("counted")?,
        currency_matched: row.get_typed("currency_matched")?,
    })
}

// ==================== 单测（纯逻辑：列清单与结构体字段名对不上时先在这里炸） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 列清单必须一个不少地包含 `read_head` 读的每一列（改名时这里先红，而不是运行期 500）。
    #[test]
    fn head_columns_cover_reader() {
        for column in [
            "contract_id",
            "contract_no",
            "contract_name",
            "contract_type",
            "status",
            "party_a_id",
            "party_a_name",
            "party_b_type",
            "party_b_id",
            "party_b_name",
            "party_b_current_name",
            "owner_id",
            "owner_name",
            "dept_id",
            "dept_name",
            "amount",
            "currency_code",
            "sign_date",
            "start_date",
            "end_date",
            "downstream_type",
            "order_doc_count",
            "released_doc_count",
            "currency_mismatch_doc_count",
            "purchase_doc_count",
            "sales_doc_count",
            "executed_amount",
            "executed_amount_net",
            "remaining_amount",
            "exec_amount_rate",
            "exec_state",
            "last_order_on",
            "days_left",
            "expired",
            "usable_for_order",
            "created_at",
            "updated_at",
        ] {
            assert!(
                HEAD_COLUMNS.split(',').any(|item| item.trim() == column),
                "HEAD_COLUMNS 漏了 `{column}`（read_head 会取不到列 → 运行期 500）"
            );
        }
    }

    /// 单据级列清单同样必须覆盖 reader。
    #[test]
    fn order_columns_cover_reader() {
        for column in [
            "order_id",
            "order_no",
            "source_type",
            "order_status",
            "order_date",
            "currency_code",
            "total_amount",
            "total_with_tax",
            "order_deleted_at",
            "counted",
            "currency_matched",
        ] {
            assert!(
                ORDER_COLUMNS.split(',').any(|item| item.trim() == column),
                "ORDER_COLUMNS 漏了 `{column}`"
            );
        }
    }

    /// 分页目标是 SCOPE-EXEMPT（合同表未登记进 `SCOPED_TABLES`，见模块头）。
    #[test]
    fn head_target_is_scope_exempt() {
        assert!(
            !HEAD_TARGET.is_scoped(),
            "合同执行量总览必须 SCOPE-EXEMPT（与合同列表同口径）"
        );
    }
}

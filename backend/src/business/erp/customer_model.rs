//! 客户档案数据层（销售链 · SO-02）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0260__sales_customer.up.sql`
//! 的 `biz_customer`（客户主档）。联系人子表 `biz_customer_contact` 本卡只读不写
//! （卡面只批主档 CRUD；联系人维护属后续卡，别在这里顺手加接口）。
//!
//! | 列 | 口径 |
//! |----|------|
//! | `code` | 业务唯一键，**部分唯一索引** `uk_biz_customer_code (code) WHERE deleted_at IS NULL` ⇒ 软删后可复用 |
//! | `credit_limit` | `NUMERIC(18,4)`，`CHECK >= 0`；`0` = 不控额度（与供应商 `credit_limit` 同语义） |
//! | `status` | `VARCHAR(20)` 域值 `ACTIVE`（正常，可开新单）/ `FROZEN`（冻结，禁开新单）—— **不是** 1/0 |
//! | `deleted_at` | 软删标记；**物理删除会被 `biz_customer_contact.customer_id` 的 `ON DELETE RESTRICT` 挡住** |
//!
//! # 为什么状态用字符串域值
//!
//! 供应商 / 单位 / 物料那批主数据是 `status SMALLINT (1 启用 / 0 停用)`，
//! 客户这边卡面与销售链设计要点都点名 `ACTIVE` / `FROZEN`（与 `LEGAL_EDGES` 的大写枚举同风格，
//! 状态机可读），且语义不同（冻结 ≠ 停用：冻结只是不接新单，历史单据照样可用）。
//! ⚠️ `contract_model.rs:31` 的文档注释里「`status = 1`」是批次 2 之前的占位推测，
//! 客户分支真正落地时必须写 `status = 'ACTIVE'`（int 比 varchar 恒判 0 行）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_customer 是全局主数据（V0260 无 dept_id / 归属人列），数据范围
//! （SELF / DEPT_*）表达不出来；可见性由 business:erp:customer:* 权限码控制。
//! 分页走 model::page_of → scope::paginate_unscoped（闸①口径），本文件不含手写 COUNT/LIMIT 分页。
//! ```

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::{find_by_id, page_of};
use crate::business::shared::scope;
use crate::utils;

/// 分页口径（默认 20 / 上限 200 / `page_size = 0` 回落）直接复用底座，不另定一套。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};

// ==================== 常量 ====================

/// `biz_customer` 列清单（与 V0260 + V0289 + V0313 逐列对应，顺序无关）。
pub const CUSTOMER_COLUMNS: &str = "id, code, name, tax_no, contact_name, contact_phone, address, \
     credit_days, credit_limit, credit_used, status, remark, created_at, updated_at, create_by, \
     update_by, deleted_at";

/// 状态域值：正常（可开新单）。
pub const STATUS_ACTIVE: &str = "ACTIVE";

/// 状态域值：冻结（禁开新单，历史单据不受影响）。
pub const STATUS_FROZEN: &str = "FROZEN";

/// 状态域值白名单（与 `ck_biz_customer_status` 逐字一致，单测钉住）。
pub const STATUSES: [&str; 2] = [STATUS_ACTIVE, STATUS_FROZEN];

/// `code` 最大字符数（`VARCHAR(40)`）。
pub const MAX_CODE_LEN: usize = 40;

/// `name` 最大字符数（`VARCHAR(200)`）。
pub const MAX_NAME_LEN: usize = 200;

/// 客户编码是否在域内（`ACTIVE` / `FROZEN`）。
pub fn is_valid_status(value: &str) -> bool {
    STATUSES.contains(&value)
}

/// 编码归一化：去首尾空白（**只 trim，不转大写**：历史编码大小写敏感，改口径会撞唯一索引）。
pub fn normalize_code(raw: &str) -> String {
    raw.trim().to_string()
}

/// 信用额度守卫：`>= 0`（DDL 的 `ck_biz_customer_credit_limit` 只是兜底，入口先拒）。
pub fn credit_limit_allowed(value: Decimal) -> bool {
    value >= Decimal::ZERO
}

/// 账期天数守卫：`>= 0`（DDL 的 `ck_biz_customer_credit_days` 只是兜底，入口先拒）。
///
/// `0` 合法且语义是「现结」（到期日 = 过账日），与 `biz_supplier.credit_days` 同口径；
/// 不设上限（供应商那边也没有）—— 真要按天封顶是业务配置，不是主数据约束。
pub fn credit_days_allowed(value: i32) -> bool {
    value >= 0
}

// ==================== 行结构 ====================

/// `biz_customer` 一行。
#[derive(Debug, Clone)]
pub struct CustomerRow {
    /// 主键。
    pub id: Uuid,
    /// 客户编码（未删除内唯一）。
    pub code: String,
    /// 客户全称。
    pub name: String,
    /// 纳税人识别号。
    pub tax_no: Option<String>,
    /// 默认联系人姓名。
    pub contact_name: Option<String>,
    /// 默认联系人电话。
    pub contact_phone: Option<String>,
    /// 地址。
    pub address: Option<String>,
    /// 账期天数（`>= 0`，`0` = 现结；销售出库挂应收据此算到期日）。
    pub credit_days: i32,
    /// 信用额度（`NUMERIC(18,4)`，`0` = 不控额度）。
    pub credit_limit: Decimal,
    /// 已用额度（SO-38 / V0313：有效订单含税 − 已核销应收 − 已过账退货，下限 0）。
    pub credit_used: Decimal,
    /// 状态：`ACTIVE` / `FROZEN`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 存活）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl CustomerRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            code: row.get_typed("code")?,
            name: row.get_typed("name")?,
            tax_no: row.get_typed("tax_no")?,
            contact_name: row.get_typed("contact_name")?,
            contact_phone: row.get_typed("contact_phone")?,
            address: row.get_typed("address")?,
            credit_days: row.get_typed("credit_days")?,
            credit_limit: row.get_typed("credit_limit")?,
            credit_used: row.get_typed("credit_used")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// 分页锚点（全局主数据，不参与数据范围）。
pub const CUSTOMER_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_customer");

/// 列表排序：编码升序（与供应商 / 单位 / 物料同口径）。
pub const CUSTOMER_ORDER: &str = "ORDER BY code ASC";

// ==================== 查询 ====================

/// 按主键取客户（软删行不返回）。
pub async fn customer_find_by_id(id: Uuid) -> DbResult<Option<CustomerRow>> {
    find_by_id(
        "biz_customer",
        CUSTOMER_COLUMNS,
        id,
        true,
        CustomerRow::from_row,
    )
    .await
}

/// 按业务编码取存活客户（编码唯一性校验与回读共用）。
pub async fn customer_find_by_code(code: &str) -> DbResult<Option<CustomerRow>> {
    let sql = format!(
        "SELECT {CUSTOMER_COLUMNS} FROM biz_customer WHERE code = ? AND deleted_at IS NULL LIMIT 1"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(code)]).await?;
    rows.first().map(CustomerRow::from_row).transpose()
}

/// 编码是否已被占用（`exclude` = 编辑时排除自己）。
///
/// 用 `COUNT(*)` 而不是「查一行再比」：部分唯一索引只约束存活行，
/// 这里也只看 `deleted_at IS NULL`，口径与 `uk_biz_customer_code` 一致。
pub async fn customer_code_taken(code: &str, exclude: Option<Uuid>) -> DbResult<bool> {
    let mut sql =
        String::from("SELECT COUNT(*) FROM biz_customer WHERE code = ? AND deleted_at IS NULL");
    let mut params = vec![Value::from(code)];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    let count: i64 = db().scalar_with_params(&sql, params).await?;
    Ok(count > 0)
}

/// 客户分页：`code`（模糊）/ `name`（模糊）/ `status`（精确）三个可选条件。
///
/// 模糊匹配口径走 [`utils::like_pattern`]（与其余主数据一致：`%kw%`，空 / 空白 = 不过滤）。
pub async fn customer_page(
    code: Option<&str>,
    name: Option<&str>,
    status: Option<&str>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<CustomerRow>, i64)> {
    let mut sql = format!("SELECT {CUSTOMER_COLUMNS} FROM biz_customer WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(code) {
        sql.push_str(" AND code LIKE ?");
        params.push(Value::from(pattern));
    }
    if let Some(pattern) = utils::like_pattern(name) {
        sql.push_str(" AND name LIKE ?");
        params.push(Value::from(pattern));
    }
    if let Some(status) = status.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    page_of(
        CUSTOMER_TARGET,
        sql,
        params,
        CUSTOMER_ORDER,
        page,
        page_size,
        CustomerRow::from_row,
    )
    .await
}

// ==================== 写入 ====================

/// 写入用的整行值（新建 / 编辑共用；handler 负责把「部分更新」合并成整行）。
#[derive(Debug, Clone)]
pub struct CustomerWrite {
    /// 客户编码（已 trim）。
    pub code: String,
    /// 客户全称（已 trim）。
    pub name: String,
    /// 纳税人识别号（空串 → NULL）。
    pub tax_no: Option<String>,
    /// 默认联系人姓名（空串 → NULL）。
    pub contact_name: Option<String>,
    /// 默认联系人电话（空串 → NULL）。
    pub contact_phone: Option<String>,
    /// 地址（空串 → NULL）。
    pub address: Option<String>,
    /// 账期天数（`>= 0`，`0` = 现结）。
    pub credit_days: i32,
    /// 信用额度（`>= 0`）。
    pub credit_limit: Decimal,
    /// 状态（`ACTIVE` / `FROZEN`）。
    pub status: String,
    /// 备注（空串 → NULL）。
    pub remark: Option<String>,
}

/// 新建客户，返回新行主键。
///
/// 编码唯一性在 handler 里先查一次（给人话文案），这里是**最后一道**：
/// 并发下仍可能撞 `uk_biz_customer_code`，由 handler 的 `write_error` 翻成 `409 + 66001`。
pub async fn customer_insert(
    id: Uuid,
    write: &CustomerWrite,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    db().execute_with_params(
        "INSERT INTO biz_customer \
             (id, code, name, tax_no, contact_name, contact_phone, address, \
              credit_days, credit_limit, status, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?, ?, ?, ?, ?, ?::text::numeric, ?, ?, now(), now(), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(write.code.clone()),
            Value::from(write.name.clone()),
            Value::from(write.tax_no.clone()),
            Value::from(write.contact_name.clone()),
            Value::from(write.contact_phone.clone()),
            Value::from(write.address.clone()),
            Value::from(write.credit_days),
            Value::Text(write.credit_limit.to_string()),
            Value::from(write.status.clone()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 整行覆盖编辑（软删行不命中 → 返回 0）。
pub async fn customer_update(
    id: Uuid,
    write: &CustomerWrite,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_customer SET code = ?, name = ?, tax_no = ?, contact_name = ?, \
             contact_phone = ?, address = ?, credit_days = ?, credit_limit = ?::text::numeric, \
             status = ?, remark = ?, \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
             WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(write.code.clone()),
            Value::from(write.name.clone()),
            Value::from(write.tax_no.clone()),
            Value::from(write.contact_name.clone()),
            Value::from(write.contact_phone.clone()),
            Value::from(write.address.clone()),
            Value::from(write.credit_days),
            Value::Text(write.credit_limit.to_string()),
            Value::from(write.status.clone()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删（已删 / 不存在 → 返回 0，由 handler 翻 404）。
///
/// **不做物理删除**：`biz_customer_contact` 的外键是 `ON DELETE RESTRICT`，
/// 物理删会被联系人行挡住；软删同时让 `uk_biz_customer_code` 释放该编码（部分索引）。
pub async fn customer_soft_delete(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_customer SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

/// 可空 UUID 参数：SQL 侧写 `NULLIF(?::text,'')::uuid`（`Value::Null` 绑 `::uuid` 驱动编不出来）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    /// 状态域值与 `ck_biz_customer_status` 同域：只认 ACTIVE / FROZEN（**不是** 1/0）。
    #[test]
    fn status_domain_matches_check_constraint() {
        assert!(is_valid_status("ACTIVE"));
        assert!(is_valid_status("FROZEN"));
        for bad in ["active", "Frozen", "1", "0", "ENABLED", ""] {
            assert!(!is_valid_status(bad), "`{bad}` 不应通过状态域校验");
        }
        assert_eq!(STATUSES, ["ACTIVE", "FROZEN"]);
    }

    /// 编码归一化只 trim：不做大小写 / 分隔符变换（改了会撞唯一索引里的历史行）。
    #[test]
    fn normalize_code_only_trims() {
        assert_eq!(normalize_code("  CUS-0001  "), "CUS-0001");
        assert_eq!(normalize_code("cus-0001"), "cus-0001");
        assert_eq!(normalize_code("CUS 0001"), "CUS 0001");
        assert_eq!(normalize_code("   "), "");
    }

    /// 信用额度边界：`0` 合法（= 不控额度），负数不合法；四位小数不丢精度。
    #[test]
    fn credit_limit_guard_boundary() {
        assert!(credit_limit_allowed(Decimal::ZERO));
        assert!(credit_limit_allowed(Decimal::from(1)));
        assert!(credit_limit_allowed(
            Decimal::from_str("12345678901234.5678").expect("定点可解析")
        ));
        assert!(!credit_limit_allowed(
            Decimal::from_str("-0.0001").expect("定点可解析")
        ));
        assert!(!credit_limit_allowed(Decimal::from(-1)));
    }

    /// 账期天数边界：`0`（现结）合法，负数不合法 —— 与 `ck_biz_customer_credit_days` 同域。
    #[test]
    fn credit_days_guard_boundary() {
        assert!(credit_days_allowed(0));
        assert!(credit_days_allowed(1));
        assert!(credit_days_allowed(3650));
        assert!(!credit_days_allowed(-1));
        assert!(!credit_days_allowed(i32::MIN));
    }

    /// 列清单必须覆盖 V0260 的全部列（漏一列 = `from_row` 运行时炸，这里提前钉住）。
    #[test]
    fn columns_cover_every_v0260_field() {
        for column in [
            "id",
            "code",
            "name",
            "tax_no",
            "contact_name",
            "contact_phone",
            "address",
            "credit_days",
            "credit_limit",
            "status",
            "remark",
            "created_at",
            "updated_at",
            "create_by",
            "update_by",
            "deleted_at",
        ] {
            assert!(
                CUSTOMER_COLUMNS.contains(column),
                "CUSTOMER_COLUMNS 缺列 `{column}`"
            );
        }
    }
}

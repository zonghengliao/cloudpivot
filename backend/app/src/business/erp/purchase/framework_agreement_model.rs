//! 框架协议数据层（M4-07 表结构）。
//!
//! 表结构唯一依据：`backend/app/migrations/business/V0450__framework_agreement.up.sql`。
//! 订单引用校验属 M4-08、执行量统计属 M4-09、单测属 M4-10、页面属 M4-11 ——
//! 本文件**只有表结构配套的读入口 / 取号 / 派生列重算**，不含校验编排与路由。
//!
//! | 表 | 行数级 | 关键不变量 |
//! |----|--------|------------|
//! | `biz_framework_agreement` | 每供应商每年几份 | 协议号存活唯一（`FWA-yyyyMMdd-000001`）；`start_date <= end_date`；`FLOATING ⇒ price_adjust_rule 非空`；`SUBMITTED ⇒ approval_instance_id NOT NULL`；上限 NULL = 不限、写了必须 > 0；状态域只有 DRAFT/SUBMITTED/APPROVED（**不造 EXPIRED**） |
//! | `biz_framework_agreement_item` | 一份协议几十~几百行 | `(agreement_id, material_id)` 唯一（**卡面的物料范围**）；`agreed_price >= 0`；`max_qty NULL = 不限 / > 0`；`executed_qty` 与 `max_qty` 之间**刻意不加** CHECK（超限拒绝属 M4-08） |
//!
//! # 三条口径（写在这里，免得下游各写一套）
//!
//! ① **有效期是时间的函数，不是状态列**：判「此刻是否生效」一律走
//!    [`is_effective_on`] —— `status = 'APPROVED' AND start_date <= 当日 <= end_date`。
//!    库里没有 `EXPIRED` / `TERMINATED`（无入口的死状态，见 V0450 头注释 1 与 6）。
//! ② **价格上限**：协议价 [`AgreementItemRow::agreed_price`]，下单价允许上浮
//!    `price_tolerance`（0 = 必须 ≤ 协议价）。判据由 [`max_allowed_price`] 给出，
//!    M4-08 拿它和订单行价比。
//! ③ **数量上限**：[`AgreementItemRow::max_qty`] 为 `None` = 不限；超上限**不在库层拦**
//!    （与采购计划 V0446 头注释 4 同口径），由 M4-08 在下单时拒、M4-09 在报表里露头。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_framework_agreement / _item 与采购计划同口径 —— 框架协议是公司级
//!         采购文件（采购要看全公司、财务要复核），`dept_id` 是「经办部门」快照语义，
//!         不是数据范围列，所以不登记 shared::scope::SCOPED_TABLES。
//!         可见性由 M4-11 的权限码控制。
//! ```

use nik::prelude::chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

// ==================== 表名 / 取号 / 枚举值 ====================

/// 框架协议单头表。
pub const AGREEMENT_TABLE: &str = "biz_framework_agreement";
/// 框架协议物料范围表。
pub const ITEM_TABLE: &str = "biz_framework_agreement_item";
/// 取号键（`biz_seq.seq_type`）。
pub const SEQ_TYPE: &str = "FWA";
/// 协议号前缀。
pub const NO_PREFIX: &str = "FWA";
/// 流水号宽度。
pub const NO_SEQ_WIDTH: usize = 6;

/// 定价方式：固定价。
pub const PRICE_MODE_FIXED: &str = "FIXED";
/// 定价方式：随行就市（必须有调价规则）。
pub const PRICE_MODE_FLOATING: &str = "FLOATING";
/// 定价方式闭集（与库层 `ck_biz_fwa_price_mode` 同一份）。
pub const PRICE_MODES: &[&str] = &[PRICE_MODE_FIXED, PRICE_MODE_FLOATING];

/// 状态中文名（报文 / 审批详情 / 列表共用一份）。未知名原样返回，绝不 panic。
pub fn status_label(status: &str) -> &str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "审批中",
        STATUS_APPROVED => "生效（已批准）",
        other => other,
    }
}

/// 状态：拟稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：审批中（必有审批实例）。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 状态：生效（是否在效期另看起止日）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 状态闭集（与库层 `ck_biz_fwa_status` 同一份；**只有三个**）。
/// `EXPIRED` / `TERMINATED` 刻意不在里面：到期是时间的函数，终止没有入口（见模块头 ①）。
pub const STATUSES: &[&str] = &[STATUS_DRAFT, STATUS_SUBMITTED, STATUS_APPROVED];

// ==================== 行结构 ====================

/// 协议单头行（列表 / 详情用；`item_count` 是派生列）。
#[derive(Debug, Clone)]
pub struct AgreementRow {
    /// 主键。
    pub id: Uuid,
    /// 协议号（`FWA-yyyyMMdd-000001`）。
    pub agreement_no: String,
    /// 协议名称。
    pub agreement_name: String,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 供应商名**快照**（签约主体历史事实，约定 13）。
    pub supplier_name: Option<String>,
    /// 供应商**现名**（实时 join；快照为空时也回退到它）。
    pub supplier_display_name: String,
    /// 经办部门。
    pub dept_id: Option<Uuid>,
    /// 经办部门名快照。
    pub dept_name: Option<String>,
    /// 经办人。
    pub owner_id: Option<Uuid>,
    /// 经办人姓名快照。
    pub owner_name: Option<String>,
    /// 有效期起（含）。
    pub start_date: NaiveDate,
    /// 有效期止（含）。
    pub end_date: NaiveDate,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 定价方式：FIXED / FLOATING。
    pub price_mode: String,
    /// 协议价是否含税。
    pub price_includes_tax: bool,
    /// 协议价币种。
    pub price_currency: String,
    /// 允许高于协议价的比例上限（0 = 必须 ≤ 协议价）。
    pub price_tolerance: Decimal,
    /// 调价 / 浮动规则原文。
    pub price_adjust_rule: Option<String>,
    /// 协议总量上限（None = 不限）。
    pub total_max_qty: Option<Decimal>,
    /// 协议总额上限（None = 不限）。
    pub total_max_amount: Option<Decimal>,
    /// 已执行量合计（派生）。
    pub total_executed_qty: Decimal,
    /// 已执行金额合计（派生）。
    pub total_executed_amount: Decimal,
    /// 状态。
    pub status: String,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 物料范围行数（派生）。
    pub item_count: i64,
}

impl AgreementRow {
    /// 该协议在 `on` 这天是否生效（模块头 ① 的唯一判据）。
    pub fn is_effective_on(&self, on: NaiveDate) -> bool {
        is_effective_on(&self.status, self.start_date, self.end_date, on)
    }
}

/// 物料范围行（一行 = 一个物料：协议价 + 税率 + 数量上限）。
#[derive(Debug, Clone)]
pub struct AgreementItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属协议。
    pub agreement_id: Uuid,
    /// 行号（1 起，单内唯一）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码（join）。
    pub material_code: String,
    /// 物料名称（join）。
    pub material_name: String,
    /// 计量单位。
    pub unit_id: Uuid,
    /// 单位名称（join）。
    pub unit_name: String,
    /// 协议单价（口径由单头 `price_includes_tax` 决定）。
    pub agreed_price: Decimal,
    /// 进项税率。
    pub tax_rate: Decimal,
    /// 该物料协议期内累计上限（None = 不限）。
    pub max_qty: Option<Decimal>,
    /// 已执行量（回写快照）。
    pub executed_qty: Decimal,
    /// 已执行金额（回写快照）。
    pub executed_amount: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

impl AgreementItemRow {
    /// 剩余可下单量：`max_qty - executed_qty`；`max_qty` 为 `None`（不限）时也是 `None`。
    pub fn remaining_qty(&self) -> Option<Decimal> {
        self.max_qty.map(|max| max - self.executed_qty)
    }
}

// ==================== 口径（纯函数） ====================

/// 单号（`FWA-{周期键}-{6 位流水}`）。
pub fn format_agreement_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 周期键（北京时间 `yyyyMMdd`）—— 与采购申请 / 采购计划同一口径。
pub fn period_key_of(now: DateTime<Utc>) -> String {
    let cst = now + Duration::hours(8);
    cst.format("%Y%m%d").to_string()
}

/// 有效期判据（模块头 ①）：`APPROVED` 且 `start_date <= on <= end_date`。
///
/// * 起止日都是**含**边界：`end_date` 当天仍可下单（合同写「至 12 月 31 日」就是到那天为止）；
/// * 只看状态与日期，**不看** `sign_date`（签订日与生效日可以不同）；
/// * `DRAFT` / `SUBMITTED` 一律不生效 —— 草稿也能下单就等于没有审批。
pub fn is_effective_on(status: &str, start_date: NaiveDate, end_date: NaiveDate, on: NaiveDate) -> bool {
    status == STATUS_APPROVED && start_date <= on && on <= end_date
}

/// 下单价允许的上限（M4-08 判据）：`agreed_price × (1 + price_tolerance)`，6 位小数。
///
/// * `price_tolerance = 0` ⇒ 上限就是协议价本身（`<=` 语义，等于允许）；
/// * 库层已保证 `0 <= price_tolerance <= 1`，这里不再重复校验；
/// * 6 位小数与 `agreed_price NUMERIC(18,6)` 同精度，避免「0.1 + 0.2」式的尾差。
pub fn max_allowed_price(agreed_price: Decimal, price_tolerance: Decimal) -> Decimal {
    (agreed_price * (Decimal::ONE + price_tolerance)).round_dp(6)
}

/// 今天（北京时间自然日）—— 判有效期用。
pub fn today_cst(now: DateTime<Utc>) -> NaiveDate {
    (now + Duration::hours(8)).date_naive()
}

/// 年份（北京时间）—— 列表筛选默认值用。
pub fn year_cst(now: DateTime<Utc>) -> i32 {
    (now + Duration::hours(8)).year()
}

// ==================== 取号 ====================

/// `biz_seq(seq_type='FWA')` 缺失时幂等补一行（迁移已种，这里是自愈兜底）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'FWA', '框架协议编号', 'FWA', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '框架协议编号：FWA-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'FWA' AND deleted_at IS NULL)"
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

/// 推进 `biz_seq(seq_type='FWA')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
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
              WHERE seq_type = 'FWA' AND deleted_at IS NULL \
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
    rows.first().map(|row| row.get_typed("current_no")).transpose()
}

/// 取一个协议号（周期键按北京时间）。返回形如 `FWA-20261118-000001`。
pub async fn next_agreement_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_agreement_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='FWA') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 读 ====================

/// 单头 select（带物料范围行数 + 供应商现名）。
fn agreement_select_sql() -> String {
    format!(
        "SELECT a.id, a.agreement_no, a.agreement_name, a.supplier_id, a.supplier_name, \
                COALESCE(a.supplier_name, s.supplier_name, '') AS supplier_display_name, \
                a.dept_id, a.dept_name, a.owner_id, a.owner_name, \
                a.start_date, a.end_date, a.sign_date, \
                a.price_mode, a.price_includes_tax, a.price_currency, a.price_tolerance, a.price_adjust_rule, \
                a.total_max_qty, a.total_max_amount, a.total_executed_qty, a.total_executed_amount, \
                a.status, a.approval_instance_id, a.remark, a.created_at, a.create_by, \
                (SELECT count(*) FROM {ITEM_TABLE} it WHERE it.agreement_id = a.id) AS item_count \
           FROM {AGREEMENT_TABLE} a \
           LEFT JOIN biz_supplier s ON s.id = a.supplier_id AND s.deleted_at IS NULL \
          WHERE a.deleted_at IS NULL"
    )
}

fn read_agreement_row(row: &Row) -> DbResult<AgreementRow> {
    Ok(AgreementRow {
        id: row.get_typed("id")?,
        agreement_no: row.get_typed("agreement_no")?,
        agreement_name: row.get_typed("agreement_name")?,
        supplier_id: row.get_typed("supplier_id")?,
        supplier_name: row.get_typed("supplier_name")?,
        supplier_display_name: row.get_typed("supplier_display_name")?,
        dept_id: row.get_typed("dept_id")?,
        dept_name: row.get_typed("dept_name")?,
        owner_id: row.get_typed("owner_id")?,
        owner_name: row.get_typed("owner_name")?,
        start_date: row.get_typed("start_date")?,
        end_date: row.get_typed("end_date")?,
        sign_date: row.get_typed("sign_date")?,
        price_mode: row.get_typed("price_mode")?,
        price_includes_tax: row.get_typed("price_includes_tax")?,
        price_currency: row.get_typed("price_currency")?,
        price_tolerance: row.get_typed("price_tolerance")?,
        price_adjust_rule: row.get_typed("price_adjust_rule")?,
        total_max_qty: row.get_typed("total_max_qty")?,
        total_max_amount: row.get_typed("total_max_amount")?,
        total_executed_qty: row.get_typed("total_executed_qty")?,
        total_executed_amount: row.get_typed("total_executed_amount")?,
        status: row.get_typed("status")?,
        approval_instance_id: row.get_typed("approval_instance_id")?,
        remark: row.get_typed("remark")?,
        created_at: row.get_typed("created_at")?,
        create_by: row.get_typed("create_by")?,
        item_count: row.get_typed("item_count")?,
    })
}

/// 按主键取一份存活协议（不存在 / 已软删 → `None`）。
pub async fn find_agreement(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AgreementRow>> {
    // SCOPE-EXEMPT: 按主键取单行，不涉及数据范围（见模块头「数据权限」）。
    let rows = ex
        .query_sql(
            format!("{} AND a.id = ?::uuid", agreement_select_sql()),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(read_agreement_row).transpose()
}

/// 按协议号取一份存活协议（`uk_biz_framework_agreement_no` 的唯一键探针 —— 建单前判重比撞库报 500 友好）。
pub async fn find_agreement_by_no(ex: &impl SqlExecutor, agreement_no: &str) -> DbResult<Option<AgreementRow>> {
    // SCOPE-EXEMPT: 唯一键探针，不涉及数据范围。
    let rows = ex
        .query_sql(
            format!("{} AND a.agreement_no = ?", agreement_select_sql()),
            vec![Value::Text(agreement_no.to_string())],
        )
        .await?;
    rows.first().map(read_agreement_row).transpose()
}

/// 一份协议的全部物料范围行（按行号升序；物料 / 单位名实时 join）。
pub async fn items_of_agreement(ex: &impl SqlExecutor, agreement_id: Uuid) -> DbResult<Vec<AgreementItemRow>> {
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头「数据权限」）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT it.id, it.agreement_id, it.line_no, it.material_id, \
                        COALESCE(m.material_code, '') AS material_code, \
                        COALESCE(m.material_name, '') AS material_name, \
                        it.unit_id, COALESCE(u.unit_name, '') AS unit_name, \
                        it.agreed_price, it.tax_rate, it.max_qty, it.executed_qty, it.executed_amount, it.remark \
                   FROM {ITEM_TABLE} it \
                   LEFT JOIN biz_material m ON m.id = it.material_id AND m.deleted_at IS NULL \
                   LEFT JOIN biz_unit u ON u.id = it.unit_id \
                  WHERE it.agreement_id = ?::uuid \
                  ORDER BY it.line_no"
            ),
            vec![Value::from(&agreement_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(AgreementItemRow {
                id: row.get_typed("id")?,
                agreement_id: row.get_typed("agreement_id")?,
                line_no: row.get_typed("line_no")?,
                material_id: row.get_typed("material_id")?,
                material_code: row.get_typed("material_code")?,
                material_name: row.get_typed("material_name")?,
                unit_id: row.get_typed("unit_id")?,
                unit_name: row.get_typed("unit_name")?,
                agreed_price: row.get_typed("agreed_price")?,
                tax_rate: row.get_typed("tax_rate")?,
                max_qty: row.get_typed("max_qty")?,
                executed_qty: row.get_typed("executed_qty")?,
                executed_amount: row.get_typed("executed_amount")?,
                remark: row.get_typed("remark")?,
            })
        })
        .collect()
}

/// 按物料取协议行（M4-08 下单校验：给定协议 + 物料，拿协议价 / 上限 / 已执行量）。
pub async fn find_item_by_material(
    ex: &impl SqlExecutor,
    agreement_id: Uuid,
    material_id: Uuid,
) -> DbResult<Option<AgreementItemRow>> {
    // SCOPE-EXEMPT: 唯一键（agreement_id, material_id）探针。
    let rows = ex
        .query_sql(
            format!(
                "SELECT it.id, it.agreement_id, it.line_no, it.material_id, \
                        COALESCE(m.material_code, '') AS material_code, \
                        COALESCE(m.material_name, '') AS material_name, \
                        it.unit_id, COALESCE(u.unit_name, '') AS unit_name, \
                        it.agreed_price, it.tax_rate, it.max_qty, it.executed_qty, it.executed_amount, it.remark \
                   FROM {ITEM_TABLE} it \
                   LEFT JOIN biz_material m ON m.id = it.material_id AND m.deleted_at IS NULL \
                   LEFT JOIN biz_unit u ON u.id = it.unit_id \
                  WHERE it.agreement_id = ?::uuid AND it.material_id = ?::uuid"
            ),
            vec![Value::from(&agreement_id), Value::from(&material_id)],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok(AgreementItemRow {
                id: row.get_typed("id")?,
                agreement_id: row.get_typed("agreement_id")?,
                line_no: row.get_typed("line_no")?,
                material_id: row.get_typed("material_id")?,
                material_code: row.get_typed("material_code")?,
                material_name: row.get_typed("material_name")?,
                unit_id: row.get_typed("unit_id")?,
                unit_name: row.get_typed("unit_name")?,
                agreed_price: row.get_typed("agreed_price")?,
                tax_rate: row.get_typed("tax_rate")?,
                max_qty: row.get_typed("max_qty")?,
                executed_qty: row.get_typed("executed_qty")?,
                executed_amount: row.get_typed("executed_amount")?,
                remark: row.get_typed("remark")?,
            })
        })
        .transpose()
}

/// 锁住一份协议（M4-08 下单校验的第一句）。
///
/// 为什么必须锁：校验要「读已执行量 → 判上限 → 回写」，两个并发下单若都先读到同一份
/// `executed_qty`，就会各自被放行一次，协议量被超卖。`FOR UPDATE` 把协议单头锁住，
/// 同一协议的并发下单在这里排队。
/// 返回 `false` = 不存在 / 已软删（调用方按 404 处理，而不是静默放行）。
pub async fn lock_agreement(ex: &impl SqlExecutor, id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: 按主键加行锁，不涉及数据范围。
    let rows = ex
        .query_sql(
            format!("SELECT id FROM {AGREEMENT_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"),
            vec![Value::from(&id)],
        )
        .await?;
    Ok(!rows.is_empty())
}

// ==================== 写（仅表结构的配套：派生列 / 执行量回写） ====================

/// 把明细的两个已执行合计回写到单头（`Σ executed_qty` / `Σ executed_amount`）。
///
/// * 单头的两个 `total_executed_*` 是**派生列**，客户端永远不许直接写；
///   执行量回写（M4-08 下单）之后调用它重算。
/// * 一条 `UPDATE ... FROM 子查询` 完成，不做「先查后写」—— 避免两个请求各读旧值再各写一遍。
/// * 返回 `false` 表示协议不存在或已软删（调用方按 404，而不是静默成功）。
pub async fn recalc_totals(ex: &impl SqlExecutor, agreement_id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: 按主键重算派生的合计列。
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {AGREEMENT_TABLE} a \
                    SET total_executed_qty = s.qty, total_executed_amount = s.amount, updated_at = now() \
                   FROM (SELECT COALESCE(SUM(executed_qty), 0) AS qty, \
                                COALESCE(SUM(executed_amount), 0) AS amount \
                           FROM {ITEM_TABLE} WHERE agreement_id = ?::uuid) s \
                  WHERE a.id = ?::uuid AND a.deleted_at IS NULL"
            ),
            vec![Value::from(&agreement_id), Value::from(&agreement_id)],
        )
        .await?;
    Ok(affected > 0)
}

/// 回写已执行量：`executed_qty += qty`、`executed_amount += amount`（M4-08 下单）。
///
/// * 用 `+ 值` 而不是「读出来再写回去」—— 并发下后者会丢更新；
/// * **不加** `executed_qty <= max_qty` 的 CAS：超上限的合法性由 M4-08 在 `lock_agreement`
///   的行锁内判定（V0450 头注释 3：库层刻意不拦，拦在这里加急放量直接 500）；
/// * 返回 0 行 = 明细行被并发删掉 → 调用方 409。
pub async fn add_executed(
    ex: &impl SqlExecutor,
    item_id: Uuid,
    qty: Decimal,
    amount: Decimal,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 按明细主键累加两列。
    ex.exec_sql(
        format!(
            "UPDATE {ITEM_TABLE} \
                SET executed_qty = executed_qty + ?::text::numeric, \
                    executed_amount = executed_amount + ?::text::numeric, \
                    updated_at = ? \
              WHERE id = ?::uuid"
        ),
        vec![
            Value::Text(qty.to_string()),
            Value::Text(amount.to_string()),
            Value::from(now),
            Value::from(&item_id),
        ],
    )
    .await
}

/// `Option<Uuid>` → `?::text::uuid` 参数（None = 空串 → NULL，与采购计划同口径）。
pub fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 部门名（建单时落快照；查不到 → `None`，由 handler 翻 400）。
pub async fn dept_name_of(ex: &impl SqlExecutor, dept_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 按主键取一个部门名做快照，不涉及数据范围。
    let rows = ex
        .query_sql(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&dept_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("dept_name")).transpose()
}

/// 可选文本（`None` / 空串都折成 NULL）。
pub fn optional_text(value: Option<&str>) -> Value {
    Value::Text(value.unwrap_or_default().to_string())
}

/// 日期字面量 `YYYY-MM-DD`（配 `?::text::date` 用；见 V0450 与 commission_rule_model 的教训：
/// 裸 `?::date` 会让 PG 把参数推断成 date，`Value::Text` 编码不进去）。
pub fn date_text(value: NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}

// ==================== 写（M4-11b：建单 / 改单 / 删单 / 状态流转） ====================
//
// 两条与表结构卡（M4-07）一脉相承的口径：
//
// 1. **单头两个 `total_executed_*` 是派生列**：建单 / 改单之后一律走 [`recalc_totals`]，
//    请求体里的同名值**不读**（客户端永远不许传）。
// 2. **明细不整体重建**：M4-08 把 `framework_agreement_item_id` 写进采购明细
//    （V0451，弱引用、无外键），所以改单必须按 `material_id` **就地**对齐 ——
//    删行重建会让已下单的采购明细指向一个不存在的 id，执行量报表（现算 FROM 采购单）
//    会静默少掉那些量。见 [`replace_agreement`]。

/// 建单入参（单头）。
#[derive(Debug, Clone)]
pub struct NewAgreementHead {
    /// 协议名称。
    pub agreement_name: String,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 供应商名快照（签约主体历史事实）。
    pub supplier_name: Option<String>,
    /// 经办部门。
    pub dept_id: Option<Uuid>,
    /// 经办部门名快照。
    pub dept_name: Option<String>,
    /// 经办人。
    pub owner_id: Option<Uuid>,
    /// 经办人姓名快照。
    pub owner_name: Option<String>,
    /// 有效期起（含）。
    pub start_date: NaiveDate,
    /// 有效期止（含）。
    pub end_date: NaiveDate,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 定价方式。
    pub price_mode: String,
    /// 协议价是否含税。
    pub price_includes_tax: bool,
    /// 协议价币种。
    pub price_currency: String,
    /// 允许高于协议价的比例上限。
    pub price_tolerance: Decimal,
    /// 调价规则原文。
    pub price_adjust_rule: Option<String>,
    /// 协议总量上限（None = 不限）。
    pub total_max_qty: Option<Decimal>,
    /// 协议总额上限（None = 不限）。
    pub total_max_amount: Option<Decimal>,
    /// 备注。
    pub remark: Option<String>,
}

/// 建单入参（物料范围行；`unit_id` 由物料基础单位带出，客户端不许传）。
#[derive(Debug, Clone)]
pub struct NewAgreementItem {
    /// 物料。
    pub material_id: Uuid,
    /// 计量单位（物料基础单位）。
    pub unit_id: Uuid,
    /// 协议单价（≥ 0）。
    pub agreed_price: Decimal,
    /// 进项税率（0–1）。
    pub tax_rate: Decimal,
    /// 该物料数量上限（None = 不限）。
    pub max_qty: Option<Decimal>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 单头 18 列的**列名**（顺序 = [`head_columns`] 的返回值顺序）。
const HEAD_COLUMNS: &str = "agreement_name, supplier_id, supplier_name, dept_id, dept_name, \
     owner_id, owner_name, start_date, end_date, sign_date, price_mode, price_includes_tax, \
     price_currency, price_tolerance, price_adjust_rule, total_max_qty, total_max_amount, remark";

/// 单头 18 列的 `VALUES` 占位（与 [`HEAD_COLUMNS`] 逐位对应）。
///
/// 全部走 `?::text::…` 显式转型：裸 `?::date` 会让 PG 把参数推断成 `date`，
/// `Value::Text` 编码不进去（V0450 与 `commission_rule_model` 都踩过）。
const HEAD_VALUES: &str = "?, ?::uuid, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
     NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
     ?::text::date, ?::text::date, NULLIF(?::text,'')::date, \
     ?, ?::text::boolean, ?, ?::text::numeric, NULLIF(?::text,''), \
     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')";

/// 单头 18 列的 `SET` 片段（改单整单替换用；顺序与 [`HEAD_COLUMNS`] 一致）。
const HEAD_SET: &str = "agreement_name = ?, supplier_id = ?::uuid, supplier_name = NULLIF(?::text,''), \
     dept_id = NULLIF(?::text,'')::uuid, dept_name = NULLIF(?::text,''), \
     owner_id = NULLIF(?::text,'')::uuid, owner_name = NULLIF(?::text,''), \
     start_date = ?::text::date, end_date = ?::text::date, sign_date = NULLIF(?::text,'')::date, \
     price_mode = ?, price_includes_tax = ?::text::boolean, price_currency = ?, \
     price_tolerance = ?::text::numeric, price_adjust_rule = NULLIF(?::text,''), \
     total_max_qty = NULLIF(?::text,'')::numeric, total_max_amount = NULLIF(?::text,'')::numeric, \
     remark = NULLIF(?::text,'')";

/// 单头 18 个参数（顺序 = [`HEAD_COLUMNS`]）。
fn head_columns(head: &NewAgreementHead) -> Vec<Value> {
    vec![
        Value::Text(head.agreement_name.clone()),
        Value::from(&head.supplier_id),
        optional_text(head.supplier_name.as_deref()),
        uuid_text(head.dept_id),
        optional_text(head.dept_name.as_deref()),
        uuid_text(head.owner_id),
        optional_text(head.owner_name.as_deref()),
        Value::Text(date_text(head.start_date)),
        Value::Text(date_text(head.end_date)),
        Value::Text(head.sign_date.map(date_text).unwrap_or_default()),
        Value::Text(head.price_mode.clone()),
        Value::Text(head.price_includes_tax.to_string()),
        Value::Text(head.price_currency.clone()),
        Value::Text(head.price_tolerance.to_string()),
        optional_text(head.price_adjust_rule.as_deref()),
        Value::Text(head.total_max_qty.map(|value| value.to_string()).unwrap_or_default()),
        Value::Text(head.total_max_amount.map(|value| value.to_string()).unwrap_or_default()),
        optional_text(head.remark.as_deref()),
    ]
}

/// 建单（单头 + 物料范围 + 派生合计，同一事务），返回协议 id。
pub async fn create_agreement(
    head: &NewAgreementHead,
    items: &[NewAgreementItem],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let tx = db().begin().await?;
    let result = async {
        let agreement_no = next_agreement_no(&tx, now).await?;
        let id = Uuid::now_v7();
        let mut params = vec![Value::from(&id), Value::Text(agreement_no)];
        params.extend(head_columns(head));
        params.push(Value::from(now));
        params.push(Value::from(now));
        params.push(uuid_text(operator));
        params.push(uuid_text(operator));
        tx.exec_sql(
            format!(
                "INSERT INTO {AGREEMENT_TABLE} \
                    (id, agreement_no, {HEAD_COLUMNS}, status, created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?, {HEAD_VALUES}, 'DRAFT', ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            ),
            params,
        )
        .await?;
        insert_items(&tx, id, items).await?;
        recalc_totals(&tx, id).await?;
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

/// 插入物料范围行（建单 / 改单补新材料共用；行号 1 起）。
async fn insert_items(ex: &impl SqlExecutor, agreement_id: Uuid, items: &[NewAgreementItem]) -> DbResult<()> {
    for (index, item) in items.iter().enumerate() {
        let line_no = i32::try_from(index + 1).unwrap_or(i32::MAX);
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (id, agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate, max_qty, remark) \
                 VALUES (?, ?::uuid, ?, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,''))"
            ),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&agreement_id),
                Value::I32(line_no),
                Value::from(&item.material_id),
                Value::from(&item.unit_id),
                Value::Text(item.agreed_price.to_string()),
                Value::Text(item.tax_rate.to_string()),
                Value::Text(item.max_qty.map(|value| value.to_string()).unwrap_or_default()),
                optional_text(item.remark.as_deref()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 锁单头并读状态（提交 / 改单事务的第一句；不带 JOIN，`FOR UPDATE` 才能用）。
pub async fn lock_status(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<(String, Option<Uuid>)>> {
    // SCOPE-EXEMPT: 按主键取单行状态并加行锁，不涉及数据范围（见模块头「数据权限」）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT status, approval_instance_id FROM {AGREEMENT_TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("status")?, row.get_typed("approval_instance_id")?))),
    }
}

/// 改单（单头整单替换 + 物料范围**按物料就地对齐**，同一事务由调用方持有）。
///
/// # 为什么明细不是「删光重建」（与采购计划的做法刻意不同）
///
/// `biz_purchase_order_item.framework_agreement_item_id` 是**弱引用**（V0451 刻意不加外键：
/// 协议是软删表，外键拦不住软删）。若改单把行删光重建：
///
/// * 行 id 变了 → 已下单的采购明细指向一个不存在的 id；
/// * M4-09 的执行量视图从**采购单链现算**（`JOIN ... ON i.framework_agreement_item_id = it.id`），
///   那些行会被 JOIN 掉 → 报表**静默少掉**这段用量；
/// * 而单头 / 明细的回写列 `executed_*` 还在 → 报表与回写列分叉（`booked_drift` 非 0），
///   谁也说不出到底用了多少。
///
/// 所以这里按 `material_id` 对齐：同一物料的既有行**原地 UPDATE**（保留 id 与已执行量），
/// 新物料 INSERT，消失的物料只在 `executed_qty = 0` 时才删（有执行量的一律 409，
/// 判据在 `framework_agreement_dto::check_missing_material`；这里的 `AND executed_qty = 0`
/// 是库层兜底，防止绕过 handler 直接调）。
///
/// CAS 口径：`WHERE status = 'DRAFT'`；返回 0 行说明单据已不是草稿（并发提交 / 已被批）
/// → 调用方 409。**只有草稿能改**：审批中的单改了会让审批人看的和落库的不是一张。
pub async fn replace_agreement(
    ex: &impl SqlExecutor,
    id: Uuid,
    head: &NewAgreementHead,
    items: &[NewAgreementItem],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let mut params = head_columns(head);
    params.push(Value::from(now));
    params.push(uuid_text(operator));
    params.push(Value::from(&id));
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {AGREEMENT_TABLE} SET {HEAD_SET}, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
            ),
            params,
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }

    // 既有行（加锁：并发改单 / 并发下单会在这里排队）
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头「数据权限」）。
    let rows = ex
        .query_sql(
            format!("SELECT id, material_id FROM {ITEM_TABLE} WHERE agreement_id = ?::uuid FOR UPDATE"),
            vec![Value::from(&id)],
        )
        .await?;
    let mut existing: std::collections::BTreeMap<Uuid, Uuid> = std::collections::BTreeMap::new();
    for row in rows.iter() {
        existing.insert(row.get_typed("material_id")?, row.get_typed("id")?);
    }

    // 行号先整体挪到高位：`uk_biz_fwai_line(agreement_id, line_no)` 是即时唯一索引，
    // 直接两行对调会撞键（第 1 行改成 2、第 2 行改成 1 的中间态）。
    // `line_no > 0` 的 CHECK 仍然满足（+1_000_000 不会变负），最终行号再逐行落定。
    if !existing.is_empty() {
        ex.exec_sql(
            format!("UPDATE {ITEM_TABLE} SET line_no = line_no + 1000000 WHERE agreement_id = ?::uuid"),
            vec![Value::from(&id)],
        )
        .await?;
    }

    let mut kept: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let line_no = i32::try_from(index + 1).unwrap_or(i32::MAX);
        match existing.get(&item.material_id) {
            Some(item_id) => {
                kept.insert(*item_id);
                ex.exec_sql(
                    format!(
                        "UPDATE {ITEM_TABLE} SET line_no = ?, agreed_price = ?::text::numeric, \
                                tax_rate = ?::text::numeric, max_qty = NULLIF(?::text,'')::numeric, \
                                remark = NULLIF(?::text,''), updated_at = ? \
                          WHERE id = ?::uuid"
                    ),
                    vec![
                        Value::I32(line_no),
                        Value::Text(item.agreed_price.to_string()),
                        Value::Text(item.tax_rate.to_string()),
                        Value::Text(item.max_qty.map(|value| value.to_string()).unwrap_or_default()),
                        optional_text(item.remark.as_deref()),
                        Value::from(now),
                        Value::from(item_id),
                    ],
                )
                .await?;
            }
            None => {
                // 新物料：单行插入（行号按新列表的位置）
                insert_one_item(ex, id, line_no, item).await?;
            }
        }
    }

    // 消失的物料行：只删没执行过量的（有量的在 handler 已经 409，这里兜底）
    for (material_id, item_id) in existing.iter() {
        if kept.contains(item_id) {
            continue;
        }
        let removed = ex
            .exec_sql(
                format!("DELETE FROM {ITEM_TABLE} WHERE id = ?::uuid AND executed_qty = 0"),
                vec![Value::from(item_id)],
            )
            .await?;
        if removed == 0 {
            return Err(DbError::InvalidValue(format!(
                "物料 {material_id} 已有执行量，不允许从协议物料范围里删掉（改单被库层守卫拦下）"
            )));
        }
    }

    recalc_totals(ex, id).await?;
    Ok(affected)
}

/// 插入一行物料（改单补新材料用；行号由调用方给定）。
async fn insert_one_item(
    ex: &impl SqlExecutor,
    agreement_id: Uuid,
    line_no: i32,
    item: &NewAgreementItem,
) -> DbResult<()> {
    ex.exec_sql(
        format!(
            "INSERT INTO {ITEM_TABLE} \
                (id, agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate, max_qty, remark) \
             VALUES (?, ?::uuid, ?, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,''))"
        ),
        vec![
            Value::from(Uuid::now_v7()),
            Value::from(&agreement_id),
            Value::I32(line_no),
            Value::from(&item.material_id),
            Value::from(&item.unit_id),
            Value::Text(item.agreed_price.to_string()),
            Value::Text(item.tax_rate.to_string()),
            Value::Text(item.max_qty.map(|value| value.to_string()).unwrap_or_default()),
            optional_text(item.remark.as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 删单（软删；只有草稿能删）。返回 0 行 = 不是草稿 / 不存在。
///
/// 为什么不物理删：明细带 `executed_qty`，且采购明细弱引用它的行 id ——
/// 物理删会让「这份协议到底签过没有」查不出来（与采购计划同口径）。
pub async fn soft_delete_agreement(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {AGREEMENT_TABLE} \
                SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 提交审批：`DRAFT → SUBMITTED` + 审批实例 id（同一事务由调用方持有）。
pub async fn mark_submitted(
    ex: &impl SqlExecutor,
    id: Uuid,
    instance_id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status_with_instance(
        ex,
        id,
        &AgreementTransition {
            from: STATUS_DRAFT,
            to: STATUS_SUBMITTED,
            instance: Some(instance_id),
            reason,
        },
        operator,
        now,
    )
    .await
}

/// 审批结论回写：`SUBMITTED → APPROVED`（通过）。审批实例 id 保留（回溯要查得了「谁批的」）。
pub async fn mark_approved(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status_with_instance(
        ex,
        id,
        &AgreementTransition {
            from: STATUS_SUBMITTED,
            to: STATUS_APPROVED,
            instance: None,
            reason,
        },
        operator,
        now,
    )
    .await
}

/// 审批结论回写：`SUBMITTED → DRAFT`（驳回 / 撤回打回草稿，清实例）。
pub async fn mark_back_to_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status_with_instance(
        ex,
        id,
        &AgreementTransition {
            from: STATUS_SUBMITTED,
            to: STATUS_DRAFT,
            instance: None,
            reason,
        },
        operator,
        now,
    )
    .await
}

/// 显式跳过审批：`DRAFT → APPROVED`（**没有**审批实例）。
///
/// 只有「没配已发布的 `fw_agreement` 流程」且操作人持 `business:erp:skip-approval`
/// 且理由 ≥10 字时才走这条路（判据在 `approval_gate::authorize_missing_flow`）。
pub async fn mark_skipped_approval(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status_with_instance(
        ex,
        id,
        &AgreementTransition {
            from: STATUS_DRAFT,
            to: STATUS_APPROVED,
            instance: None,
            reason,
        },
        operator,
        now,
    )
    .await
}

/// 一次状态流转的参数（把 6 个参数收成 5 个：clippy 的上限是 7）。
struct AgreementTransition<'a> {
    /// 期望的当前状态（CAS 条件）。
    from: &'a str,
    /// 目标状态。
    to: &'a str,
    /// 审批实例：`Some` 写入、`None` 保留原值。
    instance: Option<Uuid>,
    /// 追加到备注的说明。
    reason: Option<&'a str>,
}

/// 状态流转（CAS：`from` → `to`）。
///
/// `instance = None` 时**保留**原 `approval_instance_id`（与采购计划刻意不同）：
/// 协议是「签约主体」文件，审批通过后仍要能回溯「这一版是谁批的」；
/// 打回草稿时清实例 —— 草稿不该挂着一个已结束的审批单。
async fn apply_status_with_instance(
    ex: &impl SqlExecutor,
    id: Uuid,
    transition: &AgreementTransition<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let AgreementTransition {
        from,
        to,
        instance,
        reason,
    } = *transition;
    let set_instance = match instance {
        Some(_) => "approval_instance_id = NULLIF(?::text,'')::uuid,",
        None if to == STATUS_DRAFT => "approval_instance_id = NULL,",
        None => "",
    };
    let mut params = vec![Value::Text(to.to_string())];
    if instance.is_some() {
        params.push(uuid_text(instance));
    }
    params.extend([
        optional_text(reason),
        optional_text(reason),
        optional_text(reason),
        Value::from(now),
        uuid_text(operator),
        Value::from(&id),
        Value::Text(from.to_string()),
    ]);
    ex.exec_sql(
        format!(
            "UPDATE {AGREEMENT_TABLE} \
                SET status = ?, {set_instance} \
                    remark = CASE WHEN ?::text IS NULL OR btrim(?::text) = '' THEN remark \
                                  ELSE COALESCE(NULLIF(remark, '') || E'\\n', '') || ?::text END, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
        ),
        params,
    )
    .await
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn cst(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("rfc3339 时刻字面量")
            .with_timezone(&Utc)
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    #[test]
    fn agreement_no_is_zero_padded_to_six_digits() {
        assert_eq!(format_agreement_no("20261118", 1), "FWA-20261118-000001");
        assert_eq!(format_agreement_no("20261118", 42), "FWA-20261118-000042");
        assert_eq!(format_agreement_no("20261118", 999999), "FWA-20261118-999999");
    }

    /// 周期键按**北京时间**切天：UTC 15:59 还是当天，16:00 就是次日。
    #[test]
    fn period_key_rolls_over_at_cst_midnight() {
        assert_eq!(period_key_of(cst("2026-11-18T23:59:59+08:00")), "20261118");
        assert_eq!(period_key_of(cst("2026-11-19T00:00:00+08:00")), "20261119");
        assert_eq!(period_key_of(cst("2026-11-18T15:59:59Z")), "20261118");
        assert_eq!(period_key_of(cst("2026-11-18T16:00:00Z")), "20261119");
    }

    #[test]
    fn status_and_price_mode_sets_match_migration_check() {
        assert_eq!(STATUSES, &["DRAFT", "SUBMITTED", "APPROVED"]);
        assert_eq!(PRICE_MODES, &["FIXED", "FLOATING"]);
        // 「不造死状态」：到期是时间的函数，终止没有入口（V0450 头注释 1 / 6）
        assert!(!STATUSES.contains(&"EXPIRED"));
        assert!(!STATUSES.contains(&"TERMINATED"));
    }

    /// 有效期判据：起止日都含边界；非 APPROVED 一律不生效。
    #[test]
    fn effective_window_is_inclusive_and_gated_by_approved() {
        let start = date("2026-01-01");
        let end = date("2026-12-31");
        assert!(
            is_effective_on(STATUS_APPROVED, start, end, start),
            "起始日当天必须生效（含边界）"
        );
        assert!(
            is_effective_on(STATUS_APPROVED, start, end, end),
            "到期日当天必须还生效（含边界）"
        );
        assert!(!is_effective_on(STATUS_APPROVED, start, end, date("2025-12-31")));
        assert!(!is_effective_on(STATUS_APPROVED, start, end, date("2027-01-01")));
        // 草稿 / 审批中即使落在效期内也不生效（否则等于没有审批）
        assert!(!is_effective_on(STATUS_DRAFT, start, end, date("2026-06-01")));
        assert!(!is_effective_on(STATUS_SUBMITTED, start, end, date("2026-06-01")));
    }

    /// 容差判据：0 容差 = 必须 ≤ 协议价；5% 容差放行 1.05 倍、拒 1.06 倍。
    #[test]
    fn max_allowed_price_applies_tolerance_with_six_decimals() {
        assert_eq!(max_allowed_price(dec("10.000000"), dec("0")), dec("10.000000"));
        assert_eq!(max_allowed_price(dec("10.000000"), dec("0.05")), dec("10.500000"));
        // 12.345678 × 1.05 = 12.9629619 → 6 位四舍五入
        assert_eq!(max_allowed_price(dec("12.345678"), dec("0.05")), dec("12.962962"));
        // 1% 容差下 10.10 放行、10.11 拒绝
        let cap = max_allowed_price(dec("10.000000"), dec("0.01"));
        assert!(dec("10.100000") <= cap);
        assert!(dec("10.110000") > cap);
    }

    #[test]
    fn today_cst_and_year_follow_beijing_time() {
        let late = cst("2026-12-31T16:30:00Z"); // 北京时间已跨年
        assert_eq!(today_cst(late), date("2027-01-01"));
        assert_eq!(year_cst(late), 2027);
        let early = cst("2026-12-31T15:59:00Z");
        assert_eq!(today_cst(early), date("2026-12-31"));
        assert_eq!(year_cst(early), 2026);
    }

    #[test]
    fn remaining_qty_is_none_when_no_cap() {
        let mut item = AgreementItemRow {
            id: Uuid::nil(),
            agreement_id: Uuid::nil(),
            line_no: 1,
            material_id: Uuid::nil(),
            material_code: String::new(),
            material_name: String::new(),
            unit_id: Uuid::nil(),
            unit_name: String::new(),
            agreed_price: dec("10"),
            tax_rate: dec("0.13"),
            max_qty: None,
            executed_qty: dec("5"),
            executed_amount: dec("50"),
            remark: None,
        };
        assert_eq!(item.remaining_qty(), None, "不限量的协议没有「剩余量」概念");
        item.max_qty = Some(dec("100"));
        assert_eq!(item.remaining_qty(), Some(dec("95")));
        // 超上限后剩余为负（与采购计划同口径：负数要能露头，不许截成 0）
        item.executed_qty = dec("120");
        assert_eq!(item.remaining_qty(), Some(dec("-20")));
    }

    #[test]
    fn uuid_text_and_date_text_shapes() {
        match uuid_text(None) {
            Value::Text(text) => assert!(text.is_empty(), "None 必须折成空串（由 ?::text::uuid 转 NULL）"),
            other => panic!("uuid_text(None) 不是 Text：{other:?}"),
        }
        assert_eq!(date_text(date("2026-01-05")), "2026-01-05");
    }
}

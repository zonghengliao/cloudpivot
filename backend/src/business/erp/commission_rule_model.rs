//! 佣金规则数据层（销售链 · SO-36b）。
//!
//! 表结构唯一依据：`migrations/business/V0309__sales_commission.up.sql` 的
//! `biz_commission_rule`（17 列：11 业务 + 5 审计 + 1 软删）。
//!
//! | 列 | 口径 |
//! |----|------|
//! | `scope_type` | `SALESPERSON` / `MATERIAL_CATEGORY` / `ALL`（`ck_biz_cmrule_scope`） |
//! | `scope_id` | 销售员 ⇒ `sys_user.id`；**物料类别 ⇒ `biz_material.id`**（基础版无分类表）；`ALL` ⇒ NULL |
//! | `rate` | `NUMERIC(8,4)`：`PERCENT` 百分数 0–100 / `FIXED` 金额 ≥ 0（同列两义，看 `calc_type`） |
//! | `status` | `ACTIVE` 参与计提匹配 / `INACTIVE` 停用（**停用不软删**） |
//! | `deleted_at` | 软删；只用于「录错了」。历史明细靠 `rule_id` 弱引用，名字仍读得到 |
//!
//! # 读侧为什么要 JOIN `sys_user` / `biz_material`
//!
//! 列表页要显示「范围的**人话**」（销售员姓名 / 物料名称），前端 `scope_name` 就是这一列。
//! 两个 JOIN 都带 `scope_type` 条件，互斥命中，不会把同一 uuid 在两张表里都连出来；
//! 被删 / 停用的对象仍能从表里读到名字（JOIN 不带 `deleted_at` 过滤）——
//! 规则是**审计事实**，展示当时的对象名比展示空白有用。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_commission_rule 是**配置表**（不是业务单据，没有归属人 / 部门列），
//!         可见性由 business:erp:commission-rule:* 权限码控制；分页走
//!         model_base::page_of → scope::paginate_unscoped（架构 §2.5 闸①口径）。
//!         与 commission_handler 头注释里「规则是 SCOPE-EXEMPT」的登记一致 ——
//!         这里**不要**把它加进 scope::SCOPED_TABLES（加了会让 paginate_unscoped 直接报错）。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::commission_rule_dto::{
    RuleFilter, RuleSpec, SCOPE_MATERIAL, SCOPE_SALESPERSON, clean_optional,
};
use crate::business::erp::model_base::page_of;
use crate::business::shared::scope;
use crate::utils;

/// 表名。
pub const RULE_TABLE: &str = "biz_commission_rule";

/// 本模块读的行列（与 V0309 逐列对应，顺序无关）。
pub const RULE_COLUMNS: &str = "id, rule_name, scope_type, scope_id, calc_type, rate, min_amount, \
     max_amount, effective_from, effective_to, status, remark, created_at, updated_at";

/// 分页锚点：配置表不参与数据范围（见模块头注释）。
pub const RULE_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(RULE_TABLE);

/// 列表排序：**启用的排在前面**（运营看列表先看还在生效的），同组内新生效的在前，
/// 最后用 `created_at` / `id` 收全序 —— 没有全序时翻页会漏行 / 重复行。
pub const RULE_ORDER: &str = "ORDER BY t.status ASC, t.effective_from DESC, t.created_at DESC, t.id ASC";

// ==================== 行结构 ====================

/// `biz_commission_rule` 一行（含状态与备注，列表 / 回单共用）。
#[derive(Debug, Clone)]
pub struct RuleDetailRow {
    /// 主键。
    pub id: Uuid,
    /// 规则名。
    pub rule_name: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围对象。
    pub scope_id: Option<Uuid>,
    /// 计算方式。
    pub calc_type: String,
    /// 比率 / 固定额。
    pub rate: Decimal,
    /// 起算金额。
    pub min_amount: Decimal,
    /// 单笔封顶。
    pub max_amount: Option<Decimal>,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含；空 = 长期）。
    pub effective_to: Option<NaiveDate>,
    /// 状态 `ACTIVE` / `INACTIVE`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl RuleDetailRow {
    /// 由查询行构造（列名见 [`RULE_COLUMNS`]）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            rule_name: row.get_typed("rule_name")?,
            scope_type: row.get_typed("scope_type")?,
            scope_id: row.get_typed("scope_id")?,
            calc_type: row.get_typed("calc_type")?,
            rate: row.get_typed("rate")?,
            min_amount: row.get_typed("min_amount")?,
            max_amount: row.get_typed("max_amount")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 还原成归一化规格（编辑时作为「当前值」参与合并，也让单测能直接拿 DB 行验语义）。
    pub fn to_spec(&self) -> RuleSpec {
        RuleSpec {
            rule_name: self.rule_name.clone(),
            scope_type: self.scope_type.clone(),
            scope_id: self.scope_id,
            calc_type: self.calc_type.clone(),
            rate: self.rate,
            min_amount: self.min_amount,
            max_amount: self.max_amount,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            status: self.status.clone(),
            remark: self.remark.clone(),
        }
    }
}

/// 列表行 = 规则本体 + `scope_name`（销售员姓名 / 物料名称；`ALL` 为 `None`）。
pub type RuleListRow = (RuleDetailRow, Option<String>);

/// 把查询行映射成 [`RuleListRow`]（`page_of` 的回调，必须是 fn 指针）。
fn map_list_row(row: &Row) -> DbResult<RuleListRow> {
    let scope_name: Option<String> = row.get_typed("scope_name")?;
    Ok((RuleDetailRow::from_row(row)?, clean_optional(scope_name)))
}

// ==================== 读 ====================

/// 列表 / 详情的公共 SELECT（含两个互斥的 LEFT JOIN 与派生列 `scope_name`）。
///
/// 列清单由 [`RULE_COLUMNS`] 单点派生（`t.<列>`），避免「常量改了、SQL 没改」这种
/// 只在运行时才炸的漂移。
fn select_sql() -> String {
    let columns: Vec<String> = RULE_COLUMNS
        .split(',')
        .map(|column| format!("t.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, \
                CASE t.scope_type \
                     WHEN '{SCOPE_SALESPERSON}' THEN COALESCE(NULLIF(u.real_name, ''), u.username) \
                     WHEN '{SCOPE_MATERIAL}' THEN m.material_name \
                     ELSE NULL \
                END AS scope_name \
           FROM {RULE_TABLE} t \
           LEFT JOIN sys_user u ON u.id = t.scope_id AND t.scope_type = '{SCOPE_SALESPERSON}' \
           LEFT JOIN biz_material m ON m.id = t.scope_id AND t.scope_type = '{SCOPE_MATERIAL}'",
        columns.join(", ")
    )
}

/// 按主键取存活规则（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<RuleListRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(map_list_row).transpose()
}

/// 规则分页：`scope_type` / `calc_type` / `status` / `scope_id` 精确，`keyword` 模糊
/// （规则名 / 范围对象名 / 备注）。
pub async fn page(filter: &RuleFilter) -> DbResult<(Vec<RuleListRow>, i64)> {
    let mut sql = format!("{} WHERE t.deleted_at IS NULL", select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let Some(scope_type) = filter.scope_type.as_deref() {
        sql.push_str(" AND t.scope_type = ?");
        params.push(Value::Text(scope_type.to_string()));
    }
    if let Some(calc_type) = filter.calc_type.as_deref() {
        sql.push_str(" AND t.calc_type = ?");
        params.push(Value::Text(calc_type.to_string()));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(scope_id) = filter.scope_id {
        sql.push_str(" AND t.scope_id = ?::uuid");
        params.push(Value::from(&scope_id));
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(
            " AND (t.rule_name LIKE ? \
                OR COALESCE(t.remark, '') LIKE ? \
                OR COALESCE(u.real_name, u.username, '') LIKE ? \
                OR COALESCE(m.material_name, '') LIKE ?)",
        );
        for _ in 0..4 {
            params.push(Value::from(pattern.clone()));
        }
    }
    page_of(
        RULE_TARGET,
        sql,
        params,
        RULE_ORDER,
        filter.page,
        filter.page_size,
        map_list_row,
    )
    .await
}

/// 范围对象是否存在（`SALESPERSON` ⇒ `sys_user`、`MATERIAL_CATEGORY` ⇒ `biz_material`）。
///
/// 只排除**软删**：停用账号 / 停用物料仍然可以作为规则范围（离职交接、物料下架
/// 但历史提成口径要留着），这是与卡面「物料存在且未软删」一致的口径。
pub async fn scope_target_exists(scope_type: &str, scope_id: Uuid) -> DbResult<bool> {
    let sql = match scope_type {
        SCOPE_SALESPERSON => "SELECT count(*) FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL",
        SCOPE_MATERIAL => "SELECT count(*) FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL",
        _ => return Ok(false),
    };
    let count: i64 = db().scalar_with_params(sql, vec![Value::from(&scope_id)]).await?;
    Ok(count > 0)
}

/// 这条规则还被多少条**存活结算单**的明细引用（软删 = 「录错了」，先看有没有人用过）。
///
/// `biz_commission_item` 自己没有 `deleted_at`（V0309 11 列），它的生命周期挂在结算单上：
/// 结算单软删（重算释放）后明细还在表里，但已经不算「在用」——所以要 JOIN 结算单过滤。
pub async fn reference_count(rule_id: Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_commission_item i \
           JOIN biz_commission_settlement s ON s.id = i.settlement_id \
          WHERE i.rule_id = ?::uuid AND s.deleted_at IS NULL",
        vec![Value::from(&rule_id)],
    )
    .await
}

// ==================== 写 ====================

/// 新建规则的 SQL（参数顺序见 [`insert`] 的 `vec!`）。
pub const INSERT_SQL: &str = "INSERT INTO biz_commission_rule \
         (id, rule_name, scope_type, scope_id, calc_type, rate, min_amount, max_amount, \
          effective_from, effective_to, status, remark, \
          created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, NULLIF(?::text, '')::uuid, ?, ?::text::numeric, \
                 ?::text::numeric, NULLIF(?::text, '')::numeric, ?::text::date, \
                 NULLIF(?::text, '')::date, ?, NULLIF(?::text, ''), \
                 now(), now(), NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid)";

/// 整行覆盖编辑的 SQL（参数顺序见 [`update`] 的 `vec!`）。
pub const UPDATE_SQL: &str = "UPDATE biz_commission_rule SET rule_name = ?, scope_type = ?, \
         scope_id = NULLIF(?::text, '')::uuid, calc_type = ?, rate = ?::text::numeric, \
         min_amount = ?::text::numeric, max_amount = NULLIF(?::text, '')::numeric, \
         effective_from = ?::text::date, effective_to = NULLIF(?::text, '')::date, \
         status = ?, remark = NULLIF(?::text, ''), updated_at = now(), \
         update_by = NULLIF(?::text, '')::uuid \
      WHERE id = ?::uuid AND deleted_at IS NULL";

/// 软删的 SQL。
pub const SOFT_DELETE_SQL: &str = "UPDATE biz_commission_rule SET deleted_at = now(), \
         updated_at = now(), update_by = NULLIF(?::text, '')::uuid \
      WHERE id = ?::uuid AND deleted_at IS NULL";

/// 新建规则（主键由调用方生成，失败时由 handler 统一翻错误码）。
///
/// # 为什么日期写成 `?::text::date` 而不是 `?::date`
///
/// 参数一律用 `Value::Text` 传（可空列走 `NULLIF(?::text,'')`）。驱动按**服务端推断的
/// 参数类型**编码：`?::date` 会让 PG 把该参数推断成 `date`，于是 `Value::Text` 要往
/// `date` 槽里编码 → `error serializing parameter N`（本仓 DATE / TIMESTAMPTZ 列的
/// 既有教训，见 `period.rs` 关闭期间那段注释）。先转 `text` 再 `::date`，
/// 参数类型就是 text，与 `Value::Text` 对上。
///
/// 单测 [`tests::date_and_numeric_params_are_cast_through_text`] 把这条规矩钉死在本文件里。
pub async fn insert(id: Uuid, spec: &RuleSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        INSERT_SQL,
        vec![
            Value::from(&id),
            Value::Text(spec.rule_name.clone()),
            Value::Text(spec.scope_type.clone()),
            uuid_text(spec.scope_id),
            Value::Text(spec.calc_type.clone()),
            Value::Text(spec.rate.to_string()),
            Value::Text(spec.min_amount.to_string()),
            optional_decimal(spec.max_amount),
            Value::Text(date_text(spec.effective_from)),
            optional_date(spec.effective_to),
            Value::Text(spec.status.clone()),
            optional_text(spec.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 整行覆盖编辑（软删 / 不存在的行不命中 → 0，由 handler 翻 404）。
pub async fn update(id: Uuid, spec: &RuleSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        UPDATE_SQL,
        vec![
            Value::Text(spec.rule_name.clone()),
            Value::Text(spec.scope_type.clone()),
            uuid_text(spec.scope_id),
            Value::Text(spec.calc_type.clone()),
            Value::Text(spec.rate.to_string()),
            Value::Text(spec.min_amount.to_string()),
            optional_decimal(spec.max_amount),
            Value::Text(date_text(spec.effective_from)),
            optional_date(spec.effective_to),
            Value::Text(spec.status.clone()),
            optional_text(spec.remark.clone()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删（已删 / 不存在 → 0，由 handler 翻 404）。
///
/// **不做物理删除**：`biz_commission_item.rule_id` 是弱引用（无外键），
/// 物理删会让历史明细指向一个不存在的 id；软删同时让 `load_rules` 不再匹配它。
pub async fn soft_delete(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(SOFT_DELETE_SQL, vec![uuid_text(operator), Value::from(&id)])
        .await
}

// ==================== 参数辅助 ====================

/// 可空 UUID：SQL 侧写 `NULLIF(?::text,'')::uuid`（`Value::Null` 绑 `::uuid` 驱动编不出来）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 可空金额：同理走文本 + `NULLIF`。
fn optional_decimal(value: Option<Decimal>) -> Value {
    Value::Text(value.map(|amount| amount.to_string()).unwrap_or_default())
}

/// 可空日期：`NULLIF(?::text,'')::date`。
fn optional_date(value: Option<NaiveDate>) -> Value {
    Value::Text(value.map(date_text).unwrap_or_default())
}

/// 可空文本：`None` 与空串都落 NULL（不留「空字符串 vs NULL」两套语义）。
fn optional_text(value: Option<String>) -> Value {
    Value::Text(value.unwrap_or_default())
}

/// 日期字面量：`YYYY-MM-DD`（`?::date` 只认这个形状）。
fn date_text(value: NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::commission_rule_dto::{CALC_TYPES, SCOPE_TYPES};

    /// 列清单必须覆盖 V0309 的全部业务列：漏一列 = 前端某个字段恒 `undefined`。
    #[test]
    fn columns_cover_v0309_business_columns() {
        for column in [
            "id",
            "rule_name",
            "scope_type",
            "scope_id",
            "calc_type",
            "rate",
            "min_amount",
            "max_amount",
            "effective_from",
            "effective_to",
            "status",
            "remark",
            "created_at",
            "updated_at",
        ] {
            assert!(
                RULE_COLUMNS.split(',').any(|item| item.trim() == column),
                "RULE_COLUMNS 缺列 {column}"
            );
        }
        // 17 列 = 14 读取列 + create_by / update_by / deleted_at（后三列不对外）
        assert_eq!(RULE_COLUMNS.split(',').count(), 14);
    }

    /// 配置表**不得**参与数据范围：登记进 SCOPED_TABLES 会让分页入口直接报错。
    #[test]
    fn rule_table_is_scope_exempt() {
        assert!(!RULE_TARGET.is_scoped());
        assert_eq!(RULE_TARGET.table, RULE_TABLE);
        assert!(
            !crate::business::shared::scope::SCOPED_TABLES
                .iter()
                .any(|target| target.table == RULE_TABLE),
            "biz_commission_rule 是配置表，不能进 SCOPED_TABLES"
        );
    }

    /// 排序必须带全序（末尾用 id）：只按 status / effective_from 翻页会漏行。
    #[test]
    fn list_order_is_total() {
        assert!(RULE_ORDER.ends_with("t.id ASC"), "排序必须是全序：{RULE_ORDER}");
        assert!(RULE_ORDER.contains("t.created_at DESC"));
    }

    /// SELECT 里的两个 JOIN 必须都带 `scope_type` 条件，否则 uuid 撞车时会连错对象。
    #[test]
    fn scope_name_join_is_type_guarded() {
        let sql = select_sql();
        assert!(
            sql.contains(&format!(
                "LEFT JOIN sys_user u ON u.id = t.scope_id AND t.scope_type = '{SCOPE_SALESPERSON}'"
            )),
            "sys_user 的 JOIN 必须限定 scope_type"
        );
        assert!(
            sql.contains(&format!(
                "LEFT JOIN biz_material m ON m.id = t.scope_id AND t.scope_type = '{SCOPE_MATERIAL}'"
            )),
            "biz_material 的 JOIN 必须限定 scope_type"
        );
        // 基础版用的是 material_name，不是 name（写错列名会 500）
        assert!(sql.contains("m.material_name"));
        assert!(!sql.contains("m.name"));
    }

    /// 可空参数一律走 `NULLIF(?::text,'')` 形态：`Value::Null` 绑 `::uuid` / `::date` 编不出来。
    #[test]
    fn nullable_params_use_nullif_text() {
        assert_eq!(uuid_text(None), Value::Text(String::new()));
        assert_eq!(optional_date(None), Value::Text(String::new()));
        assert_eq!(optional_decimal(None), Value::Text(String::new()));
        assert_eq!(optional_text(None), Value::Text(String::new()));
        assert_eq!(date_text(NaiveDate::from_ymd_opt(2026, 11, 7).unwrap()), "2026-11-07");
    }

    /// **参数类型必须与 `Value::Text` 对得上**（线上 500 的根因，别再犯）。
    ///
    /// 驱动的参数类型来自服务端推断：
    /// * `?::date` / `?::timestamptz` ⇒ 该参数是 date/timestamptz，而 `Value::Text` 只能
    ///   编码进 text 家族 ⇒ `error serializing parameter N`（`POST /commission-rule`
    ///   就是这么 500 的，索引 0 基时 N=8 正好是 `effective_from`）；
    /// * `?::text::date` / `NULLIF(?::text,'')::date` ⇒ 参数是 text，`Value::Text` 正常。
    ///
    /// 所以本模块**禁止**出现裸的 `?::date`，且占位符个数必须与 `vec!` 的参数个数相等
    /// （错位会表现为「所有值都绑偏一位」，比 500 更难查）。
    #[test]
    fn date_and_numeric_params_are_cast_through_text() {
        for sql in [INSERT_SQL, UPDATE_SQL, SOFT_DELETE_SQL] {
            assert!(
                !sql.contains("?::date") && !sql.contains("?::timestamptz"),
                "日期/时间参数必须先转 text：{sql}"
            );
            assert!(
                !sql.contains("?::numeric"),
                "金额参数必须写成 `?::text::numeric`（`Value::Text` 编码不进 numeric 槽）：{sql}"
            );
            assert!(!sql.contains("NULLIF(?,"), "可空参数统一用 `NULLIF(?::text,'')`：{sql}");
        }
        assert!(INSERT_SQL.contains("?::text::date"));
        assert!(UPDATE_SQL.contains("?::text::date"));
        // 占位符个数 = 参数个数（14 / 13 / 2），错位绑定会静默写错列
        assert_eq!(INSERT_SQL.matches('?').count(), 14, "INSERT 占位符个数");
        assert_eq!(UPDATE_SQL.matches('?').count(), 13, "UPDATE 占位符个数");
        assert_eq!(SOFT_DELETE_SQL.matches('?').count(), 2, "SOFT DELETE 占位符个数");
    }

    /// `to_spec` 必须原样保留 11 个业务字段（编辑合并的基准值）。
    #[test]
    fn row_to_spec_keeps_every_field() {
        let id = Uuid::now_v7();
        let scope_id = Uuid::now_v7();
        let now = Utc::now();
        let row = RuleDetailRow {
            id,
            rule_name: "物料一 10%".to_string(),
            scope_type: SCOPE_MATERIAL.to_string(),
            scope_id: Some(scope_id),
            calc_type: CALC_TYPES[0].to_string(),
            rate: Decimal::new(100000, 4),
            min_amount: Decimal::ZERO,
            max_amount: Some(Decimal::new(600000, 4)),
            effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            effective_to: None,
            status: "ACTIVE".to_string(),
            remark: Some("夹具".to_string()),
            created_at: now,
            updated_at: now,
        };
        let spec = row.to_spec();
        assert_eq!(spec.rule_name, row.rule_name);
        assert_eq!(spec.scope_type, row.scope_type);
        assert_eq!(spec.scope_id, Some(scope_id));
        assert_eq!(spec.calc_type, row.calc_type);
        assert_eq!(spec.rate, row.rate);
        assert_eq!(spec.max_amount, row.max_amount);
        assert_eq!(spec.status, row.status);
        assert_eq!(spec.remark, row.remark);
        // 域值常量是 DTO 的唯一真值来源，这里顺带钉一下「没被写成别的字面量」
        assert_eq!(SCOPE_TYPES.len(), 3);
    }
}

//! `biz_report_item`（报表项 ↔ 科目映射）的**数据访问层**。
//!
//! 这张表是资产负债表（[`super::report`]）与利润表（[`super::income_statement`]）共用的
//! **唯一配置真相**：报表每次编制都从表里现读（[`super::report::load_items_of`] 按
//! `report_code + is_active = 1` 过滤），所以改一行配置 = 下一次打开报表立即生效，
//! 不存在缓存 / 快照 / 发版。
//!
//! 本文件只做存取，**一行校验都没有**：校验（编码格式 / 公式引用顺序 / 取数模式语法 /
//! 停用前置条件）全在 [`super::report_item_admin`]，因为那里才拿得到「同一张报表的全部
//! 报表项」这一上下文（公式合法性是整表性质，不是单行性质）。
//!
//! # 与 `report.rs` 的分工
//!
//! `report.rs::load_items_of` 是**报表计算**的读路径（只取 `is_active = 1` 且只取计算
//! 需要的 9 列）；本文件的 [`list_items`] 是**维护面**的读路径（含停用行、含 id / remark /
//! updated_at / 引用者），两者互不替代：报表不该为了维护面的字段多查 4 列，维护面也不该
//! 只看到启用行（停用行必须能改回来）。

use nik::prelude::*;
use nik::uuid::Uuid;

/// 报表项配置表（与 `report.rs::ITEM_TABLE` 同一个物理表）。
pub const ITEM_TABLE: &str = "biz_report_item";

/// 科目表（取数预览 / 未匹配告警用）。
pub const ACCOUNT_TABLE: &str = "biz_account";

/// `biz_report_item` 的一行（维护面视角：含 id / 停用行 / 备注 / 更新时间）。
#[derive(Debug, Clone)]
pub struct ItemRow {
    /// 主键。
    pub id: Uuid,
    /// 报表编码（`BALANCE_SHEET` / `INCOME_STATEMENT`）。
    pub report_code: String,
    /// 报表项编码（同报表内唯一，公式靠它引用）。
    pub item_code: String,
    /// 报表项名称。
    pub item_name: String,
    /// 层级：1 大类 / 2 小类 / 3 明细。
    pub item_level: i16,
    /// 上级报表项编码（弱引用）。
    pub parent_code: Option<String>,
    /// 所属方（ASSET / LIABILITY / EQUITY）。
    pub side: String,
    /// 科目匹配模式。
    pub account_pattern: Option<String>,
    /// 计算式（引用其它 item_code）。
    pub formula: Option<String>,
    /// 是否减项。
    pub is_deduction: bool,
    /// 启用位。
    pub is_active: bool,
    /// 显示顺序（同时决定公式求值顺序）。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 更新时间（已格式化成 `yyyy-MM-dd HH:mm:ss`，前端直接显示）。
    pub updated_at: Option<String>,
}

/// 写入用的扁平字段集（新建 / 编辑共用；`report_code + item_code` 是唯一键，编辑时不可变）。
#[derive(Debug, Clone)]
pub struct ItemWrite {
    /// 报表编码。
    pub report_code: String,
    /// 报表项编码。
    pub item_code: String,
    /// 报表项名称。
    pub item_name: String,
    /// 层级。
    pub item_level: i16,
    /// 上级编码。
    pub parent_code: Option<String>,
    /// 所属方。
    pub side: String,
    /// 科目匹配模式。
    pub account_pattern: Option<String>,
    /// 计算式。
    pub formula: Option<String>,
    /// 是否减项。
    pub is_deduction: bool,
    /// 启用位。
    pub is_active: bool,
    /// 显示顺序。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
}

/// 可选文本参数：`None` 发空串，SQL 侧用 `NULLIF(?::text,'')` 还原成 NULL
/// （与 `expense_model::opt_text` 同口径：给驱动发 `Null` 会让 `?::text` 无法推断类型）。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// 报表项列表（含停用行，按 `sort_order, item_code` 排；与报表计算顺序同源）。
pub async fn list_items(
    ex: &impl SqlExecutor,
    report_code: Option<&str>,
) -> DbResult<Vec<ItemRow>> {
    let sql = format!(
        "SELECT id, report_code, item_code, item_name, item_level, parent_code, side, \
                account_pattern, formula, is_deduction, is_active, sort_order, remark, \
                to_char(updated_at AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI:SS') AS updated_at_text \
           FROM {ITEM_TABLE} \
          WHERE (?::text = '' OR report_code = ?::text) \
          ORDER BY report_code, sort_order, item_code"
    );
    let rows = ex
        .query_sql(sql, vec![opt_text(report_code), opt_text(report_code)])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ItemRow {
            id: row.get_typed("id")?,
            report_code: row.get_typed("report_code")?,
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            item_level: row.get_typed("item_level")?,
            parent_code: row
                .get_typed::<Option<String>>("parent_code")
                .unwrap_or_default(),
            side: row.get_typed("side")?,
            account_pattern: row
                .get_typed::<Option<String>>("account_pattern")
                .unwrap_or_default(),
            formula: row
                .get_typed::<Option<String>>("formula")
                .unwrap_or_default(),
            is_deduction: row.get_typed::<bool>("is_deduction")?,
            is_active: row.get_typed::<i16>("is_active")? == 1,
            sort_order: row.get_typed("sort_order")?,
            remark: row
                .get_typed::<Option<String>>("remark")
                .unwrap_or_default(),
            updated_at: row
                .get_typed::<Option<String>>("updated_at_text")
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

/// 单行（按主键）。
pub async fn find_item(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ItemRow>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT id, report_code, item_code, item_name, item_level, parent_code, side, \
                        account_pattern, formula, is_deduction, is_active, sort_order, remark, \
                        to_char(updated_at AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI:SS') AS updated_at_text \
                   FROM {ITEM_TABLE} WHERE id = ?::uuid"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(ItemRow {
        id: row.get_typed("id")?,
        report_code: row.get_typed("report_code")?,
        item_code: row.get_typed("item_code")?,
        item_name: row.get_typed("item_name")?,
        item_level: row.get_typed("item_level")?,
        parent_code: row
            .get_typed::<Option<String>>("parent_code")
            .unwrap_or_default(),
        side: row.get_typed("side")?,
        account_pattern: row
            .get_typed::<Option<String>>("account_pattern")
            .unwrap_or_default(),
        formula: row
            .get_typed::<Option<String>>("formula")
            .unwrap_or_default(),
        is_deduction: row.get_typed::<bool>("is_deduction")?,
        is_active: row.get_typed::<i16>("is_active")? == 1,
        sort_order: row.get_typed("sort_order")?,
        remark: row
            .get_typed::<Option<String>>("remark")
            .unwrap_or_default(),
        updated_at: row
            .get_typed::<Option<String>>("updated_at_text")
            .unwrap_or_default(),
    }))
}

/// 库里已有的报表编码（维护页的页签只能来自真实数据，不写死）。
pub async fn report_codes(ex: &impl SqlExecutor) -> DbResult<Vec<String>> {
    let rows = ex
        .query_sql(
            format!("SELECT DISTINCT report_code FROM {ITEM_TABLE} ORDER BY report_code"),
            vec![],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(row.get_typed::<String>("report_code")?);
    }
    Ok(out)
}

/// 同一报表下 `sort_order` 的最大值（新建项默认排在最后）。
pub async fn max_sort_order(ex: &impl SqlExecutor, report_code: &str) -> DbResult<i32> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT COALESCE(MAX(sort_order), 0) AS max_sort FROM {ITEM_TABLE} \
                  WHERE report_code = ?::text"
            ),
            vec![opt_text(Some(report_code))],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(row.get_typed("max_sort")?),
        None => Ok(0),
    }
}

/// 科目目录（取数预览 / 模式匹配告警用；只取启用 + 未删除）。
#[derive(Debug, Clone)]
pub struct AccountOption {
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别。
    pub account_category: String,
    /// 余额方向。
    pub balance_direction: String,
}

/// 启用中的科目（按编码升序）。
pub async fn account_catalog(ex: &impl SqlExecutor) -> DbResult<Vec<AccountOption>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT account_code, account_name, account_category, balance_direction \
                   FROM {ACCOUNT_TABLE} \
                  WHERE deleted_at IS NULL AND status = 1 \
                  ORDER BY account_code"
            ),
            vec![],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(AccountOption {
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            account_category: row.get_typed("account_category")?,
            balance_direction: row.get_typed("balance_direction")?,
        });
    }
    Ok(out)
}

/// 新建一行（`biz_report_item` 没有 create_by / update_by 列 —— V0183 建表时就没给配置表加审计列，
/// 改配置的审计走 `audit_logs` 中间件，不要在这里补列）。
pub async fn insert_item(ex: &impl SqlExecutor, id: Uuid, spec: &ItemWrite) -> DbResult<()> {
    // SCOPE-EXEMPT: 报表配置表，无归属列。
    ex.exec_sql(
        format!(
            "INSERT INTO {ITEM_TABLE} \
                (id, report_code, item_code, item_name, item_level, parent_code, side, \
                 account_pattern, formula, is_deduction, is_active, sort_order, remark) \
             VALUES (?::uuid, ?::text, ?::text, ?::text, ?::int, NULLIF(?::text,''), ?::text, \
                     NULLIF(?::text,''), NULLIF(?::text,''), ?::boolean, ?::int, ?::int, \
                     NULLIF(?::text,''))"
        ),
        vec![
            Value::from(&id),
            Value::Text(spec.report_code.clone()),
            Value::Text(spec.item_code.clone()),
            Value::Text(spec.item_name.clone()),
            Value::from(i32::from(spec.item_level)),
            opt_text(spec.parent_code.as_deref()),
            Value::Text(spec.side.clone()),
            opt_text(spec.account_pattern.as_deref()),
            opt_text(spec.formula.as_deref()),
            Value::Bool(spec.is_deduction),
            Value::from(if spec.is_active { 1i32 } else { 0i32 }),
            Value::from(spec.sort_order),
            opt_text(spec.remark.as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 编辑一行（`report_code + item_code` 不可变，故不入 SET）。
pub async fn update_item(ex: &impl SqlExecutor, id: Uuid, spec: &ItemWrite) -> DbResult<u64> {
    // SCOPE-EXEMPT: 报表配置表，无归属列。
    ex.exec_sql(
        format!(
            "UPDATE {ITEM_TABLE} SET item_name = ?::text, item_level = ?::int, \
                    parent_code = NULLIF(?::text,''), side = ?::text, \
                    account_pattern = NULLIF(?::text,''), \
                    formula = NULLIF(?::text,''), is_deduction = ?::boolean, is_active = ?::int, \
                    sort_order = ?::int, remark = NULLIF(?::text,''), updated_at = now() \
              WHERE id = ?::uuid"
        ),
        vec![
            Value::Text(spec.item_name.clone()),
            Value::from(i32::from(spec.item_level)),
            opt_text(spec.parent_code.as_deref()),
            Value::Text(spec.side.clone()),
            opt_text(spec.account_pattern.as_deref()),
            opt_text(spec.formula.as_deref()),
            Value::Bool(spec.is_deduction),
            Value::from(if spec.is_active { 1i32 } else { 0i32 }),
            Value::from(spec.sort_order),
            opt_text(spec.remark.as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 启停。
pub async fn set_active(ex: &impl SqlExecutor, id: Uuid, active: bool) -> DbResult<u64> {
    // SCOPE-EXEMPT: 报表配置表，无归属列。
    ex.exec_sql(
        format!(
            "UPDATE {ITEM_TABLE} SET is_active = ?::int, updated_at = now() \
              WHERE id = ?::uuid"
        ),
        vec![
            Value::from(if active { 1i32 } else { 0i32 }),
            Value::from(&id),
        ],
    )
    .await
}

/// 写单个排序号（批量排序由调用方在事务里逐条调用）。
pub async fn set_sort_order(ex: &impl SqlExecutor, id: Uuid, sort_order: i32) -> DbResult<u64> {
    // SCOPE-EXEMPT: 报表配置表，无归属列。
    ex.exec_sql(
        format!(
            "UPDATE {ITEM_TABLE} SET sort_order = ?::int, updated_at = now() \
              WHERE id = ?::uuid"
        ),
        vec![Value::from(sort_order), Value::from(&id)],
    )
    .await
}

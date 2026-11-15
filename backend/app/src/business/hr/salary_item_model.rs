//! 薪资项数据层（`biz_salary_item` · HR-31）。
//!
//! 本文件是 `biz_salary_item` **唯一**的 SQL 出口：列表分页 / 单条读 / 依赖图快照读取 /
//! 计算顺序取号 / 编码与顺序查重 / 建 / 改。入参校验在
//! [`crate::business::hr::salary_item_dto`]，接口层在
//! [`crate::business::hr::salary_item_handler`]。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_salary_item`（设计 §3.1 ⑦）。
//! 列名 / `CHECK` 名 / 唯一索引名逐字取自该迁移；迁移已应用且不可改（AGENTS.md 约定 6），
//! **本卡零迁移**（17 个种子项归 HR-35 / V0393）。
//!
//! # 两条唯一约束决定了「一个编码一个位置」
//!
//! | 索引 | 含义 | 应用层怎么用 |
//! |---|---|---|
//! | `uk_biz_sal_item_code (org_id, item_code)` | 编码唯一（未软删） | 建项前 [`code_owner`] 查一次，报文点出占用者；插入撞键再兜一次 |
//! | `uk_biz_sal_item_sort (org_id, sort_order)` | 计算顺序唯一 | 排序冲突也用 [`sort_owner`] 先查，用户能立刻知道「40 被谁占了」 |
//!
//! 顺序唯一是设计 §3.1 ⑦ 的刻意选择：试算按 `sort_order` 稳定排序，两项同号会让
//! 「同一份数据、两次算出的顺序不同」，凭证行号也跟着漂。
//!
//! # 审计列：`create_by` / `update_by` join 实时取名
//!
//! 本表只有 `create_by` / `update_by`（uuid），没有姓名快照列。按 AGENTS.md 约定 13，
//! 「列表里的创建人是业务展示」→ **join 实时取名**（`sys_user.real_name`，退 `username`），
//! 所以 [`ROW_COLUMNS`] 带两个 `LEFT JOIN`，出参成对给 `*_id` / `*_name`。
//!
//! # 软删
//!
//! `deleted_at IS NULL` 是全部读写的硬过滤条件。本卡没有删除接口（设计 §5.2 的四条里
//! 没有 DELETE），停用走 `is_active = 0`（[`crate::business::hr::salary_item_handler`] 里
//! 「系统项不许停用」「被员工薪资标准引用的项不许停用」的两道守卫）。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::hr::salary_item_dto::{SalaryItemDraft, SalaryItemFilter, SalaryItemSnapshot};
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

// ==================== 约束名（与 V0383 逐字一致） ====================

/// 编码唯一索引。
pub const UK_CODE: &str = "uk_biz_sal_item_code";

/// 计算顺序唯一索引。
pub const UK_SORT: &str = "uk_biz_sal_item_sort";

/// 本表的 6 个 `CHECK`（`V0383` 的 `ck_biz_sal_item_*`）。
///
/// 列出来是为了让 [`check_message`] 可被单测逐条对拍：漏一条的后果是用户看到
/// 「不满足库约束 ck_biz_sal_item_account」这种运维语言（400 里说不清该改哪个字段）。
pub const SALARY_ITEM_CHECKS: [&str; 6] = [
    "ck_biz_sal_item_type",
    "ck_biz_sal_item_calc",
    "ck_biz_sal_item_source",
    "ck_biz_sal_item_formula",
    "ck_biz_sal_item_negflag",
    "ck_biz_sal_item_account",
];

// ==================== SQL ====================

/// 行投影（含两个 `sys_user` join）。
const ROW_COLUMNS: &str = "\
SELECT s.id, s.item_code, s.item_name, s.item_type, s.calc_type, s.source, s.formula, s.sort_order, \
       s.debit_account_code, s.credit_account_code, s.is_taxable, s.is_active, s.is_system, s.remark, \
       s.create_by, COALESCE(NULLIF(cu.real_name, ''), cu.username, '') AS create_by_name, \
       s.update_by, COALESCE(NULLIF(uu.real_name, ''), uu.username, '') AS update_by_name, \
       s.created_at, s.updated_at \
  FROM biz_salary_item s \
  LEFT JOIN sys_user cu ON cu.id = s.create_by \
  LEFT JOIN sys_user uu ON uu.id = s.update_by";

/// 建项（16 个绑定参数：id / org / 13 列 / create_by / update_by）。
///
/// `is_system` **不绑**：系统项（`GROSS` / `NET`）只能由迁移（HR-35 的 V0393）种进来，
/// 接口不许把一个普通项标成「不可删」。
pub const INSERT_SQL: &str = "\
INSERT INTO biz_salary_item \
    (id, org_id, item_code, item_name, item_type, calc_type, source, formula, sort_order, \
     debit_account_code, credit_account_code, is_taxable, is_active, remark, create_by, update_by) \
 VALUES (?::uuid, ?::uuid, ?, ?, ?, ?, ?, NULLIF(?::text, ''), ?::int, \
         NULLIF(?::text, ''), NULLIF(?::text, ''), ?::smallint, ?::smallint, NULLIF(?::text, ''), \
         NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid)";

/// 改项的可写列（13 个绑定参数：11 列 + 2 个审计列 + id）。
///
/// **不含** `item_code`（被公式 `{CODE}` 与 `biz_employee_salary.item_code` 弱引用，改名会让
/// 引用它的公式和薪资标准一起失效）/ `is_system`（迁移专属）/ `created_at`。
pub const UPDATE_SQL: &str = "\
UPDATE biz_salary_item SET \
    item_name = ?, item_type = ?, calc_type = ?, source = ?, formula = NULLIF(?::text, ''), \
    sort_order = ?::int, debit_account_code = NULLIF(?::text, ''), \
    credit_account_code = NULLIF(?::text, ''), is_taxable = ?::smallint, is_active = ?::smallint, \
    remark = NULLIF(?::text, ''), updated_at = ?::timestamptz, \
    update_by = NULLIF(?::text, '')::uuid \
 WHERE id = ?::uuid AND deleted_at IS NULL";

// ==================== 错误翻译 ====================

/// `CHECK` 约束 → 用户可读文案（`ck_biz_sal_item_*`，见 `V0383`）。
pub fn check_message(constraint: Option<&str>) -> String {
    let name = constraint.unwrap_or_default();
    let human = match name {
        "ck_biz_sal_item_type" => "薪资项类型只能是 EARNING / PRE_DEDUCT / WITHHOLD / EMPLOYER / SUMMARY",
        "ck_biz_sal_item_calc" => "算法只能是 FIXED（取薪资标准）/ FORMULA（公式）/ INPUT（手工录）",
        "ck_biz_sal_item_source" => "取数来源只能是 EMPLOYEE_STANDARD / ATTENDANCE / INPUT / SYSTEM / MES",
        "ck_biz_sal_item_formula" => "算法是 FORMULA 的薪资项必须给公式（不能为空）",
        "ck_biz_sal_item_negflag" => "is_taxable / is_active / is_system 只能是 0 或 1",
        "ck_biz_sal_item_account" => {
            "科目与类型不匹配：收入/应发前扣减/单位承担要借+贷两个科目，代扣只要贷方，汇总项不要科目"
        }
        _ => return format!("薪资项数据不满足库约束 {name}，请检查必填项与取值范围后重试"),
    };
    human.to_string()
}

/// 写路径的**唯一**数据库错误出口。
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | 编码重复（`uk_biz_sal_item_code`） | 409 | `10003`（正常路径已在应用层拦成「编码被占」） |
/// | 顺序重复（`uk_biz_sal_item_sort`） | 409 | `10003` |
/// | `CHECK` 违反 | 400 | `10001`（报文点名该改哪个字段） |
/// | 其它 | 500 | `10004` |
pub fn write_error(err: DbError, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => {
            let name = constraint.as_deref().unwrap_or_default().trim();
            let message = if name.eq_ignore_ascii_case(UK_CODE) {
                "薪资项编码已被占用：同一个公司内编码唯一（未删除的项都算），请换一个编码".to_string()
            } else if name.eq_ignore_ascii_case(UK_SORT) {
                "计算顺序已被占用：同一公司内 sort_order 唯一，请换一个顺序值（建议 10 递增）".to_string()
            } else {
                format!("薪资项写入冲突（唯一约束 {name}）：请刷新后重试")
            };
            ApiError::new(409, error_code::RESOURCE_CONFLICT, message)
        }
        DbError::CheckViolation { constraint, .. } => {
            ApiError::new(400, error_code::PARAM_INVALID, check_message(constraint.as_deref()))
        }
        _ => utils::db_error(err, ctx),
    }
}

/// 「薪资项不存在或已删除」的 404。
pub fn not_found_error() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "薪资项不存在或已删除")
}

// ==================== 行 ====================

/// 薪资项全字段行（含两个 `sys_user` join 出来的姓名）。
#[derive(Debug, Clone)]
pub struct SalaryItemRow {
    /// 主键。
    pub id: Uuid,
    /// 编码（公式里按 `{编码}` 引用）。
    pub item_code: String,
    /// 名称。
    pub item_name: String,
    /// 类型。
    pub item_type: String,
    /// 算法。
    pub calc_type: String,
    /// 取数来源。
    pub source: String,
    /// 公式。
    pub formula: Option<String>,
    /// 计算顺序。
    pub sort_order: i32,
    /// 借方科目。
    pub debit_account_code: Option<String>,
    /// 贷方科目。
    pub credit_account_code: Option<String>,
    /// 1 计入应税收入。
    pub is_taxable: i16,
    /// 1 启用。
    pub is_active: i16,
    /// 1 系统项（不可删 / 不可停用）。
    pub is_system: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建人 id。
    pub create_by: Option<Uuid>,
    /// 创建人姓名（join；join 不到投影成 `None`）。
    pub create_by_name: Option<String>,
    /// 最后修改人 id。
    pub update_by: Option<Uuid>,
    /// 最后修改人姓名。
    pub update_by_name: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后修改时间。
    pub updated_at: DateTime<Utc>,
}

impl SalaryItemRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            item_type: row.get_typed("item_type")?,
            calc_type: row.get_typed("calc_type")?,
            source: row.get_typed("source")?,
            formula: clean(row.get_typed("formula")?),
            sort_order: row.get_typed("sort_order")?,
            debit_account_code: clean(row.get_typed("debit_account_code")?),
            credit_account_code: clean(row.get_typed("credit_account_code")?),
            is_taxable: row.get_typed("is_taxable")?,
            is_active: row.get_typed("is_active")?,
            is_system: row.get_typed("is_system")?,
            remark: clean(row.get_typed("remark")?),
            create_by: row.get_typed("create_by")?,
            create_by_name: clean(row.get_typed("create_by_name")?),
            update_by: row.get_typed("update_by")?,
            update_by_name: clean(row.get_typed("update_by_name")?),
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 公式引用（`depends_on` 的原始来源；抽不出来就是空）。
    pub fn refs(&self) -> Vec<String> {
        self.formula
            .as_deref()
            .and_then(|text| crate::business::hr::salary_item_formula::extract_refs(text).ok())
            .unwrap_or_default()
    }
}

/// 空串 → `None`（join 出来的姓名 / 可选文本列统一口径）。
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// `Option<Uuid>` → 绑定值（给 `?::uuid` 参数位用）。
///
/// `None` 用 [`Value::Null`]（而不是空串）：SQL 里是 `?::uuid IS NULL` 形态，
/// 空串转 uuid 会在 PG 侧直接报 `invalid input syntax for type uuid`（`erp/lot_model.rs`
/// 踩过同一个坑）。
fn opt_uuid(value: Option<Uuid>) -> Value {
    match value {
        Some(id) => Value::from(&id),
        None => Value::Null,
    }
}

/// `Option<Uuid>` → 绑定值（给 `NULLIF(?::text,'')::uuid` 参数位用）。
///
/// 这类参数位的**推导类型是 text**（`$n::text`），绑 `Value::Uuid` 会被驱动判成
/// `cannot bind parameter to column type text` —— 所以必须绑字符串形态。
fn opt_uuid_text(value: Option<Uuid>) -> Value {
    match value {
        Some(id) => Value::Text(id.to_string()),
        None => Value::Null,
    }
}

/// `Option<&str>` → 绑定值（NULL 用 `NULLIF(?::text,'')` 表达）。
fn opt_text(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Text(String::new()),
    }
}

// ==================== 读 ====================

/// 按 id 取单条（不含已软删）。
pub async fn find(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<SalaryItemRow>> {
    let sql = format!("{ROW_COLUMNS} WHERE s.id = ?::uuid AND s.deleted_at IS NULL");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(SalaryItemRow::from_row).transpose()
}

/// 按 id 取单条并加行锁（`FOR UPDATE OF s`）：改之前先锁，两个并发请求不会各读一份旧值
/// 再各写一次（`uk_biz_sal_item_sort` 只在真正冲突时才拦得住，改名称/公式时拦不住）。
pub async fn lock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<SalaryItemRow>> {
    let sql = format!("{ROW_COLUMNS} WHERE s.id = ?::uuid AND s.deleted_at IS NULL FOR UPDATE OF s");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(SalaryItemRow::from_row).transpose()
}

/// 有序取全部薪资项（`sort_order` 升序）。
///
/// 用途：①公式依赖图 / 可用变量清单（HR-31 的 `check_formula`）②前端变量面板。
/// 停用的项也返回（前端要标成不可点击；依赖图要能判「引用了已停用的项」）。
pub async fn list_all(ex: &impl SqlExecutor) -> DbResult<Vec<SalaryItemRow>> {
    let sql = format!("{ROW_COLUMNS} WHERE s.deleted_at IS NULL ORDER BY s.sort_order ASC, s.item_code ASC");
    let rows = ex.query_sql(sql, vec![]).await?;
    rows.iter().map(SalaryItemRow::from_row).collect()
}

/// 依赖图快照（只取 [`SalaryItemSnapshot`] 需要的三列；比 [`list_all`] 轻）。
pub async fn list_snapshots(ex: &impl SqlExecutor) -> DbResult<Vec<SalaryItemSnapshot>> {
    let sql = "SELECT item_code, formula, is_active FROM biz_salary_item \
                WHERE deleted_at IS NULL ORDER BY sort_order ASC, item_code ASC";
    let rows = ex.query_sql(sql.to_string(), vec![]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(SalaryItemSnapshot {
            item_code: row.get_typed("item_code")?,
            formula: clean(row.get_typed("formula")?),
            is_active: row.get_typed("is_active")?,
        });
    }
    Ok(out)
}

/// 列表分页（设计 §5.2 #32 + §5.3）。
///
/// 走 [`scope::paginate_unscoped`]：本表是**配置表**（设计 §4.7 的范围表清单里没有它），
/// 显式声明不参与数据权限；分页仍走唯一入口（架构 §2.4 闸①），锚点写成
/// `ScopeTarget::unscoped`。
pub async fn page(filter: &SalaryItemFilter, page: u64, page_size: u64) -> DbResult<(Vec<SalaryItemRow>, i64)> {
    let (sql, params) = list_query(filter);
    let result = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("biz_salary_item"),
        &sql,
        params,
        Some("ORDER BY s.sort_order ASC, s.item_code ASC, s.id ASC"),
        page,
        page_size,
    )
    .await?;
    let mut out = Vec::with_capacity(result.list.len());
    for row in &result.list {
        out.push(SalaryItemRow::from_row(row)?);
    }
    Ok((out, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 拼列表 SQL 与参数（**纯函数**：不连库就能验证条件个数与占位符个数一致）。
///
/// 不含 `ORDER BY` / `LIMIT`：`scope::paginate_unscoped` 要求它们单独传。
pub fn list_query(filter: &SalaryItemFilter) -> (String, Vec<Value>) {
    let mut sql = format!("{ROW_COLUMNS} WHERE s.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();

    if let Some(pattern) = crate::utils::like_pattern(filter.keyword.as_deref()) {
        // 编码 / 名称都能搜：客户记得「餐补」但忘了编码 `MEAL` 是常态
        sql.push_str(" AND (s.item_code ILIKE ? OR s.item_name ILIKE ?)");
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    if let Some(item_type) = &filter.item_type {
        sql.push_str(" AND s.item_type = ?");
        params.push(Value::Text(item_type.clone()));
    }
    if let Some(calc_type) = &filter.calc_type {
        sql.push_str(" AND s.calc_type = ?");
        params.push(Value::Text(calc_type.clone()));
    }
    if let Some(flag) = filter.is_active {
        sql.push_str(" AND s.is_active = ?::smallint");
        params.push(Value::from(flag));
    }
    (sql, params)
}

/// 编码的占用者（未软删）：`Some("BASE")` = 已被 `BASE` 占了，`None` = 可用。
///
/// `exclude` 用于修改路径（自己不算占自己）。
pub async fn code_owner(ex: &impl SqlExecutor, code: &str, exclude: Option<Uuid>) -> DbResult<Option<String>> {
    let sql = "SELECT item_code FROM biz_salary_item \
                WHERE deleted_at IS NULL AND item_code = ? \
                  AND (?::uuid IS NULL OR id <> ?::uuid) LIMIT 1";
    let row = ex
        .query_first_sql(
            sql.to_string(),
            vec![
                Value::Text(code.trim().to_string()),
                opt_uuid(exclude),
                opt_uuid(exclude),
            ],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(row.get_typed("item_code")?)),
        None => Ok(None),
    }
}

/// 计算顺序的占用者（返回占用者的编码，便于报文点名）。
pub async fn sort_owner(ex: &impl SqlExecutor, sort_order: i32, exclude: Option<Uuid>) -> DbResult<Option<String>> {
    let sql = "SELECT item_code FROM biz_salary_item \
                WHERE deleted_at IS NULL AND sort_order = ?::int \
                  AND (?::uuid IS NULL OR id <> ?::uuid) LIMIT 1";
    let row = ex
        .query_first_sql(
            sql.to_string(),
            vec![Value::from(sort_order), opt_uuid(exclude), opt_uuid(exclude)],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(row.get_typed("item_code")?)),
        None => Ok(None),
    }
}

/// 下一个可用的计算顺序（当前最大 + 10，空表从 10 开始）。
///
/// 与设计 §3.1 ⑦ 的列默认值 10 和 §4.4.3 的种子（10/20/…/170）同一口径。
pub async fn next_sort_order(ex: &impl SqlExecutor) -> DbResult<i32> {
    let row = ex
        .query_first_sql(
            "SELECT COALESCE(MAX(sort_order), 0) AS max_sort FROM biz_salary_item WHERE deleted_at IS NULL".to_string(),
            vec![],
        )
        .await?;
    let max = match row {
        Some(row) => row.get_typed::<i32>("max_sort")?,
        None => 0,
    };
    Ok(max / 10 * 10 + 10)
}

/// 引用该编码的员工薪资标准条数（未软删）。
///
/// 停用 / 删除薪资项之前查一次：`biz_employee_salary.item_code` 是弱引用（不建 FK），
/// 项没了标准还在 → 试算静默少一项，界面上看不出「为什么少了」（AGENTS.md 约定 11 的
/// 同类问题：不许把用户留在「数据和配置说不到一块」的状态）。
pub async fn count_employee_refs(ex: &impl SqlExecutor, item_code: &str) -> DbResult<i64> {
    let row = ex
        .query_first_sql(
            "SELECT count(*) AS total FROM biz_employee_salary \
              WHERE deleted_at IS NULL AND item_code = ?"
                .to_string(),
            vec![Value::Text(item_code.trim().to_string())],
        )
        .await?;
    match row {
        Some(row) => row.get_typed("total"),
        None => Ok(0),
    }
}

/// 引用该项编码的**其它薪资项**（公式里写了 `{编码}` 的，未软删；`exclude` 排除自己）。
///
/// 停用 / 改编码前的第二道检查：公式引用了它，停用后引用它的公式会变成「未定义变量」，
/// 保存时不报错、月末试算才炸。
pub async fn formula_referrers(ex: &impl SqlExecutor, item_code: &str, exclude: Option<Uuid>) -> DbResult<Vec<String>> {
    let sql = "SELECT item_code, formula FROM biz_salary_item \
                WHERE deleted_at IS NULL AND is_active = 1 AND formula IS NOT NULL AND formula LIKE ? \
                  AND (?::uuid IS NULL OR id <> ?::uuid)";
    let pattern = format!("%{{{}}}%", item_code.trim());
    let rows = ex
        .query_sql(
            sql.to_string(),
            vec![Value::Text(pattern), opt_uuid(exclude), opt_uuid(exclude)],
        )
        .await?;
    let mut out = Vec::new();
    for row in &rows {
        let code: String = row.get_typed("item_code")?;
        let formula: Option<String> = row.get_typed("formula")?;
        let hit = formula
            .as_deref()
            .and_then(|text| crate::business::hr::salary_item_formula::extract_refs(text).ok())
            .is_some_and(|refs| refs.iter().any(|name| name == item_code.trim()));
        // `LIKE` 只是粗筛（`{A}` 会命中 `{AB}`），抽引用再精判一次
        if hit {
            out.push(code);
        }
    }
    Ok(out)
}

// ==================== 写 ====================

/// 建项。
pub async fn insert(ex: &impl SqlExecutor, id: Uuid, draft: &SalaryItemDraft, operator: Option<Uuid>) -> DbResult<()> {
    // SCOPE-EXEMPT: 配置表写入；org_id 由 SENTINEL_ORG_ID 显式给出（多组织落地时改一处）。
    ex.exec_sql(
        INSERT_SQL.to_string(),
        vec![
            Value::from(&id),
            Value::from(&SENTINEL_ORG_ID),
            Value::Text(draft.item_code.clone()),
            Value::Text(draft.item_name.clone()),
            Value::Text(draft.item_type.clone()),
            Value::Text(draft.calc_type.clone()),
            Value::Text(draft.source.clone()),
            opt_text(draft.formula.as_deref()),
            Value::from(draft.sort_order),
            opt_text(draft.debit_account_code.as_deref()),
            opt_text(draft.credit_account_code.as_deref()),
            Value::from(i32::from(draft.is_taxable)),
            Value::from(i32::from(draft.is_active)),
            opt_text(draft.remark.as_deref()),
            opt_uuid_text(operator),
            opt_uuid_text(operator),
        ],
    )
    .await?;
    Ok(())
}

/// 改项（返回受影响行数；`0` = 行已被删，调用方报 404）。
pub async fn update(ex: &impl SqlExecutor, id: Uuid, draft: &SalaryItemDraft, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 配置表写入。
    ex.exec_sql(
        UPDATE_SQL.to_string(),
        vec![
            Value::Text(draft.item_name.clone()),
            Value::Text(draft.item_type.clone()),
            Value::Text(draft.calc_type.clone()),
            Value::Text(draft.source.clone()),
            opt_text(draft.formula.as_deref()),
            Value::from(draft.sort_order),
            opt_text(draft.debit_account_code.as_deref()),
            opt_text(draft.credit_account_code.as_deref()),
            Value::from(i32::from(draft.is_taxable)),
            Value::from(i32::from(draft.is_active)),
            opt_text(draft.remark.as_deref()),
            Value::from(Utc::now()),
            opt_uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::salary_item_dto::SalaryItemFilter;

    /// 建表迁移原文（编译期嵌入）：约束名 / 索引名 / 列宽改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    /// CHECK 名单与 `V0383` 逐条对拍（`check_message` 漏一条 → 用户看到运维语言）。
    #[test]
    fn check_names_match_migration() {
        for name in SALARY_ITEM_CHECKS {
            assert!(V0383_DDL.contains(name), "V0383 里没有约束 {name}");
            let message = check_message(Some(name));
            assert!(!message.contains("不满足库约束"), "{name} 没有专门文案：{message}");
        }
        // 落款兜底分支
        assert!(check_message(Some("ck_unknown")).contains("不满足库约束"));
        assert!(check_message(None).contains("不满足库约束"));
    }

    /// 两个唯一索引名与 DDL 逐字一致（`write_error` 靠它们分流 409 文案）。
    #[test]
    fn unique_index_names_match_migration() {
        assert!(V0383_DDL.contains(&format!("{UK_CODE} ON biz_salary_item")));
        assert!(V0383_DDL.contains(&format!("{UK_SORT} ON biz_salary_item")));
    }

    /// INSERT / UPDATE 的列与 DDL 对拍：漏一列 = 该字段永远存不进去。
    #[test]
    fn write_sql_covers_every_writable_column() {
        for column in [
            "item_code",
            "item_name",
            "item_type",
            "calc_type",
            "source",
            "formula",
            "sort_order",
            "debit_account_code",
            "credit_account_code",
            "is_taxable",
            "is_active",
            "remark",
            "create_by",
            "update_by",
        ] {
            assert!(INSERT_SQL.contains(column), "INSERT_SQL 少了 {column}");
        }
        for column in [
            "item_name",
            "item_type",
            "calc_type",
            "source",
            "formula",
            "sort_order",
            "debit_account_code",
            "credit_account_code",
            "is_taxable",
            "is_active",
            "remark",
            "update_by",
        ] {
            assert!(UPDATE_SQL.contains(column), "UPDATE_SQL 少了 {column}");
        }
        // 编码 / 系统标记不可改（改动会让公式与薪资标准一起失效 / 客户能把系统项锁死）
        assert!(!UPDATE_SQL.contains("item_code ="), "UPDATE_SQL 不许改 item_code");
        assert!(!UPDATE_SQL.contains("is_system"), "UPDATE_SQL 不许改 is_system");
        assert!(UPDATE_SQL.contains("deleted_at IS NULL"));
        assert!(!INSERT_SQL.contains("is_system"), "is_system 只能由迁移种进来");
    }

    /// 占位符个数与绑定参数个数一致（列表 / 单条 / 写）。
    #[test]
    fn placeholder_counts_match_bindings() {
        assert_eq!(INSERT_SQL.matches('?').count(), 16, "INSERT 16 个占位符");
        assert_eq!(UPDATE_SQL.matches('?').count(), 14, "UPDATE 14 个占位符");

        let filter = SalaryItemFilter {
            keyword: Some("加班".to_string()),
            item_type: Some("EARNING".to_string()),
            calc_type: Some("FORMULA".to_string()),
            is_active: Some(1),
        };
        let (sql, params) = list_query(&filter);
        assert_eq!(sql.matches('?').count(), params.len(), "列表占位符与参数不一致：{sql}");
        assert!(sql.contains("s.item_code ILIKE ? OR s.item_name ILIKE ?"));
        assert!(!sql.contains("ORDER BY"), "ORDER BY 必须单独传：{sql}");
        assert!(!sql.contains("LIMIT"), "LIMIT 必须单独传：{sql}");
    }

    /// 空过滤：不加任何条件，参数为空。
    #[test]
    fn empty_filter_adds_nothing() {
        let (sql, params) = list_query(&SalaryItemFilter::default());
        assert!(params.is_empty());
        assert!(sql.ends_with("WHERE s.deleted_at IS NULL"));
    }

    /// 列表投影必须带 `*_name`（AGENTS.md 约定 13：id 与 name 成对返回）。
    #[test]
    fn row_projection_pairs_ids_with_names() {
        for column in ["create_by", "create_by_name", "update_by", "update_by_name"] {
            assert!(ROW_COLUMNS.contains(column), "投影少 {column}");
        }
        assert!(
            ROW_COLUMNS.contains("LEFT JOIN sys_user cu"),
            "创建人姓名要 join 实时取"
        );
        assert!(ROW_COLUMNS.contains("LEFT JOIN sys_user uu"));
    }

    /// 空白归一化。
    #[test]
    fn clean_trims_and_drops_empty() {
        assert_eq!(clean(Some("  BASE \n".to_string())), Some("BASE".to_string()));
        assert_eq!(clean(Some("   ".to_string())), None);
        assert_eq!(clean(None), None);
    }
}

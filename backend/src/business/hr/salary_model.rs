//! 员工薪资标准数据层（`biz_employee_salary` · HR-16）。
//!
//! 本文件是 `biz_employee_salary` **唯一**的 SQL 出口：读（员工全部标准 / 单薪资项全部版本）、
//! 写（插版本 / 收口 `effective_to` / 修正金额 / 软删）、**区间重叠检测**（设计 §4.9 巡检 A3）。
//! 出参契约在 [`crate::business::hr::employee_dto`]（HR-11 已定稿的 `SalaryInput` /
//! `EmployeeSalaryRes`，本卡不另立一套形状），接口层在
//! [`crate::business::hr::salary_handler`]，入参校验在 [`crate::business::hr::salary_dto`]。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_employee_salary`（设计 §3.1 ③）。
//! 列名 / `CHECK` 名 / 唯一索引名逐字取自该迁移；**迁移已应用且不可改**（AGENTS.md 约定 6），
//! 本卡零迁移。
//!
//! # 为什么区间不许重叠（设计 §3.1 ③ 的注）
//!
//! 试算按 `effective_from <= period_end AND (effective_to IS NULL OR effective_to >= period_start)`
//! 取当期标准。两条区间一旦重叠，同一期间会取到**两个**标准 —— 谁后被 `ORDER BY` 选中就成了
//! 随机数，工资算错还查不出来。所以：
//!
//! 1. 库层只有 `uk_biz_emp_salary (employee_id, item_code, effective_from)`（同一天不许两条）；
//! 2. 「区间相交」只能应用层拦 + 巡检 A3 兜底（`daterange` 相交检测，HR-48 的脚本里）；
//! 3. **边界相等算重叠**：列注释写的是「生效起（含）」/「生效止（含）」，两端都是闭区间，
//!    `[01-01, 06-30]` 与 `[06-30, …]` 在 06-30 这一天会有两个标准 —— 必须拒。
//!    纯函数 [`ranges_overlap`] 就按闭区间判，单测把这条钉住。
//!
//! # 三种写法（全部在 [`crate::business::hr::salary_handler`] 的一个事务里）
//!
//! | 意图 | `effective_to` | 冲突口径 | 旧行怎么处理 |
//! |---|---|---|---|
//! | 现行标准（调薪 / 首次录入） | 空（= 至今） | 与「未来版本」或「已收口但延伸过新生效日的行」相交 → 409 | 开放行（`effective_to IS NULL` 且起点更早）收口到 `新生效日 - 1` |
//! | 历史 / 计划区间 | 有值 | 与任何版本相交（含边界相等） → 409 | 一条都不动（明确区间不许隐式改历史） |
//! | 取消某项（`amount` 缺失） | — | — | 覆盖 `effective_from` 的行收口到 `-1 天`，起点 ≥ 生效日的行软删 |
//!
//! 「修正错金额」不新增版本：同 `effective_from` 的行走 [`update_amount`]（`uk_biz_emp_salary`
//! 决定了同一天只能有一行，改金额就是改这一行）。
//!
//! # 与「员工详情」的关系
//!
//! 详情里的 `salary_items[]`（HR-12 的 `load_salary_items`）直接吃本文件的
//! [`list_by_employee`] —— 详情 / 薪资 tab / 调薪回执看到的是**同一份投影**，
//! 不会出现「详情里有、薪资页没有」的字段漂移。

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::employee_dto::SalaryInput;
use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::shared::error_code;
use crate::utils;

// ==================== 约束名（与 V0383 逐字一致） ====================

/// 版本唯一索引（`(employee_id, item_code, effective_from) WHERE deleted_at IS NULL`）。
pub const UK_SALARY_VERSION: &str = "uk_biz_emp_salary";

/// 本表的 2 个 `CHECK`（`V0383` 的 `ck_biz_emp_salary_*`）。
///
/// 列出来是为了让 [`check_message`] 的映射**可被单测逐条对拍**：漏一条的后果是
/// 用户看到「薪资标准不满足库约束 ck_biz_emp_salary_range」这种运维语言。
pub const SALARY_CHECKS: [&str; 2] = ["ck_biz_emp_salary_amount", "ck_biz_emp_salary_range"];

/// `CHECK` 约束 → 用户可读文案。
pub fn check_message(constraint: Option<&str>) -> String {
    let name = constraint.unwrap_or_default();
    let human = match name {
        "ck_biz_emp_salary_amount" => "标准金额不能为负数（月标准，正数）",
        "ck_biz_emp_salary_range" => "生效止日期不能早于生效起日期",
        _ => return format!("薪资标准数据不满足库约束 {name}，请检查必填项与取值范围后重试"),
    };
    human.to_string()
}

/// 薪资标准写路径的**唯一**数据库错误出口。
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | 同一天同一薪资项已有一条标准（唯一键） | 409 | `10003`（通用冲突；正常路径已在应用层拦成「修正」） |
/// | `CHECK` 违反 | 400 | `10001`（报文点名该改哪个字段） |
/// | 其它（含外键 —— 员工被删） | 500 | `10004` |
pub fn write_error(err: DbError, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "薪资标准写入冲突（唯一约束 {}）：同一薪资项同一天只能有一条标准，请刷新后重试",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        DbError::CheckViolation { constraint, .. } => {
            ApiError::new(400, error_code::PARAM_INVALID, check_message(constraint.as_deref()))
        }
        _ => utils::db_error(err, ctx),
    }
}

/// 一条薪资标准与其它版本区间重叠（409 + `68007`）。
///
/// 报文必须点名**薪资项 + 两条区间 + 下一步该做什么**：HR 拿到这句话就能自己决定
/// 是改生效日、还是先删掉那条（不能只说「区间重叠」—— 满屏 12 个月，看不出来跟谁撞）。
pub fn overlap_error(
    item_code: &str,
    item_name: Option<&str>,
    candidate: (NaiveDate, Option<NaiveDate>),
    conflict: (NaiveDate, Option<NaiveDate>),
) -> ApiError {
    ApiError::new(
        409,
        error_code::HR_SALARY_RANGE_OVERLAP,
        format!(
            "薪资项「{}」（{item_code}）的生效区间与已有标准重叠：本次 {} 与已有 {} 在日期上有交集\
             （生效起 / 生效止两端都含当天，边界相等也算重叠）。请改生效日，或先取消 / 修正那条已有标准",
            item_name.unwrap_or(item_code),
            range_text(candidate.0, candidate.1),
            range_text(conflict.0, conflict.1)
        ),
    )
}

/// 区间的人话（`2026-01-01 ~ 至今` / `2026-01-01 ~ 2026-06-30`）。
pub fn range_text(from: NaiveDate, to: Option<NaiveDate>) -> String {
    match to {
        Some(to) => format!("{from} ~ {to}"),
        None => format!("{from} ~ 至今"),
    }
}

// ==================== 区间重叠（A3 的应用层实现） ====================

/// 两个生效区间是否相交（**闭区间**：`from` 与 `to` 都含当天；`None` = 至今 = 正无穷）。
///
/// ```
/// # use nik::prelude::chrono::NaiveDate;
/// # use cloudpivot::business::hr::salary_model::ranges_overlap;
/// let d = |m: u32, day: u32| NaiveDate::from_ymd_opt(2026, m, day).expect("合法日期");
/// // 相邻（旧止 = 06-30，新起 = 07-01）→ 不重叠
/// assert!(!ranges_overlap(d(7, 1), None, d(1, 1), Some(d(6, 30))));
/// // 边界相等（旧止 = 新起 = 06-30）→ 重叠
/// assert!(ranges_overlap(d(6, 30), None, d(1, 1), Some(d(6, 30))));
/// ```
pub fn ranges_overlap(a_from: NaiveDate, a_to: Option<NaiveDate>, b_from: NaiveDate, b_to: Option<NaiveDate>) -> bool {
    let a_reaches_b = a_to.is_none_or(|to| to >= b_from);
    let b_reaches_a = b_to.is_none_or(|to| to >= a_from);
    a_reaches_b && b_reaches_a
}

/// 一组版本里第一条与候选区间相交、且不在 `ignore` 名单里的行（→ 报 409）。
///
/// `ignore` 是「本次意图要收口 / 要修正」的行：它们与候选相交是**设计使然**，
/// 由调用方先处理（收口到 `生效日 - 1` / 原地改金额），不算冲突。
pub fn first_conflict<'a>(
    candidate_from: NaiveDate,
    candidate_to: Option<NaiveDate>,
    versions: &'a [SalaryRow],
    ignore: &[Uuid],
) -> Option<&'a SalaryRow> {
    versions.iter().find(|row| {
        !ignore.contains(&row.id) && ranges_overlap(candidate_from, candidate_to, row.effective_from, row.effective_to)
    })
}

/// 写完之后的自检：同一薪资项的任何两个版本都不许相交（A3 的事务内守卫）。
///
/// 返回 `Some((左行, 右行))` = 交集还在（说明上面某一步漏了收口）。
/// 这是**最后一道闸**：宁可让整笔回滚并点出两行区间，也不许把 A3 的脏数据落库
/// （巡检脚本是事后告警，工资已经算完了）。
pub fn find_pair_overlap(versions: &[SalaryRow]) -> Option<(&SalaryRow, &SalaryRow)> {
    for (index, left) in versions.iter().enumerate() {
        for right in &versions[index + 1..] {
            if ranges_overlap(
                left.effective_from,
                left.effective_to,
                right.effective_from,
                right.effective_to,
            ) {
                return Some((left, right));
            }
        }
    }
    None
}

// ==================== 行 ====================

/// 薪资标准的一行（含薪资项名，读版本列表时一并 join 出来，DTO 层不再查库）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryRow {
    /// 主键。
    pub id: Uuid,
    /// 薪资项编码（弱引用 `biz_salary_item.item_code`）。
    pub item_code: String,
    /// 薪资项名（join 不到时为空，DTO 回退码值）。
    pub item_name: Option<String>,
    /// 月标准金额。
    pub amount: Decimal,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含），`None` = 至今。
    pub effective_to: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
}

impl SalaryRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            item_code: row.get_typed("item_code")?,
            item_name: clean(row.get_typed("item_name")?),
            amount: row.get_typed("amount")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            remark: row.get_typed("remark")?,
        })
    }

    /// 投影成 DTO 契约（HR-11 的 [`SalaryInput`]）：详情 / 薪资页 / 调薪回执共用。
    pub fn to_input(&self) -> SalaryInput {
        SalaryInput {
            item_code: self.item_code.clone(),
            item_name: self.item_name.clone(),
            amount: self.amount,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            remark: self.remark.clone(),
        }
    }

    /// 是否是「至今有效」的开放行（每个薪资项正常最多一条）。
    pub fn is_open(&self) -> bool {
        self.effective_to.is_none()
    }

    /// 该行是否覆盖给定业务日（闭区间）。
    pub fn covers(&self, day: NaiveDate) -> bool {
        self.effective_from <= day && self.effective_to.is_none_or(|to| to >= day)
    }
}

/// 可空文本参数：空 → 空串，SQL 里包 `NULLIF(?::text,'')`。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// 可空日期参数（SQL 里包 `NULLIF(?::text,'')::date`；`''::date` 是硬错误，必须走 NULLIF）。
fn opt_date(value: Option<NaiveDate>) -> Value {
    Value::Text(value.map(|date| date.to_string()).unwrap_or_default())
}

/// 可空 uuid 参数（SQL 里包 `NULLIF(?::text,'')::uuid`）。
fn opt_uuid(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 空串 → `None`（join 不到薪资项名时驱动给的是空串）。
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

// ==================== SQL ====================

/// 一个员工的全部标准（`effective_from DESC` = 台账口径，新的在前）。
pub const LIST_SQL: &str = "\
SELECT s.id, s.item_code, i.item_name, s.amount, s.effective_from, s.effective_to, s.remark \
  FROM biz_employee_salary s \
  LEFT JOIN biz_salary_item i ON i.item_code = s.item_code AND i.deleted_at IS NULL \
 WHERE s.employee_id = ?::uuid AND s.deleted_at IS NULL \
 ORDER BY s.effective_from DESC, s.item_code";

/// 一个薪资项的全部版本（**不加锁**，读路径用）。
pub const VERSIONS_SQL: &str = "\
SELECT s.id, s.item_code, i.item_name, s.amount, s.effective_from, s.effective_to, s.remark \
  FROM biz_employee_salary s \
  LEFT JOIN biz_salary_item i ON i.item_code = s.item_code AND i.deleted_at IS NULL \
 WHERE s.employee_id = ?::uuid AND s.item_code = ? AND s.deleted_at IS NULL \
 ORDER BY s.effective_from";

/// 一个薪资项的全部版本 + `FOR UPDATE OF s`（写路径用：两个并发调薪串行化）。
///
/// 锁**只加在主表 s 上**（`FOR UPDATE OF s`）：`biz_salary_item` 是配置表，跟着锁没有意义
/// 还会让两个不同员工的调薪互相等。
pub const VERSIONS_FOR_UPDATE_SQL: &str = "\
SELECT s.id, s.item_code, i.item_name, s.amount, s.effective_from, s.effective_to, s.remark \
  FROM biz_employee_salary s \
  LEFT JOIN biz_salary_item i ON i.item_code = s.item_code AND i.deleted_at IS NULL \
 WHERE s.employee_id = ?::uuid AND s.item_code = ? AND s.deleted_at IS NULL \
 ORDER BY s.effective_from \
   FOR UPDATE OF s";

/// 插一条版本（10 个绑定参数）。
pub const INSERT_SQL: &str = "\
INSERT INTO biz_employee_salary \
    (id, org_id, employee_id, item_code, amount, effective_from, effective_to, remark, create_by, update_by) \
 VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?::text::numeric, ?::text::date, NULLIF(?::text,'')::date, \
         NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)";

/// 收口**开放行**（调薪路径：只动 `effective_to IS NULL` 的那条）。
///
/// `effective_from < 新生效日` 是必须的：否则会把「比新生效日更晚开始」的行也收口成
/// 负区间（`ck_biz_emp_salary_range` 会拒，但报出来的是库约束不是人话，所以应用层先判冲突）。
pub const CLOSE_OPEN_SQL: &str = "\
UPDATE biz_employee_salary \
   SET effective_to = ?::text::date, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
 WHERE employee_id = ?::uuid AND item_code = ? AND deleted_at IS NULL \
   AND effective_from < ?::text::date AND effective_to IS NULL";

/// 收口**所有覆盖新生效日的行**（取消路径：历史行若手工留了更晚的 `effective_to`，一起收口）。
pub const CLOSE_COVERING_SQL: &str = "\
UPDATE biz_employee_salary \
   SET effective_to = ?::text::date, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
 WHERE employee_id = ?::uuid AND item_code = ? AND deleted_at IS NULL \
   AND effective_from < ?::text::date \
   AND (effective_to IS NULL OR effective_to >= ?::text::date)";

/// 修正同生效日的金额 / 备注（「同一天只有一行」由 `uk_biz_emp_salary` 保证）。
pub const UPDATE_AMOUNT_SQL: &str = "\
UPDATE biz_employee_salary \
   SET amount = ?::text::numeric, remark = NULLIF(?::text,''), updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
 WHERE id = ?::uuid AND deleted_at IS NULL";

/// 取消：软删「起点 ≥ 生效日」的版本（它们整段都在被取消的将来）。
pub const SOFT_DELETE_FROM_SQL: &str = "\
UPDATE biz_employee_salary \
   SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP, update_by = NULLIF(?::text,'')::uuid \
 WHERE employee_id = ?::uuid AND item_code = ? AND deleted_at IS NULL AND effective_from >= ?::text::date";

/// 薪资项存在性 + 名称（弱引用不建 FK，登记前必须确认这一项真的存在）。
///
/// `biz_salary_item` 的**写**接口归 HR-31，本文件只读（`SCOPE-EXEMPT`：配置表按主键取值校验）。
pub const ITEM_SQL: &str =
    "SELECT item_name FROM biz_salary_item WHERE item_code = ? AND is_active = 1 AND deleted_at IS NULL";

// ==================== 写操作 ====================

/// 一条待写入的版本（[`insert`] 的载荷：可写列 + 生效区间）。
///
/// 单独一个结构而不是「六个位置参数」：`amount` / `effective_from` / `effective_to`
/// 都是「看着像但其实不能混」的字段，位置参数调错一次就是一条金额对不上日期的标准。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVersion {
    /// 薪资项编码。
    pub item_code: String,
    /// 月标准金额。
    pub amount: Decimal,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含），`None` = 至今。
    pub effective_to: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
}

/// 插一条版本。
///
/// # 参数
///
/// - `id`：主键（调用方生成，便于同事务内继续读回）；
/// - `employee_id`：挂在谁身上；
/// - `version`：可写列（见 [`NewVersion`]）；
/// - `operator`：操作人（写 `create_by` / `update_by`）。
pub async fn insert(
    ex: &impl SqlExecutor,
    id: Uuid,
    employee_id: Uuid,
    version: &NewVersion,
    operator: Option<Uuid>,
) -> DbResult<()> {
    // SCOPE-EXEMPT: 主数据写入；org_id 由 SENTINEL_ORG_ID 显式给出（多组织落地时改一处）。
    ex.exec_sql(
        INSERT_SQL.to_string(),
        vec![
            Value::from(&id),
            Value::from(&SENTINEL_ORG_ID),
            Value::from(&employee_id),
            Value::Text(version.item_code.trim().to_string()),
            Value::Text(version.amount.to_string()),
            Value::Text(version.effective_from.to_string()),
            opt_date(version.effective_to),
            opt_text(version.remark.as_deref()),
            opt_uuid(operator),
            opt_uuid(operator),
        ],
    )
    .await?;
    Ok(())
}

/// 收口「`from` 之前开始、且仍覆盖 `from`」的版本到 `last_day`（= `from - 1 天`）。
///
/// `open_only = true`：只收口开放行（调薪路径，已收口行延伸过 `from` 属于冲突，应在调用前拦掉）；
/// `open_only = false`：连已收口但延伸过 `from` 的行一起收口（取消路径）。
///
/// 返回受影响行数（回执里告诉前端「收口了哪几条」）。
pub async fn close_before(
    ex: &impl SqlExecutor,
    employee_id: Uuid,
    item_code: &str,
    from: NaiveDate,
    last_day: NaiveDate,
    open_only: bool,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    let mut params = vec![
        Value::Text(last_day.to_string()),
        Value::from(Utc::now()),
        opt_uuid(operator),
        Value::from(&employee_id),
        Value::Text(item_code.trim().to_string()),
        Value::Text(from.to_string()),
    ];
    let sql = if open_only {
        CLOSE_OPEN_SQL
    } else {
        params.push(Value::Text(from.to_string()));
        CLOSE_COVERING_SQL
    };
    ex.exec_sql(sql.to_string(), params).await
}

/// 修正同生效日的金额 / 备注。返回受影响行数（`0` = 行已被删，调用方报 404）。
pub async fn update_amount(
    ex: &impl SqlExecutor,
    id: Uuid,
    amount: Decimal,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        UPDATE_AMOUNT_SQL.to_string(),
        vec![
            Value::Text(amount.to_string()),
            opt_text(remark),
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 取消「`from` 起」的版本：软删起点 ≥ `from` 的行。返回受影响行数。
pub async fn soft_delete_from(
    ex: &impl SqlExecutor,
    employee_id: Uuid,
    item_code: &str,
    from: NaiveDate,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        SOFT_DELETE_FROM_SQL.to_string(),
        vec![
            opt_uuid(operator),
            Value::from(&employee_id),
            Value::Text(item_code.trim().to_string()),
            Value::Text(from.to_string()),
        ],
    )
    .await
}

// ==================== 读操作 ====================

/// 一个薪资项的全部版本（读路径，不加锁）。
pub async fn find_versions(ex: &impl SqlExecutor, employee_id: Uuid, item_code: &str) -> DbResult<Vec<SalaryRow>> {
    let rows = ex
        .query_sql(
            VERSIONS_SQL.to_string(),
            vec![Value::from(&employee_id), Value::Text(item_code.trim().to_string())],
        )
        .await?;
    rows.iter().map(SalaryRow::from_row).collect()
}

/// 一个薪资项的全部版本 + 行锁（写路径；两个并发调薪串行，后到的看到前一笔的结果）。
pub async fn lock_versions(ex: &impl SqlExecutor, employee_id: Uuid, item_code: &str) -> DbResult<Vec<SalaryRow>> {
    let rows = ex
        .query_sql(
            VERSIONS_FOR_UPDATE_SQL.to_string(),
            vec![Value::from(&employee_id), Value::Text(item_code.trim().to_string())],
        )
        .await?;
    rows.iter().map(SalaryRow::from_row).collect()
}

/// 一个员工的全部标准（台账口径：`effective_from DESC`）。
///
/// 员工详情（HR-12 的 `load_salary_items`）直接吃这一份 —— 薪资标准 SQL 只有本文件一个出口。
pub async fn list_by_employee(ex: &impl SqlExecutor, employee_id: Uuid) -> DbResult<Vec<SalaryInput>> {
    let rows = ex
        .query_sql(LIST_SQL.to_string(), vec![Value::from(&employee_id)])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(SalaryRow::from_row(row)?.to_input());
    }
    Ok(out)
}

/// 薪资项名称（不存在 / 已停用 / 已删 → `None`）。登记标准前必须先过这一关。
pub async fn item_name_of(ex: &impl SqlExecutor, item_code: &str) -> DbResult<Option<String>> {
    let row = ex
        .query_first_sql(ITEM_SQL.to_string(), vec![Value::Text(item_code.trim().to_string())])
        .await?;
    match row {
        Some(row) => Ok(Some(row.get_typed::<String>("item_name")?)),
        None => Ok(None),
    }
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 建表迁移原文（编译期嵌入）：约束名 / 索引名改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn day(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).expect("合法日期")
    }

    fn row(from: NaiveDate, to: Option<NaiveDate>) -> SalaryRow {
        SalaryRow {
            id: Uuid::now_v7(),
            item_code: "BASE".to_string(),
            item_name: Some("基本工资".to_string()),
            amount: Decimal::new(560210, 2),
            effective_from: from,
            effective_to: to,
            remark: None,
        }
    }

    fn placeholders(sql: &str) -> usize {
        sql.matches('?').count()
    }

    // ---- 验收项：不重叠 / 重叠 / 相邻 / 边界相等 ----

    /// 相邻：旧区间止于 `06-30`，新区间起于 `07-01` → 不重叠（闭区间也无缝）。
    #[test]
    fn adjacent_ranges_do_not_overlap() {
        assert!(!ranges_overlap(day(7, 1), None, day(1, 1), Some(day(6, 30))));
        // 反向（顺序无关）
        assert!(!ranges_overlap(day(1, 1), Some(day(6, 30)), day(7, 1), None));
    }

    /// 重叠：新区间落在旧区间内部 → 重叠。
    #[test]
    fn nested_ranges_overlap() {
        assert!(ranges_overlap(day(4, 1), None, day(1, 1), Some(day(12, 31))));
        assert!(ranges_overlap(
            day(1, 1),
            Some(day(12, 31)),
            day(4, 1),
            Some(day(4, 30))
        ));
    }

    /// 不重叠：两段完全不搭界的区间（中间有空洞）都不相交。
    #[test]
    fn disjoint_ranges_do_not_overlap() {
        assert!(!ranges_overlap(
            day(7, 1),
            Some(day(9, 30)),
            day(1, 1),
            Some(day(6, 30))
        ));
        assert!(!ranges_overlap(
            day(1, 1),
            Some(day(3, 31)),
            day(5, 1),
            Some(day(9, 30))
        ));
        // 开放区间（至今）只要起点更晚，就必然与更早的「至今」区间相交 —— 这是 A3 的核心
        assert!(ranges_overlap(day(7, 1), None, day(1, 1), None));
    }

    /// 边界相等算重叠：旧止 == 新起（`06-30` / `06-30`）两端都含当天 → 必须拒。
    #[test]
    fn equal_boundaries_overlap() {
        assert!(ranges_overlap(day(6, 30), None, day(1, 1), Some(day(6, 30))));
        assert!(ranges_overlap(
            day(1, 1),
            Some(day(6, 30)),
            day(6, 30),
            Some(day(6, 30))
        ));
        // 同一天起（唯一键之前的第一道闸）
        assert!(ranges_overlap(day(3, 1), None, day(3, 1), None));
    }

    /// 忽略名单里的行（本次要收口 / 要修正的那条）不算冲突。
    #[test]
    fn ignored_rows_are_not_conflicts() {
        let open = row(day(1, 1), None);
        let versions = vec![open.clone()];
        assert!(
            first_conflict(day(4, 1), None, &versions, &[]).is_some(),
            "不收口就是冲突"
        );
        assert!(
            first_conflict(day(4, 1), None, &versions, &[open.id]).is_none(),
            "被点名忽略（要收口）的行不算冲突"
        );
    }

    /// 写后自检：成对相交必须能报出是哪两行。
    #[test]
    fn pair_overlap_detects_leftover_intersection() {
        let a = row(day(1, 1), None);
        let b = row(day(4, 1), Some(day(6, 30)));
        let versions = vec![a.clone(), b.clone()];
        let found = find_pair_overlap(&versions).expect("必须发现相交");
        assert_eq!((found.0.effective_from, found.1.effective_from), (day(1, 1), day(4, 1)));
        // 收口干净的两行 → 没有交集
        let closed = row(day(1, 1), Some(day(3, 31)));
        assert!(find_pair_overlap(&[closed, b]).is_none());
    }

    // ---- 约束名 / 文案 / 参数个数 ----

    /// 唯一索引名与两个 `CHECK` 必须与 `V0383` 逐字一致，且每个都有**人话**文案。
    #[test]
    fn constraint_names_match_v0383_ddl() {
        assert!(
            V0383_DDL.contains(&format!("CREATE UNIQUE INDEX IF NOT EXISTS {UK_SALARY_VERSION}")),
            "{UK_SALARY_VERSION} 不在 V0383 里 —— 索引被改名了？"
        );
        for name in SALARY_CHECKS {
            assert!(V0383_DDL.contains(name), "{name} 不在 V0383 里 —— 约束被改名了？");
            let message = check_message(Some(name));
            assert!(!message.contains("不满足库约束"), "{name} 没有专属文案");
        }
        assert!(check_message(None).contains("不满足库约束"), "未知约束走兜底");
    }

    /// 重叠报文必须能定位：点名薪资项 + 两条区间 + 下一步。
    #[test]
    fn overlap_message_is_locatable() {
        let error = overlap_error(
            "BASE",
            Some("基本工资"),
            (day(4, 1), None),
            (day(1, 1), Some(day(12, 31))),
        );
        assert_eq!(error.code, error_code::HR_SALARY_RANGE_OVERLAP);
        assert_eq!(error.status, 409);
        let message = error.msg;
        assert!(message.contains("基本工资") && message.contains("BASE"));
        assert!(message.contains("2026-04-01 ~ 至今"), "本次区间：{message}");
        assert!(message.contains("2026-01-01 ~ 2026-12-31"), "冲突区间：{message}");
        assert!(message.contains("边界相等也算重叠"), "要把口径写给用户：{message}");
        assert_eq!(range_text(day(1, 1), None), "2026-01-01 ~ 至今");
    }

    /// SQL 占位符与绑定参数必须一一对应（少一个只在第一次真插时报错）。
    #[test]
    fn placeholder_counts_match_bindings() {
        assert_eq!(placeholders(INSERT_SQL), 10);
        assert_eq!(placeholders(CLOSE_OPEN_SQL), 6);
        assert_eq!(placeholders(CLOSE_COVERING_SQL), 7);
        assert_eq!(placeholders(UPDATE_AMOUNT_SQL), 5);
        assert_eq!(placeholders(SOFT_DELETE_FROM_SQL), 4);
        assert_eq!(placeholders(VERSIONS_SQL), 2);
        assert_eq!(placeholders(VERSIONS_FOR_UPDATE_SQL), 2);
        assert_eq!(placeholders(LIST_SQL), 1);
        assert_eq!(placeholders(ITEM_SQL), 1);
    }

    /// 写路径必须锁、读路径不许锁（否则列表页会把整员工的版本行锁到事务结束）。
    #[test]
    fn only_write_paths_take_row_locks() {
        assert!(VERSIONS_FOR_UPDATE_SQL.contains("FOR UPDATE OF s"), "写路径要锁");
        assert!(!VERSIONS_SQL.contains("FOR UPDATE"), "读路径不许锁");
        assert!(!LIST_SQL.contains("FOR UPDATE"), "读路径不许锁");
        // 收口只能收「起点更早」的行：否则会把更晚生效的版本收成负区间
        assert!(CLOSE_OPEN_SQL.contains("effective_from < ?::text::date"));
        assert!(CLOSE_COVERING_SQL.contains("effective_from < ?::text::date"));
    }

    /// 三条读路径的列投影必须一致（改了列三处一起改，不允许「详情有、回执没有」）。
    #[test]
    fn every_projection_carries_the_same_columns() {
        for sql in [LIST_SQL, VERSIONS_SQL, VERSIONS_FOR_UPDATE_SQL] {
            for column in [
                "s.item_code",
                "i.item_name",
                "s.amount",
                "s.effective_from",
                "s.effective_to",
                "s.remark",
            ] {
                assert!(sql.contains(column), "投影缺列 {column}：{sql}");
            }
            assert!(sql.contains("deleted_at IS NULL"), "必须排除软删行");
            assert!(sql.contains("LEFT JOIN biz_salary_item"), "薪资项名要 join（不许 N+1）");
        }
    }

    /// 行 → DTO 投影：区间判定按闭区间；`item_name` 原样透传（空白归 `None` 由 DTO 层做）。
    #[test]
    fn row_projects_and_covers_by_closed_interval() {
        let item = row(day(1, 1), Some(day(6, 30)));
        assert!(item.covers(day(1, 1)), "生效起当天算生效");
        assert!(item.covers(day(6, 30)), "生效止当天算生效");
        assert!(!item.covers(day(7, 1)));
        assert!(item.covers(day(2, 1)), "2 月在区间内");
        assert!(!item.is_open());
        assert!(row(day(1, 1), None).is_open());
    }
}

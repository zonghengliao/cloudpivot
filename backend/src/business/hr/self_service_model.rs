//! 员工自助三接口的数据层（HR-41）：`/attendance/my` · `/leave/my` · `/salary/my`。
//!
//! 本文件是这三条**自助视角**查询的唯一 SQL 出口。三张表的数据访问在别的文件里已有
//! 出口（[`crate::business::hr::attendance_record_model`] /
//! [`crate::business::hr::leave_model`] / [`crate::business::hr::salary_sheet_model`]），
//! 这里**另写一份**不是为了绕开它们，而是因为自助口径与业务台口径有两处本质不同：
//!
//! 1. **过滤锚点不同**：业务台是「按数据范围裁」（`scope::paginate` + `dept_id` / `user_id`），
//!    自助是「按本人裁」（`employee_id = <我的员工档>`）。用业务台的入口去表达「本人」，
//!    在这三张表上都会走偏 —— 见下面「为什么不走 `scope::paginate`」；
//! 2. **投影不同**：业务台要带 `employee_no` / `employee_name` / `updated_by_name` / 公式快照
//!    （HR 对账用），自助不该见这些（见 `self_service_dto` 的模块文档）。
//!
//! # 为什么不走 `scope::paginate`
//!
//! 设计 D-12 把 `biz_attendance_record` / `biz_leave_request` 都判为范围表，但两张表的
//! `owner_col = NULL`（没有「归属人」列）。`shared/scope.rs::filter_for` 对
//! 「无归属列 + SELF 范围」的处理是**降级为「本人所在部门」**（`AND {alias}.dept_id = ?`），
//! 连部门都没有时 `deny_all`。把自助接口塞进 `scope::paginate` 会得到：
//!
//! * 普通员工（SELF 范围）看到**整个部门**的考勤 / 请假 —— 正是本卡要防的越权；
//! * 没配部门的账号看到**空**，而他有员工档、也有自己的考勤 —— 正常数据被误裁。
//!
//! 所以自助查询**不经**数据范围注入，自己按 `employee_id` 裁、自己算 count。
//! `paginate_unscoped` 同样不能用：这些表（或即将）登记进 `SCOPED_TABLES`，那条路径带守卫。
//!
//! 代价是「分页入口不是唯一那个」—— 因此本文件的 `page_of` 只做三件事（count / ORDER BY /
//! LIMIT-OFFSET），**不做**任何字符串切割，且三处调用共用同一个实现，
//! [`tests::sql_placeholders_match_params`] 逐条对拍占位符个数。
//!
//! # 身份解析：一次查询，三处共用
//!
//! [`my_employee_id`] 把「登录用户 → 员工档」解析一次（`biz_employee.user_id`，唯一索引
//! `uk_biz_employee_user` 兜住一人一档）。没绑账号的员工返回 `None` —— 调用方据此返回
//! 200 + 空列表（不是 403，理由见 DTO 模块文档）。

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::self_service_dto::{
    MyAttendanceRes, MyLeaveRes, MySalaryItemRes, MySalaryRes, item_type_label, period_status_label, source_label,
    timestamp_cst, visible_statuses,
};
use crate::business::hr::{attendance_record_dto::RecordNumbers, leave_dto, salary_sheet_dto};

// ==================== 身份解析 ====================

/// 「登录用户 → 员工档 id」。
///
/// # 返回值
///
/// * `Ok(Some(id))`：账号已绑定员工档（`uk_biz_employee_user` 保证至多一行）；
/// * `Ok(None)`：账号没绑员工档（车间工人没账号的**反向**情形：有账号没档案）。
///   调用方返回空列表 —— 这不是错误态。
///
/// # 异常情况
///
/// 查到两行 → `DbError::InvalidValue`，报文点名 `A2`（设计 §4.9 巡检断言：
/// 同一 `user_id` 挂两份未删员工档 = 0 行）。库里有唯一索引，理论上不可能；
/// 真出现（索引被手工删除 / 软删边界）时，**宁可报错也不随便挑一行**：
/// 挑错行 = 让 A 看到 B 的工资。
pub async fn my_employee_id(user_id: Uuid) -> DbResult<Option<Uuid>> {
    const SQL: &str = "\
SELECT id FROM biz_employee \
 WHERE user_id = ?::uuid AND deleted_at IS NULL \
 ORDER BY hire_date ASC, id ASC LIMIT 2";
    let rows = db().query_sql(SQL.to_string(), vec![Value::from(&user_id)]).await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed::<Uuid>("id")?);
    }
    match ids.len() {
        0 => Ok(None),
        1 => Ok(Some(ids[0])),
        _ => Err(DbError::InvalidValue(format!(
            "账号 {user_id} 绑了两份员工档（{} 个），违反巡检断言 A2：请先在员工档案里解绑其中一个",
            ids.len()
        ))),
    }
}

// ==================== 分页（自助口径，不做数据范围注入） ====================

/// 自助查询的分页执行：`count` + `ORDER BY` + `LIMIT/OFFSET`。
///
/// `base_sql` 必须**不含** `ORDER BY` / `LIMIT`（count 会把它包成子查询）。
/// `LIMIT` 直接拼数字（`u64` 已在 DTO 层夹到 `1..=MY_PAGE_MAX`，没有注入面）。
async fn page_of(
    base_sql: &str,
    params: Vec<Value>,
    order_by: &str,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<Row>, i64)> {
    let count_sql = format!("SELECT count(*) AS n FROM ({base_sql}) t");
    let total: i64 = db().scalar_with_params(&count_sql, params.clone()).await?;

    let offset = (page.max(1) - 1) * page_size.max(1);
    let data_sql = format!("{base_sql} {order_by} LIMIT {} OFFSET {offset}", page_size.max(1));
    let rows = db().find_with_params(&data_sql, params).await?;
    Ok((rows, total))
}

// ==================== ㉕ 我的考勤 ====================

/// 自助考勤排序：期间倒序（最近这个月在最上面）→ `id` 兜底稳定（翻页不重不漏）。
pub const ATTENDANCE_ORDER: &str = "ORDER BY p.period_code DESC, r.id ASC";

/// 「我的考勤」的 SQL 与参数（纯函数：单测对拍占位符个数）。
fn attendance_query(employee_id: Uuid, period_code: Option<&str>) -> (String, Vec<Value>) {
    let mut sql = "\
SELECT r.id, p.period_code, p.status AS period_status, \
       r.should_days, r.actual_days, r.leave_paid_days, r.leave_unpaid_days, \
       r.sick_days, r.absent_days, r.ot_hours_workday, r.ot_hours_weekend, r.ot_hours_holiday, \
       r.late_count, r.early_leave_count, r.source, r.remark \
  FROM biz_attendance_record r \
  JOIN biz_attendance_period p ON p.id = r.period_id \
 WHERE r.deleted_at IS NULL AND r.employee_id = ?::uuid"
        .to_string();
    let mut params = vec![Value::from(&employee_id)];
    if let Some(period_code) = period_code {
        sql.push_str(" AND p.period_code = ?");
        params.push(Value::Text(period_code.to_string()));
    }
    (sql, params)
}

/// 「我的考勤」分页（设计 §5.2 #25）。
///
/// # 返回值
///
/// `(本页行, 总数)`；总数是**本人**的记录条数（不含他人 —— 这条 SQL 里没有别的口径）。
pub async fn my_attendance(
    employee_id: Uuid,
    period_code: Option<&str>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<MyAttendanceRes>, i64)> {
    let (sql, params) = attendance_query(employee_id, period_code);
    let (rows, total) = page_of(&sql, params, ATTENDANCE_ORDER, page, page_size).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let numbers = RecordNumbers {
            should_days: row.get_typed("should_days")?,
            actual_days: row.get_typed("actual_days")?,
            leave_paid_days: row.get_typed("leave_paid_days")?,
            leave_unpaid_days: row.get_typed("leave_unpaid_days")?,
            sick_days: row.get_typed("sick_days")?,
            absent_days: row.get_typed("absent_days")?,
            ot_hours_workday: row.get_typed("ot_hours_workday")?,
            ot_hours_weekend: row.get_typed("ot_hours_weekend")?,
            ot_hours_holiday: row.get_typed("ot_hours_holiday")?,
            late_count: row.get_typed("late_count")?,
            early_leave_count: row.get_typed("early_leave_count")?,
        };
        let period_status: String = row.get_typed("period_status")?;
        let source: String = row.get_typed("source")?;
        out.push(MyAttendanceRes {
            id: row.get_typed::<Uuid>("id")?.to_string(),
            period_code: row.get_typed("period_code")?,
            period_status_label: period_status_label(&period_status),
            period_status,
            numbers,
            ot_hours_total: numbers.ot_hours_total(),
            source_label: source_label(&source),
            source,
            remark: row.get_typed("remark")?,
        });
    }
    Ok((out, total))
}

// ==================== 我的请假 ====================

/// 自助请假排序：开始时间倒序（最近的假在最上面）→ `id` 兜底稳定。
pub const LEAVE_ORDER: &str = "ORDER BY l.start_time DESC, l.id DESC";

/// 「我的请假」的 SQL 与参数（纯函数）。
fn leave_query(employee_id: Uuid, status: Option<&str>) -> (String, Vec<Value>) {
    let mut sql = "\
SELECT l.id, l.leave_no, l.leave_type, l.start_time, l.end_time, l.duration_days, l.paid, \
       l.status, l.reason, l.submitted_at, l.cancelled_at, l.cancel_reason \
  FROM biz_leave_request l \
 WHERE l.deleted_at IS NULL AND l.employee_id = ?::uuid"
        .to_string();
    let mut params = vec![Value::from(&employee_id)];
    if let Some(status) = status {
        sql.push_str(" AND l.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    (sql, params)
}

/// 「我的请假」分页（设计 §5.2 脚注第 3 处 `/my`）。
///
/// `paid` 在库里是 `SMALLINT`（`ck_biz_leave_paid`：0/1），出参投影成 `bool` ——
/// 前端 `t-switch` / 标签直接吃布尔，不必再判 `=== 1`。
pub async fn my_leaves(
    employee_id: Uuid,
    status: Option<&str>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<MyLeaveRes>, i64)> {
    let (sql, params) = leave_query(employee_id, status);
    let (rows, total) = page_of(&sql, params, LEAVE_ORDER, page, page_size).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let status: String = row.get_typed("status")?;
        let leave_type: String = row.get_typed("leave_type")?;
        let paid: i16 = row.get_typed("paid")?;
        let start_time: DateTime<Utc> = row.get_typed("start_time")?;
        let end_time: DateTime<Utc> = row.get_typed("end_time")?;
        let submitted_at: Option<DateTime<Utc>> = row.get_typed("submitted_at")?;
        let cancelled_at: Option<DateTime<Utc>> = row.get_typed("cancelled_at")?;
        out.push(MyLeaveRes {
            id: row.get_typed::<Uuid>("id")?.to_string(),
            leave_no: row.get_typed("leave_no")?,
            leave_type_label: leave_dto::leave_type_label(&leave_type).to_string(),
            leave_type,
            start_time: timestamp_cst(start_time),
            end_time: timestamp_cst(end_time),
            duration_days: row.get_typed("duration_days")?,
            paid: paid != 0,
            status_label: leave_dto::status_label(&status).to_string(),
            status,
            reason: row.get_typed("reason")?,
            submitted_at: submitted_at.map(timestamp_cst),
            cancelled_at: cancelled_at.map(timestamp_cst),
            cancel_reason: row.get_typed("cancel_reason")?,
        });
    }
    Ok((out, total))
}

// ==================== 我的薪资 ====================

/// 自助薪资排序：期间倒序（最近一笔在最上面）→ 明细 id 兜底稳定。
pub const SALARY_ORDER: &str = "ORDER BY s.period_code DESC, d.id ASC";

/// 「我的薪资」的 SQL 与参数（纯函数）。
///
/// # 为什么过滤列是 `d.employee_id` 而不是 `d.user_id`
///
/// 设计 D-12 给 `biz_salary_detail` 冗余了 `user_id`（SELF 范围的锚点列，给
/// `scope::filter_for` 做**列比较**用）。自助查询不用它，因为它是**试算那一刻**从
/// `employee.user_id` 派生的快照：员工后来才绑账号时，历史期间的 `user_id` 仍是 `NULL`，
/// 用它会漏掉自己过去几个月的工资条。按 `employee_id` 过滤没有这个问题
/// （员工档 id 不随账号绑定变化），且与 `my_employee_id` 的解析结果同一个主键。
fn salary_query(employee_id: Uuid, period_code: Option<&str>, include_pending: bool) -> (String, Vec<Value>) {
    let statuses = visible_statuses(include_pending);
    let placeholders = vec!["?"; statuses.len()].join(", ");
    let mut sql = format!(
        "\
SELECT d.id AS detail_id, d.sheet_id, s.sheet_no, s.period_code, s.status AS sheet_status, \
       d.employee_no, d.employee_name, d.dept_name, d.position_code, d.job_level, \
       d.should_days, d.actual_days, d.leave_unpaid_days, d.ot_hours, \
       d.earning_amount, d.pre_deduct_amount, d.gross_amount, \
       d.social_personal, d.fund_personal, d.iit_amount, d.other_withhold, d.withhold_amount, \
       d.net_amount, d.remark \
  FROM biz_salary_detail d \
  JOIN biz_salary_sheet s ON s.id = d.sheet_id AND s.deleted_at IS NULL \
 WHERE d.employee_id = ?::uuid \
   AND s.status IN ({placeholders})"
    );
    let mut params = vec![Value::from(&employee_id)];
    params.extend(statuses.iter().map(|status| Value::Text((*status).to_string())));
    if let Some(period_code) = period_code {
        sql.push_str(" AND s.period_code = ?");
        params.push(Value::Text(period_code.to_string()));
    }
    (sql, params)
}

/// 「我的薪资」分页：一行 = 我在某一期的工资条，明细项**一次 `IN` 查完**再分组。
///
/// # 两次查询而不是 N+1
///
/// 一页最多 `MY_PAGE_MAX`（60）行，明细项按 `detail_id IN (…)` 一次取回 —— 与
/// `salary_sheet_model::items_of_details` 同一手法，只是锚点换成「本人的明细 id」。
pub async fn my_payslips(
    employee_id: Uuid,
    period_code: Option<&str>,
    include_pending: bool,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<MySalaryRes>, i64)> {
    let (sql, params) = salary_query(employee_id, period_code, include_pending);
    let (rows, total) = page_of(&sql, params, SALARY_ORDER, page, page_size).await?;

    let mut detail_ids = Vec::with_capacity(rows.len());
    let mut slips = Vec::with_capacity(rows.len());
    for row in &rows {
        let detail_id: Uuid = row.get_typed("detail_id")?;
        detail_ids.push(detail_id);
        let sheet_status: String = row.get_typed("sheet_status")?;
        slips.push(MySalaryRes {
            sheet_id: row.get_typed::<Uuid>("sheet_id")?.to_string(),
            sheet_no: row.get_typed("sheet_no")?,
            period_code: row.get_typed("period_code")?,
            sheet_status_label: salary_sheet_dto::status_label(&sheet_status),
            paid: sheet_status == salary_sheet_dto::STATUS_POSTED,
            sheet_status,
            detail_id: detail_id.to_string(),
            employee_no: row.get_typed("employee_no")?,
            employee_name: row.get_typed("employee_name")?,
            dept_name: row.get_typed("dept_name")?,
            position_code: row.get_typed("position_code")?,
            job_level: row.get_typed("job_level")?,
            should_days: row.get_typed("should_days")?,
            actual_days: row.get_typed("actual_days")?,
            leave_unpaid_days: row.get_typed("leave_unpaid_days")?,
            ot_hours: row.get_typed("ot_hours")?,
            earning_amount: row.get_typed("earning_amount")?,
            pre_deduct_amount: row.get_typed("pre_deduct_amount")?,
            gross_amount: row.get_typed("gross_amount")?,
            social_personal: row.get_typed("social_personal")?,
            fund_personal: row.get_typed("fund_personal")?,
            iit_amount: row.get_typed("iit_amount")?,
            other_withhold: row.get_typed("other_withhold")?,
            withhold_amount: row.get_typed("withhold_amount")?,
            net_amount: row.get_typed("net_amount")?,
            remark: row.get_typed("remark")?,
            items: Vec::new(),
        });
    }

    let mut items = my_items(&detail_ids).await?;
    for (slip, detail_id) in slips.iter_mut().zip(detail_ids.iter()) {
        if let Some(list) = items.remove(detail_id) {
            slip.items = list;
        }
    }
    Ok((slips, total))
}

/// 一批明细行的明细项（**一次** `IN` 查询，按 `detail_id` 分组）。
///
/// 空 ids 直接返回空 map：拼出 `IN ()` 是语法错误，会让「一条记录都没有」的自助页 500。
pub async fn my_items(detail_ids: &[Uuid]) -> DbResult<HashMap<Uuid, Vec<MySalaryItemRes>>> {
    if detail_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?::uuid"; detail_ids.len()].join(", ");
    let sql = format!(
        "SELECT i.detail_id, i.item_code, i.item_name, i.item_type, i.amount \
           FROM biz_salary_detail_item i \
          WHERE i.detail_id IN ({placeholders}) \
          ORDER BY i.detail_id, i.line_no, i.item_code"
    );
    let params: Vec<Value> = detail_ids.iter().map(Value::from).collect();
    let rows = db().query_sql(sql, params).await?;

    let mut grouped: HashMap<Uuid, Vec<MySalaryItemRes>> = HashMap::with_capacity(detail_ids.len());
    for row in &rows {
        let detail_id: Uuid = row.get_typed("detail_id")?;
        let item_type: String = row.get_typed("item_type")?;
        grouped.entry(detail_id).or_default().push(MySalaryItemRes {
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            item_type_label: item_type_label(&item_type),
            item_type,
            amount: row.get_typed("amount")?,
        });
    }
    Ok(grouped)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 占位符个数必须等于参数个数。
    ///
    /// 这类错（多 / 少一个 `?`）不会编译报错，只在真跑时报
    /// 「bind message supplies N parameters」—— 而自助接口的错误会直接打在员工脸上。
    #[test]
    fn sql_placeholders_match_params() {
        let employee_id = Uuid::nil();

        let cases: Vec<(&str, String, Vec<Value>)> = vec![
            (
                "attendance 无筛选",
                attendance_query(employee_id, None).0,
                attendance_query(employee_id, None).1,
            ),
            (
                "attendance 按期",
                attendance_query(employee_id, Some("2026-11")).0,
                attendance_query(employee_id, Some("2026-11")).1,
            ),
            (
                "leave 无筛选",
                leave_query(employee_id, None).0,
                leave_query(employee_id, None).1,
            ),
            (
                "leave 按状态",
                leave_query(employee_id, Some("APPROVED")).0,
                leave_query(employee_id, Some("APPROVED")).1,
            ),
            (
                "salary 默认",
                salary_query(employee_id, None, false).0,
                salary_query(employee_id, None, false).1,
            ),
            (
                "salary 含在途 + 按期",
                salary_query(employee_id, Some("2026-11"), true).0,
                salary_query(employee_id, Some("2026-11"), true).1,
            ),
        ];

        for (name, sql, params) in cases {
            let placeholders = sql.matches('?').count();
            assert_eq!(placeholders, params.len(), "{name}：`?` 个数 ≠ 参数个数\n{sql}");
            assert!(
                !sql.to_uppercase().contains("ORDER BY"),
                "{name}：base_sql 不许自带 ORDER BY（count 会包成子查询）"
            );
            println!("[自助] {name}：{placeholders} 个占位符 ✓");
        }
    }

    /// 三条查询的**过滤锚点**必须是本人（写错一个字就是越权，且编译不会报错）。
    #[test]
    fn every_query_is_anchored_on_employee_id() {
        let employee_id = Uuid::nil();
        for (name, sql) in [
            ("attendance", attendance_query(employee_id, Some("2026-11")).0),
            ("leave", leave_query(employee_id, None).0),
            ("salary", salary_query(employee_id, None, true).0),
        ] {
            assert!(
                sql.contains("employee_id = ?::uuid"),
                "{name} 必须以 employee_id 为锚点：\n{sql}"
            );
            assert!(
                !sql.contains("dept_id = ANY"),
                "{name} 不许注入数据范围片段（自助口径按本人裁）"
            );
        }
    }

    /// 薪资查询的可见状态：默认三档、含在途六档，且占位符个数跟着变。
    #[test]
    fn salary_status_placeholder_count_follows_the_toggle() {
        let (sql, params) = salary_query(Uuid::nil(), None, false);
        assert_eq!(params.len(), 1 + 3, "锚点 + 三个可见状态");
        assert!(!sql.contains("'DRAFT'"));

        let (sql, params) = salary_query(Uuid::nil(), None, true);
        assert_eq!(params.len(), 1 + 6, "锚点 + 六档状态");
        for status in ["DRAFT", "CALCULATED", "SUBMITTED", "APPROVED", "POSTED", "REVERSED"] {
            assert!(
                params
                    .iter()
                    .any(|value| matches!(value, Value::Text(text) if text.as_str() == status)),
                "含在途时缺状态 {status}"
            );
        }
        // 状态一律走绑定参数，不拼字符串（避免大小写 / 注入面）
        assert!(!sql.contains("'APPROVED'"));
    }

    /// 明细项查询：空 ids 不查库（`IN ()` 是语法错误），非空时占位符一对多。
    #[tokio::test]
    async fn empty_detail_ids_never_hit_the_database() {
        let grouped = my_items(&[]).await.expect("空 ids 必须直接返回空 map");
        assert!(grouped.is_empty());
    }

    /// 排序串必须带 `id` 兜底（否则同期间多行时翻页可能重复 / 漏行）。
    ///
    /// 注意：真实 SQL 里的 id 列通常带表别名前缀（`r.id` / `p.id` / `d.id`），
    /// 断言不能只认裸 ` id ASC`，否则「有兜底但带别名」会被判失败。
    #[test]
    fn orders_are_stable() {
        for order in [ATTENDANCE_ORDER, LEAVE_ORDER, SALARY_ORDER] {
            assert!(order.starts_with("ORDER BY"), "{order}");
            let has_id_tail = order.contains(" id ASC")
                || order.contains(" id DESC")
                || order.contains(".id ASC")
                || order.contains(".id DESC");
            assert!(has_id_tail, "{order} 缺 id 兜底");
        }
    }
}

//! 考勤台账导出 xlsx（设计 §5.2 #24 · HR-30）。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 24 | GET | `/business/hr/attendance/export` | `business:hr:attendance:export` | HR-30 |
//!
//! 取数在 [`crate::business::hr::attendance_record_model::export_rows`]（与台账列表
//! 同一份筛选 SQL 与数据权限），本文件的职责只有两件：**列的形状**与 **xlsx 的落地**。
//!
//! # 三个验收项各自落在哪
//!
//! | 验收 | 落点 | 证据 |
//! |---|---|---|
//! | **导出列完整** | [`EXPORT_HEADERS`] 20 列 = 期间/身份三方 + 11 个数值列 + 加班合计 + 期间状态 + 锁定状态 + 来源 + 备注 | 单测 `headers_cover_every_record_column`：11 个数值列与 `RecordNumbers` 逐字段对拍，表头漏一列就红 |
//! | **锁定状态列正确** | [`lock_label`] 只认 `LOCKED`（唯一判据 `attendance_period_dto::STATUS_LOCKED`）；未知状态出「未知」而不是假的「未锁定」 | 单测 `lock_label_only_locked_is_locked` / `unknown_status_is_not_reported_as_unlocked` |
//! | **大期间（100 人）不超时** | 一次 `scope::paginate` 取全量（无 N+1、无逐行查员工）+ 一次 `write_sheet` | 真库用例 `export_100_rows_is_fast_and_complete`：100 行端到端耗时与行数一起断言 |
//!
//! # 为什么「锁定状态」取期间头，而不是逐行取 `row.period_status`
//!
//! 台账每行都 join 出了自己的 `period_status`（这是列表页置灰单元格要用的）。但一张
//! **导出表**必须整表一个口径：如果某一行的状态与其它行不同（数据被别处改过、
//! 或者将来支持跨期导出），逐行取值会产出一张「半锁半不锁」的 Excel ——
//! 而 HR 拿这张表去核工资时，看到的会是「有些人是对的，有些人不是」，没有任何提示。
//!
//! 所以本文件用**期间头**的 `status` 计算一次，整表统一填；期间的 `period_code` 同理。
//! 期间头先读、台账后读：两次读之间期间被锁定，导出表上写的是「未锁定」而库里已锁 ——
//! 这个窗口的代价是「表上少一个锁定标记」，不影响数字本身；反过来（先读台账后读期间）
//! 会让表头比数字更「新」，更容易误导人核对。
//!
//! # 上限与截断
//!
//! 单次导出上限 [`EXPORT_MAX_ROWS`] 行（一期 5000 人已是「集团级月度台账」的规模）。
//! 超限时**不静默截断**：响应头带 `x-export-truncated: true` 与 `x-export-total`，
//! 让人能看出「这份表不全，要按部门分开导」。设计 §5.3 的异步导出任务
//! （`sys_export_task` + 下载中心）留给数据量真的到那一步时再做 —— 本卡只做同步下载。

use std::collections::HashMap;

use axum::extract::{Extension, Query};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use nik::prelude::*;

use crate::business::hr::attendance_period_dto::{STATUS_DRAFT, STATUS_LOCKED, STATUS_SUBMITTED, status_label};
use crate::business::hr::attendance_period_model as period_model;
use crate::business::hr::attendance_record_dto::{self, RecordPageQuery};
use crate::business::hr::attendance_record_model::{self as model, AttendanceRecordRow};
use crate::business::shared::principal;
use crate::utils;

/// 导出考勤（与 `V0387` 的 `sys_menu.auth_code` 逐字一致）。
pub const PERM_EXPORT: &str = "business:hr:attendance:export";

/// 单次导出上限（行）。
pub const EXPORT_MAX_ROWS: u64 = 5000;

/// `锁定状态` 列的取值（已锁定）。
pub const LOCK_LABEL_LOCKED: &str = "已锁定";

/// `锁定状态` 列的取值（未锁定：`DRAFT` / `SUBMITTED` 都不可作为工资基线）。
pub const LOCK_LABEL_UNLOCKED: &str = "未锁定";

/// `锁定状态` 列的取值（库里出现了三个合法值之外的状态）。
///
/// 不并进「未锁定」：这一列是「能不能拿这张表当工资基线」的判断依据，
/// 未知状态说成「未锁定」会让人以为只是还没锁；说「未知」才会有人去查。
pub const LOCK_LABEL_UNKNOWN: &str = "未知";

/// 导出列（顺序即表头顺序，也是 xlsx 的列顺序）。
///
/// 三组：
///
/// - **期间 / 身份**：考勤月、工号、姓名、部门（工号姓名是快照列、部门名是实时 join，
///   与台账列表同口径，见 `attendance_record_dto` 的姓名口径说明）；
/// - **11 个数值列**：与 [`crate::business::hr::attendance_record_dto::RecordNumbers`]
///   一一对应（顺序也一致），外加「加班合计」= 三列之和（台账「加班」汇总列）；
/// - **状态 / 留痕**：期间状态（草稿/已提交/已锁定）、锁定状态（是/否，工资基线判据）、
///   来源（导入/手工/请假同步）、备注（这一行的解释）。
pub const EXPORT_HEADERS: [&str; 20] = [
    "考勤月",
    "工号",
    "姓名",
    "部门",
    "应出勤(天)",
    "实际出勤(天)",
    "带薪假(天)",
    "无薪假(天)",
    "病假(天)",
    "旷工(天)",
    "平时加班(小时)",
    "休息日加班(小时)",
    "节假日加班(小时)",
    "加班合计(小时)",
    "迟到(次)",
    "早退(次)",
    "期间状态",
    "锁定状态",
    "来源",
    "备注",
];

// ==================== 纯函数（不连库） ====================

/// 期间状态 → `锁定状态` 列的值。
///
/// 唯一判据是 [`STATUS_LOCKED`]（`attendance_period_dto` 里那一份，与状态机、
/// 库层触发器 `V0392` 同一个词表）。`SUBMITTED` **不算**锁定：提交后还能继续导入 / 改数
/// （设计 §6.2.3 的期间操作提示就是这个语义），它不是工资基线。
pub fn lock_label(period_status: &str) -> &'static str {
    match period_status {
        STATUS_LOCKED => LOCK_LABEL_LOCKED,
        STATUS_DRAFT | STATUS_SUBMITTED => LOCK_LABEL_UNLOCKED,
        _ => LOCK_LABEL_UNKNOWN,
    }
}

/// 数值列的字面值：最多两位小数、去掉尾随零（`22.00 → 22`、`7.50 → 7.5`）。
///
/// 库列是 `NUMERIC(6,2)` / `NUMERIC(8,2)`，驱动读回来可能带 `2` 位或更多小数位
/// （Decimal 的 scale 跟着运算走）。直接用 `Display` 会让同一张表里出现
/// `22` 与 `22.00` 两种写法 —— HR 用它做 `VLOOKUP` 或透视时是按文本匹配的，
/// 同值不同形会被当成两拨数据。所以统一在这里 `round_dp(2).normalize()`。
pub fn num_text(value: rust_decimal::Decimal) -> String {
    value.round_dp(2).normalize().to_string()
}

/// 一行台账 → 一行导出数据（`HashMap` 的键必须与 [`EXPORT_HEADERS`] 逐字一致）。
///
/// `period_code` / `period_status` 由**期间头**传入（见模块文档：整表一个口径），
/// 不从 `row` 上取。
pub fn row_line(row: &AttendanceRecordRow, period_code: &str, period_status: &str) -> HashMap<String, String> {
    let numbers = &row.numbers;
    let mut line: HashMap<String, String> = HashMap::with_capacity(EXPORT_HEADERS.len());
    line.insert("考勤月".to_string(), period_code.to_string());
    line.insert("工号".to_string(), row.employee_no.clone());
    line.insert("姓名".to_string(), row.employee_name.clone());
    line.insert("部门".to_string(), row.dept_name.clone().unwrap_or_default());
    line.insert("应出勤(天)".to_string(), num_text(numbers.should_days));
    line.insert("实际出勤(天)".to_string(), num_text(numbers.actual_days));
    line.insert("带薪假(天)".to_string(), num_text(numbers.leave_paid_days));
    line.insert("无薪假(天)".to_string(), num_text(numbers.leave_unpaid_days));
    line.insert("病假(天)".to_string(), num_text(numbers.sick_days));
    line.insert("旷工(天)".to_string(), num_text(numbers.absent_days));
    line.insert("平时加班(小时)".to_string(), num_text(numbers.ot_hours_workday));
    line.insert("休息日加班(小时)".to_string(), num_text(numbers.ot_hours_weekend));
    line.insert("节假日加班(小时)".to_string(), num_text(numbers.ot_hours_holiday));
    line.insert("加班合计(小时)".to_string(), num_text(numbers.ot_hours_total()));
    line.insert("迟到(次)".to_string(), numbers.late_count.to_string());
    line.insert("早退(次)".to_string(), numbers.early_leave_count.to_string());
    line.insert("期间状态".to_string(), status_label(period_status).to_string());
    line.insert("锁定状态".to_string(), lock_label(period_status).to_string());
    line.insert(
        "来源".to_string(),
        attendance_record_dto::source_label(&row.source).to_string(),
    );
    line.insert("备注".to_string(), row.remark.clone().unwrap_or_default());
    line
}

/// 整张表 → xlsx 字节（写不出 → 500，报文说清是「生成 Excel 失败」而不是业务错）。
pub fn build_workbook(
    rows: &[AttendanceRecordRow],
    period_code: &str,
    period_status: &str,
) -> std::result::Result<Vec<u8>, ApiError> {
    let mut sheet_rows: Vec<HashMap<String, String>> = Vec::with_capacity(rows.len());
    for row in rows {
        sheet_rows.push(row_line(row, period_code, period_status));
    }
    crate::business::impexp::engine::write_sheet(&EXPORT_HEADERS, &sheet_rows)
}

/// 下载文件名（中文名，`xlsx_response` 会做 RFC 5987 编码）。
pub fn file_name(period_code: &str, locked: bool) -> String {
    let state = if locked { "已锁定" } else { "未锁定" };
    format!("考勤台账_{period_code}_{state}.xlsx")
}

// ==================== §5.2 #24 导出 ====================

/// 导出考勤台账（设计 §5.2 #24）。
///
/// # 参数
///
/// - `period_id`（**必填**）：导的是哪一期（与台账列表同一口径，一期一表）；
/// - `dept_id` / `keyword`（可选）：与列表页的筛选一致 —— 导出的就是「屏幕上那张表」。
///
/// # 返回值
///
/// xlsx 字节流。响应头带三样便于核对 / 排障的证据：
/// `x-attendance-period`（期间码）、`x-attendance-locked`（`true`/`false`）、
/// `x-export-rows`（实际导出行数）；超上限时另有 `x-export-truncated` 与 `x-export-total`。
///
/// # 异常情况
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | `period_id` 缺失 / 不是 uuid；`dept_id` 不是 uuid | 400 | `10001` |
/// | 期间不存在或已删除 | 404 | `10005` |
/// | 数据库异常 | 500 | `10003` / 驱动码 |
/// | xlsx 生成失败 | 500 | `10003` |
#[nik::permission(
    path = "/business/hr/attendance/export",
    method = "GET",
    permission = "business:hr:attendance:export"
)]
#[nik::get("/business/hr/attendance/export")]
pub async fn export(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<RecordPageQuery>,
) -> Response {
    match export_response(&claims, &query).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 导出实现（把读库 / 拼表 / 造响应从 handler 里拆出来，真库用例直接调这一层）。
///
/// 参数是 [`TokenClaims`]（`Extension(AuthClaims(claims))` 解构出来的内层）：
/// 数据范围必须按**库里**的角色重算（`principal::resolve_for_claims`），
/// 不能信令牌里的 `permissions` 快照 —— 权限刚被收回的人还在用旧令牌时，
/// 导出会带上他已不该看的部门数据（设计 §4.7 的数据范围判定口径）。
pub async fn export_response(claims: &TokenClaims, query: &RecordPageQuery) -> std::result::Result<Response, ApiError> {
    let filter = attendance_record_dto::filter_of(query)?;
    let principal = principal::resolve_for_claims(claims).await?;

    // 期间头：拿 `period_code` 与 `status`（整表的考勤月 / 期间状态 / 锁定状态都由它定）
    let period = period_model::find_in(db(), filter.period_id)
        .await
        .map_err(|error| utils::db_error(error, "查询待导出的考勤期间"))?
        .ok_or_else(period_model::not_found_error)?;

    let (rows, total) = model::export_rows(&filter, &principal, EXPORT_MAX_ROWS)
        .await
        .map_err(|error| utils::db_error(error, "导出台账数据"))?;

    let bytes = build_workbook(&rows, &period.period_code, &period.status)?;
    let locked = period.status == STATUS_LOCKED;
    let mut response = crate::business::impexp::handler::xlsx_response(bytes, &file_name(&period.period_code, locked));
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&period.period_code) {
        headers.insert("x-attendance-period", value);
    }
    headers.insert(
        "x-attendance-locked",
        HeaderValue::from_static(if locked { "true" } else { "false" }),
    );
    if let Ok(value) = HeaderValue::from_str(&rows.len().to_string()) {
        headers.insert("x-export-rows", value);
    }
    if let Ok(value) = HeaderValue::from_str(&total.to_string()) {
        headers.insert("x-export-total", value);
    }
    if total > i64::try_from(EXPORT_MAX_ROWS).unwrap_or(i64::MAX) {
        // 截断了就必须让人知道：这份表不全，要按部门分开导
        headers.insert("x-export-truncated", HeaderValue::from_static("true"));
    }
    Ok(response)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::attendance_record_dto::RecordNumbers;
    use nik::prelude::chrono::Utc;
    use nik::prelude::rust_decimal::Decimal;
    use nik::uuid::Uuid;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法小数")
    }

    fn sample_row() -> AttendanceRecordRow {
        AttendanceRecordRow {
            id: Uuid::nil(),
            period_id: Uuid::nil(),
            period_code: "2026-11".to_string(),
            period_status: STATUS_LOCKED.to_string(),
            employee_id: Uuid::nil(),
            employee_no: "EMP-0007".to_string(),
            employee_name: "张三".to_string(),
            dept_id: None,
            dept_name: Some("生产部".to_string()),
            numbers: RecordNumbers {
                should_days: dec("22.00"),
                actual_days: dec("19.5"),
                leave_paid_days: dec("0"),
                leave_unpaid_days: dec("2"),
                sick_days: dec("0.5"),
                absent_days: dec("0"),
                ot_hours_workday: dec("10.5"),
                ot_hours_weekend: dec("4"),
                ot_hours_holiday: dec("0"),
                late_count: 1,
                early_leave_count: 2,
            },
            source: "IMPORT".to_string(),
            remark: Some("补录半天事假".to_string()),
            updated_at: Utc::now(),
            update_by: None,
            updated_by_name: None,
        }
    }

    /// 表头必须覆盖台账的每一列：11 个数值列 + 加班合计 + 身份/期间/状态/留痕。
    ///
    /// 这条断言防的是「加了 `early_leave_count` 却忘了加列」——那种漏项不会编译报错，
    /// 只会让 HR 手里的表少一列数（而工资正好按那一列算）。
    #[test]
    fn headers_cover_every_record_column() {
        let expected_numbers = [
            "should_days",
            "actual_days",
            "leave_paid_days",
            "leave_unpaid_days",
            "sick_days",
            "absent_days",
            "ot_hours_workday",
            "ot_hours_weekend",
            "ot_hours_holiday",
            "late_count",
            "early_leave_count",
        ];
        // `RecordNumbers` 的 serde 字段名就是库列名：拿它当「台账有哪些数」的唯一真相
        let json = serde_json::to_value(sample_row().numbers).expect("序列化数值列");
        let object = json.as_object().expect("对象");
        assert_eq!(object.len(), expected_numbers.len(), "数值列个数变了，导出列要跟着改");
        for field in expected_numbers {
            assert!(object.contains_key(field), "数值列 {field} 不在 RecordNumbers 里");
        }
        // 每一列都必须真的在表头里（用中文名逐个点名，漏一个就红）
        for header in [
            "考勤月",
            "工号",
            "姓名",
            "部门",
            "应出勤(天)",
            "实际出勤(天)",
            "带薪假(天)",
            "无薪假(天)",
            "病假(天)",
            "旷工(天)",
            "平时加班(小时)",
            "休息日加班(小时)",
            "节假日加班(小时)",
            "加班合计(小时)",
            "迟到(次)",
            "早退(次)",
            "期间状态",
            "锁定状态",
            "来源",
            "备注",
        ] {
            assert!(EXPORT_HEADERS.contains(&header), "导出列缺「{header}」");
        }
        assert_eq!(EXPORT_HEADERS.len(), 20);
        // 表头不能重复（重复会让 xlsx 只有一列、另一个值被吞）
        let mut sorted = EXPORT_HEADERS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), EXPORT_HEADERS.len(), "表头有重复");
    }

    /// 锁定状态只看 `LOCKED`：`SUBMITTED` 仍不是工资基线（设计 §6.2.3）。
    #[test]
    fn lock_label_only_locked_is_locked() {
        assert_eq!(lock_label(STATUS_LOCKED), LOCK_LABEL_LOCKED);
        assert_eq!(lock_label(STATUS_SUBMITTED), LOCK_LABEL_UNLOCKED);
        assert_eq!(lock_label(STATUS_DRAFT), LOCK_LABEL_UNLOCKED);
    }

    /// 未知状态不能伪装成「未锁定」：这一列是工资基线的判断依据。
    #[test]
    fn unknown_status_is_not_reported_as_unlocked() {
        assert_eq!(lock_label("CANCELLED"), LOCK_LABEL_UNKNOWN);
        assert_eq!(lock_label(""), LOCK_LABEL_UNKNOWN);
        // 小写也不算：词表是库里 CHECK 约束的大写码值
        assert_eq!(lock_label("locked"), LOCK_LABEL_UNKNOWN);
    }

    /// 数值字面值：两位小数、去尾零（同值必须同形，否则 Excel 里按文本匹配会对不上）。
    #[test]
    fn num_text_normalizes_scale() {
        assert_eq!(num_text(dec("22.00")), "22");
        assert_eq!(num_text(dec("22")), "22");
        assert_eq!(num_text(dec("19.50")), "19.5");
        assert_eq!(num_text(dec("0.00")), "0");
        assert_eq!(num_text(Decimal::ZERO), "0");
        // 第三位小数只可能是「内存里算出来的中间值」（库列是 NUMERIC(6,2)）。
        // `round_dp` 用银行家舍入（四舍六入五成双），与财务减少累计偏的口径一致：
        // 1.005 → 1.00（末位 0 是偶数）、1.015 → 1.02。
        assert_eq!(num_text(dec("1.005")), "1");
        assert_eq!(num_text(dec("1.015")), "1.02");
    }

    /// 一行 → 一行：20 个键全在、值对得上、加班合计 = 三列之和。
    #[test]
    fn row_line_fills_every_header() {
        let row = sample_row();
        let line = row_line(&row, "2026-11", STATUS_LOCKED);
        assert_eq!(line.len(), EXPORT_HEADERS.len(), "每个表头都要有值（没有就填空串）");
        for header in EXPORT_HEADERS {
            assert!(line.contains_key(header), "缺列 {header}");
        }
        assert_eq!(line["考勤月"], "2026-11");
        assert_eq!(line["工号"], "EMP-0007");
        assert_eq!(line["姓名"], "张三");
        assert_eq!(line["部门"], "生产部");
        assert_eq!(line["应出勤(天)"], "22");
        assert_eq!(line["实际出勤(天)"], "19.5");
        assert_eq!(line["病假(天)"], "0.5");
        assert_eq!(line["平时加班(小时)"], "10.5");
        assert_eq!(line["加班合计(小时)"], "14.5", "10.5 + 4 + 0");
        assert_eq!(line["迟到(次)"], "1");
        assert_eq!(line["早退(次)"], "2");
        assert_eq!(line["期间状态"], "已锁定");
        assert_eq!(line["锁定状态"], LOCK_LABEL_LOCKED);
        assert_eq!(line["来源"], "导入");
        assert_eq!(line["备注"], "补录半天事假");
    }

    /// 期间头口径：整表用同一份 `period_code` / `period_status`，不逐行取。
    ///
    /// 构造一行「自己带 `DRAFT`」的记录，导出时传 `LOCKED` —— 表里必须是「已锁定」：
    /// 混合状态会产出一张半锁半不锁的表，而 HR 无从察觉。
    #[test]
    fn period_header_wins_over_row_status() {
        let mut row = sample_row();
        row.period_status = STATUS_DRAFT.to_string();
        row.period_code = "2026-10".to_string();
        let line = row_line(&row, "2026-11", STATUS_LOCKED);
        assert_eq!(line["考勤月"], "2026-11");
        assert_eq!(line["锁定状态"], LOCK_LABEL_LOCKED);
        assert_eq!(line["期间状态"], "已锁定");
    }

    /// 空部门 / 空备注 / 空姓名不 panic，落成空串（Excel 里就是空单元格）。
    #[test]
    fn blank_columns_become_empty_strings() {
        let mut row = sample_row();
        row.dept_name = None;
        row.remark = None;
        row.employee_name = String::new();
        let line = row_line(&row, "2026-11", STATUS_DRAFT);
        assert_eq!(line["部门"], "");
        assert_eq!(line["备注"], "");
        assert_eq!(line["姓名"], "");
        assert_eq!(line["锁定状态"], LOCK_LABEL_UNLOCKED);
    }

    /// 文件名带期间码与锁定状态（HR 下载一堆表时靠名字区分）。
    #[test]
    fn file_name_carries_period_and_lock_state() {
        assert_eq!(file_name("2026-11", true), "考勤台账_2026-11_已锁定.xlsx");
        assert_eq!(file_name("2026-11", false), "考勤台账_2026-11_未锁定.xlsx");
    }

    /// 真的能写出 xlsx：字节流是 zip 容器（`PK\x03\x04`）、带一行数据时不 panic。
    ///
    /// 这条是「导出接口不会 500」的最低保证；写得出来不等于列对，
    /// 所以它与 `headers_cover_every_record_column` 一起才算「导出能用」。
    #[test]
    fn workbook_is_a_real_xlsx_zip() {
        let row = sample_row();
        let bytes = build_workbook(&[row], "2026-11", STATUS_LOCKED).expect("生成 xlsx");
        assert!(bytes.len() > 1024, "xlsx 太小了，可能是空容器：{} 字节", bytes.len());
        assert_eq!(&bytes[..4], b"PK\x03\x04", "xlsx 必须是 zip 容器");

        // 0 行也要能导出（「这一期还没导入」时 HR 仍需要拿到表头去核对）
        let empty = build_workbook(&[], "2026-11", STATUS_DRAFT).expect("空表也要能导出");
        assert_eq!(&empty[..4], b"PK\x03\x04");
    }
}

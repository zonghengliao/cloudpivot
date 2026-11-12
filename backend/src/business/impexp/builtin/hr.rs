//! 考勤记录导入（`hr.attendance` · HR-21）。
//!
//! 设计定稿：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.1 ⑤（`biz_attendance_record`
//! 的 DDL）/ §5.2 #23（`POST /business/hr/attendance/import`，`biz_type='hr.attendance'`）/
//! §6.2（考勤页导入入口 + 错误行表格）。**HR-21 交「定义 + 注册 + 模板」；HR-22 交
//! 导入后处理**（「无薪假 ≤ 已批请假天数」校验 / 汇总重算 / 错误行报告，见 [`import_post`]
//! 与 `validate_batch` / `persist` 的实现注释）。
//!
//! # 列契约（模板列 = §3.1 ⑤ 的列 + 一个可选的姓名核对列）
//!
//! | # | 列键 | 表头 | 必填 | 类型 | 落库列 |
//! |:--:|---|---|:--:|---|---|
//! | 1 | `period_code` | 考勤月 | ✅ | 文本 `yyyy-MM` | 查 `biz_attendance_period` → `period_id` |
//! | 2 | `emp_no` | 工号 | ✅ | 文本 ≤32 | 查 `biz_employee` → `employee_id` / 快照 |
//! | 3 | `employee_name` | 姓名 | — | 文本 ≤50 | **只做核对**，不落库（快照取自员工档案） |
//! | 4 | `should_days` | 应出勤天数 | ✅ | 小数 2 位 ≥0 ≤31 | `should_days` |
//! | 5 | `actual_days` | 实际出勤天数 | ✅ | 同上 | `actual_days` |
//! | 6 | `leave_paid_days` | 带薪假天数 | — | 同上 | `leave_paid_days` |
//! | 7 | `leave_unpaid_days` | 无薪假天数 | — | 同上 | `leave_unpaid_days` |
//! | 8 | `sick_days` | 病假天数 | — | 同上 | `sick_days` |
//! | 9 | `absent_days` | 旷工天数 | — | 同上 | `absent_days` |
//! | 10 | `ot_hours_workday` | 平时加班小时 | — | 小数 2 位 ≥0 ≤744 | `ot_hours_workday` |
//! | 11 | `ot_hours_weekend` | 休息日加班小时 | — | 同上 | `ot_hours_weekend` |
//! | 12 | `ot_hours_holiday` | 法定节假日加班小时 | — | 同上 | `ot_hours_holiday` |
//! | 13 | `late_count` | 迟到次数 | — | 整数 0–999 | `late_count` |
//! | 14 | `early_leave_count` | 早退次数 | — | 整数 0–999 | `early_leave_count` |
//! | 15 | `remark` | 备注 | — | 文本 ≤200 | `remark` |
//!
//! 列顺序与设计 §6.2 的台账列顺序一致（工号 / 姓名 / 应出勤 / 实际 / 带薪假 / 无薪假 / 病假 /
//! 旷工 / 加班三列 / 迟到 / 早退），HR 从台账导出的 Excel 与导入模板能直接对得上。
//!
//! # 服务端算的列（模板里**不出现**，用户填不进来）
//!
//! | 列 | 取值 | 为什么不让用户填 |
//! |---|---|---|
//! | `id` / `org_id` | `gen_random_uuid()` / [`SENTINEL_ORG_ID`] | 主键与组织维度不是业务数据 |
//! | `period_id` | 按 `period_code` 查期间头 | 让用户填 UUID 等于让人手工拼主键 |
//! | `employee_id` / `employee_no` / `employee_name` / `dept_id` | 按 `emp_no` 读员工档案（**导入那一刻的快照**） | 快照必须来自主数据；从文件里取会让「改个名字连历史考勤都变」（AGENTS.md 约定 13） |
//! | `source` | 固定 `'IMPORT'` | 让用户填就能伪造 `MANUAL` / `LEAVE_SYNC`，绕过来源审计 |
//!
//! # 两处「库层才有答案」的校验，放在 [`import`] 的一次预检里（不是逐行查库）
//!
//! 1. **期间存在且未锁定**：`biz_attendance_period.status = 'LOCKED'` 的月份**一行都不许进**
//!    （设计 §4.2.3：锁定 = 定稿，工资就按它算；再导一次等于改历史工资的依据）。
//! 2. **工号存在**（框架的 `FkSpec` 已覆盖）**且姓名对得上**（框架覆盖不到）：填了姓名而
//!    与档案不一致 → 直接报错并点出行号，防「错行」（Excel 里插删一行，工号与姓名整体错位
//!    是最常见的导入事故，而错位后的数据**完全合法**，库层永远不会报）。
//!
//! 三条都用**一条 SQL** 覆盖整批（`jsonb_to_recordset` + 两个 `LEFT JOIN`），
//! 每行一次查询会变成 N+1（1000 人 = 1000 次往返，见 `validate.rs` 模块头对 `FkResolver`
//! 的同一取舍）。命中任一条 → 整批回滚并返回带 Excel 行号的错误，绝不静默丢行。
//!
//! # 幂等：同一份文件重导不产生第二行
//!
//! `uk_biz_att_rec (period_id, employee_id) WHERE deleted_at IS NULL` 是唯一键，
//! 本定义按它 `ON CONFLICT … DO UPDATE`（重复数据策略 `SKIP` 时改为 `DO NOTHING`，
//! 与 `sys_import_task.options.duplicate_strategy` 一致，口径同 `master_data`）。
//! 一人一月一行是设计 D-10 的硬约束：两行会让期间汇总翻倍、工资按两倍算。
//!
//! # 与 `master_data` 的关系
//!
//! 没有复用 `master_data::{duplicate_strategy, conflict_clause, …}`：那些是
//! `pub(super)` 的**主数据私有**工具（主数据口径是「编码即幂等键」），考勤的口径是
//! 「(期间, 人) 才是幂等键」，两者只是长得像。硬拽过来会把主数据的假设带进 HR。

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::impexp::definition::{
    ColumnKind, DuplicateStrategy, ErrorKind, FieldIssue, FkSpec, ImportColumn, ImportCtx, ImportDefinition, RowIssue,
    RowValues,
};

/// 导入后处理（「无薪假 ≤ 已批请假天数」校验 + 汇总重算），HR-22。
///
/// 口径与取舍见该模块文档：校验在 `validate_batch`（整批 + 库内数据，命中即整批拒绝、
/// 带 Excel 行号进错误明细），汇总重算在 `persist` 的同一事务里（记录与汇总同生共死）。
mod import_post;

use import_post::{LeaveMatchRow, mismatch_message, recalc_periods, require_leave_match};

/// 考勤导入的业务编码（设计 §5.2 #23 逐字：`biz_type='hr.attendance'`）。
pub const ATTENDANCE_BIZ_TYPE: &str = "hr.attendance";

/// 单次导入行数上限（一家 500 人客户的月度台账 500 行，2 万留足余量）。
pub const MAX_ROWS: usize = 20_000;

/// 分批大小 = 行数上限 ⇒ **一份文件一个事务**。
///
/// 与 `master_data` 同一取舍：框架的 `STRICT` 语义是「任一行错就整体不入库」，
/// 而入库是按批开事务的。若一批只装 2000 行，第 2 批失败时第 1 批已经提交，
/// 界面会显示「前 2000 行已入库」—— 考勤出现「半个月的人有记录、半个月没有」，
/// 汇总与工资都会按残缺数据算。一份文件落在一个事务里，`STRICT` 才真的是全有或全无。
pub const BATCH_SIZE: usize = MAX_ROWS;

/// 天数列的合理上界。
///
/// 应出勤口径客户之间不同（自然月 30/31、固定 21.75、排班制），甚至计薪周期可以是
/// 「26 日~次月 25 日」（设计 §3.1 ④），所以上界取**一个月最多 31 天**而不是 22。
/// 这不是业务规则，是「明显录错」的拦截线（把 220 打成 22 才该放过，220 必然错）。
const DAYS_MAX: f64 = 31.0;

/// 加班小时的上界（31 天 × 24 小时 = 744）。
///
/// 与天数列同理：只为拦明显录错（如把分钟数 480 当天数填进来），不表达任何工时口径。
const OT_HOURS_MAX: f64 = 744.0;

/// 次数列的上界（迟到 / 早退次数）。
///
/// 一个月最多 31 个工作日，两位数是常态；999 是「明显不是次数」的分界线。
const COUNT_MAX: i64 = 999;

/// 备注的上界（与 `sys_import_task_error.raw_value` 的 200 字截断同宽，回显不丢信息）。
const REMARK_MAX: usize = 200;

/// 考勤月列（`period_code`）的键。
const KEY_PERIOD_CODE: &str = "period_code";
/// 工号列的键。
const KEY_EMP_NO: &str = "emp_no";
/// 姓名核对列的键。
const KEY_EMPLOYEE_NAME: &str = "employee_name";

/// 导入列定义（顺序即模板列顺序，见模块文档的表）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: KEY_PERIOD_CODE,
        header: "考勤月",
        required: true,
        kind: ColumnKind::Text { max_len: Some(7) },
        unique_key: true,
        fk: Some(FkSpec {
            table: "biz_attendance_period",
            column: "period_code",
            label: "考勤期间",
            soft_delete: true,
        }),
        example: "2026-11",
        width: Some(12),
        remark: "考勤月 yyyy-MM；必须是「考勤期间」里已建好的批次（草稿 / 已提交可导，**已锁定不可导**）",
    },
    ImportColumn {
        key: KEY_EMP_NO,
        header: "工号",
        required: true,
        kind: ColumnKind::Text { max_len: Some(32) },
        unique_key: true,
        fk: Some(FkSpec {
            table: "biz_employee",
            column: "emp_no",
            label: "员工档案",
            soft_delete: true,
        }),
        example: "EMP-0001",
        width: Some(14),
        remark: "人事档案里的工号（不是登录账号）；员工 id / 姓名 / 部门按它从档案取快照",
    },
    ImportColumn {
        key: KEY_EMPLOYEE_NAME,
        header: "姓名",
        required: false,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "张三",
        width: Some(12),
        remark: "可空。填了会与工号对应的档案姓名核对，不一致直接报错（防错行）；员工改过名请留空",
    },
    ImportColumn {
        key: "should_days",
        header: "应出勤天数",
        required: true,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(DAYS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "22",
        width: Some(12),
        remark: "≥0 且 ≤31，最多 2 位小数；口径由客户定（自然月 / 21.75 / 排班），可写 21.75",
    },
    ImportColumn {
        key: "actual_days",
        header: "实际出勤天数",
        required: true,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(DAYS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "20",
        width: Some(14),
        remark: "≥0 且 ≤31，最多 2 位小数",
    },
    ImportColumn {
        key: "leave_paid_days",
        header: "带薪假天数",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(DAYS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "1",
        width: Some(12),
        remark: "年假 / 婚丧假 / 产假等；留空按 0",
    },
    ImportColumn {
        key: "leave_unpaid_days",
        header: "无薪假天数",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(DAYS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "0",
        width: Some(12),
        remark: "事假等；薪资扣款依据。留空按 0（「无薪假 ≤ 已批请假天数」由导入后处理校验）",
    },
    ImportColumn {
        key: "sick_days",
        header: "病假天数",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(DAYS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "0",
        width: Some(10),
        remark: "按比例发，比例在薪资项公式里配；留空按 0",
    },
    ImportColumn {
        key: "absent_days",
        header: "旷工天数",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(DAYS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "0",
        width: Some(10),
        remark: "留空按 0",
    },
    ImportColumn {
        key: "ot_hours_workday",
        header: "平时加班小时",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(OT_HOURS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "12.5",
        width: Some(14),
        remark: "工作日 8 小时以外；留空按 0",
    },
    ImportColumn {
        key: "ot_hours_weekend",
        header: "休息日加班小时",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(OT_HOURS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "8",
        width: Some(16),
        remark: "周六日加班；留空按 0（倍率在薪资项公式里配）",
    },
    ImportColumn {
        key: "ot_hours_holiday",
        header: "法定节假日加班小时",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(OT_HOURS_MAX),
        },
        unique_key: false,
        fk: None,
        example: "0",
        width: Some(20),
        remark: "法定节假日加班；留空按 0（倍率在薪资项公式里配）",
    },
    ImportColumn {
        key: "late_count",
        header: "迟到次数",
        required: false,
        kind: ColumnKind::Int {
            min: Some(0),
            max: Some(COUNT_MAX),
        },
        unique_key: false,
        fk: None,
        example: "0",
        width: Some(10),
        remark: "整数 ≥0；留空按 0",
    },
    ImportColumn {
        key: "early_leave_count",
        header: "早退次数",
        required: false,
        kind: ColumnKind::Int {
            min: Some(0),
            max: Some(COUNT_MAX),
        },
        unique_key: false,
        fk: None,
        example: "0",
        width: Some(10),
        remark: "整数 ≥0；留空按 0",
    },
    ImportColumn {
        key: "remark",
        header: "备注",
        required: false,
        kind: ColumnKind::Text {
            max_len: Some(REMARK_MAX),
        },
        unique_key: false,
        fk: None,
        example: "10 月请事假 1 天",
        width: Some(24),
        remark: "可选，≤200 字；会落到台帐的备注列，工资单不展示",
    },
];

/// upsert 时更新哪些列（`id` / `period_id` / `employee_id` / `created_at` / `create_by` 保持首次入库的值）。
///
/// 快照列（`employee_no` / `employee_name` / `dept_id`）**要更新**：重导意味着「重新取一次
/// 当期档案」，员工刚改了名或刚调了部门，考勤记录也应跟上（这张表不是已过账的凭证，
/// 它还要参与这次工资试算）。`source` 也覆盖成 `IMPORT`：这一行的数字确实来自这次导入。
const UPSERT_SET: &str = "should_days = EXCLUDED.should_days, \
     actual_days = EXCLUDED.actual_days, \
     leave_paid_days = EXCLUDED.leave_paid_days, \
     leave_unpaid_days = EXCLUDED.leave_unpaid_days, \
     sick_days = EXCLUDED.sick_days, \
     absent_days = EXCLUDED.absent_days, \
     ot_hours_workday = EXCLUDED.ot_hours_workday, \
     ot_hours_weekend = EXCLUDED.ot_hours_weekend, \
     ot_hours_holiday = EXCLUDED.ot_hours_holiday, \
     late_count = EXCLUDED.late_count, \
     early_leave_count = EXCLUDED.early_leave_count, \
     employee_no = EXCLUDED.employee_no, \
     employee_name = EXCLUDED.employee_name, \
     dept_id = EXCLUDED.dept_id, \
     source = EXCLUDED.source, \
     remark = EXCLUDED.remark, \
     updated_at = CURRENT_TIMESTAMP, \
     update_by = EXCLUDED.update_by";

/// 整批预检 SQL（期间缺失 / 期间已锁定 / 员工缺失 / 姓名不符；一条往返、最多回 20 行）。
///
/// 返回值只用来**拼错误文案**（带 Excel 行号），不参与落库。
pub const PRECHECK_SQL: &str = "\
WITH v AS ( \
    SELECT * FROM jsonb_to_recordset(?::jsonb) \
        AS x(row_no int, period_code text, emp_no text, employee_name text) \
) \
SELECT v.row_no::text AS row_no, v.period_code, v.emp_no, \
       (p.id IS NULL) AS period_missing, COALESCE(p.status, '') AS period_status, \
       (e.id IS NULL) AS employee_missing, \
       COALESCE(e.name, '') AS db_name, COALESCE(v.employee_name, '') AS file_name \
  FROM v \
  LEFT JOIN biz_attendance_period p \
    ON p.period_code = v.period_code AND p.org_id = ?::uuid AND p.deleted_at IS NULL \
  LEFT JOIN biz_employee e \
    ON e.emp_no = v.emp_no AND e.org_id = ?::uuid AND e.deleted_at IS NULL \
 WHERE p.id IS NULL OR p.status = 'LOCKED' OR e.id IS NULL \
    OR (v.employee_name <> '' AND e.name <> v.employee_name) \
 ORDER BY v.row_no \
 LIMIT 20";

/// 入库 SQL 的固定前缀（另一半是 [`conflict_clause`] 拼出来的冲突处理子句）。
///
/// `AND p.status <> 'LOCKED'` 是**锁定守卫**：预检与本条在同一事务快照里，正常情况预检
/// 已经挡住；万一「预检之后、入库之前」期间被别的会话锁定，这里让该行不落库，配合
/// [`missing_rows_error`] 的行数对账把整批打回，而不是悄悄改写锁定期间的数字。
const INSERT_SQL_HEAD: &str = "\
INSERT INTO biz_attendance_record \
    (id, org_id, period_id, employee_id, employee_no, employee_name, dept_id, \
     should_days, actual_days, leave_paid_days, leave_unpaid_days, sick_days, absent_days, \
     ot_hours_workday, ot_hours_weekend, ot_hours_holiday, late_count, early_leave_count, \
     source, remark, created_at, updated_at, create_by, update_by) \
SELECT gen_random_uuid(), ?::uuid, p.id, e.id, e.emp_no, e.name, e.dept_id, \
       COALESCE(v.should_days, 0), COALESCE(v.actual_days, 0), COALESCE(v.leave_paid_days, 0), \
       COALESCE(v.leave_unpaid_days, 0), COALESCE(v.sick_days, 0), COALESCE(v.absent_days, 0), \
       COALESCE(v.ot_hours_workday, 0), COALESCE(v.ot_hours_weekend, 0), \
       COALESCE(v.ot_hours_holiday, 0), \
       COALESCE(v.late_count, 0), COALESCE(v.early_leave_count, 0), \
       'IMPORT', v.remark, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
       NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
  FROM jsonb_to_recordset(?::jsonb) AS v( \
         row_no int, period_code text, emp_no text, employee_name text, \
         should_days numeric, actual_days numeric, leave_paid_days numeric, \
         leave_unpaid_days numeric, sick_days numeric, absent_days numeric, \
         ot_hours_workday numeric, ot_hours_weekend numeric, ot_hours_holiday numeric, \
         late_count int, early_leave_count int, remark text) \
  JOIN biz_attendance_period p \
    ON p.period_code = v.period_code AND p.org_id = ?::uuid AND p.deleted_at IS NULL \
   AND p.status <> 'LOCKED' \
  JOIN biz_employee e \
    ON e.emp_no = v.emp_no AND e.org_id = ?::uuid AND e.deleted_at IS NULL";

/// 考勤月格式是否合法（`yyyy-MM`，月份 `01`–`12`）。
///
/// # 参数
///
/// - `code`：单元格原文（框架已 trim）。
///
/// # 返回值
///
/// 合法返回 `true`。
///
/// # 为什么不用「宽正则」
///
/// 与 `V0383` 的 `ck_biz_att_period_code` 同口径：`^[0-9]{4}-[0-9]{2}$` 会放过
/// `2026-13`（设计 §3.1 ④ 的注释里点过这个坑：ARCH-9-1 踩过一次）。月份必须落
/// 在 01–12，否则「2026-13 的考勤」会一路走到薪资试算才发现取不到期间。
pub fn period_code_valid(code: &str) -> bool {
    let bytes = code.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return false;
    }
    let digits_ok = bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| index == 4 || byte.is_ascii_digit());
    if !digits_ok {
        return false;
    }
    let month = (bytes[5] - b'0') * 10 + (bytes[6] - b'0');
    (1..=12).contains(&month)
}

/// 读回本任务生效的重复数据策略。
///
/// # 参数
///
/// - `ctx`：导入上下文（只用到 `task_id`）。
///
/// # 返回值
///
/// `options.duplicate_strategy` 的解析结果；未设置时按默认 `UPSERT`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（宁可整批失败，也不猜一个策略去覆盖考勤数据）。
async fn duplicate_strategy(ctx: &ImportCtx) -> DbResult<DuplicateStrategy> {
    let raw = crate::business::impexp::model::import_task_options(&ctx.task_id).await?;
    let parsed = raw
        .as_deref()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| {
            value
                .get("duplicate_strategy")
                .and_then(|item| item.as_str())
                .map(str::to_string)
        })
        .and_then(|text| DuplicateStrategy::parse(&text));
    Ok(parsed.unwrap_or_default())
}

/// 操作人文本（空串 → SQL 里 `NULLIF(?::text,'')::uuid` → NULL）。
///
/// # 参数
///
/// - `ctx`：导入上下文。
fn operator_text(ctx: &ImportCtx) -> String {
    ctx.operator.map(|id| id.to_string()).unwrap_or_default()
}

/// 文本列：空串 → `None`（落库 NULL，而不是空串）。
fn text_of(row: &RowValues, key: &str) -> Option<String> {
    let value = row.get(key).trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// 数值列：空串 → JSON `null`（SQL 侧 `COALESCE(…, 0)` 兜底成 0）。
fn num_or_null(row: &RowValues, key: &str) -> Option<String> {
    text_of(row, key)
}

/// 天数列当 `f64` 读（`validate_row` 的跨列规则用；值已由 `ColumnKind::Decimal` 校验过）。
fn days_of(row: &RowValues, key: &str) -> f64 {
    row.get(key).trim().parse::<f64>().unwrap_or(0.0)
}

/// 取列定义（按 key），用于给规则错误挂列名。
fn column_of(key: &str) -> Option<&'static ImportColumn> {
    COLUMNS.iter().find(|column| column.key == key)
}

/// 构造一条带列名与原始值的业务规则错误。
fn column_issue(row: &RowValues, key: &str, kind: ErrorKind, message: impl Into<String>) -> Option<RowIssue> {
    let column = column_of(key)?;
    Some(RowIssue::column(
        row.row_no,
        column,
        row.get(column.key),
        FieldIssue {
            kind,
            message: message.into(),
        },
    ))
}

/// 冲突处理子句（`UPSERT` 覆盖 / `SKIP` 跳过）。
///
/// # 参数
///
/// - `strategy`：本任务的重复数据策略。
fn conflict_clause(strategy: DuplicateStrategy) -> String {
    match strategy {
        DuplicateStrategy::Upsert => {
            format!(" ON CONFLICT (period_id, employee_id) WHERE deleted_at IS NULL DO UPDATE SET {UPSERT_SET}")
        }
        DuplicateStrategy::Skip => {
            " ON CONFLICT (period_id, employee_id) WHERE deleted_at IS NULL DO NOTHING".to_string()
        }
    }
}

/// 「本批有行没落库」的错误（JOIN 把行过滤掉时**不静默返回成功**）。
///
/// # 参数
///
/// - `expected`：本批次行数。
/// - `affected`：数据库报告的影响行数。
fn missing_rows_error(expected: usize, affected: usize) -> DbError {
    DbError::InvalidValue(format!(
        "本批 {expected} 行中只有 {affected} 行落库：预检之后这些行的考勤期间被锁定、\
         或员工档案被删除了（导入与「锁定 / 删档」并发），已整批回滚，请刷新后重试"
    ))
}

/// 考勤记录导入定义（`hr.attendance`）。
pub struct AttendanceImport;

impl AttendanceImport {
    /// 把已校验的行拼成 `jsonb_to_recordset` 的载荷。
    ///
    /// 数值列空串 → `null`（SQL 侧 `COALESCE(…, 0)`）；`row_no` 一并带上，预检的报错
    /// 才能点出「Excel 第几行」。
    fn payload(rows: &[RowValues]) -> std::result::Result<String, DbError> {
        let items = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "row_no": row.row_no,
                    "period_code": row.get(KEY_PERIOD_CODE),
                    "emp_no": row.get(KEY_EMP_NO),
                    "employee_name": row.get(KEY_EMPLOYEE_NAME),
                    "should_days": num_or_null(row, "should_days"),
                    "actual_days": num_or_null(row, "actual_days"),
                    "leave_paid_days": num_or_null(row, "leave_paid_days"),
                    "leave_unpaid_days": num_or_null(row, "leave_unpaid_days"),
                    "sick_days": num_or_null(row, "sick_days"),
                    "absent_days": num_or_null(row, "absent_days"),
                    "ot_hours_workday": num_or_null(row, "ot_hours_workday"),
                    "ot_hours_weekend": num_or_null(row, "ot_hours_weekend"),
                    "ot_hours_holiday": num_or_null(row, "ot_hours_holiday"),
                    "late_count": num_or_null(row, "late_count"),
                    "early_leave_count": num_or_null(row, "early_leave_count"),
                    "remark": text_of(row, "remark"),
                })
            })
            .collect::<Vec<_>>();
        serde_json::to_string(&items).map_err(|error| DbError::InvalidValue(format!("考勤行序列化失败：{error}")))
    }

    /// 把已校验的行摘成「无薪假校验」要的四个字段（期间码 / 工号 / 无薪假 / 行号）。
    ///
    /// 只取需要的四列：这条规则与应出勤、加班无关，多带列只会让 SQL 载荷变大。
    fn leave_match_rows(rows: &[RowValues]) -> Vec<LeaveMatchRow> {
        rows.iter()
            .map(|row| LeaveMatchRow {
                row_no: row.row_no,
                period_code: row.get(KEY_PERIOD_CODE).trim().to_string(),
                emp_no: row.get(KEY_EMP_NO).trim().to_string(),
                leave_unpaid_days: text_of(row, "leave_unpaid_days"),
            })
            .collect()
    }

    /// 本批涉及的考勤月（去重由 [`recalc_periods`] 再做一次，这里只保证不丢）。
    fn period_codes(rows: &[RowValues]) -> Vec<String> {
        rows.iter()
            .map(|row| row.get(KEY_PERIOD_CODE).trim().to_string())
            .filter(|code| !code.is_empty())
            .collect()
    }

    /// 整批预检：把「期间不存在 / 期间已锁定 / 工号不存在 / 姓名不符」一次性查出来。
    ///
    /// # 返回值
    ///
    /// 没有任何问题 → `Ok(())`。
    ///
    /// # 异常情况
    ///
    /// - 命中问题 → `Err(DbError::InvalidValue)`，报文里逐条带 Excel 行号；
    /// - 数据库异常 → 原样透传。
    async fn precheck(tx: &Transaction, payload: &str) -> DbResult<()> {
        let rows = tx
            .find_with_params(
                PRECHECK_SQL,
                vec![
                    Value::Json(payload.to_string()),
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(&SENTINEL_ORG_ID),
                ],
            )
            .await?;
        if rows.is_empty() {
            return Ok(());
        }
        let mut lines = Vec::with_capacity(rows.len());
        for row in &rows {
            let row_no: String = row.get_typed("row_no")?;
            let period_code: String = row.get_typed("period_code")?;
            let emp_no: String = row.get_typed("emp_no")?;
            let period_missing: bool = row.get_typed("period_missing")?;
            let period_status: String = row.get_typed("period_status")?;
            let employee_missing: bool = row.get_typed("employee_missing")?;
            let db_name: String = row.get_typed("db_name")?;
            let file_name: String = row.get_typed("file_name")?;
            let reason = if period_missing {
                format!("考勤月「{period_code}」在考勤期间里不存在，请先在「考勤期间」建这个月")
            } else if period_status == "LOCKED" {
                format!("考勤月「{period_code}」已锁定（LOCKED），锁定期间不可导入（要改请先解锁）")
            } else if employee_missing {
                format!("工号「{emp_no}」在员工档案里不存在或已删除")
            } else {
                format!(
                    "工号「{emp_no}」的档案姓名是「{db_name}」，文件里填的是「{file_name}」，\
                     请核对工号（员工改过名请留空姓名列）"
                )
            };
            lines.push(format!("Excel 第 {row_no} 行：{reason}"));
        }
        let more = if rows.len() >= 20 { "（仅列前 20 行）" } else { "" };
        Err(DbError::InvalidValue(format!(
            "考勤导入被拦下，整批未入库（{} 行有问题{more}）：{}",
            rows.len(),
            lines.join("；")
        )))
    }
}

#[async_trait]
impl ImportDefinition for AttendanceImport {
    fn biz_type(&self) -> &'static str {
        ATTENDANCE_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "考勤记录"
    }

    fn max_rows(&self) -> usize {
        MAX_ROWS
    }

    fn batch_size(&self) -> usize {
        BATCH_SIZE
    }

    fn allow_partial(&self) -> bool {
        true
    }

    fn columns(&self) -> &'static [ImportColumn] {
        COLUMNS
    }

    /// 单行业务规则（纯计算，不碰库；库层才有答案的三条见 [`AttendanceImport::precheck`]）。
    ///
    /// # 参数
    ///
    /// - `row`：已过逐格校验的行；
    /// - `_ctx`：导入上下文（本定义不需要）。
    ///
    /// # 异常情况
    ///
    /// 返回非空 `Vec<RowIssue>` 表示该行不可入库（每条都带 Excel 行号与列名）。
    async fn validate_row(&self, row: &RowValues, _ctx: &ImportCtx) -> std::result::Result<(), Vec<RowIssue>> {
        let mut issues = Vec::new();

        // ① 考勤月格式：`ColumnKind::Text` 只校验长度，月份区间必须自己判（见 period_code_valid）
        let period_code = row.get(KEY_PERIOD_CODE).trim();
        if !period_code.is_empty()
            && !period_code_valid(period_code)
            && let Some(issue) = column_issue(
                row,
                KEY_PERIOD_CODE,
                ErrorKind::Rule,
                format!("考勤月必须是 yyyy-MM 且月份在 01–12 之间（实际「{period_code}」）"),
            )
        {
            issues.push(issue);
        }

        // ② 自相矛盾：应出勤 0 天却填了出勤 / 请假 / 旷工天数
        //
        // 设计 §3.1 ⑤ 明确「不写守恒 CHECK」（应出勤口径客户之间不同：30/31、21.75、排班制），
        // 所以这里只拦**任何口径下都说不通**的一种：应出勤为 0（这个月一天都不用上班）却
        // 有人来了 / 请假了 / 旷工了。守恒式（实出勤 + 各类假 + 旷工 = 应出勤）**不校验**，
        // 那会误伤 21.75 口径下的 22 天。
        let should = days_of(row, "should_days");
        let attended = [
            "actual_days",
            "leave_paid_days",
            "leave_unpaid_days",
            "sick_days",
            "absent_days",
        ]
        .iter()
        .map(|key| days_of(row, key))
        .sum::<f64>();
        if should <= 0.0
            && attended > 0.0
            && let Some(issue) = column_issue(
                row,
                "should_days",
                ErrorKind::Rule,
                "应出勤天数是 0，但填了出勤 / 请假 / 旷工天数：请核对应出勤口径（整月无排班请把这些列也留空）",
            )
        {
            issues.push(issue);
        }

        if issues.is_empty() { Ok(()) } else { Err(issues) }
    }

    /// 声明本定义有批次级校验（框架据此在预检时也保留整批行）。
    fn validates_batch(&self) -> bool {
        true
    }

    /// 批次级校验：**「无薪假 ≤ 已批请假天数」**（HR-22，设计 §3.2 ⑩ 的开关控制）。
    ///
    /// # 参数
    ///
    /// - `rows`：整批已过逐行校验的行；
    /// - `_ctx`：导入上下文（本规则与任务/操作人无关）。
    ///
    /// # 返回值
    ///
    /// 命中「无薪假 > 已批准的无薪请假单天数」的行 → 每人一条 [`RowIssue`]（带 Excel 行号 +
    /// `leave_unpaid_days` 列名）。框架会把这些行从可入库集合摘掉并计入 `error`：
    /// `STRICT` 模式据此整批拒绝（一行都不入库），`PARTIAL` 只跳过错行。
    ///
    /// # 异常情况
    ///
    /// 数据库异常 → [`DbError`]（框架转 500，不算用户错）。
    ///
    /// # 开关
    ///
    /// `sys_config.hr.attendance.require_leave_match = FALSE` 时直接放过（历史客户「没有
    /// 请假单」的过渡期），但汇总重算照做 —— 开关只管这条业务规则，不影响数据一致性。
    async fn validate_batch(&self, rows: &[RowValues], _ctx: &ImportCtx) -> DbResult<Vec<RowIssue>> {
        if rows.is_empty() || !require_leave_match().await {
            return Ok(Vec::new());
        }
        let mismatches = import_post::leave_mismatches(db(), &Self::leave_match_rows(rows)).await?;
        let Some(column) = column_of("leave_unpaid_days") else {
            // 列定义没了就是模板被改坏（编译期常量），不是用户错误
            return Err(DbError::InvalidValue(
                "考勤导入模板缺少「无薪假天数」列定义，无法校验「无薪假 ≤ 已批请假天数」".to_string(),
            ));
        };
        Ok(mismatches
            .iter()
            .map(|mismatch| {
                RowIssue::column(
                    mismatch.row_no,
                    column,
                    &import_post::days_text(mismatch.unpaid_days),
                    FieldIssue {
                        kind: ErrorKind::Rule,
                        message: mismatch_message(mismatch),
                    },
                )
            })
            .collect())
    }

    /// 整批入库（框架已在事务内）。
    ///
    /// # 参数
    ///
    /// - `tx`：当前事务；
    /// - `rows`：已过预检的行；
    /// - `ctx`：导入上下文（操作人与重复数据策略）。
    ///
    /// # 返回值
    ///
    /// 实际写入（含覆盖）的行数。`SKIP` 策略下已存在的行不算写入，返回值会小于 `rows.len()`。
    ///
    /// # 异常情况
    ///
    /// - 期间 / 员工 / 姓名预检不通过 → [`DbError::InvalidValue`]（报文带行号，整批回滚）；
    /// - 行数对账不平（并发锁定 / 删档）→ [`DbError::InvalidValue`]（整批回滚，不留半批）。
    async fn persist(&self, tx: &Transaction, rows: &[RowValues], ctx: &ImportCtx) -> DbResult<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let strategy = duplicate_strategy(ctx).await?;
        let payload = Self::payload(rows)?;
        Self::precheck(tx, &payload).await?;

        let sql = format!("{INSERT_SQL_HEAD}{}", conflict_clause(strategy));
        let operator = operator_text(ctx);
        let affected = tx
            .execute_with_params(
                &sql,
                vec![
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(operator.clone()),
                    Value::from(operator),
                    Value::Json(payload),
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(&SENTINEL_ORG_ID),
                ],
            )
            .await?;
        let affected = affected as usize;
        // SKIP 策略下「少写」是设计意图（已在库的行跳过），不算异常；
        // UPSERT 策略下少写只能是 JOIN 把行过滤掉了（并发锁定 / 删档）→ 整批打回。
        if strategy == DuplicateStrategy::Upsert && affected != rows.len() {
            return Err(missing_rows_error(rows.len(), affected));
        }
        // 导入后处理 ②：汇总重算（HR-22）。**在同一事务里**做 —— 记录与期间的四个汇总列
        // 一起提交、一起回滚：提交后再重算失败会留下「记录进了库、汇总还是旧值」的中间态，
        // 而汇总列是工资试算的依据（AGENTS.md 约定 11）。`recalc_totals` 的 SQL 自带
        // `status <> 'LOCKED'` 守卫，锁定期间重算影响 0 行（锁定期间本来也导不进来）。
        // 一份文件可能跨月（模板按行带考勤月），所以按**本批出现的每个期间**各重算一次。
        let periods = Self::period_codes(rows);
        if let Err(error) = recalc_periods(tx, &periods, ctx.operator).await {
            return Err(DbError::InvalidValue(format!(
                "汇总重算失败：{error}（本次导入整批回滚，考勤记录与期间的汇总列都未落库）"
            )));
        }
        Ok(affected)
    }
}

#[cfg(test)]
mod db_tests;

/// HR-22 的真库用例：导入后处理（「无薪假 ≤ 已批请假天数」整批拒绝 / 错误行号 /
/// 三个验收点 + 开关 + 跨月汇总重算）。只在 `cargo test` 时编译。
#[cfg(test)]
mod import_post_db_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::definition::{EnumItem, ImportMode};

    /// 12 个数值列（`late_count` / `early_leave_count` 是整数，其余是小数）。
    const DECIMAL_KEYS: [&str; 9] = [
        "should_days",
        "actual_days",
        "leave_paid_days",
        "leave_unpaid_days",
        "sick_days",
        "absent_days",
        "ot_hours_workday",
        "ot_hours_weekend",
        "ot_hours_holiday",
    ];
    const INT_KEYS: [&str; 2] = ["late_count", "early_leave_count"];

    fn row_of(row_no: u32, pairs: &[(&'static str, &str)]) -> RowValues {
        let mut values = RowValues {
            row_no,
            ..Default::default()
        };
        for (key, value) in pairs {
            values.fields.insert(key, value.to_string());
        }
        values
    }

    fn good_row(row_no: u32) -> RowValues {
        row_of(
            row_no,
            &[
                ("period_code", "2026-11"),
                ("emp_no", "EMP-0001"),
                ("employee_name", ""),
                ("should_days", "22"),
                ("actual_days", "20"),
                ("leave_paid_days", "1"),
                ("leave_unpaid_days", "1"),
                ("sick_days", "0"),
                ("absent_days", "0"),
                ("ot_hours_workday", "12.5"),
                ("ot_hours_weekend", "8"),
                ("ot_hours_holiday", "0"),
                ("late_count", "2"),
                ("early_leave_count", "0"),
                ("remark", "10 月请事假 1 天"),
            ],
        )
    }

    #[test]
    fn column_contract_matches_design_table() {
        let keys = COLUMNS.iter().map(|column| column.key).collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "period_code",
                "emp_no",
                "employee_name",
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
                "remark"
            ],
            "模板列顺序 = 设计 §6.2 台账列顺序，改这里就是改模板，必须同步设计文档"
        );
        // 必填：考勤月 / 工号 / 应出勤 / 实际出勤。其余留空按 0 或 NULL
        let required = COLUMNS
            .iter()
            .filter(|column| column.required)
            .map(|column| column.key)
            .collect::<Vec<_>>();
        assert_eq!(required, vec!["period_code", "emp_no", "should_days", "actual_days"]);
        // 文件内唯一 = (period_code, emp_no) 组合：一人一月一行
        let unique = COLUMNS
            .iter()
            .filter(|column| column.unique_key)
            .map(|column| column.key)
            .collect::<Vec<_>>();
        assert_eq!(unique, vec!["period_code", "emp_no"]);
        // 外键：期间 + 员工档案（都带软删，导入只认未删除行）
        let fks = COLUMNS
            .iter()
            .filter_map(|column| {
                column
                    .fk
                    .map(|spec| (column.key, spec.table, spec.column, spec.soft_delete))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            fks,
            vec![
                ("period_code", "biz_attendance_period", "period_code", true),
                ("emp_no", "biz_employee", "emp_no", true),
            ]
        );
        // 表头不重复（Excel 表头重复会被框架判结构错误）
        let mut headers = COLUMNS.iter().map(|column| column.header).collect::<Vec<_>>();
        headers.sort_unstable();
        let before = headers.len();
        headers.dedup();
        assert_eq!(headers.len(), before, "模板表头必须互不相同");
    }

    #[test]
    fn numeric_columns_align_with_table_checks() {
        // V0383 的 ck_biz_att_rec_nonneg：11 个数值列都 ≥0 —— 导入列定义必须同口径，
        // 否则用户会先拿到「库约束 ck_biz_att_rec_nonneg」，而不是「天数不能是负数」
        for key in DECIMAL_KEYS {
            let column = column_of(key).expect("列必须存在");
            match column.kind {
                ColumnKind::Decimal { scale, min, max } => {
                    assert_eq!(scale, 2, "{key} 与 NUMERIC(6,2)/(8,2) 同精度");
                    assert_eq!(min, Some(0.0), "{key} 下界必须 ≥0");
                    let expected_max = if key.starts_with("ot_hours") {
                        OT_HOURS_MAX
                    } else {
                        DAYS_MAX
                    };
                    assert_eq!(max, Some(expected_max), "{key} 上界");
                }
                other => panic!("{key} 应为小数列，实际 {other:?}"),
            }
        }
        for key in INT_KEYS {
            let column = column_of(key).expect("列必须存在");
            match column.kind {
                ColumnKind::Int { min, max } => {
                    assert_eq!(min, Some(0), "{key} 下界必须 ≥0");
                    assert_eq!(max, Some(COUNT_MAX), "{key} 上界");
                }
                other => panic!("{key} 应为整数列，实际 {other:?}"),
            }
        }
        // 非数值列不该被塞进这两组清单
        for key in DECIMAL_KEYS.iter().chain(INT_KEYS.iter()) {
            assert!(column_of(key).is_some());
        }
    }

    #[test]
    fn decimal_column_rejects_three_places_and_negatives() {
        let column = column_of("should_days").expect("列必须存在");
        assert_eq!(column.kind.parse("21.75", None).expect("21.75 口径合法"), "21.75");
        let scale = column.kind.parse("21.755", None).expect_err("3 位小数应拒绝");
        assert_eq!(scale.kind, ErrorKind::Range);
        let negative = column.kind.parse("-1", None).expect_err("负数应拒绝");
        assert_eq!(negative.kind, ErrorKind::Range);
        let too_big = column.kind.parse("220", None).expect_err("超过 31 天应拒绝");
        assert_eq!(too_big.kind, ErrorKind::Range);
        // 空值是「未填」，交必填判定 / COALESCE 默认值，不该在这里报类型错
        assert_eq!(column.kind.parse("  ", None).expect("空值不算错"), "");
    }

    #[test]
    fn period_code_valid_covers_month_boundaries() {
        for ok in ["2026-01", "2026-11", "2026-12", "1999-06"] {
            assert!(period_code_valid(ok), "{ok} 应合法");
        }
        for bad in [
            "",
            "2026-13",
            "2026-00",
            "2026-1",
            "26-11",
            "2026/11",
            "2026-11-01",
            "20261-1",
            "abcd-ef",
        ] {
            assert!(!period_code_valid(bad), "{bad} 应非法");
        }
    }

    #[tokio::test]
    async fn validate_row_accepts_a_complete_row() {
        let ctx = ImportCtx {
            task_id: uuid::Uuid::nil(),
            operator: None,
            mode: ImportMode::Strict,
        };
        AttendanceImport
            .validate_row(&good_row(3), &ctx)
            .await
            .expect("完整行应通过");
    }

    #[tokio::test]
    async fn validate_row_reports_bad_period_with_row_number() {
        let ctx = ImportCtx {
            task_id: uuid::Uuid::nil(),
            operator: None,
            mode: ImportMode::Strict,
        };
        let mut row = good_row(9);
        row.fields.insert("period_code", "2026-13".to_string());
        let issues = AttendanceImport
            .validate_row(&row, &ctx)
            .await
            .expect_err("考勤月非法应报错");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].row_no, 9);
        assert_eq!(issues[0].column_key, Some("period_code"));
        assert_eq!(issues[0].kind, ErrorKind::Rule);
        assert!(issues[0].message.contains("2026-13"), "{}", issues[0].message);
        assert!(issues[0].message.contains("01–12"), "{}", issues[0].message);
    }

    #[tokio::test]
    async fn validate_row_rejects_zero_should_days_with_attendance() {
        let ctx = ImportCtx {
            task_id: uuid::Uuid::nil(),
            operator: None,
            mode: ImportMode::Strict,
        };
        let mut row = good_row(4);
        row.fields.insert("should_days", "0".to_string());
        row.fields.insert("actual_days", "5".to_string());
        let issues = AttendanceImport
            .validate_row(&row, &ctx)
            .await
            .expect_err("应出勤 0 却有出勤天数应报错");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].column_key, Some("should_days"));
        assert!(issues[0].message.contains("应出勤天数是 0"), "{}", issues[0].message);

        // 整月无排班（各列都 0）是合法的：没排班 ≠ 数据错误
        let mut idle = good_row(5);
        for key in [
            "should_days",
            "actual_days",
            "leave_paid_days",
            "leave_unpaid_days",
            "sick_days",
            "absent_days",
        ] {
            idle.fields.insert(key, "0".to_string());
        }
        AttendanceImport
            .validate_row(&idle, &ctx)
            .await
            .expect("全 0 行应通过（如停薪留职）");
    }

    #[test]
    fn payload_carries_row_no_and_nulls_blank_numbers() {
        let rows = vec![
            good_row(7),
            row_of(8, &[("period_code", "2026-11"), ("emp_no", "EMP-0002")]),
        ];
        let payload = AttendanceImport::payload(&rows).expect("序列化应成功");
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("应是合法 JSON");
        let items = parsed.as_array().expect("应是数组");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["row_no"], 7);
        assert_eq!(items[0]["emp_no"], "EMP-0001");
        assert_eq!(items[0]["should_days"], "22");
        assert_eq!(items[0]["ot_hours_workday"], "12.5");
        // 未填的数值列 → null（落库走 COALESCE 默认 0），不是空串（numeric 收到 '' 会报错）
        assert!(items[1]["should_days"].is_null());
        assert!(items[1]["late_count"].is_null());
        assert!(items[1]["remark"].is_null());
    }

    #[test]
    fn insert_sql_placeholder_count_is_stable() {
        // 占位符个数与 persist 里的绑定顺序**必须**一致：多一个少一个都是运行期
        // 「parameter count mismatch」，而这条 SQL 在单测里跑不到（要先有库）。
        let upsert = format!("{INSERT_SQL_HEAD}{}", conflict_clause(DuplicateStrategy::Upsert));
        assert_eq!(upsert.matches('?').count(), 6, "org ×3 + 操作人 ×2 + jsonb ×1");
        let skip = format!("{INSERT_SQL_HEAD}{}", conflict_clause(DuplicateStrategy::Skip));
        assert_eq!(skip.matches('?').count(), 6);
        // 冲突目标必须是 (period_id, employee_id) 的部分唯一索引
        assert!(upsert.contains("ON CONFLICT (period_id, employee_id) WHERE deleted_at IS NULL"));
        // 锁定守卫必须在 SQL 里（预检之外的第二道）
        assert!(INSERT_SQL_HEAD.contains("p.status <> 'LOCKED'"));
        // source 写死 IMPORT，模板里没有这一列，用户伪造不了来源
        assert!(INSERT_SQL_HEAD.contains("'IMPORT'"));
        // 快照列必须从 biz_employee 取，不能从载荷取
        assert!(INSERT_SQL_HEAD.contains("e.emp_no, e.name, e.dept_id"));
        // upsert 不许改主键 / 期间 / 员工（改这三列等于换一条记录）
        // 按「赋值语句左侧字段名」逐个对拍，不用 `contains("id =")`：那会被
        // `dept_id = ` / `period_id = ` 里的子串误伤（本用例自己踩过）。
        let assigned = UPSERT_SET
            .split(',')
            .map(|fragment| fragment.trim().split('=').next().unwrap_or("").trim())
            .collect::<Vec<_>>();
        for guarded in ["id", "period_id", "employee_id", "created_at", "create_by"] {
            assert!(
                !assigned.contains(&guarded),
                "upsert 不许改写 {guarded}，实际更新列：{assigned:?}"
            );
        }
    }

    #[test]
    fn precheck_sql_catches_the_four_blocking_cases() {
        for fragment in [
            "p.id IS NULL",
            "p.status = 'LOCKED'",
            "e.id IS NULL",
            "v.employee_name <> '' AND e.name <> v.employee_name",
        ] {
            assert!(PRECHECK_SQL.contains(fragment), "预检必须覆盖：{fragment}");
        }
        // 行数上限兜底：坏文件里 5000 行都有问题时，报文不能长到写不进日志
        assert!(PRECHECK_SQL.contains("LIMIT 20"));
        assert_eq!(PRECHECK_SQL.matches('?').count(), 3);
    }

    #[test]
    fn definition_metadata_is_stable() {
        let definition = AttendanceImport;
        assert_eq!(definition.biz_type(), "hr.attendance");
        assert_eq!(definition.biz_name(), "考勤记录");
        assert_eq!(definition.template_version(), 1);
        assert_eq!(definition.max_rows(), MAX_ROWS);
        // 一份文件一个事务：STRICT 才是真「全有或全无」
        assert_eq!(definition.batch_size(), MAX_ROWS);
        assert!(definition.allow_partial(), "允许部分导入（页面可选 STRICT / PARTIAL）");
        assert_eq!(definition.columns().len(), 15);
    }

    #[test]
    fn definition_is_registered_in_global_registry() {
        // 注册才是「接进框架」的判据：漏挂 builtin::register 的话，模板下载 /
        // 上传接口按 biz_type 查不到定义 → 60011，前端下拉里也没有这一项。
        let registry = crate::business::impexp::registry::global_registry();
        let definition = registry
            .import_definition(ATTENDANCE_BIZ_TYPE)
            .expect("hr.attendance 应已注册为导入定义");
        assert_eq!(definition.biz_type(), ATTENDANCE_BIZ_TYPE);
        assert_eq!(definition.columns().len(), COLUMNS.len());
        let brief = registry
            .briefs()
            .into_iter()
            .find(|brief| brief.biz_type == ATTENDANCE_BIZ_TYPE)
            .expect("应在定义摘要列表里（前端下拉用它渲染）");
        assert_eq!(brief.biz_name, "考勤记录");
        assert!(brief.supports_import);
        assert!(
            !brief.supports_export,
            "考勤导出走 /business/hr/attendance/export，不进 impexp"
        );
        assert_eq!(brief.template_version, 1);
        assert_eq!(brief.max_rows, MAX_ROWS);
    }

    #[test]
    fn template_can_be_built() {
        // 模板是「定义 → xlsx」的纯函数，不需要库；这条用例把「模板生成不 panic」钉住
        let bytes = crate::business::impexp::template::build_template(&AttendanceImport).expect("模板应生成成功");
        assert!(bytes.len() > 1_000, "xlsx 至少几 KB，实际 {} 字节", bytes.len());
        // 模板文件名带上业务名与版本号（下载时用户能看出是考勤模板的第几版）
        let name = crate::business::impexp::template::template_file_name(&AttendanceImport);
        assert_eq!(name, "考勤记录导入模板_v1.xlsx");
    }

    #[test]
    fn enum_items_are_not_used_here() {
        // 考勤模板没有枚举列（源/状态都是服务端算的），这条用例防「以后有人加了个
        // 枚举列却忘了在模板下拉里给它取值」
        let enum_columns = COLUMNS
            .iter()
            .filter(|column| matches!(column.kind, ColumnKind::Enum(_)))
            .count();
        assert_eq!(enum_columns, 0);
        let _ = std::mem::size_of::<EnumItem>();
    }
}

//! 工时台账 xlsx 导入（成本核算 · ARCH-9 §4.5.3 八列模板 / §5.2 #15–#16 / §5.3）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//!
//! | 章节 | 本模块用到的地方 |
//! |---|---|
//! | §4.5.3 | 8 列表头、每列校验、**全有或全无**、逐行错误（行号 + 列名 + 原因） |
//! | §5.2 #15 | `POST /business/erp/cost-labor/import`：multipart xlsx，返回 `{success, failed, rows:[{line, column, message}]}` |
//! | §5.2 #16 | `GET /business/erp/cost-labor/template`：下载 8 列模板 xlsx |
//! | §3.1 ④ | `biz_cost_labor` 的 6 条 CHECK（工时 > 0、机器工时 ≥ 0、费率 ≥ 0、金额 = round(工时×费率,4)…） |
//!
//! # 为什么必须「全有或全无」
//!
//! 设计 §4.5.3 的原话：*半批导入会让「人工成本」少一块而没人发现*。
//! 比如 30 行里第 17 行的成本对象号打错了，如果「跳过坏行、入库好行」，
//! 结果就是「这一批的人工少了 4 小时」—— 成本计算单照样能算出来，只是算错了，
//! 而且没有任何红灯（D-7「不猜数据」的同一条底线）。
//!
//! 所以本模块的执行顺序写死为：
//!
//! ```text
//! ① 解析（结构错 → 400/422，整表拒绝，连行号都没有）
//! ② 逐行纯校验（不碰库：格式 / 必填 / 期间 / 精度）
//! ③ 引用校验（成本对象存在且 OPEN / 部门存在 / 工号可解析）—— 一次批量查库，不做 N+1
//! ④ 只要**任何一行**有问题 → 直接返回逐行错误，**一条都不入库**
//! ⑤ 全部干净 → 单事务批量写入（`source_type='IMPORT'`、`status='DRAFT'`），失败整批回滚
//! ```
//!
//! # 进度状态为什么是 `DRAFT` 而不是直接 `CONFIRMED`
//!
//! 设计 §4.3：`calc` 第 2 段只归集 `status='CONFIRMED'` 的工时。导入的数据是
//! 「别人报上来的数」，先落 `DRAFT` 让人复核（前端 `BatchActionBar` 批量确认，B2-13），
//! 否则一次误传就把当期人工成本推进结算，只能走红冲（R-8）。
//!
//! # 两处实现口径（都有卡内说明）
//!
//! 1. **工资率列留空** = 按设计 §4.5.4 自动取数（`成本对象 > 车间 > 公司`，B2-3）：
//!    取数发生在引用校验之后（要对象的 id、车间的 id、工作日期），
//!    三级都没配到 ⇒ **该行报错**（列 = 工资率，报文点名缺哪一级、缺哪一天），
//!    **绝不默认按 0 计**（设计 D-7：编造出来的人工成本比算不出来更危险）。
//! 2. **员工工号**：`biz_employee` 属 ARCH9-2，尚未上线。工号留空 = 按班组计
//!    （`employee_id` 为 NULL，设计 §4.5.3 允许）；填了则在 `biz_employee` 存在时
//!    解析工号，表不存在时该行报错并要求留空 —— 不静默丢弃工号。
//!
//! # 行号口径
//!
//! 报错里的 `line` 是 **Excel 真实行号**（表头是第 1 行，第一条数据是第 2 行），
//! 与用户屏幕上看到的行号一致；用户能直接跳过去改。

use std::collections::HashMap;
use std::str::FromStr;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::cost_labor_dto;
use crate::business::erp::cost_labor_dto::{
    CostLaborSpec, DEPT_NAME_MAX, EMPLOYEE_NAME_MAX, HOURS_MAX, HOURS_SCALE, RATE_MAX, RATE_SCALE, STATUS_DRAFT,
};
use crate::business::erp::cost_labor_model::{self as model};
use crate::business::erp::cost_object_dto;
use crate::business::erp::cost_rate_dto::{self, RateRequest, RateSubject};
use crate::business::impexp::definition::{ColumnKind, ImportColumn, parse_date};
use crate::business::impexp::engine::{SheetReader, WorkbookSheet, write_workbook};
use crate::shared::error_code;
use crate::utils;

// ==================== 8 列模板（设计 §4.5.3 逐列） ====================

/// 成本对象号列。
pub const HEADER_OBJECT_NO: &str = "成本对象号";
/// 日期列。
pub const HEADER_WORK_DATE: &str = "日期";
/// 员工工号列。
pub const HEADER_EMPLOYEE_NO: &str = "员工工号";
/// 员工姓名列。
pub const HEADER_EMPLOYEE_NAME: &str = "员工姓名";
/// 工作中心（部门）列。
pub const HEADER_DEPT: &str = "工作中心（部门）";
/// 人工工时列。
pub const HEADER_HOURS: &str = "人工工时";
/// 机器工时列。
pub const HEADER_MACHINE_HOURS: &str = "机器工时";
/// 工资率列。
pub const HEADER_RATE: &str = "工资率";

/// 单次导入的行数上限（内存 / 事务长度护栏）。
///
/// 一个月的工时台账量级是「人数 × 天数」：50 人 × 31 天 ≈ 1550 行。5000 行已经
/// 覆盖到 150 人规模，再大请按月拆分 —— 与导入框架 `max_rows` 同一条思路。
pub const MAX_ROWS: usize = 5000;

/// 模板列定义（顺序即模板列顺序，设计 §4.5.3 的 8 列）。
///
/// `kind` 只用于模板「字段说明」sheet；真正的逐格校验在 [`parse_rows`]
/// （导入框架的 `ColumnKind::parse` 不带「期间内 / 引用存在性 / 精度口径」这些业务规则）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "object_no",
        header: HEADER_OBJECT_NO,
        required: true,
        kind: ColumnKind::Text { max_len: Some(32) },
        unique_key: false,
        fk: None,
        example: "CB-20261101-000001",
        width: Some(24),
        remark: "必须是已存在的成本对象号且状态为 OPEN（未关批）；不存在不会自动建",
    },
    ImportColumn {
        key: "work_date",
        header: HEADER_WORK_DATE,
        required: true,
        kind: ColumnKind::Date,
        unique_key: false,
        fk: None,
        example: "2026-11-05",
        width: Some(14),
        remark: "必须落在所选会计期间内（跨月补录请把期间选成工作日期所在月）",
    },
    ImportColumn {
        key: "employee_no",
        header: HEADER_EMPLOYEE_NO,
        required: false,
        kind: ColumnKind::Text { max_len: Some(64) },
        unique_key: false,
        fk: None,
        example: "",
        width: Some(16),
        remark: "留空＝按班组计（employee_id 为空）；填了则必须能在员工主数据里解析到",
    },
    ImportColumn {
        key: "employee_name",
        header: HEADER_EMPLOYEE_NAME,
        required: true,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "张三",
        width: Some(14),
        remark: "姓名快照：用户改名 / 删号后要能回溯「当时是谁」",
    },
    ImportColumn {
        key: "dept",
        header: HEADER_DEPT,
        required: true,
        kind: ColumnKind::Text { max_len: Some(120) },
        unique_key: false,
        fk: None,
        example: "生产一部",
        width: Some(20),
        remark: "部门编码或部门名称二选一；同名的多个部门请改填部门编码",
    },
    ImportColumn {
        key: "hours",
        header: HEADER_HOURS,
        required: true,
        kind: ColumnKind::Decimal {
            scale: HOURS_SCALE,
            min: Some(0.01),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "8",
        width: Some(12),
        remark: "人工工时，> 0，最多 2 位小数（超出直接报错，不静默四舍五入）",
    },
    ImportColumn {
        key: "machine_hours",
        header: HEADER_MACHINE_HOURS,
        required: false,
        kind: ColumnKind::Decimal {
            scale: HOURS_SCALE,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "6.5",
        width: Some(12),
        remark: "机器工时，≥ 0，可空（按机器工时分摊制造费用时有意义）",
    },
    ImportColumn {
        key: "rate",
        header: HEADER_RATE,
        required: false,
        kind: ColumnKind::Decimal {
            scale: RATE_SCALE,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "50",
        width: Some(12),
        remark: "工资率（元/小时），≥ 0，最多 6 位小数；留空 = 按「成本对象 > 车间 > 公司」自动取数（缺费率该行报错，不会按 0 计）",
    },
];

/// 模板文件名（中文，由 handler 做 RFC 5987 编码）。
pub const TEMPLATE_FILE_NAME: &str = "工时台账导入模板_v1.xlsx";

/// 模板「字段说明」sheet 的表头。
const NOTE_HEADERS: [&str; 6] = ["字段名", "是否必填", "类型", "长度/范围", "可选值", "说明"];

/// 模板「填写须知」sheet 的固定文案。
const GUIDE_LINES: [&str; 7] = [
    "1. 请勿修改第 1 行表头，勿增删列（表头带 * 的为必填列）；",
    "2. 第 2 行是示例，导入前请删除（系统也会自动识别并跳过完全相同的示例行）；",
    "3. 单次最多 5000 行，超出请按月 / 按车间拆分；",
    "4. 导入是全有或全无：任何一行有错，整批都不入库，错误按「Excel 行号 + 列名 + 原因」返回；",
    "5. 日期必须落在所选会计期间内；成本对象号必须存在且状态为 OPEN（未关批）；",
    "6. 工资率可留空（按「成本对象 > 车间 > 公司」自动取数，三级都没配到则该行报错）；员工工号可留空（按班组计）；",
    "7. 保存为 .xlsx 格式上传（不支持 .xls 旧格式）。",
];

// ==================== 出参（设计 §5.2 #15 的形状） ====================

/// 一行错误：`line` = Excel 真实行号，`column` = 中文列名，`message` = 人话原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportIssue {
    /// Excel 真实行号（表头是 1，第一条数据是 2）。
    pub line: u32,
    /// 中文列名（整行级错误时为 `-`）。
    pub column: String,
    /// 原因（人能看懂、能据此改数）。
    pub message: String,
}

impl ImportIssue {
    /// 造一条列级错误。
    pub fn new(line: u32, column: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            line,
            column: column.into(),
            message: message.into(),
        }
    }

    /// 造一条整行级错误（不属于某一列，如「这一行整行为空」）。
    pub fn row_level(line: u32, message: impl Into<String>) -> Self {
        Self::new(line, "-", message)
    }
}

/// 导入结果（设计 §5.2 #15）。
///
/// * 全部成功 → `success = n, failed = 0, rows = []`；
/// * 有任何一行失败 → `success = 0, failed = 有错的行数, rows = 全部错误`（**一条都没入库**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CostLaborImportRes {
    /// 成功入库的行数（全有或全无：要么 0，要么等于文件数据行数）。
    pub success: usize,
    /// 有错的行数（按行去重）。
    pub failed: usize,
    /// 逐行错误（按行号、再按列顺序）。成功时为空。
    pub rows: Vec<ImportIssue>,
}

impl CostLaborImportRes {
    /// 全成功。
    pub fn done(success: usize) -> Self {
        Self {
            success,
            failed: 0,
            rows: Vec::new(),
        }
    }

    /// 整批拒绝（一条都没入库）。
    pub fn rejected(issues: Vec<ImportIssue>) -> Self {
        let failed = issues
            .iter()
            .map(|issue| issue.line)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        Self {
            success: 0,
            failed,
            rows: issues,
        }
    }
}

// ==================== ① 解析 ====================

/// 一行原始单元格文本（未做任何业务校验）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawRow {
    /// Excel 真实行号。
    pub line: u32,
    /// 成本对象号原文。
    pub object_no: String,
    /// 日期原文。
    pub work_date: String,
    /// 员工工号原文。
    pub employee_no: String,
    /// 员工姓名原文。
    pub employee_name: String,
    /// 工作中心原文。
    pub dept: String,
    /// 人工工时原文。
    pub hours: String,
    /// 机器工时原文。
    pub machine_hours: String,
    /// 工资率原文。
    pub rate: String,
}

/// 解析 xlsx 字节流 → 原始行（结构错整表拒绝，行错留给 [`parse_rows`]）。
///
/// # 返回值
///
/// 从 Excel 第 2 行开始的数据行；空行与「与模板示例完全一致」的示例行被跳过。
///
/// # 异常情况
///
/// - 400 + `61001`：不是 xlsx / 旧版 xls / 文件损坏 / 没有工作表；
/// - 422 + `61001`：只有表头没有数据行 / 表头缺必需列；
/// - 422 + `61002`：数据行超过 [`MAX_ROWS`]。
pub fn read_rows(bytes: &[u8]) -> std::result::Result<Vec<RawRow>, ApiError> {
    let reader = SheetReader::open(bytes, COLUMNS).map_err(|error| error.into_api_error())?;
    let mut rows = Vec::new();
    for (index, raw) in reader.data_rows().enumerate() {
        if SheetReader::is_blank(raw) || reader.is_example_row(raw) {
            continue;
        }
        rows.push(RawRow {
            line: SheetReader::excel_row_no(index),
            object_no: cell(&reader, raw, 0),
            work_date: cell(&reader, raw, 1),
            employee_no: cell(&reader, raw, 2),
            employee_name: cell(&reader, raw, 3),
            dept: cell(&reader, raw, 4),
            hours: cell(&reader, raw, 5),
            machine_hours: cell(&reader, raw, 6),
            rate: cell(&reader, raw, 7),
        });
        if rows.len() > MAX_ROWS {
            return Err(ApiError::new(
                422,
                error_code::IMPORT_ROW_LIMIT,
                format!("单次最多导入 {MAX_ROWS} 行，请按月 / 按车间拆分后再上传"),
            ));
        }
    }
    if rows.is_empty() {
        return Err(ApiError::new(
            422,
            error_code::IMPORT_FILE_INVALID,
            "文件里没有可导入的数据行（空行与模板示例行会被跳过），请填写后重新上传",
        ));
    }
    Ok(rows)
}

/// 取第 `index` 个定义列的单元格文本（trim 过）。
fn cell(reader: &SheetReader, row: &[calamine::Data], index: usize) -> String {
    reader.cell(row, index).trim().to_string()
}

// ==================== ② 逐行纯校验 ====================

/// 一行解析后的结果。
///
/// 不变量：`issues` 为空 ⇔ 所有 `Option` 字段都是 `Some`
/// （`rate` 例外：留空的行要等 [`attach_rate_issues`] 取数后才变 `Some`，
/// 由 [`build_specs`] 在落库前再核一次）。
#[derive(Debug, Clone)]
pub struct ParsedRow {
    /// Excel 真实行号。
    pub line: u32,
    /// 成本对象号。
    pub object_no: String,
    /// 工作中心（编码或名称）。
    pub dept: String,
    /// 员工工号（可空）。
    pub employee_no: Option<String>,
    /// 员工姓名。
    pub employee_name: Option<String>,
    /// 工作日期。
    pub work_date: Option<NaiveDate>,
    /// 会计期间（`work_date` 所属月或调用方指定的期间）。
    pub period_code: Option<String>,
    /// 人工工时。
    pub hours: Option<Decimal>,
    /// 机器工时（缺省 0）。
    pub machine_hours: Option<Decimal>,
    /// 工资率（留空 ⇒ 引用校验后自动取数，见 [`attach_rate_issues`]）。
    pub rate: Option<Decimal>,
    /// 工资率列是否**留空**（留空才自动取数；填错数字不走取数，直接报格式错）。
    pub rate_blank: bool,
    /// 这一行已经发现的问题。
    pub issues: Vec<ImportIssue>,
}

impl ParsedRow {
    /// 这一行是否已经干净（没有语法层问题）。
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    /// 追加一条列级错误。
    pub fn push_issue(&mut self, column: &str, message: impl Into<String>) {
        self.issues.push(ImportIssue::new(self.line, column, message));
    }

    /// 组装落库规格（**只在 `issues` 为空时调用**；引用 id 由调用方查库给出）。
    ///
    /// # 异常情况
    ///
    /// 不变量被破坏时 panic（属于编程错误，不是用户输入错误）。
    pub fn to_spec(&self, object_id: Uuid, dept_id: Uuid, dept_name: &str, employee_id: Option<Uuid>) -> CostLaborSpec {
        CostLaborSpec {
            period_code: self.period_code.clone().expect("校验通过的行一定有期间"),
            work_date: self.work_date.expect("校验通过的行一定有日期"),
            cost_object_id: object_id,
            employee_id,
            employee_name: self.employee_name.clone().expect("校验通过的行一定有姓名"),
            dept_id,
            dept_name: dept_name.to_string(),
            hours: self.hours.expect("校验通过的行一定有人工工时"),
            machine_hours: self.machine_hours.unwrap_or(Decimal::ZERO),
            rate: self.rate.expect("校验阶段已取数（留空的行由 attach_rate_issues 回填）"),
            amount: cost_labor_dto::expected_amount(
                self.hours.expect("校验通过的行一定有人工工时"),
                self.rate.expect("校验阶段已取数"),
            )
            .expect("校验通过的行一定能算出金额"),
            // 导入写 IMPORT（设计 §3.1 ④ 值域），状态落 DRAFT 等人工复核。
            source_type: cost_labor_dto::SOURCE_IMPORT.to_string(),
            status: STATUS_DRAFT.to_string(),
            remark: Some(format!("Excel 导入（第 {} 行）", self.line)),
        }
    }
}

/// 逐行纯校验（**不碰库**）：格式 / 必填 / 期间 / 精度。
///
/// # 参数
///
/// - `rows`：[`read_rows`] 的输出。
/// - `period_code`：调用方指定的会计期间（`yyyy-MM`）；`None` 表示按每行日期自动归属。
///
/// # 返回值
///
/// 与输入等长、同序的解析结果；发现的问题都写在各自的 `issues` 里（**不中断**，
/// 一次把能报的都报给用户）。
pub fn parse_rows(rows: &[RawRow], period_code: Option<&str>) -> Vec<ParsedRow> {
    rows.iter().map(|raw| parse_row(raw, period_code)).collect()
}

/// 单行解析。
fn parse_row(raw: &RawRow, period_code: Option<&str>) -> ParsedRow {
    let mut parsed = ParsedRow {
        line: raw.line,
        object_no: raw.object_no.trim().to_string(),
        dept: raw.dept.trim().to_string(),
        employee_no: None,
        employee_name: None,
        work_date: None,
        period_code: None,
        hours: None,
        machine_hours: None,
        rate: None,
        rate_blank: false,
        issues: Vec::new(),
    };

    // ---------- 成本对象号 ----------
    if parsed.object_no.is_empty() {
        parsed.push_issue(
            HEADER_OBJECT_NO,
            "不能为空：每一行工时都必须挂到一个成本对象上（不存在不会自动建）",
        );
    } else if parsed.object_no.chars().count() > 32 {
        // `biz_cost_object.object_no` 是 VARCHAR(32)：超长让库抛 22001 只会得到英文报错
        parsed.push_issue(HEADER_OBJECT_NO, "成本对象号最长 32 个字符（与库里的编号列一致）");
    }

    // ---------- 日期 + 期间 ----------
    if raw.work_date.trim().is_empty() {
        parsed.push_issue(HEADER_WORK_DATE, "不能为空（格式 YYYY-MM-DD，Excel 日期单元格也可以）");
    } else {
        match parse_date(raw.work_date.trim()) {
            Some(date) => {
                parsed.work_date = Some(date);
                let month = date.format("%Y-%m").to_string();
                match period_code {
                    Some(period) => {
                        if month != period {
                            parsed.push_issue(
                                HEADER_WORK_DATE,
                                format!("{date} 不在本次导入的会计期间 {period} 内：工时按会计期间归集，跨月补录请把期间选成 {month}"),
                            );
                        }
                        parsed.period_code = Some(period.to_string());
                    }
                    None => parsed.period_code = Some(month),
                }
            }
            None => parsed.push_issue(
                HEADER_WORK_DATE,
                format!(
                    "「{}」不是合法日期（要 YYYY-MM-DD / YYYY/MM/DD，或 Excel 日期单元格）",
                    raw.work_date.trim()
                ),
            ),
        }
    }

    // ---------- 员工工号（可空） ----------
    parsed.employee_no = cost_labor_dto::clean_optional(Some(raw.employee_no.clone()));
    if let Some(no) = parsed.employee_no.as_deref()
        && no.chars().count() > 64
    {
        parsed.push_issue(HEADER_EMPLOYEE_NO, "最多 64 个字符");
    }

    // ---------- 员工姓名（必填，快照） ----------
    match cost_labor_dto::clean_optional(Some(raw.employee_name.clone())) {
        None => parsed.push_issue(
            HEADER_EMPLOYEE_NAME,
            "不能为空：成本记录要留「当时是谁」的姓名快照（按班组计也请填班组名）",
        ),
        Some(name) => {
            if name.chars().count() > EMPLOYEE_NAME_MAX {
                parsed.push_issue(
                    HEADER_EMPLOYEE_NAME,
                    format!("最长 {EMPLOYEE_NAME_MAX} 个字符（收到 {} 个）", name.chars().count()),
                );
            } else {
                parsed.employee_name = Some(name);
            }
        }
    }

    // ---------- 工作中心 ----------
    if parsed.dept.is_empty() {
        parsed.push_issue(
            HEADER_DEPT,
            "不能为空：必须是 sys_dept 里现有的部门（填部门编码或部门名称）",
        );
    } else if parsed.dept.chars().count() > DEPT_NAME_MAX {
        parsed.push_issue(HEADER_DEPT, format!("最长 {DEPT_NAME_MAX} 个字符"));
    }

    // ---------- 人工工时 ----------
    parsed.hours = numeric_column(
        &mut parsed.issues,
        raw.line,
        HEADER_HOURS,
        &raw.hours,
        true,
        HOURS_SCALE,
        HOURS_MAX,
        false,
    );

    // ---------- 机器工时（可空 → 0） ----------
    parsed.machine_hours = match cost_labor_dto::clean_optional(Some(raw.machine_hours.clone())) {
        None => Some(Decimal::ZERO),
        Some(text) => numeric_column(
            &mut parsed.issues,
            raw.line,
            HEADER_MACHINE_HOURS,
            &text,
            false,
            HOURS_SCALE,
            HOURS_MAX,
            true,
        ),
    };

    // ---------- 工资率（留空 ⇒ 引用校验后按 §4.5.4 自动取数；填了就按填的） ----------
    parsed.rate = match cost_labor_dto::clean_optional(Some(raw.rate.clone())) {
        None => {
            // 留空不是错：取数要对象 id / 车间 id / 工作日期，得等引用校验跑完（attach_rate_issues）
            parsed.rate_blank = true;
            None
        }
        Some(text) => numeric_column(
            &mut parsed.issues,
            raw.line,
            HEADER_RATE,
            &text,
            true,
            RATE_SCALE,
            RATE_MAX,
            true,
        ),
    };

    parsed
}

/// 解析一个数值列：非法 → 记错误并返回 `None`；精度 / 范围复用 dto 的唯一口径。
#[allow(clippy::too_many_arguments)]
fn numeric_column(
    issues: &mut Vec<ImportIssue>,
    line: u32,
    column: &str,
    text: &str,
    required: bool,
    scale: u32,
    max: &str,
    allow_zero: bool,
) -> Option<Decimal> {
    let text = text.trim();
    if text.is_empty() {
        if required {
            issues.push(ImportIssue::new(line, column, "不能为空"));
        }
        return None;
    }
    let value = match Decimal::from_str(text) {
        Ok(value) => value,
        Err(_) => {
            issues.push(ImportIssue::new(
                line,
                column,
                format!("「{text}」不是合法数字（不要带千分位逗号或单位）"),
            ));
            return None;
        }
    };
    match cost_labor_dto::ensure_decimal(value, column, scale, max, allow_zero) {
        Ok(value) => Some(value),
        Err(error) => {
            issues.push(ImportIssue::new(line, column, error.msg));
            None
        }
    }
}

// ==================== ③ 引用校验（纯索引部分） ====================

/// 成本对象引用：`object_no → (id, status)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef {
    /// 成本对象主键。
    pub id: Uuid,
    /// 状态（`OPEN` / `CLOSED` / …）。
    pub status: String,
}

/// 部门引用（`sys_dept` 的一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeptRef {
    /// 部门主键。
    pub id: Uuid,
    /// 部门名称（快照来源）。
    pub dept_name: String,
}

/// 部门查表结果（编码 / 名称两种填法都要能区分「没有」与「重名」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeptLookup {
    /// 唯一命中。
    Found(DeptRef),
    /// 没有这个编码 / 名称。
    Missing,
    /// 多个部门同名 —— 必须改填编码，不能猜。
    Ambiguous(usize),
}

/// 把库里的成本对象行索引成 `object_no → ObjectRef`。
pub fn index_objects(rows: Vec<(String, Uuid, String)>) -> HashMap<String, ObjectRef> {
    rows.into_iter()
        .map(|(no, id, status)| (no, ObjectRef { id, status }))
        .collect()
}

/// 把库里的部门行索引成「编码 / 名称 → 部门」，并回答每个待查键的去向。
///
/// 规则（与模板说明一致）：
///
/// 1. **部门编码**精确命中优先（编码全库唯一）；
/// 2. 否则按**部门名称**精确命中；同名多个 → [`DeptLookup::Ambiguous`]（要求改填编码，
///    不按「随便挑一个」处理 —— 猜错车间就是把成本记到别人头上）。
pub fn index_depts(depts: Vec<(Uuid, String, String)>, keys: &[String]) -> HashMap<String, DeptLookup> {
    let mut by_code: HashMap<&str, DeptRef> = HashMap::new();
    let mut by_name: HashMap<&str, Vec<DeptRef>> = HashMap::new();
    for (id, code, name) in depts.iter() {
        let plain = DeptRef {
            id: *id,
            dept_name: name.clone(),
        };
        by_code.entry(code.as_str()).or_insert(plain.clone());
        by_name.entry(name.as_str()).or_default().push(plain);
    }
    keys.iter()
        .map(|key| {
            let found = if let Some(hit) = by_code.get(key.as_str()) {
                DeptLookup::Found(hit.clone())
            } else {
                match by_name.get(key.as_str()) {
                    Some(hits) if hits.len() == 1 => DeptLookup::Found(hits[0].clone()),
                    Some(hits) => DeptLookup::Ambiguous(hits.len()),
                    None => DeptLookup::Missing,
                }
            };
            (key.clone(), found)
        })
        .collect()
}

// ==================== ④ 导入主流程 ====================

/// 导入编排的入参（把「用户选了什么」与「文件里写了什么」分开）。
#[derive(Debug, Clone, Default)]
pub struct ImportContext {
    /// 调用方指定的会计期间（`yyyy-MM`，已归一化）；`None` = 按每行日期归属。
    pub period_code: Option<String>,
    /// 操作人。
    pub operator: Option<Uuid>,
}

/// 导入 xlsx 字节流（**全有或全无**）。
///
/// # 参数
///
/// - `bytes`：上传的 xlsx 内容。
/// - `context`：期间 + 操作人。
///
/// # 返回值
///
/// [`CostLaborImportRes`]：全成功 → `success = 行数`；任何一行有错 → `success = 0`
/// 且 `rows` 是逐行错误（一条都没入库）。
///
/// # 异常情况
///
/// - 400/422：文件结构错（不是 xlsx / 表头缺列 / 没有数据行 / 超行数上限）；
/// - 500：查库 / 写库的意外失败（事务已回滚，库里不会有半批数据）。
pub async fn import_labors(bytes: &[u8], context: &ImportContext) -> std::result::Result<CostLaborImportRes, ApiError> {
    let raw = read_rows(bytes)?;
    let mut parsed = parse_rows(&raw, context.period_code.as_deref());

    // ---------- ③ 引用校验：一次批量查库（不做 N+1），同时拿到 id 索引 ----------
    let refs = validate_refs(&mut parsed).await?;

    // ---------- ④ 有任何一行不干净 ⇒ 整批拒绝 ----------
    let mut issues = parsed
        .iter()
        .flat_map(|row| row.issues.iter().cloned())
        .collect::<Vec<_>>();
    if !issues.is_empty() {
        issues.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| column_rank(&left.column).cmp(&column_rank(&right.column)))
        });
        return Ok(CostLaborImportRes::rejected(issues));
    }

    // ---------- ⑤ 全部干净 ⇒ 单事务批量写入 ----------
    let specs = build_specs(&parsed, &refs)?;
    let inserted = write_all(&specs, context.operator).await?;
    Ok(CostLaborImportRes::done(inserted))
}

/// 引用索引（一次查库，供校验与拼装共用 —— 保证「校验看到的」与「写入用的」是同一批行）。
#[derive(Debug, Default)]
pub struct RefIndex {
    /// `object_no → (id, status)`。
    pub objects: HashMap<String, ObjectRef>,
    /// 部门键（编码或名称）→ 部门。
    pub depts: HashMap<String, DeptRef>,
    /// `employee_no → id`。
    pub employees: HashMap<String, Uuid>,
}

/// 引用校验：成本对象存在且 `OPEN`、部门唯一可解析、工号可解析（表存在时）。
///
/// 发现的问题追加到各行的 `issues`，**不中断**（一次把能报的都报出来）。
async fn validate_refs(parsed: &mut [ParsedRow]) -> std::result::Result<RefIndex, ApiError> {
    let mut refs = RefIndex::default();
    attach_object_issues(parsed, &mut refs).await?;
    attach_dept_issues(parsed, &mut refs).await?;
    attach_employee_issues(parsed, &mut refs).await?;
    attach_rate_issues(parsed, &refs).await?;
    Ok(refs)
}

/// 列的展示顺序（同一条错误按模板列顺序稳定输出，而不是随机）。
fn column_rank(column: &str) -> usize {
    COLUMNS
        .iter()
        .position(|candidate| candidate.header == column)
        .unwrap_or(COLUMNS.len())
}

/// 成本对象校验：必须存在且 `OPEN`。
async fn attach_object_issues(parsed: &mut [ParsedRow], refs: &mut RefIndex) -> std::result::Result<(), ApiError> {
    let codes = distinct(parsed.iter().map(|row| row.object_no.clone()));
    if codes.is_empty() {
        return Ok(());
    }
    refs.objects = index_objects(
        model::object_refs_by_no(&codes)
            .await
            .map_err(|error| utils::db_error(error, "校验导入的成本对象"))?,
    );
    for row in parsed.iter_mut() {
        if row.object_no.is_empty() {
            continue;
        }
        match refs.objects.get(&row.object_no) {
            None => row.push_issue(
                HEADER_OBJECT_NO,
                format!(
                    "成本对象「{}」不存在（或已删除）：导入不会自动建对象，请先在成本对象页建批后再导",
                    row.object_no
                ),
            ),
            Some(object) if object.status != cost_object_dto::STATUS_OPEN => row.push_issue(
                HEADER_OBJECT_NO,
                format!(
                    "成本对象「{}」当前状态是 {}，只有 OPEN（未关批）的对象能记工时 —— 已关批的成本已结转，改它会让账对不上",
                    row.object_no,
                    cost_labor_dto::object_status_label(&object.status)
                ),
            ),
            Some(_) => {}
        }
    }
    Ok(())
}

/// 部门校验：必须能唯一解析到一个 `sys_dept` 行。
async fn attach_dept_issues(parsed: &mut [ParsedRow], refs: &mut RefIndex) -> std::result::Result<(), ApiError> {
    let keys = distinct(parsed.iter().map(|row| row.dept.clone()));
    if keys.is_empty() {
        return Ok(());
    }
    let lookups = index_depts(
        model::dept_refs_by_keys(&keys)
            .await
            .map_err(|error| utils::db_error(error, "校验导入的工作中心"))?,
        &keys,
    );
    for row in parsed.iter_mut() {
        if row.dept.is_empty() {
            continue;
        }
        match lookups.get(&row.dept) {
            Some(DeptLookup::Found(dept)) => {
                refs.depts.insert(row.dept.clone(), dept.clone());
            }
            Some(DeptLookup::Ambiguous(count)) => row.push_issue(
                HEADER_DEPT,
                format!(
                    "有 {count} 个部门都叫「{}」，请改填部门编码（唯一）以免工时记错车间",
                    row.dept
                ),
            ),
            _ => row.push_issue(
                HEADER_DEPT,
                format!("工作中心「{}」在部门表里找不到（可填部门编码或部门名称）", row.dept),
            ),
        }
    }
    Ok(())
}

/// 员工校验：`biz_employee`（ARCH9-2）未上线时工号必须留空；上线后必须能解析。
async fn attach_employee_issues(parsed: &mut [ParsedRow], refs: &mut RefIndex) -> std::result::Result<(), ApiError> {
    let numbers = distinct(parsed.iter().filter_map(|row| row.employee_no.clone()));
    if numbers.is_empty() {
        return Ok(());
    }
    let present = model::employee_table_present()
        .await
        .map_err(|error| utils::db_error(error, "检查员工主数据表"))?;
    if !present {
        for row in parsed.iter_mut() {
            if row.employee_no.is_some() {
                row.push_issue(
                    HEADER_EMPLOYEE_NO,
                    "员工主数据（ARCH9-2 人力资源）尚未上线，工号无法解析：请留空按班组计（姓名列照填），或等 ARCH9-2 上线后再填",
                );
            }
        }
        return Ok(());
    }
    refs.employees = model::employee_ids_by_no(&numbers)
        .await
        .map_err(|error| utils::db_error(error, "校验导入的员工工号"))?
        .into_iter()
        .collect::<HashMap<String, Uuid>>();
    for row in parsed.iter_mut() {
        if let Some(no) = row.employee_no.as_deref()
            && !refs.employees.contains_key(no)
        {
            row.push_issue(
                HEADER_EMPLOYEE_NO,
                format!("员工工号「{no}」在员工主数据里不存在：请核对工号，或留空按班组计"),
            );
        }
    }
    Ok(())
}

/// 工资率取数（B2-3）：**留空的行**按 `成本对象 > 车间 > 公司` 自动取（设计 §4.5.4）。
///
/// 为什么放在引用校验之后：取数要「对象的 id + 车间的 id + 工作日期」，
/// 而这三样要等 `attach_object_issues` / `attach_dept_issues` 解析完才有。
///
/// 一次查库（[`cost_rate_handler::candidates`]），逐行用纯函数判定 —— 5000 行导入
/// 也只有 1 次往返。三级都没配到 ⇒ 该行在「工资率」列报错（`66059` 的同一条人话），
/// 整批照样「全有或全无」拒绝，**不按 0 计**。
async fn attach_rate_issues(parsed: &mut [ParsedRow], refs: &RefIndex) -> std::result::Result<(), ApiError> {
    // 没有留空的行就一次库都不查（填了费率的导入与本卡无关）
    let needs_rate = parsed
        .iter()
        .any(|row| row.rate_blank && row.rate.is_none() && row.issues.is_empty());
    if !needs_rate {
        return Ok(());
    }
    let rates = crate::business::erp::cost_rate_handler::candidates(cost_rate_dto::RATE_TYPE_LABOR).await?;
    for row in parsed.iter_mut() {
        // 已经有错的行不去取数：它的对象 / 车间 / 日期本身可能就不成立，
        // 拿它去查费率只会再刷一条噪音错误（真正要改的只有第一条）
        if !row.rate_blank || row.rate.is_some() || !row.issues.is_empty() {
            continue;
        }
        let (Some(work_date), Some(object), Some(dept)) = (
            row.work_date,
            refs.objects.get(&row.object_no),
            refs.depts.get(&row.dept),
        ) else {
            // 引用没解析出来 ⇒ 该行已有 issue（上面跳过不了到这里），兜底不 panic
            continue;
        };
        match cost_rate_dto::resolve(
            &rates,
            &RateRequest::labor(object.id, dept.id, work_date),
            &RateSubject {
                cost_object_no: &row.object_no,
                dept_name: &dept.dept_name,
            },
        ) {
            Ok(resolved) => row.rate = Some(resolved.rate),
            // 报错文案与单条录入完全一致（同一个纯函数产出）
            Err(error) => row.push_issue(HEADER_RATE, error.msg),
        }
    }
    Ok(())
}

/// 组装落库规格（引用索引来自 [`validate_refs`]，此处同步拼装，不再查库）。
fn build_specs(parsed: &[ParsedRow], refs: &RefIndex) -> std::result::Result<Vec<(Uuid, CostLaborSpec)>, ApiError> {
    let mut out = Vec::with_capacity(parsed.len());
    for row in parsed {
        let object = refs.objects.get(&row.object_no).ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("成本对象引用在拼装阶段丢失：{}", row.object_no),
            )
        })?;
        let dept = refs.depts.get(&row.dept).ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("部门引用在拼装阶段丢失：{}", row.dept),
            )
        })?;
        let employee_id = row
            .employee_no
            .as_deref()
            .and_then(|no| refs.employees.get(no).copied());
        // 留空的费率必须已经在 attach_rate_issues 取到；走到这里还空说明编排漏了一步
        // （编程错误：宁可 500 让人看见，也不许按 0 落库）
        if row.rate.is_none() {
            return Err(ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!(
                    "第 {} 行的工资率取数结果在拼装阶段丢失（本应已按「成本对象 > 车间 > 公司」取到）",
                    row.line
                ),
            ));
        }
        out.push((
            Uuid::now_v7(),
            row.to_spec(object.id, dept.id, &dept.dept_name, employee_id),
        ));
    }
    Ok(out)
}

/// 单事务批量写入（失败整批回滚，不留半批数据）。
async fn write_all(specs: &[(Uuid, CostLaborSpec)], operator: Option<Uuid>) -> std::result::Result<usize, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启工时导入事务"))?;
    let outcome = model::insert_batch(&tx, specs, operator).await;
    match outcome {
        Ok(affected) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交工时导入事务"))?;
            tracing::info!(rows = affected, "工时台账 xlsx 导入完成");
            Ok(usize::try_from(affected).unwrap_or(0))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            // 金额 / 精度类约束命中 → 与单条录入同一套人话（66058）；其余按通用口径翻译。
            Err(match error {
                DbError::CheckViolation { .. } => ApiError::new(
                    400,
                    cost_labor_dto::AMOUNT_CODE,
                    "导入的某一行的「工时 × 工资率」算出的金额与库里的 CHECK 不符（通常是有行的小数位超了列的精度），\
                     整批已回滚，请检查人工工时 / 工资率的小数位后重试",
                ),
                other => utils::db_error(other, "写入工时导入结果"),
            })
        }
    }
}

/// 去重并保持首次出现顺序（`Vec<String>` → 去重集合，供批量查库用）。
fn distinct(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for value in values {
        if value.is_empty() {
            continue;
        }
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

// ==================== 模板 ====================

/// 模板的 8 个中文表头（不带 `*`，即 `SheetReader` 归一化后的样子）。
pub fn column_headers() -> Vec<&'static str> {
    COLUMNS.iter().map(|column| column.header).collect()
}

/// 把「一行行文本」写成与模板同表头的 xlsx（8 列，第 1 行表头）。
///
/// 用途：真库用例与运维自检要造真实的上传文件 —— 自己写一份 xlsx 再读回来，
/// 才能验「解析 → 校验 → 入库」整条链路（而不是只验纯函数）。
///
/// # 异常情况
///
/// 写入失败返回 500。
pub fn build_rows_xlsx(rows: &[RawRow]) -> std::result::Result<Vec<u8>, ApiError> {
    let headers = column_headers();
    let maps = rows
        .iter()
        .map(|row| {
            let mut map = std::collections::HashMap::new();
            map.insert(HEADER_OBJECT_NO.to_string(), row.object_no.clone());
            map.insert(HEADER_WORK_DATE.to_string(), row.work_date.clone());
            map.insert(HEADER_EMPLOYEE_NO.to_string(), row.employee_no.clone());
            map.insert(HEADER_EMPLOYEE_NAME.to_string(), row.employee_name.clone());
            map.insert(HEADER_DEPT.to_string(), row.dept.clone());
            map.insert(HEADER_HOURS.to_string(), row.hours.clone());
            map.insert(HEADER_MACHINE_HOURS.to_string(), row.machine_hours.clone());
            map.insert(HEADER_RATE.to_string(), row.rate.clone());
            map
        })
        .collect::<Vec<_>>();
    crate::business::impexp::engine::write_sheet(&headers, &maps)
}

/// 生成 8 列导入模板 xlsx（三个 sheet：数据 / 字段说明 / 填写须知）。
///
/// # 返回值
///
/// xlsx 字节流。
///
/// # 异常情况
///
/// 写入失败返回 500。
pub fn build_template() -> std::result::Result<Vec<u8>, ApiError> {
    let headers = COLUMNS.iter().map(ImportColumn::template_header).collect::<Vec<_>>();
    let mut example = std::collections::HashMap::new();
    for column in COLUMNS {
        example.insert(column.template_header(), column.example.to_string());
    }
    let example_rows = vec![example];

    let note_rows = COLUMNS
        .iter()
        .map(|column| {
            let mut row = std::collections::HashMap::new();
            row.insert("字段名".to_string(), column.template_header());
            row.insert(
                "是否必填".to_string(),
                if column.required { "是" } else { "否" }.to_string(),
            );
            row.insert("类型".to_string(), column.kind.type_label().to_string());
            row.insert("长度/范围".to_string(), column.kind.rule_text());
            row.insert("可选值".to_string(), column.kind.option_text());
            row.insert("说明".to_string(), column.remark.to_string());
            row
        })
        .collect::<Vec<_>>();

    let guide_rows = GUIDE_LINES
        .iter()
        .map(|line| {
            let mut row = std::collections::HashMap::new();
            row.insert("填写须知".to_string(), (*line).to_string());
            row
        })
        .collect::<Vec<_>>();

    let header_refs = headers.iter().map(String::as_str).collect::<Vec<_>>();
    write_workbook(&[
        WorkbookSheet::new("数据", header_refs.as_slice(), example_rows.as_slice()),
        WorkbookSheet::new("字段说明", NOTE_HEADERS.as_slice(), note_rows.as_slice()),
        WorkbookSheet::new("填写须知", &["填写须知"], guide_rows.as_slice()),
    ])
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(line: u32) -> RawRow {
        RawRow {
            line,
            object_no: "CB-20261101-000777".to_string(),
            work_date: "2026-11-06".to_string(),
            employee_no: String::new(),
            employee_name: "李四".to_string(),
            dept: "装配车间".to_string(),
            hours: "7.5".to_string(),
            machine_hours: "3".to_string(),
            rate: "48.5".to_string(),
        }
    }

    /// 与模板示例值**逐列一致**的一行（用来验「照抄示例行会被跳过」）。
    fn example_row(line: u32) -> RawRow {
        RawRow {
            line,
            object_no: COLUMNS[0].example.to_string(),
            work_date: COLUMNS[1].example.to_string(),
            employee_no: COLUMNS[2].example.to_string(),
            employee_name: COLUMNS[3].example.to_string(),
            dept: COLUMNS[4].example.to_string(),
            hours: COLUMNS[5].example.to_string(),
            machine_hours: COLUMNS[6].example.to_string(),
            rate: COLUMNS[7].example.to_string(),
        }
    }

    fn messages(row: &ParsedRow) -> Vec<String> {
        row.issues
            .iter()
            .map(|issue| format!("{}|{}", issue.column, issue.message))
            .collect()
    }

    // ---------- 模板 ----------

    #[test]
    fn template_has_eight_columns_in_design_order() {
        assert_eq!(COLUMNS.len(), 8, "设计 §4.5.3 是 8 列");
        let headers = COLUMNS.iter().map(|column| column.header).collect::<Vec<_>>();
        assert_eq!(
            headers,
            vec![
                "成本对象号",
                "日期",
                "员工工号",
                "员工姓名",
                "工作中心（部门）",
                "人工工时",
                "机器工时",
                "工资率"
            ]
        );
        // 必填口径与 §4.5.3 的表格一致（工资率在设计里是可空 → 自动取数，本卡暂必填但表头不算 *）
        let required = COLUMNS.iter().map(|column| column.required).collect::<Vec<_>>();
        assert_eq!(required, vec![true, true, false, true, true, true, false, false]);
    }

    #[test]
    fn template_xlsx_can_be_read_back_by_the_same_reader() {
        let bytes = build_template().expect("生成模板");
        assert_eq!(&bytes[..4], b"PK\x03\x04", "必须是 xlsx（zip）容器");
        // 模板只有示例行（会被 is_example_row 跳过）⇒ 读回来「没有数据行」
        let error = read_rows(&bytes).expect_err("只有示例行应报没有数据行");
        assert_eq!(error.status, 422);
        assert!(error.msg.contains("没有可导入的数据行"), "{}", error.msg);
    }

    // ---------- xlsx 往返（真解析，不只测纯函数） ----------

    #[test]
    fn xlsx_round_trip_keeps_every_column_and_skips_nothing() {
        let mut second = raw(3);
        second.employee_no = "E1001".to_string();
        second.machine_hours = String::new();
        let bytes = build_rows_xlsx(&[raw(2), second]).expect("造 xlsx");
        let rows = read_rows(&bytes).expect("读回来");
        assert_eq!(rows.len(), 2, "两行数据都要读回来（不能被当成示例行跳过）");
        assert_eq!(rows[0].line, 2, "行号是 Excel 真实行号");
        assert_eq!(rows[1].line, 3);
        assert_eq!(rows[0].object_no, "CB-20261101-000777");
        assert_eq!(rows[0].work_date, "2026-11-06");
        assert_eq!(rows[0].employee_name, "李四");
        assert_eq!(rows[0].dept, "装配车间");
        assert_eq!(rows[0].hours, "7.5");
        assert_eq!(rows[0].machine_hours, "3");
        assert_eq!(rows[0].rate, "48.5");
        assert_eq!(rows[1].employee_no, "E1001");
    }

    #[test]
    fn template_example_row_is_skipped_but_real_rows_are_not() {
        // 只有示例行 ⇒ 与模板一模一样，报「没有数据行」
        let bytes = build_rows_xlsx(&[example_row(2)]).expect("造 xlsx");
        assert!(read_rows(&bytes).is_err(), "照抄示例行必须被跳过");

        // 示例行 + 一行真数据 ⇒ 只剩真数据那一行
        let bytes = build_rows_xlsx(&[example_row(2), raw(3)]).expect("造 xlsx");
        let rows = read_rows(&bytes).expect("读回来");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, 3, "示例行被跳过，行号仍以 Excel 为准");
        assert_eq!(rows[0].employee_name, "李四");
    }

    #[test]
    fn xlsx_round_trip_surfaces_row_level_errors_with_excel_line_numbers() {
        // `build_rows_xlsx` 按数组顺序写字（`RawRow::line` 只是元数据），
        // 所以第 2 条数据行 = Excel 第 3 行。
        let mut bad = raw(3);
        bad.hours = "8.555".to_string();
        let bytes = build_rows_xlsx(&[raw(2), bad, raw(4)]).expect("造 xlsx");
        let parsed = parse_rows(&read_rows(&bytes).expect("读回来"), Some("2026-11"));
        let dirty = parsed.iter().filter(|row| !row.is_clean()).collect::<Vec<_>>();
        assert_eq!(dirty.len(), 1, "只有第 2 条数据行有错");
        assert_eq!(dirty[0].line, 3, "报错行号要对得上 Excel 第 3 行");
        assert!(dirty[0].issues[0].message.contains("最多 2 位小数"));
    }

    #[test]
    fn xlsx_with_missing_required_header_is_rejected_as_structure_error() {
        // 少一列（把「人工工时」整列去掉）
        let headers = [
            "成本对象号",
            "日期",
            "员工工号",
            "员工姓名",
            "工作中心（部门）",
            "机器工时",
            "工资率",
        ];
        let mut row = std::collections::HashMap::new();
        row.insert("成本对象号".to_string(), "CB-1".to_string());
        let bytes = crate::business::impexp::engine::write_sheet(&headers, &[row]).expect("造 xlsx");
        let error = read_rows(&bytes).expect_err("缺必需列必须整表拒绝");
        assert_eq!(error.status, 422);
        assert!(error.msg.contains("人工工时"), "要点名缺哪一列：{}", error.msg);
    }

    // ---------- 逐行校验 ----------
    #[test]
    fn clean_row_parses_into_expected_numbers() {
        let rows = parse_rows(&[raw(2)], None);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_clean(), "{:?}", messages(&rows[0]));
        assert_eq!(rows[0].period_code.as_deref(), Some("2026-11"), "期间按日期自动归属");
        assert_eq!(rows[0].hours, Some(Decimal::from_str("7.5").unwrap()));
        assert_eq!(rows[0].machine_hours, Some(Decimal::from_str("3").unwrap()));
        assert_eq!(rows[0].employee_no, None, "工号留空 = 按班组计");
    }

    #[test]
    fn machine_hours_is_optional_and_defaults_to_zero() {
        let mut row = raw(2);
        row.machine_hours = String::new();
        let parsed = parse_rows(&[row], None);
        assert!(parsed[0].is_clean(), "{:?}", messages(&parsed[0]));
        assert_eq!(parsed[0].machine_hours, Some(Decimal::ZERO));
    }

    #[test]
    fn date_outside_selected_period_is_rejected_with_month_hint() {
        let rows = parse_rows(&[raw(2)], Some("2026-10"));
        assert!(!rows[0].is_clean());
        let text = messages(&rows[0]).join(" / ");
        assert!(text.contains(HEADER_WORK_DATE), "{text}");
        assert!(text.contains("不在本次导入的会计期间 2026-10 内"), "{text}");
        assert!(text.contains("请把期间选成 2026-11"), "要给下一步动作：{text}");
    }

    #[test]
    fn empty_rate_is_marked_for_auto_lookup_not_reported_as_error() {
        let mut row = raw(2);
        row.rate = String::new();
        let parsed = parse_rows(&[row], None);
        // B2-3 之后：留空 = 待取数，语法层不该报错（取数在引用校验之后，三级全空才报 66059）
        assert!(parsed[0].is_clean(), "{:?}", messages(&parsed[0]));
        assert_eq!(parsed[0].rate, None);
        assert!(parsed[0].rate_blank, "留空的行必须打上「待取数」标记");
    }

    /// 填了数字但不是合法费率 ⇒ 照旧报格式错，**不走**自动取数（错数字不能被取数掩盖）。
    #[test]
    fn filled_but_invalid_rate_is_still_a_format_error() {
        let mut row = raw(2);
        row.rate = "abc".to_string();
        let parsed = parse_rows(&[row], None);
        assert!(!parsed[0].is_clean());
        assert!(!parsed[0].rate_blank, "填错了不是「留空」，不能自动补");
        let text = messages(&parsed[0]).join(" / ");
        assert!(text.contains(HEADER_RATE), "{text}");
        assert!(text.contains("不是合法数字"), "{text}");
    }

    #[test]
    fn hours_must_be_positive_and_within_two_decimals() {
        for (value, expect) in [("0", "必须大于 0"), ("-1", "不能是负数"), ("8.555", "最多 2 位小数")] {
            let mut row = raw(2);
            row.hours = value.to_string();
            let parsed = parse_rows(&[row], None);
            assert!(!parsed[0].is_clean(), "{value} 应被拒");
            let text = messages(&parsed[0]).join(" / ");
            assert!(text.contains(HEADER_HOURS), "{value}: {text}");
            assert!(text.contains(expect), "{value}: {text}");
        }
    }

    #[test]
    fn rate_rejects_more_than_six_decimals_and_negative() {
        for value in ["50.1234567", "-1"] {
            let mut row = raw(2);
            row.rate = value.to_string();
            let parsed = parse_rows(&[row], None);
            assert!(!parsed[0].is_clean(), "{value} 应被拒");
            assert!(messages(&parsed[0]).join(" / ").contains(HEADER_RATE));
        }
    }

    #[test]
    fn non_numeric_text_is_reported_as_not_a_number() {
        let mut row = raw(2);
        row.hours = "8小时".to_string();
        let parsed = parse_rows(&[row], None);
        assert!(messages(&parsed[0]).join(" / ").contains("不是合法数字"));
    }

    #[test]
    fn bad_date_text_is_reported() {
        let mut row = raw(2);
        row.work_date = "2026-13-01".to_string();
        let parsed = parse_rows(&[row], None);
        assert!(messages(&parsed[0]).join(" / ").contains("不是合法日期"));
    }

    #[test]
    fn required_columns_report_each_own_column() {
        let mut row = raw(2);
        row.object_no = String::new();
        row.employee_name = "  ".to_string();
        row.dept = String::new();
        row.hours = String::new();
        let parsed = parse_rows(&[row], None);
        let columns = parsed[0]
            .issues
            .iter()
            .map(|issue| issue.column.clone())
            .collect::<Vec<_>>();
        assert!(columns.contains(&HEADER_OBJECT_NO.to_string()), "{columns:?}");
        assert!(columns.contains(&HEADER_EMPLOYEE_NAME.to_string()), "{columns:?}");
        assert!(columns.contains(&HEADER_DEPT.to_string()), "{columns:?}");
        assert!(columns.contains(&HEADER_HOURS.to_string()), "{columns:?}");
    }

    #[test]
    fn employee_name_and_dept_respect_column_lengths() {
        let mut row = raw(2);
        row.employee_name = "名".repeat(EMPLOYEE_NAME_MAX + 1);
        let parsed = parse_rows(&[row], None);
        assert!(messages(&parsed[0]).join(" / ").contains("最长 50 个字符"));

        let mut row = raw(2);
        row.dept = "部".repeat(DEPT_NAME_MAX + 1);
        let parsed = parse_rows(&[row], None);
        assert!(messages(&parsed[0]).join(" / ").contains("最长 120 个字符"));
    }

    // ---------- 引用索引 ----------

    #[test]
    fn index_objects_maps_number_to_status() {
        let id = Uuid::now_v7();
        let index = index_objects(vec![("CB-1".to_string(), id, "OPEN".to_string())]);
        assert_eq!(index.get("CB-1").map(|object| object.id), Some(id));
        assert_eq!(index.get("CB-1").map(|object| object.status.as_str()), Some("OPEN"));
        assert!(!index.contains_key("CB-2"));
    }

    #[test]
    fn dept_lookup_prefers_code_then_unique_name_then_ambiguity() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        let depts = vec![
            (a, "D001".to_string(), "生产一部".to_string()),
            (b, "D002".to_string(), "包装车间".to_string()),
            (c, "D003".to_string(), "包装车间".to_string()),
        ];
        let keys = vec![
            "D001".to_string(),
            "生产一部".to_string(),
            "包装车间".to_string(),
            "不存在的部门".to_string(),
        ];
        let lookups = index_depts(depts, &keys);
        assert_eq!(
            lookups.get("D001"),
            Some(&DeptLookup::Found(DeptRef {
                id: a,
                dept_name: "生产一部".to_string()
            }))
        );
        assert_eq!(
            lookups.get("生产一部"),
            Some(&DeptLookup::Found(DeptRef {
                id: a,
                dept_name: "生产一部".to_string()
            })),
            "编码与名称同属一个部门时两种填法都要命中"
        );
        assert_eq!(
            lookups.get("包装车间"),
            Some(&DeptLookup::Ambiguous(2)),
            "重名必须让人改填编码"
        );
        assert_eq!(lookups.get("不存在的部门"), Some(&DeptLookup::Missing));
    }

    // ---------- 结果形状 ----------

    #[test]
    fn rejected_result_counts_distinct_lines_and_never_claims_success() {
        let res = CostLaborImportRes::rejected(vec![
            ImportIssue::new(3, HEADER_HOURS, "x"),
            ImportIssue::new(3, HEADER_RATE, "y"),
            ImportIssue::new(5, HEADER_DEPT, "z"),
        ]);
        assert_eq!(res.success, 0, "全有或全无：一行有错就一条都没入库");
        assert_eq!(res.failed, 2, "failed 是行数（行 3 两条错只算一行）");
        assert_eq!(res.rows.len(), 3, "错误明细不合并");
    }

    #[test]
    fn done_result_has_no_rows() {
        let res = CostLaborImportRes::done(42);
        assert_eq!((res.success, res.failed, res.rows.len()), (42, 0, 0));
    }

    #[test]
    fn column_rank_follows_template_order() {
        assert_eq!(column_rank(HEADER_OBJECT_NO), 0);
        assert_eq!(column_rank(HEADER_RATE), 7);
        assert_eq!(column_rank("-"), COLUMNS.len(), "整行级错误排在最后");
    }

    // ---------- 组装 ----------

    #[test]
    fn to_spec_marks_rows_as_import_draft_and_computes_amount() {
        let parsed = parse_rows(&[raw(7)], Some("2026-11"));
        assert!(parsed[0].is_clean(), "{:?}", messages(&parsed[0]));
        let object = Uuid::now_v7();
        let dept = Uuid::now_v7();
        let spec = parsed[0].to_spec(object, dept, "装配车间", None);
        assert_eq!(spec.source_type, cost_labor_dto::SOURCE_IMPORT);
        assert_eq!(spec.status, STATUS_DRAFT, "人工复核后才确认（calc 只归集 CONFIRMED）");
        assert_eq!(spec.dept_name, "装配车间");
        assert_eq!(spec.amount, Decimal::from_str("363.75").unwrap(), "7.5 × 48.5 = 363.75");
        assert_eq!(spec.remark.as_deref(), Some("Excel 导入（第 7 行）"));
    }

    #[test]
    fn distinct_skips_blank_and_keeps_first_seen_order() {
        let values = vec!["B".to_string(), "".to_string(), "A".to_string(), "B".to_string()];
        assert_eq!(distinct(values.into_iter()), vec!["B".to_string(), "A".to_string()]);
    }
}

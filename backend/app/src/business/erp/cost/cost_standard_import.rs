//! 标准成本卡 **xlsx 批量导入**（M5-09【M5·成本】标准成本：维护页 CRUD · 批量导入）。
//!
//! 上游：M5-08 交付了 8 条版本表接口（`cost_standard_{dto,model,handler}.rs`），
//! 本模块只补「一次导入一整张标准的卡头 + 标准 BOM」，写库走 M5-08 的同一条代码路径。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 9 | POST | `/business/erp/cost-standard/import` | `…:cost-method:create` | xlsx 批量导入（**全有或全无**） |
//! | 10 | GET | `/business/erp/cost-standard/import-template` | `…:cost-method:list` | 9 列模板下载 |
//!
//! # 为什么是「9 列一张表」而不是「两张 sheet」
//!
//! 一张标准成本卡的天然形状是「卡头 + N 行 BOM」，看起来该拆两张 sheet。但导入的人
//! 手上拿到的往往是「一张月度标准成本表」：一物料占几行，卡头三要素在每组行上重复填写。
//! 拆 sheet 会让「物料编码 / 期间」两个键在两张表里各写一遍，对不上时**没人知道哪张对**；
//! 一张表则天然只有一处真相。于是本模块的口径是：
//!
//! * **一组 `(物料编码, 期间)` = 一版标准成本**（组内行序即 BOM 行号）；
//! * 组内每一行的卡头四列（标准工时 / 标准工资率 / 标准制造费用 / 备注）必须**逐字一致**
//!   —— 不一致时不是「取第一行」而是报错：静默取一行会让同一张卡出现两种口径，
//!   而这四列正是差异分析的分母；
//! * 耗用物料三列（耗用物料编码 / 标准用量 / 标准单价）**要么全空**（这一版没有材料耗用），
//!   **要么全填**（半填一律报错，绝不按 0 补）。
//!
//! # 全有或全无（与 `cost_labor_import` 同一条底线）
//!
//! 半批导入会让「标准成本」少一版而没人发现：差异分析按「取不到标准」跳过，
//! 报表照样出得来，只是数字是错的、还没有红灯（ARCH-9 D-7「不猜数据」）。
//! 所以执行顺序写死为：
//!
//! ```text
//! ① 解析（结构错 → 400/422，整表拒绝，连行号都没有）
//! ② 逐行纯校验（不碰库：形状 / 必填 / 数值值域 / 组内一致性 / 组内明细重复）
//! ③ 引用校验（物料存在且启用且非服务类 / 耗用物料存在 / 期间未被既有启用版本占用）
//!    —— 一次批量查库，不做 N+1
//! ④ 只要**任何一行**有问题 → 直接返回逐行错误，**一条都不入库**
//! ⑤ 全部干净 → **单事务**逐组写卡头 + 标准 BOM（复用 `handler::create_in_tx`），
//!    任一组失败（并发下撞上重叠）⇒ 整批回滚
//! ```
//!
//! # 为什么不复用 `POST /cost-standard` 一条条调
//!
//! 那样每组各自开事务、各自提交，第 3 组失败时前 2 组已经落库 —— 就成了「半批」，
//! 正好是本模块要避免的事。所以写库只借 `create_in_tx`（事务体），事务边界留在本模块。
//!
//! # 行号口径
//!
//! 报错里的 `line` 是 **Excel 真实行号**（表头是第 1 行，第一条数据是第 2 行），
//! 与用户屏幕上看到的行号一致，能直接跳过去改。
//!
//! # 与「已生效版本不许改」的关系（M5-08 的 66109 / 66110）
//!
//! 导入**只新建**版本，不改不删任何既有行；唯一的外部约束是「同一物料启用版本不许重叠」。
//! 所以想「改」某一期的标准，正路是：维护页改未生效版 / 走 `POST …/{id}/status` 失效旧版
//! 再导入新版 —— 与本模块的错误报文里给的出路一致。

use std::collections::HashMap;
use std::str::FromStr;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_labor_import::ImportIssue;
use crate::business::erp::cost::cost_standard_dto::{
    self as dto, AMOUNT_MAX, AMOUNT_SCALE, HOURS_MAX, HOURS_SCALE, REMARK_MAX, STATUS_ACTIVE, StandardItemSpec,
    StandardSpec,
};
use crate::business::erp::cost::cost_standard_handler;
use crate::business::erp::cost::cost_standard_model::{self as model, MaterialRef};
use crate::business::impexp::definition::{ColumnKind, ImportColumn};
use crate::business::impexp::engine::{SheetReader, WorkbookSheet, write_workbook};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 9 列模板（列序即模板列序） ====================

/// 成品 / 半成品物料编码。
pub const HEADER_MATERIAL_CODE: &str = "物料编码";
/// 期间（yyyy-MM）。
pub const HEADER_PERIOD: &str = "期间";
/// 单位标准工时。
pub const HEADER_LABOR_HOURS: &str = "标准工时";
/// 标准工资率。
pub const HEADER_LABOR_RATE: &str = "标准工资率";
/// 单位标准制造费用。
pub const HEADER_OVERHEAD: &str = "标准制造费用";
/// 耗用物料编码。
pub const HEADER_COMPONENT_CODE: &str = "耗用物料编码";
/// 单位标准用量。
pub const HEADER_STD_QTY: &str = "标准用量";
/// 标准单价。
pub const HEADER_STD_PRICE: &str = "标准单价";
/// 备注。
pub const HEADER_REMARK: &str = "备注";

/// 描述列定义的常量（9 列）—— 单测会断言表头与本模块的 `HEADER_*` 逐字一致。
///
/// 为什么用 `ImportColumn` 而不是裸 `&[&str]`：表头归一化（去 `*` / 全角括号 / 空格）、
/// 缺必需列的定位报文都由 `impexp::engine` 的既有实现兜住，不另写一套。
pub const COLUMNS: [ImportColumn; 9] = [
    ImportColumn {
        key: "material_code",
        header: HEADER_MATERIAL_CODE,
        required: true,
        kind: ColumnKind::Text { max_len: Some(64) },
        unique_key: false,
        fk: None,
        example: "FG-1001",
        width: Some(16),
        remark: "成品 / 半成品物料编码；物料必须存在且已启用，服务类（SERVICE）不能建标准成本",
    },
    ImportColumn {
        key: "period_code",
        header: HEADER_PERIOD,
        required: true,
        kind: ColumnKind::Text { max_len: Some(7) },
        unique_key: false,
        fk: None,
        example: "2026-11",
        width: Some(10),
        remark: "yyyy-MM；导入版本的生效区间 = 该期间整期（期初 ~ 期末）",
    },
    ImportColumn {
        key: "std_labor_hours",
        header: HEADER_LABOR_HOURS,
        required: false,
        kind: ColumnKind::Decimal {
            scale: HOURS_SCALE,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "2.5",
        width: Some(12),
        remark: "单位标准工时，≥ 0，最多 4 位小数；留空 = 0（组内各行必须一致）",
    },
    ImportColumn {
        key: "std_labor_rate",
        header: HEADER_LABOR_RATE,
        required: false,
        kind: ColumnKind::Decimal {
            scale: AMOUNT_SCALE,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "48.5",
        width: Some(12),
        remark: "标准工资率（元/小时），≥ 0，最多 6 位小数；留空 = 0（组内各行必须一致）",
    },
    ImportColumn {
        key: "std_overhead_cost",
        header: HEADER_OVERHEAD,
        required: false,
        kind: ColumnKind::Decimal {
            scale: AMOUNT_SCALE,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "12.34",
        width: Some(14),
        remark: "单位标准制造费用，≥ 0，最多 6 位小数；留空 = 0（组内各行必须一致）",
    },
    ImportColumn {
        key: "component_code",
        header: HEADER_COMPONENT_CODE,
        required: false,
        kind: ColumnKind::Text { max_len: Some(64) },
        unique_key: false,
        fk: None,
        example: "RM-2001",
        width: Some(16),
        remark: "标准 BOM 的耗用物料编码；与「标准用量 / 标准单价」**要么全空、要么全填**",
    },
    ImportColumn {
        key: "std_qty",
        header: HEADER_STD_QTY,
        required: false,
        kind: ColumnKind::Decimal {
            scale: AMOUNT_SCALE,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "1.2",
        width: Some(12),
        remark: "单位标准用量，> 0（填了耗用物料则必填），最多 6 位小数",
    },
    ImportColumn {
        key: "std_price",
        header: HEADER_STD_PRICE,
        required: false,
        kind: ColumnKind::Decimal {
            scale: AMOUNT_SCALE,
            min: Some(0.0),
            max: None,
        },
        unique_key: false,
        fk: None,
        example: "8.5",
        width: Some(12),
        remark: "标准单价，≥ 0（填了耗用物料则必填），最多 6 位小数；金额 = 用量 × 单价由服务端算",
    },
    ImportColumn {
        key: "remark",
        header: HEADER_REMARK,
        required: false,
        kind: ColumnKind::Text {
            max_len: Some(REMARK_MAX),
        },
        unique_key: false,
        fk: None,
        example: "2026 版标准成本",
        width: Some(24),
        remark: "备注，≤ 200 字；组内各行必须一致（留空 = 不带备注）",
    },
];

/// 模板文件名（中文，由 handler 做 RFC 5987 编码）。
pub const TEMPLATE_FILE_NAME: &str = "标准成本导入模板_v1.xlsx";

/// 单次导入的数据行上限。
///
/// 一行 = 一条标准 BOM 明细（或「无材料耗用」的那一行），所以 5000 行大约等于
/// 「几百张标准卡 × 十几行 BOM」；再多请按期间拆分，避免一次事务长时间持锁。
pub const MAX_ROWS: usize = 5000;

/// 模板「字段说明」sheet 的表头。
const NOTE_HEADERS: [&str; 6] = ["字段名", "是否必填", "类型", "长度/范围", "可选值", "说明"];

/// 模板「填写须知」sheet 的固定文案。
const GUIDE_LINES: [&str; 7] = [
    "1. 请勿修改第 1 行表头，勿增删列（表头带 * 的为必填列）；",
    "2. 第 2 行是示例，导入前请删除（系统也会自动识别并跳过完全相同的示例行）；",
    "3. 一组「物料编码 + 期间」= 一版标准成本；组内一行 = 一条标准 BOM 明细；",
    "4. 组内各行的「标准工时 / 标准工资率 / 标准制造费用 / 备注」必须逐字一致，不一致整批拒绝；",
    "5. 「耗用物料编码 / 标准用量 / 标准单价」要么全空（无材料耗用），要么全填；",
    "6. 导入只**新建**版本：同一物料在该期间已有启用版本时会报错，请先失效旧版或到维护页修改；",
    "7. 全有或全无：任何一行有错，整批都不入库，错误按「Excel 行号 + 列名 + 原因」返回。",
];

// ==================== 出参 ====================

/// 导入结果。
///
/// * 全部成功 → `success = 数据行数`、`versions_created = 新建版本数`、`rows = []`；
/// * 任何一行失败 → `success = 0`、`versions_created = 0`、`rows = 全部错误`（**一条都没入库**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StandardImportRes {
    /// 成功入库的数据行数（全有或全无：要么 0，要么等于文件数据行数）。
    pub success: usize,
    /// 有错的行数（按行去重）。
    pub failed: usize,
    /// 新建的标准成本版本数（= 文件里不同「物料 + 期间」组的个数）。
    pub versions_created: usize,
    /// 逐行错误（按行号、再按列序）。成功时为空。
    pub rows: Vec<ImportIssue>,
}

impl StandardImportRes {
    /// 全成功。
    pub fn done(rows: usize, versions: usize) -> Self {
        Self {
            success: rows,
            failed: 0,
            versions_created: versions,
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
            versions_created: 0,
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
    /// 物料编码原文。
    pub material_code: String,
    /// 期间原文。
    pub period_code: String,
    /// 标准工时原文。
    pub std_labor_hours: String,
    /// 标准工资率原文。
    pub std_labor_rate: String,
    /// 标准制造费用原文。
    pub std_overhead_cost: String,
    /// 耗用物料编码原文。
    pub component_code: String,
    /// 标准用量原文。
    pub std_qty: String,
    /// 标准单价原文。
    pub std_price: String,
    /// 备注原文。
    pub remark: String,
}

impl RawRow {
    /// 组内一致性判据用的「卡头签名」（四列原文，trim 后拼接）。
    ///
    /// 用**原文**而不是 `Decimal`：`2.50` 与 `2.5` 数值相同，用户在同一组里混着写
    /// 不该报错。所以签名前先把数值列归一化（`parse_decimal().to_string()`），
    /// 见 [`Self::head_signature`]。
    pub fn head_signature(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            normalize_number(&self.std_labor_hours),
            normalize_number(&self.std_labor_rate),
            normalize_number(&self.std_overhead_cost),
            self.remark.trim()
        )
    }
}

/// 数值原文归一化（空白 → `0`；非法原文原样返回，由逐行校验报错）。
fn normalize_number(raw: &str) -> String {
    let text = raw.trim();
    if text.is_empty() {
        return "0".to_string();
    }
    match Decimal::from_str(text) {
        Ok(value) => value.normalize().to_string(),
        Err(_) => text.to_string(),
    }
}

/// 解析 xlsx 字节流 → 原始行（结构错整表拒绝，行错留给 [`scan_groups`]）。
///
/// # 异常情况
///
/// - 400 + `61001`：不是 xlsx / 旧版 xls / 文件损坏 / 没有工作表；
/// - 422 + `61001`：只有表头没有数据行 / 表头缺必需列；
/// - 422 + `61002`：数据行超过 [`MAX_ROWS`]。
pub fn read_rows(bytes: &[u8]) -> std::result::Result<Vec<RawRow>, ApiError> {
    let reader = SheetReader::open(bytes, &COLUMNS).map_err(|error| error.into_api_error())?;
    let mut rows = Vec::new();
    for (index, raw) in reader.data_rows().enumerate() {
        if SheetReader::is_blank(raw) || reader.is_example_row(raw) {
            continue;
        }
        rows.push(RawRow {
            line: SheetReader::excel_row_no(index),
            material_code: cell(&reader, raw, 0),
            period_code: cell(&reader, raw, 1),
            std_labor_hours: cell(&reader, raw, 2),
            std_labor_rate: cell(&reader, raw, 3),
            std_overhead_cost: cell(&reader, raw, 4),
            component_code: cell(&reader, raw, 5),
            std_qty: cell(&reader, raw, 6),
            std_price: cell(&reader, raw, 7),
            remark: cell(&reader, raw, 8),
        });
        if rows.len() > MAX_ROWS {
            return Err(ApiError::new(
                422,
                error_code::IMPORT_ROW_LIMIT,
                format!("单次最多导入 {MAX_ROWS} 行，请按期间拆分后再上传"),
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

// ==================== ② 逐行纯校验 + 分组 ====================

/// 一组的卡头三要素 + 备注（已归一化）。
#[derive(Debug, Clone, PartialEq)]
pub struct HeadNumbers {
    /// 单位标准工时（已舍到 4 位）。
    pub hours: Decimal,
    /// 标准工资率。
    pub rate: Decimal,
    /// 标准制造费用。
    pub overhead: Decimal,
    /// 备注（`None` = 空）。
    pub remark: Option<String>,
}

/// 组内一条标准 BOM 草稿（耗用物料编码尚未换成 id）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemDraft {
    /// 该行 Excel 行号。
    pub line: u32,
    /// 耗用物料编码。
    pub component_code: String,
    /// 单位标准用量。
    pub qty: Decimal,
    /// 标准单价。
    pub price: Decimal,
}

/// 一版标准成本草稿（= 一组 `物料编码 + 期间`）。
#[derive(Debug, Clone, PartialEq)]
pub struct GroupDraft {
    /// 物料编码。
    pub material_code: String,
    /// 期间（`yyyy-MM`，已归一化）。
    pub period_code: String,
    /// 生效区间（期间整期）。
    pub window: (NaiveDate, NaiveDate),
    /// 组内所有 Excel 行号（报错时逐行点名）。
    pub lines: Vec<u32>,
    /// 卡头三要素 + 备注。
    pub head: HeadNumbers,
    /// 标准 BOM 草稿（可空 = 无材料耗用）。
    pub items: Vec<ItemDraft>,
}

impl GroupDraft {
    /// 组标称（报文用）：`FG-1001 @ 2026-11`。
    pub fn label(&self) -> String {
        format!("{} @ {}", self.material_code, self.period_code)
    }
}

/// 把原始行按 `(物料编码, 期间)` 分组并按组内顺序校验（**纯逻辑，不碰库**）。
///
/// # 返回值
///
/// `(组草稿, 逐行错误)`。有错误时组草稿仍照常返回能拼出来的部分 —— 调用方只要有
/// 任何一条错误就整批拒绝，所以不必保证「有错时组草稿完整」。
pub fn scan_groups(rows: &[RawRow]) -> (Vec<GroupDraft>, Vec<ImportIssue>) {
    let mut issues: Vec<ImportIssue> = Vec::new();

    // ---- 分组：按首次出现顺序（BOM 行序 = 组内出现顺序） ----
    let mut order: Vec<(String, String)> = Vec::new();
    let mut buckets: HashMap<(String, String), Vec<&RawRow>> = HashMap::new();
    for row in rows {
        let key = (row.material_code.clone(), row.period_code.clone());
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(row);
    }

    let mut groups = Vec::with_capacity(order.len());
    for key in order {
        let members = buckets.remove(&key).unwrap_or_default();
        let (material_code, period_raw) = key;
        let lines: Vec<u32> = members.iter().map(|row| row.line).collect();
        let first_line = lines.first().copied().unwrap_or(0);

        // ---- 组键本身 ----
        if material_code.is_empty() {
            issues.push(ImportIssue::new(
                first_line,
                HEADER_MATERIAL_CODE,
                "物料编码不能为空：一组「物料编码 + 期间」才是一版标准成本",
            ));
        }
        let window = match dto::period_window(&period_raw) {
            Ok(window) => Some(window),
            Err(error) => {
                for line in &lines {
                    issues.push(ImportIssue::new(
                        *line,
                        HEADER_PERIOD,
                        format!("期间必须是 yyyy-MM（收到「{period_raw}」）：{}", error.msg),
                    ));
                }
                None
            }
        };

        // ---- 组内卡头一致性 ----
        let mut head_signature: Option<String> = None;
        let mut head_lines: HashMap<String, Vec<u32>> = HashMap::new();
        for row in &members {
            let signature = row.head_signature();
            match &head_signature {
                None => head_signature = Some(signature.clone()),
                Some(first) if *first != signature => {
                    head_lines.entry(signature.clone()).or_default().push(row.line);
                }
                Some(_) => {}
            }
        }
        if let Some(extra) = head_lines.values().next()
            && let Some(first) = members.first()
        {
            let mut bad: Vec<u32> = extra.clone();
            bad.sort_unstable();
            for line in bad {
                issues.push(ImportIssue::new(
                    line,
                    HEADER_LABOR_HOURS,
                    format!(
                        "同一组「{} @ {}」各行的卡头（标准工时 / 标准工资率 / 标准制造费用 / 备注）\
                         必须逐字一致：第 {} 行写的是「{} / {} / {} / {}」，与第 {} 行不同。\
                         一版标准成本只有一套卡头，不一致请拆成两版（换期间或换物料）",
                        material_code,
                        period_raw,
                        first.line,
                        num_text(&first.std_labor_hours),
                        num_text(&first.std_labor_rate),
                        num_text(&first.std_overhead_cost),
                        if first.remark.trim().is_empty() {
                            "（空）"
                        } else {
                            first.remark.trim()
                        },
                        line
                    ),
                ));
            }
        }

        // ---- 组头数值 ----
        let head = build_head(members.first().copied(), &mut issues);

        // ---- 明细 ----
        let mut items: Vec<ItemDraft> = Vec::new();
        let mut seen: HashMap<String, u32> = HashMap::new();
        for row in &members {
            let component = row.component_code.trim();
            let has_qty = !row.std_qty.trim().is_empty();
            let has_price = !row.std_price.trim().is_empty();
            if component.is_empty() {
                if has_qty || has_price {
                    issues.push(ImportIssue::new(
                        row.line,
                        HEADER_COMPONENT_CODE,
                        "填了「标准用量 / 标准单价」却没填「耗用物料编码」：\
                         没有耗用物料的金额无处可挂，请补编码或把这两列清空",
                    ));
                }
                continue;
            }
            if let Some(previous) = seen.get(component) {
                issues.push(ImportIssue::new(
                    row.line,
                    HEADER_COMPONENT_CODE,
                    format!(
                        "耗用物料「{component}」在本组已出现在第 {previous} 行：\
                         同一物料在一张标准卡里只能有一行（要拆用量请合并到同一行）"
                    ),
                ));
            } else {
                seen.insert(component.to_string(), row.line);
            }
            // 用量 / 单价：填了耗用物料就**都必须给**（留空不等于 0 —— 按 0 补会把
            // 材料成本算少，而且没有任何红灯）。两项都解析成功才拼成一条明细，
            // 但**两项都要解析**（一次把能报的都报出来，不因前一项错而漏掉后一项）。
            let qty = parse_required_decimal(
                row,
                HEADER_STD_QTY,
                &row.std_qty,
                AMOUNT_SCALE,
                AMOUNT_MAX,
                false,
                &mut issues,
            );
            let price = parse_required_decimal(
                row,
                HEADER_STD_PRICE,
                &row.std_price,
                AMOUNT_SCALE,
                AMOUNT_MAX,
                true,
                &mut issues,
            );
            if let (Some(qty), Some(price)) = (qty, price) {
                items.push(ItemDraft {
                    line: row.line,
                    component_code: component.to_string(),
                    qty,
                    price,
                });
            }
        }

        if let Some(window) = window {
            groups.push(GroupDraft {
                material_code,
                period_code: period_raw,
                window,
                lines,
                head,
                items,
            });
        }
    }

    (groups, issues)
}

/// 明细列的数值解析（**留空即错**：填了耗用物料就必须给用量与单价）。
///
/// 与 [`parse_decimal_allow_blank`] 的区别就在「留空」这一条：卡头三要素留空 = 0 是
/// 合法口径（不含人工 / 不含制造费用），明细三列留空 = 这一行没写完，必须报错。
fn parse_required_decimal(
    row: &RawRow,
    column: &str,
    raw: &str,
    scale: u32,
    max: &str,
    allow_zero: bool,
    issues: &mut Vec<ImportIssue>,
) -> Option<Decimal> {
    let text = raw.trim();
    if text.is_empty() {
        issues.push(ImportIssue::new(
            row.line,
            column,
            format!(
                "{column}不能为空：填了「耗用物料编码」就必须同时给「标准用量」和「标准单价」\
                 （留空不等于 0；这一版确实不用料就把三列都清空）"
            ),
        ));
        return None;
    }
    parse_number_text(row, column, text, scale, max, allow_zero, issues)
}

/// 单元格原文 → `Decimal`（形状 + 值域；超精度 / 负数 / 不允许的 0 一律拒）。
fn parse_number_text(
    row: &RawRow,
    column: &str,
    text: &str,
    scale: u32,
    max: &str,
    allow_zero: bool,
    issues: &mut Vec<ImportIssue>,
) -> Option<Decimal> {
    let value = match Decimal::from_str(text) {
        Ok(value) => value,
        Err(_) => {
            issues.push(ImportIssue::new(
                row.line,
                column,
                format!("{column}不是数字（收到「{text}」）：请填纯数字，不要带单位 / 千分位 / 百分号"),
            ));
            return None;
        }
    };
    // 复用单条录入的同一套值域判据（同一个纯函数产出同一句话术）
    match cost_labor_dto::ensure_decimal(value, column, scale, max, allow_zero) {
        Ok(value) => Some(value),
        Err(error) => {
            issues.push(ImportIssue::new(row.line, column, error.msg));
            None
        }
    }
}

/// 组头三要素（取组内第一行的原文，已由一致性校验保证各行相同）。
fn build_head(first: Option<&RawRow>, issues: &mut Vec<ImportIssue>) -> HeadNumbers {
    let Some(row) = first else {
        return HeadNumbers {
            hours: Decimal::ZERO,
            rate: Decimal::ZERO,
            overhead: Decimal::ZERO,
            remark: None,
        };
    };
    let hours = parse_decimal_allow_blank(
        row,
        HEADER_LABOR_HOURS,
        &row.std_labor_hours,
        HOURS_SCALE,
        HOURS_MAX,
        issues,
    )
    .map(|value| value.round_dp_with_strategy(HOURS_SCALE, dto::ROUNDING))
    .unwrap_or(Decimal::ZERO);
    let rate = parse_decimal_allow_blank(
        row,
        HEADER_LABOR_RATE,
        &row.std_labor_rate,
        AMOUNT_SCALE,
        AMOUNT_MAX,
        issues,
    )
    .unwrap_or(Decimal::ZERO);
    let overhead = parse_decimal_allow_blank(
        row,
        HEADER_OVERHEAD,
        &row.std_overhead_cost,
        AMOUNT_SCALE,
        AMOUNT_MAX,
        issues,
    )
    .unwrap_or(Decimal::ZERO);
    let remark = match dto::clean_remark(Some(row.remark.clone())) {
        Ok(value) => value,
        Err(error) => {
            issues.push(ImportIssue::new(row.line, HEADER_REMARK, error.msg));
            None
        }
    };
    HeadNumbers {
        hours,
        rate,
        overhead,
        remark,
    }
}

/// 可空数值列（空白 → 0；非空则走 [`parse_number_text`] 的同一套值域判据）。
fn parse_decimal_allow_blank(
    row: &RawRow,
    column: &str,
    raw: &str,
    scale: u32,
    max: &str,
    issues: &mut Vec<ImportIssue>,
) -> Option<Decimal> {
    let text = raw.trim();
    if text.is_empty() {
        return Some(Decimal::ZERO);
    }
    parse_number_text(row, column, text, scale, max, true, issues)
}

/// 报错里回显用户的原文（空 → `（空）`）。
fn num_text(raw: &str) -> &str {
    let text = raw.trim();
    if text.is_empty() { "（空）" } else { text }
}

// ==================== ③ 引用校验（一次批量查库） ====================

/// 引用索引（一次查库，供校验与拼装共用 —— 保证「校验看到的」与「写入用的」是同一批行）。
#[derive(Debug, Default)]
pub struct RefIndex {
    /// 物料编码 → `(id, 物料引用)`。
    pub materials: HashMap<String, (Uuid, MaterialRef)>,
}

/// 引用校验：物料存在且启用且非服务类、耗用物料存在、期间未被既有启用版本占用。
///
/// 发现的问题追加到 `issues`，**不中断**（一次把能报的都报出来）。
async fn validate_refs(
    groups: &[GroupDraft],
    issues: &mut Vec<ImportIssue>,
) -> std::result::Result<RefIndex, ApiError> {
    let mut refs = RefIndex::default();
    let mut codes: Vec<String> = Vec::new();
    for group in groups {
        if !group.material_code.is_empty() && !codes.contains(&group.material_code) {
            codes.push(group.material_code.clone());
        }
        for item in &group.items {
            if !codes.contains(&item.component_code) {
                codes.push(item.component_code.clone());
            }
        }
    }
    let found = model::materials_by_codes(&codes)
        .await
        .map_err(|error| utils::db_error(error, "校验导入的标准成本物料"))?;
    for (code, id, reference) in found {
        refs.materials.insert(code, (id, reference));
    }

    for group in groups {
        // ---- 组头物料 ----
        match refs.materials.get(&group.material_code) {
            None => {
                for line in &group.lines {
                    issues.push(ImportIssue::new(
                        *line,
                        HEADER_MATERIAL_CODE,
                        format!(
                            "物料「{}」不存在（或已删除）：导入不会自动建物料，请先在物料主数据里建好再导",
                            group.material_code
                        ),
                    ));
                }
            }
            Some((_, reference)) if reference.status != 1 => {
                for line in &group.lines {
                    issues.push(ImportIssue::new(
                        *line,
                        HEADER_MATERIAL_CODE,
                        format!(
                            "物料 {} 已停用（status=0）：停用物料不参与生产与成本核算，给它建标准成本卡没有意义。\
                             请先启用该物料，或换一个在用物料",
                            reference.label()
                        ),
                    ));
                }
            }
            Some((_, reference)) if reference.material_type == "SERVICE" => {
                for line in &group.lines {
                    issues.push(ImportIssue::new(
                        *line,
                        HEADER_MATERIAL_CODE,
                        format!(
                            "物料 {} 是服务类（SERVICE）：标准成本卡针对有物料耗用的成品 / 半成品，服务类没有标准 BOM 可算",
                            reference.label()
                        ),
                    ));
                }
            }
            Some(_) => {}
        }

        // ---- 耗用物料 ----
        for item in &group.items {
            if !refs.materials.contains_key(&item.component_code) {
                issues.push(ImportIssue::new(
                    item.line,
                    HEADER_COMPONENT_CODE,
                    format!(
                        "耗用物料「{}」不存在（或已删除）：请先在物料主数据里建好再导（导入不会自动建物料）",
                        item.component_code
                    ),
                ));
            }
        }

        // ---- 期间是否已被既有启用版本占用（M5-08 的 66108 前置版） ----
        let Some((material_id, _)) = refs.materials.get(&group.material_code) else {
            continue;
        };
        let versions = model::versions_of(db(), *material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询导入物料的既有标准成本版本"))?;
        if let Some(hit) = versions.iter().find(|row| {
            row.status == STATUS_ACTIVE
                && dto::window_intersects(row.effective_from, row.effective_to, group.window.0, group.window.1)
        }) {
            let label = hit.material_label();
            let window = dto::window_text(hit.effective_from, hit.effective_to);
            for line in &group.lines {
                issues.push(ImportIssue::new(
                    *line,
                    HEADER_PERIOD,
                    format!(
                        "「{}」在 {} 已经有启用版本 v{}（生效期 {}），与本次导入的 {} 重叠：\
                         同一物料同一期间只能有一套启用标准。三条出路：① 先在维护页把旧版「失效」再导；\
                         ② 先把旧版停止日收口到 {} 前一天；③ 直接改旧版（未生效可全改）",
                        label,
                        group.period_code,
                        hit.version_no,
                        window,
                        dto::window_text(group.window.0, Some(group.window.1)),
                        group.window.0
                    ),
                ));
            }
        }
    }

    Ok(refs)
}

// ==================== ④⑤ 编排 ====================

/// 导入 xlsx 字节流（**全有或全无**）。
///
/// # 参数
///
/// - `bytes`：上传的 xlsx 内容。
/// - `operator`：操作人（写 `created_by`）。
///
/// # 返回值
///
/// 全成功 → `success = 数据行数`；任何一行有错 → `success = 0` 且 `rows` 是逐行错误
/// （一条都没入库）。
///
/// # 异常情况
///
/// - 400/422：文件结构错（不是 xlsx / 表头缺列 / 没有数据行 / 超行数上限）；
/// - 409 + `66108`：并发下写入时撞上重叠（预检之后有别人抢先建了版本，整批回滚）；
/// - 500：查库 / 写库的意外失败（事务已回滚，库里不会有半批数据）。
pub async fn import_standards(
    bytes: &[u8],
    operator: Option<Uuid>,
) -> std::result::Result<StandardImportRes, ApiError> {
    let raw = read_rows(bytes)?;
    let (groups, mut issues) = scan_groups(&raw);

    // ---------- ③ 引用校验：一次批量查库（不做 N+1） ----------
    if groups.is_empty() {
        // 组都拼不出来（全是值域 / 形状错），不加引用校验，直接把行错返回
        issues.sort_by_key(|issue| issue.line);
        return Ok(StandardImportRes::rejected(issues));
    }
    let refs = validate_refs(&groups, &mut issues).await?;

    // ---------- ④ 有任何一行不干净 ⇒ 整批拒绝 ----------
    if !issues.is_empty() {
        issues.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| column_rank(&left.column).cmp(&column_rank(&right.column)))
        });
        return Ok(StandardImportRes::rejected(issues));
    }

    // ---------- ⑤ 全部干净 ⇒ 单事务逐组写入 ----------
    let versions = write_all(&groups, &refs, operator).await?;
    Ok(StandardImportRes::done(raw.len(), versions))
}

/// 列的展示顺序（同一条错误按模板列顺序稳定输出，而不是随机）。
fn column_rank(column: &str) -> usize {
    COLUMNS
        .iter()
        .position(|candidate| candidate.header == column)
        .unwrap_or(COLUMNS.len())
}

/// 单事务逐组写入（任一组失败 ⇒ 整批回滚，不留半批数据）。
///
/// `pub(crate)`：真库用例直接构造「两组必然撞重叠」的输入，验证事务边界
/// （`scan_groups` 产出不出这种输入，因为组键就是 `物料 + 期间`）。
pub(crate) async fn write_all(
    groups: &[GroupDraft],
    refs: &RefIndex,
    operator: Option<Uuid>,
) -> std::result::Result<usize, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启标准成本导入事务"))?;
    let mut written = 0usize;
    for group in groups {
        let spec = match build_spec(group, refs) {
            Ok(spec) => spec,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        };
        let amounts = spec.amounts();
        let id = Uuid::now_v7();
        // 事务体复用 M5-08 的实现：锁物料 → 判重叠 → 取版本号 → 写卡头 + 明细。
        // 这里不另写一份 SQL，避免「单条录入」与「导入」在护栏上漂移。
        match cost_standard_handler::create_in_tx(&tx, id, &spec, &amounts, operator).await {
            Ok(()) => written += 1,
            Err(error) => {
                if let Err(rollback) = tx.rollback().await {
                    tracing::error!(error = %rollback, "标准成本导入回滚失败");
                }
                return Err(decorate(error, group));
            }
        }
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交标准成本导入事务"))?;
    tracing::info!(rows = written, "标准成本卡 xlsx 导入完成");
    Ok(written)
}

/// 组草稿 + 引用索引 → 归一化规格（引用索引来自 [`validate_refs`]，此处不再查库）。
fn build_spec(group: &GroupDraft, refs: &RefIndex) -> std::result::Result<StandardSpec, ApiError> {
    let material_id = refs
        .materials
        .get(&group.material_code)
        .map(|(id, _)| *id)
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("物料引用在拼装阶段丢失：{}", group.material_code),
            )
        })?;
    let mut items = Vec::with_capacity(group.items.len());
    for item in &group.items {
        let (component_id, _) = refs.materials.get(&item.component_code).ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!(
                    "耗用物料引用在拼装阶段丢失：{}（Excel 第 {} 行）",
                    item.component_code, item.line
                ),
            )
        })?;
        items.push(StandardItemSpec {
            component_material_id: *component_id,
            std_qty: item.qty,
            std_price: item.price,
        });
    }
    if items.len() > dto::ITEM_MAX {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "「{}」这一版标准 BOM 有 {} 行，超过上限 {} 行；请合并同物料用量或拆成多版",
                group.label(),
                items.len(),
                dto::ITEM_MAX
            ),
        ));
    }
    Ok(StandardSpec {
        material_id,
        effective_from: group.window.0,
        effective_to: Some(group.window.1),
        std_labor_hours: group.head.hours,
        std_labor_rate: group.head.rate,
        std_overhead_cost: group.head.overhead,
        status: STATUS_ACTIVE,
        remark: group.head.remark.clone(),
        items,
    })
}

/// 写库阶段的错误 → 带上「哪一组 / 哪几行」，让人能回到 Excel 里改。
///
/// 用 [`ApiError::appended`] 而不是 `ApiError::new(...)` 重造：后者会把业务码 /
/// HTTP 状态 / 结构化 `data` 丢掉（vendo 注释里点名的 13 次历史事故就是这种写法）。
fn decorate(error: ApiError, group: &GroupDraft) -> ApiError {
    let lines = group.lines.iter().map(u32::to_string).collect::<Vec<_>>().join("、");
    let first_line = group
        .lines
        .first()
        .map(u32::to_string)
        .unwrap_or_else(|| "-".to_string());
    error.appended(format!(
        "（导入第 {first_line} 行这一组「{}」写入失败，第 {lines} 行；整批已回滚：一条都没入库）",
        group.label()
    ))
}

// ==================== 模板 ====================

/// 把「一行行文本」写成与模板同表头的 xlsx（9 列，第 1 行表头）。
///
/// 用途：真库用例要造真实的上传文件 —— 自己写一份 xlsx 再读回来，才能验
/// 「解析 → 校验 → 入库」整条链路（而不是只验纯函数）。
///
/// # 异常情况
///
/// 写入失败返回 500。
pub fn build_rows_xlsx(rows: &[RawRow]) -> std::result::Result<Vec<u8>, ApiError> {
    let headers = COLUMNS.iter().map(|column| column.header).collect::<Vec<_>>();
    let maps = rows
        .iter()
        .map(|row| {
            let mut map = HashMap::new();
            map.insert(HEADER_MATERIAL_CODE.to_string(), row.material_code.clone());
            map.insert(HEADER_PERIOD.to_string(), row.period_code.clone());
            map.insert(HEADER_LABOR_HOURS.to_string(), row.std_labor_hours.clone());
            map.insert(HEADER_LABOR_RATE.to_string(), row.std_labor_rate.clone());
            map.insert(HEADER_OVERHEAD.to_string(), row.std_overhead_cost.clone());
            map.insert(HEADER_COMPONENT_CODE.to_string(), row.component_code.clone());
            map.insert(HEADER_STD_QTY.to_string(), row.std_qty.clone());
            map.insert(HEADER_STD_PRICE.to_string(), row.std_price.clone());
            map.insert(HEADER_REMARK.to_string(), row.remark.clone());
            map
        })
        .collect::<Vec<_>>();
    crate::business::impexp::engine::write_sheet(&headers, &maps)
}

/// 生成 9 列导入模板 xlsx（三个 sheet：数据 / 字段说明 / 填写须知）。
///
/// # 异常情况
///
/// 写入失败返回 500。
pub fn build_template() -> std::result::Result<Vec<u8>, ApiError> {
    let headers = COLUMNS.iter().map(ImportColumn::template_header).collect::<Vec<_>>();
    let mut example = HashMap::new();
    for column in &COLUMNS {
        example.insert(column.template_header(), column.example.to_string());
    }
    let example_rows = vec![example];

    let note_rows = COLUMNS
        .iter()
        .map(|column| {
            let mut row = HashMap::new();
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
            let mut row = HashMap::new();
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

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(line: u32, material: &str, period: &str, component: &str, qty: &str, price: &str) -> RawRow {
        RawRow {
            line,
            material_code: material.to_string(),
            period_code: period.to_string(),
            std_labor_hours: "2.5".to_string(),
            std_labor_rate: "48.5".to_string(),
            std_overhead_cost: "12.34".to_string(),
            component_code: component.to_string(),
            std_qty: qty.to_string(),
            std_price: price.to_string(),
            remark: String::new(),
        }
    }

    #[test]
    fn columns_are_the_nine_documented_ones() {
        let headers: Vec<&str> = COLUMNS.iter().map(|column| column.header).collect();
        assert_eq!(
            headers,
            vec![
                "物料编码",
                "期间",
                "标准工时",
                "标准工资率",
                "标准制造费用",
                "耗用物料编码",
                "标准用量",
                "标准单价",
                "备注"
            ]
        );
        assert_eq!(COLUMNS.len(), 9);
        // 只有前两列必填（组键）
        assert!(COLUMNS[0].required && COLUMNS[1].required);
        assert!(COLUMNS[2..].iter().all(|column| !column.required));
    }

    #[test]
    fn groups_by_material_and_period_in_first_seen_order() {
        let rows = vec![
            raw(2, "FG-1", "2026-11", "RM-1", "1", "2"),
            raw(3, "FG-2", "2026-11", "RM-1", "1", "2"),
            raw(4, "FG-1", "2026-11", "RM-2", "3", "4"),
            raw(5, "FG-1", "2026-12", "", "", ""),
        ];
        let (groups, issues) = scan_groups(&rows);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].material_code, "FG-1");
        assert_eq!(groups[0].period_code, "2026-11");
        assert_eq!(groups[0].lines, vec![2, 4], "同一组的两行都要记下行号");
        assert_eq!(groups[0].items.len(), 2);
        assert_eq!(groups[1].material_code, "FG-2");
        assert_eq!(groups[2].period_code, "2026-12");
        assert!(groups[2].items.is_empty(), "耗用物料全空 = 这一版没有材料耗用");
    }

    #[test]
    fn period_window_is_the_whole_month() {
        let rows = vec![raw(2, "FG-1", "2026-02", "", "", "")];
        let (groups, issues) = scan_groups(&rows);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(
            groups[0].window,
            (
                NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()
            )
        );
    }

    #[test]
    fn bad_period_is_reported_on_every_row_of_the_group() {
        let rows = vec![
            raw(2, "FG-1", "2026-13", "", "", ""),
            raw(3, "FG-1", "2026-13", "RM-1", "1", "2"),
        ];
        let (groups, issues) = scan_groups(&rows);
        assert!(groups.is_empty(), "期间不合法就没有可写的组");
        let lines: Vec<u32> = issues.iter().map(|issue| issue.line).collect();
        assert_eq!(lines, vec![2, 3]);
        assert!(issues.iter().all(|issue| issue.column == HEADER_PERIOD));
    }

    #[test]
    fn head_columns_must_agree_inside_a_group() {
        let mut second = raw(3, "FG-1", "2026-11", "RM-2", "1", "2");
        second.std_labor_rate = "50".to_string();
        let rows = vec![raw(2, "FG-1", "2026-11", "RM-1", "1", "2"), second];
        let (_, issues) = scan_groups(&rows);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].line, 3);
        assert_eq!(issues[0].column, HEADER_LABOR_HOURS);
        assert!(issues[0].message.contains("必须逐字一致"), "{}", issues[0].message);
    }

    #[test]
    fn same_head_written_differently_is_not_a_conflict() {
        // 2.50 与 2.5 是同一个数，不该报「不一致」
        let mut second = raw(3, "FG-1", "2026-11", "RM-2", "1", "2");
        second.std_labor_hours = "2.50".to_string();
        let rows = vec![raw(2, "FG-1", "2026-11", "RM-1", "1", "2"), second];
        let (groups, issues) = scan_groups(&rows);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(groups[0].items.len(), 2);
    }

    #[test]
    fn component_needs_qty_and_price_or_none_at_all() {
        let rows = vec![
            raw(2, "FG-1", "2026-11", "RM-1", "", "2"),
            raw(3, "FG-1", "2026-11", "RM-2", "1", ""),
            raw(4, "FG-1", "2026-11", "", "1", ""),
        ];
        let (_, issues) = scan_groups(&rows);
        assert_eq!(issues.len(), 3);
        assert!(
            issues
                .iter()
                .any(|issue| issue.line == 2 && issue.column == HEADER_STD_QTY)
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.line == 3 && issue.column == HEADER_STD_PRICE)
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.line == 4 && issue.column == HEADER_COMPONENT_CODE)
        );
    }

    #[test]
    fn duplicate_component_in_the_same_group_is_rejected() {
        let rows = vec![
            raw(2, "FG-1", "2026-11", "RM-1", "1", "2"),
            raw(3, "FG-1", "2026-11", "RM-1", "2", "2"),
        ];
        let (_, issues) = scan_groups(&rows);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].line, 3);
        assert!(issues[0].message.contains("第 2 行"), "{}", issues[0].message);
    }

    #[test]
    fn same_component_in_different_groups_is_fine() {
        let rows = vec![
            raw(2, "FG-1", "2026-11", "RM-1", "1", "2"),
            raw(3, "FG-2", "2026-11", "RM-1", "1", "2"),
        ];
        let (groups, issues) = scan_groups(&rows);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn numbers_are_validated_with_column_and_line() {
        let mut row = raw(7, "FG-1", "2026-11", "RM-1", "0", "-3");
        row.std_labor_hours = "abc".to_string();
        let rows = vec![row];
        let (_, issues) = scan_groups(&rows);
        let columns: Vec<&str> = issues.iter().map(|issue| issue.column.as_str()).collect();
        assert!(columns.contains(&HEADER_LABOR_HOURS), "工时不是数字");
        assert!(columns.contains(&HEADER_STD_QTY), "用量 0 不合法");
        assert!(columns.contains(&HEADER_STD_PRICE), "单价为负不合法");
        assert!(issues.iter().all(|issue| issue.line == 7));
    }

    #[test]
    fn blank_head_numbers_mean_zero_and_round_to_column_precision() {
        let mut row = raw(2, "FG-1", "2026-11", "", "", "");
        row.std_labor_hours = "  ".to_string();
        row.std_labor_rate = "".to_string();
        row.std_overhead_cost = "0.5".to_string();
        let (groups, issues) = scan_groups(&[row]);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(groups[0].head.hours, Decimal::ZERO);
        assert_eq!(groups[0].head.rate, Decimal::ZERO);
        assert_eq!(groups[0].head.overhead, Decimal::from_str("0.5").unwrap());
    }

    #[test]
    fn remark_is_capped_at_the_single_entry_limit() {
        let mut row = raw(2, "FG-1", "2026-11", "", "", "");
        row.remark = "备".repeat(REMARK_MAX + 1);
        let (_, issues) = scan_groups(&[row]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].column, HEADER_REMARK);
    }

    #[test]
    fn rejected_result_counts_distinct_lines() {
        let issues = vec![
            ImportIssue::new(2, HEADER_MATERIAL_CODE, "a"),
            ImportIssue::new(2, HEADER_PERIOD, "b"),
            ImportIssue::new(5, HEADER_PERIOD, "c"),
        ];
        let res = StandardImportRes::rejected(issues);
        assert_eq!(res.success, 0);
        assert_eq!(res.failed, 2, "同一行两条错只算一行");
        assert_eq!(res.versions_created, 0);
        assert_eq!(res.rows.len(), 3);
    }

    #[test]
    fn done_result_reports_rows_and_versions() {
        let res = StandardImportRes::done(9, 3);
        assert_eq!((res.success, res.failed, res.versions_created), (9, 0, 3));
    }

    #[test]
    fn template_headers_carry_the_required_star_only_on_group_keys() {
        let headers: Vec<String> = COLUMNS.iter().map(ImportColumn::template_header).collect();
        assert_eq!(headers[0], "物料编码*");
        assert_eq!(headers[1], "期间*");
        assert!(headers[2..].iter().all(|header| !header.contains('*')));
    }

    #[test]
    fn rows_xlsx_round_trips_through_the_reader() {
        let rows = vec![
            raw(2, "FG-1", "2026-11", "RM-1", "1.5", "2"),
            raw(3, "FG-1", "2026-11", "RM-2", "2", "3"),
        ];
        let bytes = build_rows_xlsx(&rows).expect("写 xlsx");
        let read = read_rows(&bytes).expect("读回来");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].material_code, "FG-1");
        assert_eq!(read[0].component_code, "RM-1");
        assert_eq!(read[1].std_qty, "2");
        // 行号从 2 起（表头是第 1 行）
        assert_eq!(read[0].line, 2);
        assert_eq!(read[1].line, 3);
    }

    #[test]
    fn template_opens_and_its_example_row_is_skipped() {
        let bytes = build_template().expect("写模板");
        let reader = SheetReader::open(&bytes, &COLUMNS).expect("模板能被自己的列定义打开");
        assert_eq!(reader.header().missing_required.len(), 0);
        // 示例行必须能被识别（否则用户照模板填完直接导入会多出一行假数据）
        let data: Vec<RawRow> = reader
            .data_rows()
            .enumerate()
            .filter(|(_, row)| !SheetReader::is_blank(row) && !reader.is_example_row(row))
            .map(|(index, row)| RawRow {
                line: SheetReader::excel_row_no(index),
                material_code: cell(&reader, row, 0),
                ..RawRow::default()
            })
            .collect();
        assert!(data.is_empty(), "模板里除示例行外不该有数据行，实际 {}", data.len());
    }

    #[test]
    fn not_an_xlsx_is_rejected_with_400() {
        let error = read_rows(b"not-an-xlsx-at-all").expect_err("非 xlsx 必须拒");
        assert_eq!(error.code, error_code::IMPORT_FILE_INVALID);
    }

    #[test]
    fn column_rank_follows_template_order() {
        assert_eq!(column_rank(HEADER_MATERIAL_CODE), 0);
        assert_eq!(column_rank(HEADER_REMARK), 8);
        assert_eq!(column_rank("不存在"), COLUMNS.len());
    }

    #[test]
    fn decorate_names_the_group_and_every_line() {
        let rows = vec![
            raw(2, "FG-1", "2026-11", "RM-1", "1", "2"),
            raw(3, "FG-1", "2026-11", "RM-2", "1", "2"),
        ];
        let (groups, _) = scan_groups(&rows);
        let error = decorate(dto::bad_request("撞了"), &groups[0]);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("FG-1 @ 2026-11"), "{}", error.msg);
        assert!(error.msg.contains("第 2、3 行"), "{}", error.msg);
        assert!(error.msg.contains("一条都没入库"), "{}", error.msg);
    }
}

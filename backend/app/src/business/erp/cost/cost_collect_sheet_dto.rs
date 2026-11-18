//! 按生产订单/批次归集表 · 契约层（成本核算 · M5-02【M5·成本】分批法 ①）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//! D-3（一期只做分批法、**不引入工单**）、D-4（成本对象＝生产批本身，
//! 按订单生产用 `biz_cost_object.source_doc_no` 记订单号即可覆盖）、
//! §3.1 ②③（`biz_cost_object` / `biz_cost_collect`）、§4.3（料/工/费三路归集）。
//!
//! # 这张表是什么（一张表，一个口径）
//!
//! **行 ＝ 一个批次（成本对象）**，列 ＝ 该批次在**指定会计期间**已归集的
//! 直接材料 / 直接人工 / 制造费用 / 合计，外加批次身份列（生产订单号、产出批号、
//! 产出品、车间、计划/完工/在制数量、状态）。表尾给整张表的合计。
//!
//! ```text
//! 期间 2026-11（本月）
//! ┌──────────────┬────────────┬──────────┬────────┬────────┬────────┬────────┬──────────┐
//! │ 批次         │ 生产订单号 │ 产出品   │ 材料   │ 人工   │ 制造费 │ 合计   │ 归集行数 │
//! ├──────────────┼────────────┼──────────┼────────┼────────┼────────┼────────┼──────────┤
//! │ CB-20261101-…│ SO-2026-018│ 支架     │ 1200.00│ 320.00 │  88.00 │ 1608.00│ 6        │
//! │ CB-20261103-…│ SO-2026-021│ 底座     │    0.00│   0.00 │   0.00 │    0.00│ 0        │
//! └──────────────┴────────────┴──────────┴────────┴────────┴────────┴────────┴──────────┘
//! 合计：材料 1200.00 / 人工 320.00 / 制造费用 88.00 / 合计 1608.00（2 个批次，1 个有归集）
//! ```
//!
//! # 为什么行是「批次」而不是「生产订单」（卡面「按生产订单/批次」的落地口径）
//!
//! ARCH-9 D-3 / D-4 定稿：**一期不引入工单**，一单一批是分批法的常规实现 ——
//! 生产订单号就是批次的 `source_doc_no` 列。所以「按生产订单归集」与「按批次归集」
//! 在一期是**同一张表**：批次行 + 生产订单号列，`keyword` 可按订单号搜、
//! `sort=order_no` 可按订单号排。真要做「一张订单多个批次合并汇总」，
//! 要等三期 `object_type='ORDER'`（接 MES 工单）落地 —— 那时加 `group_by`
//! 参数才有意义；现在加一个只有唯一取值 `batch` 的参数是死旋钮。
//!
//! # 为什么「没有归集的批次」也出现在表里
//!
//! 期初在产、本月还没投料的批次，归集额是三行 0 —— 它不是噪声，而是**缺口**：
//! 看着 0 才知道「这批本月既没领料也没报工」。把它们隐掉，用户只能靠
//! 未归集清单（B2-12）反推，而那张清单只列「挂了对象但缺工时/缺费率」的情形，
//! 不列「整批一条都没归集」。所以本表**不提供 `include_empty` 开关**，一律列出；
//! `line_count = 0` 就是那面旗。
//!
//! # 期间是必填（而不是默认本月）
//!
//! 归集额是「期间 × 批次」的量，没有期间就没有这张表。默认本月会让用户
//! 在 1 月 1 日打开页面时看到一张空表却以为数据丢了。必填 + 前端记上次选择，
//! 比服务端猜一个强。
//!
//! # 数据权限
//!
//! 与成本对象列表**同一份**范围（`biz_cost_object.workshop_dept_id`，
//! 登记在 `scope::SCOPED_TABLES`）：表里的行就是成本对象，本接口不多露一列
//! 「别的车间的成本」。所以权限码也**复用** `business:erp:cost-object:list`
//! （见 [`PERM_LIST`] 的说明），不新增码、不新增菜单。
//!
//! # 错误码
//!
//! 不新增业务码：期间格式错 ⇒ `400 + PARAM_INVALID`（复用
//! `cost_collect_dto::period_invalid` 的同一句报文）；`status` / `sort` 值域外 ⇒
//! `400 + PARAM_INVALID`；批次不存在或不在你的数据范围 ⇒ `404 + RESOURCE_NOT_FOUND`。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::cost::cost_collect_dto;
use crate::business::erp::cost::cost_object_dto;
use crate::business::erp::ledger;

// ==================== 权限码 ====================

/// 列表 / 明细的权限码：**复用** `business:erp:cost-object:list`。
///
/// 三条理由（决定，不是待办）：
///
/// 1. **不多露数据**：本表的每一行都是一个成本对象，每一列（归集额、订单号、
///    批号、产量）在 `GET /business/erp/cost-object` 与详情里都已经返回给
///    同一个码的持有人 —— 新开一个码不改变任何人能看到什么。
/// 2. **不留死码**：新开码要连带种 `sys_permission` / `sys_menu`（迁移），
///    而一张新页面得等 M5-07「前端：批次成本页」才落地；先插一条指向
///    不存在组件的菜单行，会让侧栏点开即 404 / 前端路由守卫报组件缺失。
/// 3. **同模块先例**：`cost_uncollected_handler`（B2-12 未归集清单）与
///    `cost_period_sheet`（B4-1 计算单导出）都复用既有 `cost-period:list`，
///    「不新增码、不新增迁移」是本模块的既有口径。
pub const PERM_LIST: &str = "business:erp:cost-object:list";

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形，便于汇总脚本收集）。
pub const PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 值域 ====================

/// 状态筛选：全生产状态（`ALL` 也是**默认**，见 [`DEFAULT_STATUS`]）。
pub const STATUS_ALL: &str = "ALL";
/// 在产。
pub const STATUS_OPEN: &str = "OPEN";
/// 关批。
pub const STATUS_CLOSED: &str = "CLOSED";
/// 作废。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 状态筛选取值（顺序即界面下拉框顺序：先「全部」，再按生命周期）。
pub const STATUSES: [&str; 4] = [STATUS_ALL, STATUS_OPEN, STATUS_CLOSED, STATUS_CANCELLED];

/// 默认状态：**不限**。
///
/// 为什么不默认 `OPEN`：本期已关批的批次同样会本期归集（月内关批是常态），
/// 默认藏起来会让「表尾合计」与用户心里的「本月花了多少」对不上。
/// 想看在产清单的人自己选 `OPEN`（前端记住上次选择）。
pub const DEFAULT_STATUS: &str = STATUS_ALL;

/// 排序：合计金额倒序（默认 —— 打开就看「本月最贵的批」）。
pub const SORT_TOTAL: &str = "total";
/// 排序：批次号倒序。
pub const SORT_OBJECT: &str = "object_no";
/// 排序：生产订单号倒序。
pub const SORT_ORDER: &str = "order_no";
/// 排序：开工日期倒序。
pub const SORT_START: &str = "start_date";

/// 排序白名单（**只有这四个**，`ORDER BY` 原文由 [`order_by_sql`] 按常量返回，
/// 用户输入永远不进 SQL 文本）。
pub const SORTS: [&str; 4] = [SORT_TOTAL, SORT_OBJECT, SORT_ORDER, SORT_START];

/// 默认排序。
pub const DEFAULT_SORT: &str = SORT_TOTAL;

// ==================== 入参 ====================

/// `GET /business/erp/cost-collect-sheet` 的查询参数。
///
/// 全部字段 `Option`：缺省值由 [`filter_of`] 归一化（错误信息里点名「收到什么」）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SheetQuery {
    /// 会计期间 `yyyy-MM`（**必填**）。
    pub period_code: Option<String>,
    /// 关键字：批次号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称（模糊）。
    pub keyword: Option<String>,
    /// 生产车间（精确；`sys_dept.id`）。
    pub workshop_dept_id: Option<String>,
    /// 批次状态（`ALL` / `OPEN` / `CLOSED` / `CANCELLED`；默认 `ALL`）。
    pub status: Option<String>,
    /// 排序（[`SORTS`] 之一；默认 `total`）。
    pub sort: Option<String>,
    /// 页码（从 1 开始；缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省取模块默认值，上限 200；见 `erp::model::normalize_page`）。
    pub page_size: Option<u64>,
}

/// 明细接口的查询参数：只需要期间（批次 id 在路径上）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SheetDetailQuery {
    /// 会计期间 `yyyy-MM`（**必填**）。
    pub period_code: Option<String>,
}

/// 归一化后的筛选条件（落库与单测都拿它）。
#[derive(Debug, Clone)]
pub struct SheetFilter {
    /// 会计期间 `yyyy-MM`（已校验）。
    pub period_code: String,
    /// 关键字（已 trim；`None` = 不筛）。
    pub keyword: Option<String>,
    /// 生产车间。
    pub workshop_dept_id: Option<Uuid>,
    /// 状态（`None` = 不限，即 `ALL`）。
    pub status: Option<String>,
    /// 排序键（[`SORTS`] 之一）。
    pub sort: String,
    /// 页码（≥ 1）。
    pub page: u64,
    /// 每页条数（1–200）。
    pub page_size: u64,
}

// ==================== 归一化（纯逻辑） ====================

/// 期间必填 + `yyyy-MM` 形状校验（列表与明细共用）。
pub fn period_of(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).filter(|text| !text.is_empty()).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "period_code 必填（yyyy-MM，例如 2026-11）：归集额是「期间 × 批次」的量，没有期间就没有这张表".to_string(),
        )
    })?;
    // 形状 + 真实日期双查：`2026-13` 过不了 `month_range`（与 B2-6 起所有期间入参同口径）。
    if ledger::month_range(text).is_none() {
        return Err(cost_collect_dto::period_invalid(text));
    }
    Ok(text.to_string())
}

/// 列表入参 → 归一化筛选（**纯逻辑**，单测直接喂 `SheetQuery`）。
pub fn filter_of(query: &SheetQuery) -> std::result::Result<SheetFilter, ApiError> {
    let period_code = period_of(query.period_code.as_deref())?;

    let status = match cost_object_dto::clean_optional(query.status.clone()) {
        None => None,
        Some(raw) => {
            let upper = raw.to_ascii_uppercase();
            if !STATUSES.contains(&upper.as_str()) {
                return Err(bad_request(format!(
                    "status 只认 {STATUSES:?}（收到「{raw}」）；ALL = 不限状态"
                )));
            }
            // `ALL` 归一成 `None`：SQL 里少一个条件，不用 `status <> 'ALL'` 这种假谓词。
            if upper == STATUS_ALL { None } else { Some(upper) }
        }
    };

    let sort = match cost_object_dto::clean_optional(query.sort.clone()) {
        None => DEFAULT_SORT.to_string(),
        Some(raw) => {
            let lower = raw.to_ascii_lowercase();
            if !SORTS.contains(&lower.as_str()) {
                return Err(bad_request(format!("sort 只认 {SORTS:?}（收到「{raw}」）")));
            }
            lower
        }
    };

    let (page, page_size) =
        crate::business::erp::model::normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));

    Ok(SheetFilter {
        period_code,
        keyword: cost_object_dto::clean_optional(query.keyword.clone()),
        workshop_dept_id: cost_object_dto::parse_opt_uuid(query.workshop_dept_id.as_deref(), "workshop_dept_id")?,
        status,
        sort,
        page,
        page_size,
    })
}

/// 排序键 → `ORDER BY` 原文（**常量返回**，用户输入永不进 SQL）。
///
/// 每个排序都以 `t.object_no DESC, t.id ASC` 收尾：没有全序时翻页会漏行 / 重复行
/// （与其余列表同款，架构 §2.3）。
pub fn order_by_sql(sort: &str) -> &'static str {
    match sort {
        SORT_OBJECT => "ORDER BY t.object_no DESC, t.id ASC",
        SORT_ORDER => "ORDER BY COALESCE(t.source_doc_no, '') DESC, t.object_no DESC, t.id ASC",
        SORT_START => "ORDER BY t.start_date DESC, t.object_no DESC, t.id ASC",
        // `total` 与任何未知值都落到默认排序：值域在 `filter_of` 已拦，这里只是兜底。
        _ => {
            "ORDER BY (COALESCE(a.mat_amount, 0) + COALESCE(a.labor_amount, 0) \
             + COALESCE(a.oh_amount, 0)) DESC, t.object_no DESC, t.id ASC"
        }
    }
}

/// 400 的薄包装（与 `cost_object_dto::bad_request` 同一条出口）。
fn bad_request(message: String) -> ApiError {
    cost_object_dto::bad_request(message)
}

// ==================== 出参 ====================

/// 三要素金额 + 合计（合计由三项相加，不另查一次库 —— 三个数加起来必须等于总数）。
#[derive(Debug, Clone, Serialize)]
pub struct SheetAmounts {
    /// 直接材料。
    pub mat: String,
    /// 直接人工。
    pub labor: String,
    /// 制造费用。
    pub oh: String,
    /// 合计 = 材料 + 人工 + 制造费用。
    pub total: String,
}

impl SheetAmounts {
    /// 由三项构造（`total` 一定等于三项之和）。
    pub fn of(mat: Decimal, labor: Decimal, oh: Decimal) -> Self {
        Self {
            mat: amount_text(mat),
            labor: amount_text(labor),
            oh: amount_text(oh),
            total: amount_text(mat + labor + oh),
        }
    }
}

/// 归集表的合计行（整张表，**不是当前页**）。
#[derive(Debug, Clone, Serialize)]
pub struct SheetTotalsRes {
    /// 三项金额 + 合计。
    #[serde(flatten)]
    pub amounts: SheetAmounts,
    /// 归集行数合计（料 + 工 + 费）。
    pub line_count: i64,
    /// 有归集行（`line_count > 0`）的批次数。
    pub batch_count: i64,
}

/// 归集表一行（一个批次）。
#[derive(Debug, Clone, Serialize)]
pub struct SheetRowRes {
    /// 成本对象 id（明细接口用）。
    pub object_id: String,
    /// 批次号 `CB-yyyyMMdd-NNNNNN`。
    pub object_no: String,
    /// 对象类型（一期恒 `BATCH`）。
    pub object_type: String,
    /// 生产订单号（`source_doc_no`；按订单生产的批次才有）。
    pub source_doc_no: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: String,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 生产车间 id。
    pub workshop_dept_id: String,
    /// 生产车间名称（实时 join 优先、建单快照兜底）。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: String,
    /// 累计完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 期末在制完工进度%。
    pub completion_pct: String,
    /// 批次状态。
    pub status: String,
    /// 开工日期。
    pub start_date: String,
    /// 计划完工日期。
    pub due_date: Option<String>,
    /// 本期归集金额（材料/人工/费用/合计）。
    #[serde(flatten)]
    pub amounts: SheetAmounts,
    /// 本期归集行数（`0` = 本期该批一条都没归集，是缺口标记）。
    pub line_count: i64,
    /// 本期最后一笔归集的业务日期（没有归集则为空）。
    pub last_occur_date: Option<String>,
}

/// 归集表分页响应。
#[derive(Debug, Clone, Serialize)]
pub struct SheetRes {
    /// 本次查询的期间。
    pub period_code: String,
    /// 整张表的合计（**不是当前页**）。
    pub totals: SheetTotalsRes,
    /// 当前页的行。
    pub list: Vec<SheetRowRes>,
    /// 总条数（符合筛选的批次数）。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 明细接口里的批次抬头。
#[derive(Debug, Clone, Serialize)]
pub struct SheetBatchRes {
    /// 成本对象 id。
    pub object_id: String,
    /// 批次号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 生产订单号。
    pub source_doc_no: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: String,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 生产车间 id。
    pub workshop_dept_id: String,
    /// 生产车间名称。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: String,
    /// 累计完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 完工进度%。
    pub completion_pct: String,
    /// 状态。
    pub status: String,
    /// 开工日期。
    pub start_date: String,
    /// 计划完工日期。
    pub due_date: Option<String>,
}

/// 归集明细一行（料 / 工 / 费三路都在这里，靠 `element` 分流）。
#[derive(Debug, Clone, Serialize)]
pub struct SheetLineRes {
    /// 归集行 id。
    pub id: String,
    /// 要素 `MATERIAL` / `LABOR` / `OVERHEAD`。
    pub element: String,
    /// 要素中文名（界面直接用，不在前端再抄一份映射）。
    pub element_label: String,
    /// 来源 `STOCK_OUT` / `LABOR_TICKET` / `OVERHEAD_TICKET` / `DEPRECIATION` /
    /// `EXPENSE` / `ALLOC` / `MANUAL`。
    pub source_type: String,
    /// 来源类型中文名。
    pub source_type_label: String,
    /// 来源行 id（手工行为空）。
    pub source_id: Option<String>,
    /// 来源单号快照。
    pub source_no: Option<String>,
    /// 来源行号。
    pub line_no: i32,
    /// 业务日期。
    pub occur_date: String,
    /// 料行物料 id（工 / 费行为空）。
    pub material_id: Option<String>,
    /// 料行物料编码。
    pub material_code: Option<String>,
    /// 料行物料名称。
    pub material_name: Option<String>,
    /// 料行数量（工 / 费行为空）。
    pub qty: Option<String>,
    /// 料行单价（工 / 费行为空）。
    pub unit_price: Option<String>,
    /// 归集金额。
    pub amount: String,
    /// 归属车间名快照（费行才有）。
    pub dept_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 明细响应：抬头 + 本期合计 + 逐笔归集行。
#[derive(Debug, Clone, Serialize)]
pub struct SheetDetailRes {
    /// 本次查询的期间。
    pub period_code: String,
    /// 批次抬头。
    pub batch: SheetBatchRes,
    /// 本期合计（由 `lines` 相加，单一口径）。
    pub totals: SheetAmounts,
    /// 逐笔归集行（按要素 → 业务日期 → 来源单号排序）。
    pub lines: Vec<SheetLineRes>,
}

// ==================== 标签 / 文本 ====================

/// 金额 → 字符串（与 `cost_object_model::amount_text` 同口径：不去尾零、不改标度）。
pub fn amount_text(value: Decimal) -> String {
    value.to_string()
}

/// 要素 → 中文名。
pub fn element_label(element: &str) -> &'static str {
    match element {
        "MATERIAL" => "直接材料",
        "LABOR" => "直接人工",
        "OVERHEAD" => "制造费用",
        _ => "其它",
    }
}

/// 来源类型 → 中文名（与 V0371 的 `ck_biz_cost_collect_src` 逐值对应）。
pub fn source_type_label(source_type: &str) -> &'static str {
    match source_type {
        "STOCK_OUT" => "领料出库",
        "LABOR_TICKET" => "工时台账",
        "OVERHEAD_TICKET" => "费用台账",
        "DEPRECIATION" => "折旧",
        "EXPENSE" => "报销",
        "ALLOC" => "分摊转入",
        "MANUAL" => "手工录入",
        _ => "其它",
    }
}

/// 日期 → `yyyy-MM-dd`（空值原样返回 `None`，前端不用再判）。
pub fn date_text(value: Option<NaiveDate>) -> Option<String> {
    value.map(|date| date.to_string())
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn query(period: &str) -> SheetQuery {
        SheetQuery {
            period_code: Some(period.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn period_is_required_and_shaped() {
        // 缺失 / 空白 ⇒ 400（报文点名「必填」）
        let error = filter_of(&SheetQuery::default()).expect_err("缺期间必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("必填"), "报文要说清必填：{}", error.msg);

        // 形状对但月份非法（13 月）⇒ 400，报文复用 B2-6 起的那句
        let error = filter_of(&query("2026-13")).expect_err("13 月必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("yyyy-MM"), "{}", error.msg);

        // 合法期间原样落地（含前后空白的 trim）
        let filter = filter_of(&query(" 2026-11 ")).expect("合法期间");
        assert_eq!(filter.period_code, "2026-11");
    }

    #[test]
    fn status_and_sort_defaults_and_value_domain() {
        let filter = filter_of(&query("2026-11")).expect("默认值应当合法");
        assert_eq!(filter.status, None, "默认 ALL ⇒ SQL 里不带状态条件");
        assert_eq!(filter.sort, SORT_TOTAL, "默认按合计金额倒序");
        assert_eq!((filter.page, filter.page_size), (1, 20), "缺省分页");

        // ALL 归一成 None；大小写不敏感
        let filter = filter_of(&SheetQuery {
            status: Some(" all ".to_string()),
            ..query("2026-11")
        })
        .expect("ALL 合法");
        assert_eq!(filter.status, None);

        let filter = filter_of(&SheetQuery {
            status: Some("open".to_string()),
            ..query("2026-11")
        })
        .expect("小写也认");
        assert_eq!(filter.status.as_deref(), Some("OPEN"));

        let error = filter_of(&SheetQuery {
            status: Some("SETTLED".to_string()),
            ..query("2026-11")
        })
        .expect_err("值域外必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("SETTLED"), "报文要回显收到什么：{}", error.msg);

        // 排序：大小写不敏感 + 值域外 400
        let filter = filter_of(&SheetQuery {
            sort: Some(" Order_No ".to_string()),
            ..query("2026-11")
        })
        .expect("order_no 合法");
        assert_eq!(filter.sort, SORT_ORDER);

        let error = filter_of(&SheetQuery {
            sort: Some("amount".to_string()),
            ..query("2026-11")
        })
        .expect_err("未知排序必须 400（绝不把用户输入拼进 ORDER BY）");
        assert_eq!(error.status, 400);
    }

    #[test]
    fn order_by_is_a_closed_set_of_constants() {
        // 每个白名单值都有确定原文，且都以对象号 + id 收全序（翻页不缺行）
        for sort in SORTS {
            let sql = order_by_sql(sort);
            assert!(sql.starts_with("ORDER BY "), "{sort} → {sql}");
            assert!(sql.ends_with("t.id ASC"), "{sort} 缺全序收尾：{sql}");
        }
        // 未知值兜底到默认排序，且默认排序按三要素之和
        assert_eq!(order_by_sql("whatever"), order_by_sql(SORT_TOTAL));
        assert!(order_by_sql(SORT_TOTAL).contains("a.mat_amount"));
        assert!(order_by_sql(SORT_TOTAL).contains("a.labor_amount"));
        assert!(order_by_sql(SORT_TOTAL).contains("a.oh_amount"));
    }

    #[test]
    fn pagination_is_clamped() {
        let filter = filter_of(&SheetQuery {
            page: Some(0),
            page_size: Some(10_000),
            ..query("2026-11")
        })
        .expect("边界值要能夹住而不是报错");
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 200, "模块上限 200（与 erp::model 同口径）");
    }

    #[test]
    fn workspace_filter_parses_uuid_and_rejects_garbage() {
        let uuid = Uuid::nil();
        let filter = filter_of(&SheetQuery {
            workshop_dept_id: Some(uuid.to_string()),
            ..query("2026-11")
        })
        .expect("合法 uuid");
        assert_eq!(filter.workshop_dept_id, Some(uuid));

        let error = filter_of(&SheetQuery {
            workshop_dept_id: Some("not-a-uuid".to_string()),
            ..query("2026-11")
        })
        .expect_err("非法 uuid 必须 400");
        assert_eq!(error.status, 400);
    }

    #[test]
    fn totals_always_add_up() {
        use std::str::FromStr;
        let amounts = SheetAmounts::of(
            Decimal::from_str("1200.00").expect("金额"),
            Decimal::from_str("320.5").expect("金额"),
            Decimal::from_str("88.25").expect("金额"),
        );
        assert_eq!(amounts.mat, "1200.00");
        assert_eq!(amounts.total, "1608.75", "合计必须等于三项之和");
    }

    #[test]
    fn labels_cover_every_domain_value() {
        // 与 V0371 的 CHECK 逐值对应：漏一个界面就会显示「其它」
        for element in ["MATERIAL", "LABOR", "OVERHEAD"] {
            assert_ne!(element_label(element), "其它", "{element} 缺中文名");
        }
        for source in [
            "STOCK_OUT",
            "LABOR_TICKET",
            "OVERHEAD_TICKET",
            "DEPRECIATION",
            "EXPENSE",
            "ALLOC",
            "MANUAL",
        ] {
            assert_ne!(source_type_label(source), "其它", "{source} 缺中文名");
        }
        assert_eq!(element_label("WEIRD"), "其它");
    }

    #[test]
    fn permission_code_is_reused_not_invented() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(PERMISSIONS, [PERM_LIST]);
    }
}

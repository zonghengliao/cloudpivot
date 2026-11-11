//! 凭证查询（批次 D · P2-D1）：**只读**接口 —— 凭证分页 + 凭证详情（含全部分录）。
//!
//! 表结构以 `backend/migrations/business/V0149__erp_voucher.up.sql` 为准
//! （2026-10-31 开工 `\d biz_voucher` / `\d biz_voucher_entry` 实测，`cloudpivot_dev` 已到 V0149）：
//!
//! ```text
//! biz_voucher        id, voucher_word(默认「记」), voucher_no, period(yyyy-MM), voucher_date,
//!                    voucher_type(AUTO/MANUAL), source_type, source_id, source_no, summary,
//!                    total_debit, total_credit, entry_count, status(POSTED/DRAFT/AUDITED/CANCELLED),
//!                    is_red, reverse_of, reversed_by, posted_at, posted_by, remark + 审计五件套
//! biz_voucher_entry  id, voucher_id, line_no, summary, account_id, account_code, account_name,
//!                    debit_amount, credit_amount, aux_supplier_id, aux_material_id,
//!                    aux_warehouse_id, aux_dept_id, aux_qty, aux_extra, created_at
//! ```
//!
//! # 接口（卡面 §二 ①②）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/voucher/page` | `business:erp:voucher:list` | 分页（期间 / 状态 / 凭证类型 / 来源类型 / 凭证号） |
//! | GET | `/business/erp/voucher/{id}` | `business:erp:voucher:detail` | 详情 + **全部分录**（科目编码 / 名称已快照在分录行上） |
//! | GET | `/business/erp/voucher/by-source` | `business:erp:voucher:list` | **按来源单据反查**该单据产生的全部凭证（含红字，记账时间倒序） |
//!
//! # 只读，不改任何账
//!
//! 本模块**没有**写路径：凭证由 P2-C1 的 [`crate::business::erp::voucher`] 在收货过账的同一事务里生成，
//! 红冲也由那边负责。这里只查（卡面：「只读接口，不做前端」）。
//!
//! # 与卡面的三处口径差异（都写在这里，免得下游照卡面踩）
//!
//! 1. **列表分页走 `scope::paginate_unscoped`**（卡面硬红线 6）：凭证是会计台账，
//!    V0149 无 `dept_id` / 归属人列，数据范围表达不出来，与 `business/erp/model.rs` 同口径。
//! 2. **`status` 只做白名单校验**（`POSTED/DRAFT/AUDITED/CANCELLED`）：非法值返回 400 而不是
//!    「静默返回空列表」——静默空列表会让前端以为「本期没凭证」，是静默错数据。
//!    凭证**默认展示全部状态**（列表是账务人员的核对入口，草稿也要看得见）；
//!    「只算 POSTED」是**总账 / 余额表**的口径（见 [`crate::business::erp::ledger`]），不是列表的口径。
//! 3. **`voucher_no` 是模糊匹配**（`LIKE %x%`）：精确号也在其中，前端搜索框直接可用。
//!
//! # 按来源单据反查（VTRACE-3）
//!
//! `GET /business/erp/voucher/by-source?source_type=&source_id=` 回答的是
//! **「这张业务单据产生过哪些凭证」**（前端「查看凭证 (N)」按钮与全链路视图的数据源）：
//!
//! - 返回**全部**凭证，红字也在内（红冲后原凭证 + 红字凭证两条都能看到）；
//! - 按 `posted_at DESC NULLS LAST, created_at DESC` 排（最新一笔在最前）；
//! - **空结果返回 `[]`，不是 404**：没生成过凭证是正常业务状态（未记账 / 只建单），
//!   前端据此决定按钮显示与否；404 会让「查不到」和「单据不存在」两件事混在一起。
//! - `source_type` 必须在 [`crate::business::shared::doc_kind::VoucherSourceType`] 值域内，
//!   非法值 **400**（静默返回空数组 = 让前端以为「这张单没凭证」，是静默错数据，与
//!   `status` / `voucher_type` 的校验口径一致）。
//!
//! 与单张版 [`crate::business::erp::voucher::find_voucher_by_source`] 的关系：
//! 那个函数是过账幂等 / 红冲前置检查用的闸（`is_red = 0` + `LIMIT 1`，只回答「有没有」），
//! 本接口要的是清单（含红字 + 关联红冲凭证号），所以走本模块的 `HEADER_COLUMNS`；
//! 两者的口径一致性由真库单测 `vtrace3_by_source_agrees_with_find_voucher_by_source` 钉住。
//!
//! # 详情的一致性闸：分录合计必须等于单头合计
//!
//! `biz_voucher` 有 `CHECK (total_debit = total_credit)`，但**没有**「分录合计 = 单头合计」的约束
//! （分录可以事后被别处改坏）。详情接口在返回前把分录合计与单头再比一次，不等 → `500 + 66009`
//! 并把差额写进消息（卡面硬红线 3 的同一口径：账不平不许装作没事）。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_voucher / biz_voucher_entry 是会计台账（V0149 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:voucher:* 权限码控制。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::model::normalize_page;
use crate::business::shared::doc_kind::VoucherSourceType;
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

// ==================== 表名 / 常量 ====================

/// 凭证主表。
pub const TABLE: &str = "biz_voucher";

/// 凭证分录表。
pub const ENTRY_TABLE: &str = "biz_voucher_entry";

/// 状态白名单（与 `ck_biz_voucher_status` 逐字一致）。
pub const STATUSES: [&str; 4] = ["POSTED", "DRAFT", "AUDITED", "CANCELLED"];

/// 凭证类型白名单（与 `ck_biz_voucher_type` 逐字一致）。
pub const VOUCHER_TYPES: [&str; 2] = ["AUTO", "MANUAL"];

/// `biz_voucher` 列清单。
///
/// 末尾两列是**关联凭证号**（P7 起）：红字凭证要知道自己红冲的是哪张（`reverse_of_no`），
/// 被红冲的原凭证要知道是谁红冲的（`reversed_by_no`）—— 列表 / 详情都要显示
/// 「红冲 PZ-xxx」/「已红冲 → PZR-xxx」，只给 id 前端还得再打一次详情。
///
/// `pending_reverse_instance_id` / `reverse_reason`（P-FLOW-3B 补漏）也必须在这里查出来：
/// 列表行不带这两个字段时，前端只能再打一次 `flow/instance/page?biz_type=VOUCHER_REVERSE`
/// 才能画出「红冲审批中」徽标 —— 多一次往返，而且**没有 `business:flow:instance:read` 的账号
/// 压根看不到徽标**（数据可见性会过滤掉那些实例行），是可见性缺陷而不是性能问题。
pub const HEADER_COLUMNS: &str = "id, voucher_word, voucher_no, period, voucher_date, voucher_type, \
     source_type, source_id, source_no, source_doc_no, summary, total_debit, total_credit, entry_count, status, \
     is_red, reverse_of, reversed_by, posted_at, posted_by, remark, \
     pending_reverse_instance_id, reverse_reason, created_at, updated_at, \
     (SELECT r.voucher_no FROM biz_voucher r WHERE r.id = biz_voucher.reverse_of) AS reverse_of_no, \
     (SELECT r.voucher_no FROM biz_voucher r WHERE r.id = biz_voucher.reversed_by) AS reversed_by_no";

/// `biz_voucher_entry` 列清单 + **四个辅助核算名字**（`line_no` 升序即凭证内顺序）。
///
/// 名字必须和 id 一起下发（AGENTS.md 约定 13）：只给 `aux_supplier_id` 前端就只能显示
/// 8 位短 uuid —— 查询页没有「新增凭证」弹窗那份按科目懒加载的 `auxOptions` 字典，
/// 永远落不到名字。
///
/// 取名字走 [`ENTRY_SOURCE`] 里的四个 `LEFT JOIN`，**一条 SQL 取齐**，绝不逐行查。
///
/// 列一律带 `e.` 前缀（因为 FROM 里还有四张主数据表同名 id 列）；四个名字显式
/// `AS aux_*_name`，`VoucherEntryRow::from_row` 按别名取值。
pub const ENTRY_COLUMNS: &str = "e.id, e.voucher_id, e.line_no, e.summary, e.account_id, e.account_code, \
     e.account_name, e.debit_amount, e.credit_amount, e.aux_supplier_id, e.aux_material_id, \
     e.aux_warehouse_id, e.aux_dept_id, e.aux_qty, \
     sup.supplier_name AS aux_supplier_name, mat.material_name AS aux_material_name, \
     wh.warehouse_name AS aux_warehouse_name, dept.dept_name AS aux_dept_name";

/// [`ENTRY_COLUMNS`] 配套的 FROM 子句：分录别名 `e` + 四张主数据表的 `LEFT JOIN`。
///
/// * 四个维度都是**弱引用**（V0149 不建外键），所以必须 `LEFT JOIN`：
///   用 `JOIN` 会让「辅助核算指向不存在的主数据」的行**整条消失**，凭证详情直接少分录 ——
///   账不平的报错反而更难查。
/// * 关联条件带 `deleted_at IS NULL`：主数据软删后名字置 `null`（不是报错），前端有短 id 兜底。
///   这与约定 13「主数据关联 → join 实时取名」一致：**改名后显示新名**，删了就不再声称它存在。
/// * `sys_dept` 是底座表，列名是 `dept_name`（不是 `name`，V0006）。
pub const ENTRY_SOURCE: &str = "biz_voucher_entry e \
     LEFT JOIN biz_supplier sup ON sup.id = e.aux_supplier_id AND sup.deleted_at IS NULL \
     LEFT JOIN biz_material mat ON mat.id = e.aux_material_id AND mat.deleted_at IS NULL \
     LEFT JOIN biz_warehouse wh ON wh.id = e.aux_warehouse_id AND wh.deleted_at IS NULL \
     LEFT JOIN sys_dept dept ON dept.id = e.aux_dept_id AND dept.deleted_at IS NULL";

/// 分页排序：日期倒序，同日按凭证字号倒序（最新凭证在前）。
pub const LIST_ORDER: &str = "ORDER BY voucher_date DESC, voucher_word DESC, voucher_no DESC";

/// 分页目标（`unscoped`：台账无归属列，理由见模块文档的 `SCOPE-EXEMPT`）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

// ==================== 入参 ====================

/// 凭证列表筛选（数据层形态）。
#[derive(Debug, Clone, Default)]
pub struct VoucherFilter {
    /// 会计期间（`yyyy-MM`，精确）。
    pub period: Option<String>,
    /// 凭证状态（精确，白名单）。
    pub status: Option<String>,
    /// 凭证类型（`AUTO` / `MANUAL`，精确）。
    pub voucher_type: Option<String>,
    /// 来源单据类型（`STOCK_IN` / `MANUAL` …，精确）。
    pub source_type: Option<String>,
    /// 业务单据号（**精确**，E2E-FIX-5 ②）：退货凭证按退货单号查就走这里。
    pub source_doc_no: Option<String>,
    /// 凭证号（**模糊**，`%x%`）。
    pub voucher_no: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 凭证列表 Query（`page` / `page_size` 缺失时由 [`normalize_page`] 规整）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VoucherPageQuery {
    /// 会计期间（`yyyy-MM`）。
    pub period: Option<String>,
    /// 凭证状态。
    pub status: Option<String>,
    /// 凭证类型。
    pub voucher_type: Option<String>,
    /// 来源单据类型。
    pub source_type: Option<String>,
    /// 业务单据号（精确，E2E-FIX-5 ②）。
    pub source_doc_no: Option<String>,
    /// 凭证号（模糊）。
    pub voucher_no: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl VoucherPageQuery {
    /// 转数据层筛选条件（`None` / 空串统一成「不筛」，空白串不当成条件）。
    pub fn to_filter(&self) -> VoucherFilter {
        VoucherFilter {
            period: blank_to_none(self.period.as_deref()),
            status: blank_to_none(self.status.as_deref()),
            voucher_type: blank_to_none(self.voucher_type.as_deref()),
            source_type: blank_to_none(self.source_type.as_deref()),
            source_doc_no: blank_to_none(self.source_doc_no.as_deref()),
            voucher_no: blank_to_none(self.voucher_no.as_deref()),
            page: self.page,
            page_size: self.page_size,
        }
    }

    /// 入参校验：状态 / 类型 / 期间格式非法一律 400（绝不静默忽略）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = blank_to_none(self.status.as_deref())
            && !STATUSES.contains(&status.as_str())
        {
            return Err(bad_request(format!(
                "status 只允许 {}，收到「{status}」",
                STATUSES.join(" / ")
            )));
        }
        if let Some(voucher_type) = blank_to_none(self.voucher_type.as_deref())
            && !VOUCHER_TYPES.contains(&voucher_type.as_str())
        {
            return Err(bad_request(format!(
                "voucher_type 只允许 {}，收到「{voucher_type}」",
                VOUCHER_TYPES.join(" / ")
            )));
        }
        if let Some(period) = blank_to_none(self.period.as_deref())
            && !is_month_key(&period)
        {
            return Err(bad_request(format!("period 必须是 yyyy-MM，收到「{period}」")));
        }
        Ok(())
    }
}

/// 按来源单据反查的 Query（VTRACE-3）：`?source_type=STOCK_IN&source_id=<uuid>`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VoucherBySourceQuery {
    /// 来源单据类型（`biz_voucher.source_type`，必须在 [`VoucherSourceType`] 值域内）。
    pub source_type: Option<String>,
    /// 来源单据 id（`biz_voucher.source_id`，UUID）。
    pub source_id: Option<String>,
}

impl VoucherBySourceQuery {
    /// 入参校验 → 规整后的 `(source_type, source_id)`。
    ///
    /// 三处都是 **400**（不是「静默查空」）：
    /// - `source_type` 空 → 让调用方知道参数没传（否则会查出一个空数组，看着像「这单没凭证」）；
    /// - `source_type` 不在值域 → 列全值域，照抄就能改对；
    /// - `source_id` 不是 UUID → 与 [`parse_uuid`] 同一口径。
    pub fn validate(&self) -> std::result::Result<(String, Uuid), ApiError> {
        let source_type = self.source_type.as_deref().map(str::trim).unwrap_or("");
        if source_type.is_empty() {
            return Err(bad_request("source_type 不能为空"));
        }
        let Some(kind) = VoucherSourceType::parse(source_type) else {
            return Err(bad_request(format!(
                "source_type 只允许 {}，收到「{source_type}」",
                VoucherSourceType::ALL.map(VoucherSourceType::as_str).join(" / ")
            )));
        };
        let source_id = parse_uuid(self.source_id.as_deref().unwrap_or(""), "source_id")?;
        Ok((kind.as_str().to_string(), source_id))
    }
}

/// 空白串 → `None`（`?period=` 不该变成「筛期间 = 空串」）。
fn blank_to_none(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// `yyyy-MM` 形态校验（长度 7、第 5 位是 `-`、月份 01–12）。
pub fn is_month_key(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return false;
    }
    let year_ok = bytes[..4].iter().all(u8::is_ascii_digit);
    let month_ok = bytes[5..].iter().all(u8::is_ascii_digit);
    if !(year_ok && month_ok) {
        return false;
    }
    matches!(text[5..].parse::<u8>(), Ok(month) if (1..=12).contains(&month))
}

/// 参数类 400（`10001`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 读：行结构 ====================

/// `biz_voucher` 行。
#[derive(Debug, Clone)]
pub struct VoucherRow {
    /// 主键。
    pub id: Uuid,
    /// 凭证字（默认「记」）。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 会计期间 `yyyy-MM`。
    pub period: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证类型（`AUTO` / `MANUAL`）。
    pub voucher_type: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Option<Uuid>,
    /// 来源单据号。
    pub source_no: Option<String>,
    /// 业务单据号（`biz_voucher.source_doc_no`，E2E-FIX-5 ②）：**退货单号落在这里**。
    ///
    /// 与 `source_no` 的差别只在「一张业务单据 → 多张子单据 → 多张凭证」的退货场景：
    /// 退货凭证的 `source_no` 是入库单 / 红字发票号（记账依据），本列是退货单号。
    /// 「按业务单号查凭证」永远查这一列（绝大多数单据两列同值）。
    pub source_doc_no: Option<String>,
    /// 摘要。
    pub summary: String,
    /// 借方合计。
    pub total_debit: Decimal,
    /// 贷方合计。
    pub total_credit: Decimal,
    /// 分录行数。
    pub entry_count: i32,
    /// 状态。
    pub status: String,
    /// 是否红字凭证（`1` = 红冲）。
    pub is_red: i16,
    /// 被红冲的原凭证。
    pub reverse_of: Option<Uuid>,
    /// 红冲它的凭证。
    pub reversed_by: Option<Uuid>,
    /// 记账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 记账人。
    pub posted_by: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 红冲审批实例 id（P-FLOW-3B）：有值 = 正在审批红冲（`status` 仍是 `POSTED`）。
    pub pending_reverse_instance_id: Option<Uuid>,
    /// 红冲原因（P-FLOW-3B）。
    pub reverse_reason: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 被红冲的原凭证号（`is_red = 1` 时有值）。
    pub reverse_of_no: Option<String>,
    /// 红冲它的红字凭证号（已红冲时有值）。
    pub reversed_by_no: Option<String>,
}

impl VoucherRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            voucher_word: row.get_typed("voucher_word")?,
            voucher_no: row.get_typed("voucher_no")?,
            period: row.get_typed("period")?,
            voucher_date: row.get_typed("voucher_date")?,
            voucher_type: row.get_typed("voucher_type")?,
            source_type: row.get_typed("source_type")?,
            source_id: row.get_typed("source_id")?,
            source_no: row.get_typed("source_no")?,
            source_doc_no: row.get_typed("source_doc_no")?,
            summary: row.get_typed("summary")?,
            total_debit: row.get_typed("total_debit")?,
            total_credit: row.get_typed("total_credit")?,
            entry_count: row.get_typed("entry_count")?,
            status: row.get_typed("status")?,
            is_red: row.get_typed("is_red")?,
            reverse_of: row.get_typed("reverse_of")?,
            reversed_by: row.get_typed("reversed_by")?,
            posted_at: row.get_typed("posted_at")?,
            posted_by: row.get_typed("posted_by")?,
            remark: row.get_typed("remark")?,
            pending_reverse_instance_id: row.get_typed("pending_reverse_instance_id")?,
            reverse_reason: row.get_typed("reverse_reason")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            reverse_of_no: row.get_typed("reverse_of_no")?,
            reversed_by_no: row.get_typed("reversed_by_no")?,
        })
    }
}

/// `biz_voucher_entry` 行（科目编码 / 名称是 P2-C1 落库时的**快照**）。
#[derive(Debug, Clone)]
pub struct VoucherEntryRow {
    /// 主键。
    pub id: Uuid,
    /// 所属凭证。
    pub voucher_id: Uuid,
    /// 行号（凭证内从 1 起）。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目 id。
    pub account_id: Uuid,
    /// 科目编码（快照）。
    pub account_code: String,
    /// 科目名称（快照）。
    pub account_name: String,
    /// 借方金额（与贷方互斥，见 `ck_biz_voucher_entry_side`）。
    pub debit_amount: Decimal,
    /// 贷方金额。
    pub credit_amount: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<Uuid>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<Uuid>,
    /// 辅助核算：部门。
    pub aux_dept_id: Option<Uuid>,
    /// 辅助核算：数量。
    pub aux_qty: Option<Decimal>,
    /// 供应商名（`LEFT JOIN biz_supplier` 实时取名；主数据已删 / 不存在 → `None`）。
    pub aux_supplier_name: Option<String>,
    /// 物料名（`LEFT JOIN biz_material`）。
    pub aux_material_name: Option<String>,
    /// 仓库名（`LEFT JOIN biz_warehouse`）。
    pub aux_warehouse_name: Option<String>,
    /// 部门名（`LEFT JOIN sys_dept`）。
    pub aux_dept_name: Option<String>,
}

impl VoucherEntryRow {
    /// 由查询行构造（要求 SELECT 用 [`ENTRY_COLUMNS`] + [`ENTRY_SOURCE`]）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            voucher_id: row.get_typed("voucher_id")?,
            line_no: row.get_typed("line_no")?,
            summary: row.get_typed("summary")?,
            account_id: row.get_typed("account_id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            debit_amount: row.get_typed("debit_amount")?,
            credit_amount: row.get_typed("credit_amount")?,
            aux_supplier_id: row.get_typed("aux_supplier_id")?,
            aux_material_id: row.get_typed("aux_material_id")?,
            aux_warehouse_id: row.get_typed("aux_warehouse_id")?,
            aux_dept_id: row.get_typed("aux_dept_id")?,
            aux_qty: row.get_typed("aux_qty")?,
            aux_supplier_name: row.get_typed("aux_supplier_name")?,
            aux_material_name: row.get_typed("aux_material_name")?,
            aux_warehouse_name: row.get_typed("aux_warehouse_name")?,
            aux_dept_name: row.get_typed("aux_dept_name")?,
        })
    }
}

// ==================== 出参 ====================

/// 凭证（列表行）。
#[derive(Debug, Clone, Serialize)]
pub struct VoucherRes {
    /// 主键。
    pub id: String,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 会计期间。
    pub period: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证类型。
    pub voucher_type: String,
    /// 凭证类型中文标签。
    pub voucher_type_label: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Option<String>,
    /// 来源单据号（记账依据 / 子单据号）。
    pub source_no: Option<String>,
    /// 业务单据号（E2E-FIX-5 ②）：退货凭证这里是**退货单号**，按业务单号查凭证用这一列。
    pub source_doc_no: Option<String>,
    /// 摘要。
    pub summary: String,
    /// 借方合计。
    pub total_debit: Decimal,
    /// 贷方合计。
    pub total_credit: Decimal,
    /// 分录行数。
    pub entry_count: i32,
    /// 状态。
    pub status: String,
    /// 状态中文标签。
    pub status_label: String,
    /// 是否红字凭证。
    pub is_red: bool,
    /// 被红冲的原凭证。
    pub reverse_of: Option<String>,
    /// 红冲它的凭证。
    pub reversed_by: Option<String>,
    /// 被红冲的原凭证号（`is_red = true` 时有值，用于显示「红冲 PZ-xxx」）。
    pub reverse_of_no: Option<String>,
    /// 红冲它的红字凭证号（已红冲时有值，用于显示「已红冲 → PZR-xxx」）。
    pub reversed_by_no: Option<String>,
    /// 记账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 记账人。
    pub posted_by: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 红冲审批实例 id（P-FLOW-3B）：**有值 = 正在审批红冲**（`status` 仍是 `POSTED`）。
    ///
    /// 前端据此画「红冲审批中」徽标并把「红冲」按钮换成「撤回红冲审批」，
    /// 判据是「字段存在且为 `null`」而不是「整个字段缺席」—— 所以列表 / 详情 /
    /// `ReverseSubmitRes` 三处都必须透出（后两者靠 `#[serde(flatten)]` 自动带上）。
    pub pending_reverse_instance_id: Option<String>,
    /// 红冲原因（P-FLOW-3B）：送审时填的那段文字，审批通过后成为红字凭证的分录摘要。
    pub reverse_reason: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

impl VoucherRes {
    /// 由行构造。
    pub fn from_row(row: &VoucherRow) -> Self {
        Self {
            id: row.id.to_string(),
            voucher_word: row.voucher_word.clone(),
            voucher_no: row.voucher_no.clone(),
            period: row.period.clone(),
            voucher_date: row.voucher_date,
            voucher_type: row.voucher_type.clone(),
            voucher_type_label: voucher_type_label(&row.voucher_type).to_string(),
            source_type: row.source_type.clone(),
            source_id: row.source_id.map(|id| id.to_string()),
            source_no: row.source_no.clone(),
            source_doc_no: row.source_doc_no.clone(),
            summary: row.summary.clone(),
            total_debit: row.total_debit,
            total_credit: row.total_credit,
            entry_count: row.entry_count,
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            is_red: row.is_red == 1,
            reverse_of: row.reverse_of.map(|id| id.to_string()),
            reversed_by: row.reversed_by.map(|id| id.to_string()),
            reverse_of_no: row.reverse_of_no.clone(),
            reversed_by_no: row.reversed_by_no.clone(),
            posted_at: row.posted_at,
            posted_by: row.posted_by.map(|id| id.to_string()),
            remark: row.remark.clone(),
            pending_reverse_instance_id: row.pending_reverse_instance_id.map(|id| id.to_string()),
            reverse_reason: row.reverse_reason.clone(),
            created_at: row.created_at,
        }
    }
}

/// 分录（详情内嵌）。
#[derive(Debug, Clone, Serialize)]
pub struct VoucherEntryRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目 id。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 借方金额。
    pub debit_amount: Decimal,
    /// 贷方金额。
    pub credit_amount: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<String>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<String>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<String>,
    /// 辅助核算：部门。
    pub aux_dept_id: Option<String>,
    /// 辅助核算：数量。
    pub aux_qty: Option<Decimal>,
    /// 辅助核算名字：供应商（`LEFT JOIN biz_supplier` 实时取名）。
    ///
    /// 主数据已删 / 不存在时为 `null`（**不是错误**）—— 前端有短 id 兜底，
    /// 契约是「名字能取到就给名字，取不到自己降级」，别在这里 500。
    pub aux_supplier_name: Option<String>,
    /// 辅助核算名字：物料（`LEFT JOIN biz_material`）。
    pub aux_material_name: Option<String>,
    /// 辅助核算名字：仓库（`LEFT JOIN biz_warehouse`）。
    pub aux_warehouse_name: Option<String>,
    /// 辅助核算名字：部门（`LEFT JOIN sys_dept`）。
    pub aux_dept_name: Option<String>,
}

impl VoucherEntryRes {
    /// 由行构造。
    pub fn from_row(row: &VoucherEntryRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            summary: row.summary.clone(),
            account_id: row.account_id.to_string(),
            account_code: row.account_code.clone(),
            account_name: row.account_name.clone(),
            debit_amount: row.debit_amount,
            credit_amount: row.credit_amount,
            aux_supplier_id: row.aux_supplier_id.map(|id| id.to_string()),
            aux_material_id: row.aux_material_id.map(|id| id.to_string()),
            aux_warehouse_id: row.aux_warehouse_id.map(|id| id.to_string()),
            aux_dept_id: row.aux_dept_id.map(|id| id.to_string()),
            aux_qty: row.aux_qty,
            aux_supplier_name: row.aux_supplier_name.clone(),
            aux_material_name: row.aux_material_name.clone(),
            aux_warehouse_name: row.aux_warehouse_name.clone(),
            aux_dept_name: row.aux_dept_name.clone(),
        }
    }
}

/// 凭证详情：单头 + 全部分录 + 一致性闸的结论。
#[derive(Debug, Clone, Serialize)]
pub struct VoucherDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub voucher: VoucherRes,
    /// 全部分录（`line_no` 升序）。
    pub entries: Vec<VoucherEntryRes>,
    /// 分录借方合计（与 [`VoucherRes::total_debit`] 必须相等）。
    pub entry_total_debit: Decimal,
    /// 分录贷方合计（与 [`VoucherRes::total_credit`] 必须相等）。
    pub entry_total_credit: Decimal,
    /// 单头与分录是否一致（恒为 `true`：不一致已在 [`voucher_detail`] 里被 500 拦掉）。
    pub balanced: bool,
}

// ==================== 标签 ====================

/// 凭证状态中文标签（未知状态原样返回，不 panic）。
pub fn status_label(status: &str) -> &str {
    match status {
        "POSTED" => "已记账",
        "DRAFT" => "草稿",
        "AUDITED" => "已审核",
        "CANCELLED" => "已作废",
        other => other,
    }
}

/// 凭证类型中文标签。
pub fn voucher_type_label(voucher_type: &str) -> &str {
    match voucher_type {
        "AUTO" => "自动",
        "MANUAL" => "手工",
        other => other,
    }
}

// ==================== 数据层 ====================

/// 凭证分页（**全部状态**；只算 POSTED 是总账口径，不是列表口径）。
pub async fn voucher_page(q: &VoucherFilter) -> DbResult<(Vec<VoucherRow>, i64)> {
    let mut sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(period) = q.period.as_deref() {
        sql.push_str(" AND period = ?");
        params.push(Value::from(period));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(voucher_type) = q.voucher_type.as_deref() {
        sql.push_str(" AND voucher_type = ?");
        params.push(Value::from(voucher_type));
    }
    if let Some(source_type) = q.source_type.as_deref() {
        sql.push_str(" AND source_type = ?");
        params.push(Value::from(source_type));
    }
    // 业务单据号（精确）：退货凭证按退货单号查凭证的唯一入口（E2E-FIX-5 ②）。
    if let Some(source_doc_no) = q.source_doc_no.as_deref() {
        sql.push_str(" AND source_doc_no = ?");
        params.push(Value::from(source_doc_no));
    }
    if let Some(pattern) = utils::like_pattern(q.voucher_no.as_deref()) {
        // 用户粘一个单号进来，不论它是凭证号、来源单号还是业务单号，都应该能搜到。
        sql.push_str(" AND (voucher_no LIKE ? OR source_no LIKE ? OR source_doc_no LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    let (page, page_size) = normalize_page(q.page, q.page_size);
    let result = scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), page, page_size).await?;
    let rows = result
        .list
        .iter()
        .map(VoucherRow::from_row)
        .collect::<DbResult<Vec<VoucherRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 单张凭证（未删除）。
pub async fn find_voucher(id: Uuid) -> DbResult<Option<VoucherRow>> {
    let sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL");
    let row = db().query_first_sql(sql, vec![Value::from(id)]).await?;
    row.as_ref().map(VoucherRow::from_row).transpose()
}

/// 按来源单据取**该单据产生的全部凭证**（含红字），记账时间倒序（VTRACE-3）。
///
/// 「全部」是有意的：红冲之后，一张单据在账上就是**两条**凭证（原凭证 `status` 仍为
/// `POSTED` 但 `reversed_by` 有值 + 一张 `is_red = 1` 的红字凭证），前端要把两条都列出来，
/// 才解释得清「这笔业务被红冲了」。所以这里**不过滤 `is_red`、不 `LIMIT`**。
///
/// 排序：`posted_at DESC NULLS LAST`（未记账的草稿排在已记账之后）+ `created_at DESC` 兜底
/// —— 同一秒内建的凭证也要有稳定顺序（`voucher_no DESC`），否则列表每次刷新都换位置。
///
/// 与 [`crate::business::erp::voucher::find_voucher_by_source`] 的分工在模块文档里：
/// 那个是「有没有」的闸（`is_red = 0` + `LIMIT 1`），本函数是「有哪些」的清单；
/// 两者共用 `idx_biz_voucher_source (source_type, source_id)`（V0149:80）。
///
/// 只要 id 的同类取数（固定资产处置 / 折旧的「N 张凭证」）见
/// [`crate::business::erp::asset_model::voucher_ids_of_source`] —— 它不返回行内容，本函数才返回。
pub async fn vouchers_by_source(source_type: &str, source_id: Uuid) -> DbResult<Vec<VoucherRow>> {
    // SCOPE-EXEMPT: biz_voucher 是会计台账（V0149 无 dept_id / 归属人列），可见性由权限码控制。
    let sql = format!(
        "SELECT {HEADER_COLUMNS} FROM {TABLE} \
          WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL \
          ORDER BY posted_at DESC NULLS LAST, created_at DESC, voucher_no DESC"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![Value::Text(source_type.to_string()), Value::from(&source_id)],
        )
        .await?;
    rows.iter().map(VoucherRow::from_row).collect()
}

/// 某张凭证的全部分录（`line_no` 升序），**四个辅助核算的名字一次 JOIN 取齐**。
///
/// SQL 形态：`SELECT {ENTRY_COLUMNS} FROM {ENTRY_SOURCE} WHERE e.voucher_id = ?`。
/// 四个名字走 `LEFT JOIN`，**不逐行回查**（AGENTS.md 约定 13 的防 N+1）；
/// 主数据已删 / 不存在的行名字为 `None`，行本身照样返回（弱引用，不是错误）。
pub async fn entries_of(voucher_id: Uuid) -> DbResult<Vec<VoucherEntryRow>> {
    let sql = format!("SELECT {ENTRY_COLUMNS} FROM {ENTRY_SOURCE} WHERE e.voucher_id = ?::uuid ORDER BY e.line_no");
    let rows = db().find_with_params(&sql, vec![Value::from(voucher_id)]).await?;
    rows.iter().map(VoucherEntryRow::from_row).collect()
}

/// 凭证详情：单头 + 全部分录；分录合计 ≠ 单头合计 → `500 + 66009`。
pub async fn voucher_detail(id: Uuid) -> std::result::Result<VoucherDetailRes, ApiError> {
    let row = find_voucher(id)
        .await
        .map_err(|error| utils::db_error(error, "查询凭证"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "凭证不存在或已删除".to_string()))?;
    let entries = entries_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询凭证分录"))?;
    let entry_total_debit = entries.iter().fold(Decimal::ZERO, |acc, line| acc + line.debit_amount);
    let entry_total_credit = entries.iter().fold(Decimal::ZERO, |acc, line| acc + line.credit_amount);
    if entry_total_debit != row.total_debit || entry_total_credit != row.total_credit {
        return Err(unbalanced_error(
            "凭证",
            &row.voucher_no,
            entry_total_debit,
            entry_total_credit,
            row.total_debit,
            row.total_credit,
        ));
    }
    Ok(VoucherDetailRes {
        voucher: VoucherRes::from_row(&row),
        entries: entries.iter().map(VoucherEntryRes::from_row).collect(),
        entry_total_debit,
        entry_total_credit,
        balanced: true,
    })
}

/// 账不平统一 `500 + 66009`（消息里带齐四个数，运维照抄就能定位）。
pub fn unbalanced_error(
    what: &str,
    key: &str,
    debit: Decimal,
    credit: Decimal,
    expected_debit: Decimal,
    expected_credit: Decimal,
) -> ApiError {
    ApiError::new(
        500,
        error_code::ERP_LEDGER_UNBALANCED,
        format!(
            "{what}「{key}」借贷不平衡：Σ借方={debit} Σ贷方={credit}（单头 {expected_debit} / {expected_credit}，\
             差额 {}）",
            debit - credit
        ),
    )
}

// ==================== 接口层 ====================

/// 凭证分页。
#[nik::permission(
    path = "/business/erp/voucher/page",
    method = "GET",
    permission = "business:erp:voucher:list"
)]
#[nik::get("/business/erp/voucher/page")]
pub async fn voucher_page_handler(Query(query): Query<VoucherPageQuery>) -> ResResult<PageData<Vec<VoucherRes>>> {
    query.validate()?;
    let (rows, total) = voucher_page(&query.to_filter())
        .await
        .map_err(|error| utils::db_error(error, "分页查询凭证"))?;
    let list = rows.iter().map(VoucherRes::from_row).collect();
    let (page, page_size) = normalize_page(query.page, query.page_size);
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 凭证详情（含全部分录）。
#[nik::permission(
    path = "/business/erp/voucher/{id}",
    method = "GET",
    permission = "business:erp:voucher:detail"
)]
#[nik::get("/business/erp/voucher/{id}")]
pub async fn voucher_detail_handler(Path(id): Path<String>) -> ResResult<VoucherDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(voucher_detail(id).await?))
}

/// 按来源单据反查凭证（VTRACE-3）：该单据产生的**全部**凭证（含红字），记账时间倒序。
///
/// 空结果返回 `[]`（不是 404）：没生成过凭证是正常业务状态。
#[nik::permission(
    path = "/business/erp/voucher/by-source",
    method = "GET",
    permission = "business:erp:voucher:list"
)]
#[nik::get("/business/erp/voucher/by-source")]
pub async fn voucher_by_source_handler(Query(query): Query<VoucherBySourceQuery>) -> ResResult<Vec<VoucherRes>> {
    let (source_type, source_id) = query.validate()?;
    let rows = vouchers_by_source(&source_type, source_id)
        .await
        .map_err(|error| utils::db_error(error, "按来源单据反查凭证"))?;
    Ok(Res::ok(rows.iter().map(VoucherRes::from_row).collect()))
}

/// 路径里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

// ==================== 单测 ====================
//
// 纯逻辑（月份键 / 标签）不连库；真库用例需要 `ERP_DB_URL` 指向「已应用 V0149 的隔离库」，
// 缺省则全部 `SKIP`：
//
// ```bash
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p2d1'
// cargo test --lib p2d1_
// ```
//
// 夹具前缀 `P2D1-` / 科目 `P2D1A`…；每次 `setup()` 先按 `voucher_word='P2D1'` 清掉上一轮的凭证
// （分录靠 `ON DELETE CASCADE`），所以重复跑不积累。
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::str::FromStr;

    /// 科目 A（资产 / 借方向）。
    pub(crate) const ACCOUNT_A: &str = "d2d10001-0000-4000-8000-000000000001";
    /// 科目 B（负债 / 贷方向）。
    pub(crate) const ACCOUNT_B: &str = "d2d10001-0000-4000-8000-000000000002";

    /// 真库用例必须串行（共用全局 `db()` 与同一批夹具）。
    pub(crate) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 每个 `#[tokio::test]` 是独立 runtime，`db()` 是进程级全局池：
                // 把池子寿命压到 0，避免借到上一个 runtime 的死连接（W1b 实测）。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    pub(crate) async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 清上一轮 → 建科目 → 建 6 张凭证（含 DRAFT / 红冲 / 跨期间）。
    ///
    /// | # | 日期 | 期间 | 状态 | 分录 |
    /// |---|------|------|------|------|
    /// | 1 | 2026-09-10 | 2026-09 | POSTED | 借 A 100 / 贷 B 100 |
    /// | 2 | 2026-10-05 | 2026-10 | POSTED | 借 A 50 / 贷 B 50 |
    /// | 3 | 2026-10-08 | 2026-10 | POSTED | 借 A 30 / 贷 B 30 |
    /// | 4 | 2026-10-12 | 2026-10 | **DRAFT** | 借 A 999 / 贷 B 999 |
    /// | 5 | 2026-10-15 | 2026-10 | POSTED | **红字**（2 号的借贷互换）：贷 A 50 / 借 B 50 |
    /// | 6 | 2026-11-03 | 2026-11 | POSTED | 借 A 7 / 贷 B 7 |
    pub(crate) async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        exec("DELETE FROM biz_voucher WHERE voucher_word = 'P2D1'").await;
        let fixtures = [
            format!(
                "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, \
                 account_category, balance_direction, is_leaf, status) \
                 VALUES ('{ACCOUNT_A}', 'P2D1A', 'P2D1 测试现金', NULL, 1, 'ASSET', 'DEBIT', 1, 1), \
                        ('{ACCOUNT_B}', 'P2D1B', 'P2D1 测试应付', NULL, 1, 'LIABILITY', 'CREDIT', 1, 1) \
                 ON CONFLICT DO NOTHING"
            ),
            // 期间表：2026-10 有正式期间行，2026-11 故意不建（走「按月份推导」兜底）。
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) \
             VALUES (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN') \
             ON CONFLICT DO NOTHING"
                .to_string(),
        ];
        for sql in &fixtures {
            exec(sql).await;
        }
        for (no, date, period, status, debit_a, credit_a, debit_b, credit_b, red, reverse) in [
            ("0001", "2026-09-10", "2026-09", "POSTED", 100, 0, 0, 100, 0, ""),
            ("0002", "2026-10-05", "2026-10", "POSTED", 50, 0, 0, 50, 0, ""),
            ("0003", "2026-10-08", "2026-10", "POSTED", 30, 0, 0, 30, 0, ""),
            ("0004", "2026-10-12", "2026-10", "DRAFT", 999, 0, 0, 999, 0, ""),
            ("0005", "2026-10-15", "2026-10", "POSTED", 0, 50, 50, 0, 1, "0002"),
            ("0006", "2026-11-03", "2026-11", "POSTED", 7, 0, 0, 7, 0, ""),
        ] {
            let id = format!("d2d10001-0000-4000-8000-00000000{no}");
            let reverse_of = if reverse.is_empty() {
                "NULL".to_string()
            } else {
                format!("'d2d10001-0000-4000-8000-00000000{reverse}'::uuid")
            };
            let total = if debit_a > 0 { debit_a } else { credit_a };
            exec(&format!(
                "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, \
                 voucher_type, source_type, source_no, summary, total_debit, total_credit, \
                 entry_count, status, is_red, reverse_of) \
                 VALUES ('{id}', 'P2D1', 'P2D1-{no}', '{period}', '{date}', 'AUTO', 'STOCK_IN', \
                         'P2D1-SRC-{no}', 'P2D1 测试凭证 {no}', {total}, {total}, 2, '{status}', \
                         {red}, {reverse_of})"
            ))
            .await;
            exec(&format!(
                "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, \
                 account_code, account_name, debit_amount, credit_amount) \
                 VALUES ('{id}', 1, 'P2D1 行 1', '{ACCOUNT_A}', 'P2D1A', 'P2D1 测试现金', \
                         {debit_a}, {credit_a}), \
                        ('{id}', 2, 'P2D1 行 2', '{ACCOUNT_B}', 'P2D1B', 'P2D1 测试应付', \
                         {debit_b}, {credit_b})"
            ))
            .await;
        }
        Some(guard)
    }

    pub(crate) fn uid(raw: &str) -> Uuid {
        Uuid::from_str(raw).expect("夹具 UUID 写错了")
    }

    /// 夹具凭证号前缀（`setup()` 建的 6 张全部用它）。
    ///
    /// 断言必须带这个前缀，**不能只按 `period` 计数**：真库用例共用同一个库，
    /// P2-B1 / P2-C1 的入库过账、P7 的付款 / 手工凭证都会**自动**往 `biz_voucher`
    /// 写 2026-10 的 `STOCK_IN` 凭证（与夹具同期间、同来源类型），全量套件下只数
    /// 期间实测漂成 37 ≠ 4（单跑模块时库里只有夹具，所以是绿的）。
    ///
    /// `VoucherFilter` 没有 `voucher_word` 字段，且卡面要求不改生产 SQL；`voucher_no`
    /// 是模糊匹配（`LIKE %x%`，同时比对 `source_no`），正好用前缀把断言锁在夹具上。
    /// 夹具的 `source_no` 也带同一前缀（`P2D1-SRC-n`），两条路都只命中夹具。
    ///
    /// 用 SQL 侧过滤（而不是「取回全库再在内存里挑」）还顺手去掉了一个隐患：
    /// 期间内凭证超过一页时，夹具可能被分页挤掉，按行内过滤的写法会假红。
    pub(crate) const FIXTURE_PREFIX: &str = "P2D1-";

    /// 夹具筛选：给定条件 + 「凭证号 / 来源单号带 [`FIXTURE_PREFIX`]」。
    fn fixture_filter(period: Option<&str>, status: Option<&str>, source_type: Option<&str>) -> VoucherFilter {
        VoucherFilter {
            period: period.map(str::to_string),
            status: status.map(str::to_string),
            voucher_type: None,
            source_type: source_type.map(str::to_string),
            source_doc_no: None,
            voucher_no: Some(FIXTURE_PREFIX.to_string()),
            page: 1,
            page_size: 50,
        }
    }

    /// 本次夹具的行数（`voucher_word = 'P2D1'`）：全库口径的 `total` 会随别的模块的凭证漂移，
    /// 断言一律按本夹具的行数来。
    fn p2d1_count(rows: &[VoucherRow]) -> usize {
        rows.iter().filter(|row| row.voucher_word == "P2D1").count()
    }

    /// 行级护栏：取回的行必须**全部**是夹具（前缀口径万一失效，宁可红也不假绿）。
    fn assert_all_fixtures(rows: &[VoucherRow]) {
        assert!(!rows.is_empty(), "夹具行不该为空");
        for row in rows {
            assert_eq!(
                row.voucher_word, "P2D1",
                "混进了非夹具凭证（voucher_word={} / voucher_no={}）",
                row.voucher_word, row.voucher_no
            );
            assert!(
                row.voucher_no.starts_with(FIXTURE_PREFIX),
                "混进了非夹具凭证（voucher_no={}）",
                row.voucher_no
            );
        }
    }

    /// ① 列表：按 `period` / `status` / `source_type` / `voucher_no` 筛选。
    #[tokio::test]
    async fn p2d1_voucher_page_filters() {
        let Some(_guard) = setup().await else {
            return;
        };
        // 期间：本次夹具在 2026-10 有 4 张（含 DRAFT 与红字）。
        //
        // ⚠️ 不能只按 `period` 断言 `total == 4`：`voucher_page` 的 `period` 筛选是
        // **全库**口径，别的模块（P2-C1 的收货凭证、P7 的付款 / 手工凭证）也会往
        // 2026-10 记账，总数会随「今天跑过哪些用例」漂移（实测 37 ≠ 4）。
        // 所以筛选条件里叠上 [`FIXTURE_PREFIX`]，把 `total` / 行数都锁到本夹具。
        let (rows, total) = voucher_page(&fixture_filter(Some("2026-10"), None, None))
            .await
            .expect("期间筛选");
        assert_eq!(total, 4, "2026-10 应有 4 张 P2D1 夹具凭证");
        assert_eq!(rows.len(), 4, "取回行数应等于 total");
        assert_eq!(p2d1_count(&rows), 4, "行内复核：4 张都是本夹具");
        assert_all_fixtures(&rows);
        assert!(rows.iter().all(|row| row.period == "2026-10"));
        // 只看 POSTED：DRAFT 被筛掉 → 3 张。
        let (rows, total) = voucher_page(&fixture_filter(Some("2026-10"), Some("POSTED"), None))
            .await
            .expect("状态筛选");
        assert_eq!(total, 3, "2026-10 的 POSTED 应有 3 张");
        assert_eq!(p2d1_count(&rows), 3, "行内复核：3 张都是本夹具");
        assert_all_fixtures(&rows);
        assert!(rows.iter().all(|row| row.status == "POSTED"));
        // 来源类型：本夹具全是 STOCK_IN；换成 MANUAL 应 0 行（仍是夹具口径，别数到别人的凭证）。
        let (rows, total) = voucher_page(&fixture_filter(None, None, Some("MANUAL")))
            .await
            .expect("来源筛选");
        assert_eq!(total, 0, "没有 MANUAL 来源的 P2D1 凭证");
        assert_eq!(p2d1_count(&rows), 0, "行内复核：一行都不该有");
        // 凭证号模糊：P2D1-0002（含红冲它的 0005 之外只应命中 1 张）。
        let filter = VoucherFilter {
            voucher_no: Some("P2D1-0002".to_string()),
            page: 1,
            page_size: 50,
            ..VoucherFilter::default()
        };
        let (rows, total) = voucher_page(&filter).await.expect("凭证号筛选");
        assert_eq!(total, 1, "凭证号精确串应只命中 1 张");
        assert_eq!(rows[0].voucher_no, "P2D1-0002");
        assert_all_fixtures(&rows);
        println!("[P2D1-①] 期间/状态/来源/凭证号筛选 ✓");
    }

    /// ② 详情：含全部分录，借贷合计等于单头 `total_debit` / `total_credit`。
    #[tokio::test]
    async fn p2d1_voucher_detail_matches_totals() {
        let Some(_guard) = setup().await else {
            return;
        };
        let detail = voucher_detail(uid("d2d10001-0000-4000-8000-000000000002"))
            .await
            .expect("凭证详情");
        assert_eq!(detail.entries.len(), 2, "夹具凭证有 2 行分录");
        assert_eq!(detail.entries[0].line_no, 1, "分录按 line_no 升序");
        assert_eq!(detail.entry_total_debit, detail.voucher.total_debit);
        assert_eq!(detail.entry_total_credit, detail.voucher.total_credit);
        assert_eq!(detail.entry_total_debit, Decimal::from(50));
        assert!(detail.balanced);
        assert_eq!(detail.entries[0].account_code, "P2D1A");
        assert_eq!(detail.entries[1].account_name, "P2D1 测试应付");
        // 不存在 / 已删除 → 404。
        let err = voucher_detail(uid("d2d10001-0000-4000-8000-00000000ffff"))
            .await
            .expect_err("不存在的凭证");
        assert_eq!(err.status, 404);
        println!("[P2D1-②] 详情分录 + 借贷合计一致 + 404 ✓");
    }

    /// ③ 红字凭证在详情里可辨认（`is_red` / `reverse_of`）。
    #[tokio::test]
    async fn p2d1_red_voucher_detail_flags() {
        let Some(_guard) = setup().await else {
            return;
        };
        let detail = voucher_detail(uid("d2d10001-0000-4000-8000-000000000005"))
            .await
            .expect("红字凭证详情");
        assert!(detail.voucher.is_red, "0005 是红字凭证");
        assert_eq!(
            detail.voucher.reverse_of.as_deref(),
            Some("d2d10001-0000-4000-8000-000000000002")
        );
        // 借贷互换：A 科目这张凭证是**贷** 50。
        assert_eq!(detail.entries[0].debit_amount, Decimal::ZERO);
        assert_eq!(detail.entries[0].credit_amount, Decimal::from(50));
        println!("[P2D1-③] 红字凭证 is_red / reverse_of / 借贷互换 ✓");
    }

    /// ④ 入参校验：非法状态 / 类型 / 期间一律 400（不静默忽略）。
    #[test]
    fn p2d1_query_validation() {
        let bad_status = VoucherPageQuery {
            status: Some("POSTEDD".to_string()),
            ..VoucherPageQuery::default()
        };
        assert_eq!(bad_status.validate().expect_err("非法状态").status, 400);
        let bad_type = VoucherPageQuery {
            voucher_type: Some("SYSTEM".to_string()),
            ..VoucherPageQuery::default()
        };
        assert_eq!(bad_type.validate().expect_err("非法类型").status, 400);
        let bad_period = VoucherPageQuery {
            period: Some("2026-13".to_string()),
            ..VoucherPageQuery::default()
        };
        assert_eq!(bad_period.validate().expect_err("非法期间").status, 400);
        // 空白串不算条件。
        let blank = VoucherPageQuery {
            status: Some("  ".to_string()),
            period: Some(String::new()),
            ..VoucherPageQuery::default()
        };
        assert!(blank.validate().is_ok());
        let filter = blank.to_filter();
        assert!(filter.status.is_none() && filter.period.is_none());
        assert!(is_month_key("2026-10") && !is_month_key("2026-1"));
        println!("[P2D1-④] 入参校验（状态/类型/期间）✓");
    }

    // ---------- P-FLOW-3B 补漏：VoucherRes 透出红冲审批字段 ----------

    /// 造一行「已记账 + 正在审批红冲」的查询行（纯内存，不碰库）。
    fn reverse_pending_row(pending: Option<Uuid>, reason: Option<&str>) -> VoucherRow {
        VoucherRow {
            id: Uuid::now_v7(),
            voucher_word: "记".to_string(),
            voucher_no: "PZ-20261102-000018".to_string(),
            period: "2026-11".to_string(),
            voucher_date: NaiveDate::from_ymd_opt(2026, 11, 2).expect("固定日期"),
            voucher_type: "AUTO".to_string(),
            source_type: "STOCK_IN".to_string(),
            source_id: None,
            source_no: None,
            source_doc_no: None,
            summary: "P-FLOW-3B 补漏用例".to_string(),
            total_debit: Decimal::from(800),
            total_credit: Decimal::from(800),
            entry_count: 2,
            status: "POSTED".to_string(),
            is_red: 0,
            reverse_of: None,
            reversed_by: None,
            posted_at: Some(Utc::now()),
            posted_by: None,
            remark: None,
            pending_reverse_instance_id: pending,
            reverse_reason: reason.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            reverse_of_no: None,
            reversed_by_no: None,
        }
    }

    /// ① `pending_reverse_instance_id` 必须从行透出成字符串（前端据此画「红冲审批中」徽标）。
    #[test]
    fn pflow3b_res_carries_pending_reverse_instance_id() {
        let instance = Uuid::now_v7();
        let res = VoucherRes::from_row(&reverse_pending_row(Some(instance), Some("凭证金额录错")));
        assert_eq!(
            res.pending_reverse_instance_id.as_deref(),
            Some(instance.to_string().as_str()),
            "审批实例 id 必须原样透出（缺了前端只能再打一次 flow/instance/page）"
        );
        // 契约判据是「字段存在」：serde 把 None 序列化成 null，前端 `!== undefined` 恒为真
        let json = serde_json::to_value(&res).expect("序列化");
        assert_eq!(
            json.get("pending_reverse_instance_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            Some(instance.to_string())
        );

        // 不在审批中：字段仍然存在（值 null），否则前端会误判成「后端没这列」而走兜底
        let plain = VoucherRes::from_row(&reverse_pending_row(None, None));
        assert!(plain.pending_reverse_instance_id.is_none());
        let json = serde_json::to_value(&plain).expect("序列化");
        assert!(
            json.get("pending_reverse_instance_id").is_some(),
            "字段必须始终在响应里（null 也要有），缺席会让前端兜底逻辑继续发请求"
        );
        assert!(json["pending_reverse_instance_id"].is_null());
    }

    /// ① `reverse_reason` 必须从行透出（前端在「红冲审批中」徽标的 tooltip 里显示原因）。
    #[test]
    fn pflow3b_res_carries_reverse_reason() {
        let res = VoucherRes::from_row(&reverse_pending_row(Some(Uuid::now_v7()), Some("凭证金额录错")));
        assert_eq!(res.reverse_reason.as_deref(), Some("凭证金额录错"));
        let json = serde_json::to_value(&res).expect("序列化");
        assert_eq!(
            json.get("reverse_reason").and_then(|v| v.as_str()),
            Some("凭证金额录错")
        );

        // 没有原因（未送审 / 老数据）时字段仍在，值为 null
        let plain = VoucherRes::from_row(&reverse_pending_row(None, None));
        assert!(plain.reverse_reason.is_none());
        let json = serde_json::to_value(&plain).expect("序列化");
        assert!(json.get("reverse_reason").is_some());
        assert!(json["reverse_reason"].is_null());
    }

    // ==================== VTRACE-3：按来源单据反查凭证 ====================

    /// 反查夹具：一张业务单据（[`BY_SOURCE_ID`]）产生**蓝字 + 红字**两张凭证。
    ///
    /// 与 [`setup`] 的 6 张 P2D1 夹具的区别：那边 `source_id` 全是 NULL（它们测的是列表/详情，
    /// 不需要来源），本夹具给同一 `source_id` 挂两张，专门测「红冲后能不能看到两条」。
    const BY_SOURCE_ID: &str = "d2d10002-0000-4000-8000-0000000000a1";
    /// 蓝字凭证（先记账）。
    const BY_SOURCE_BLUE: &str = "d2d10002-0000-4000-8000-0000000000b1";
    /// 红字凭证（红冲蓝字，晚一天记账）。
    const BY_SOURCE_RED: &str = "d2d10002-0000-4000-8000-0000000000b2";

    /// 建反查夹具（先按 `voucher_word` 清上一轮，重复跑不积累）。
    async fn seed_by_source_fixture() {
        exec("DELETE FROM biz_voucher WHERE voucher_word = 'P2D1V'").await;
        // ① 蓝字：`posted_at` 2026-10-20，`reversed_by` 指回红字。
        exec(&format!(
            "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type, \
             source_type, source_id, source_no, source_doc_no, summary, total_debit, total_credit, \
             entry_count, status, is_red, reverse_of, reversed_by, posted_at) \
             VALUES ('{BY_SOURCE_BLUE}', 'P2D1V', 'P2D1V-0001', '2026-10', '2026-10-20', 'AUTO', \
                     'STOCK_IN', '{BY_SOURCE_ID}', 'IN-P2D1V', 'PO-P2D1V', 'P2D1V 蓝字', 100, 100, \
                     2, 'POSTED', 0, NULL, '{BY_SOURCE_RED}', '2026-10-20 09:00:00+00')"
        ))
        .await;
        // ② 红字：`posted_at` 2026-10-21（更晚 ⇒ 倒序时排第一），`reverse_of` 指回蓝字。
        exec(&format!(
            "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type, \
             source_type, source_id, source_no, source_doc_no, summary, total_debit, total_credit, \
             entry_count, status, is_red, reverse_of, reversed_by, posted_at) \
             VALUES ('{BY_SOURCE_RED}', 'P2D1V', 'P2D1V-0002', '2026-10', '2026-10-21', 'AUTO', \
                     'STOCK_IN', '{BY_SOURCE_ID}', 'IN-P2D1V', 'PO-P2D1V', 'P2D1V 红字', 100, 100, \
                     2, 'POSTED', 1, '{BY_SOURCE_BLUE}', NULL, '2026-10-21 09:00:00+00')"
        ))
        .await;
    }

    /// ㉖ 按来源反查：**红冲后原凭证 + 红字凭证都要返回**，记账时间倒序。
    #[tokio::test]
    async fn vtrace3_by_source_returns_blue_and_red_vouchers() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_by_source_fixture().await;
        let rows = vouchers_by_source("STOCK_IN", uid(BY_SOURCE_ID))
            .await
            .expect("按来源反查");
        assert_eq!(rows.len(), 2, "同一单据应返回蓝字 + 红字两张凭证");
        // 红字记账更晚 ⇒ 排第一。
        assert_eq!(rows[0].voucher_no, "P2D1V-0002", "记账时间倒序：红字在前");
        assert_eq!(rows[0].is_red, 1, "第一行应是红字凭证");
        assert_eq!(rows[0].reverse_of, Some(uid(BY_SOURCE_BLUE)));
        assert_eq!(
            rows[0].reverse_of_no.as_deref(),
            Some("P2D1V-0001"),
            "红字要能带出被红冲的凭证号"
        );
        assert_eq!(rows[0].status, "POSTED");
        // 原凭证：状态仍 POSTED，但 `reversed_by_no` 说明它被红冲了。
        assert_eq!(rows[1].voucher_no, "P2D1V-0001");
        assert_eq!(rows[1].is_red, 0);
        assert_eq!(
            rows[1].reversed_by_no.as_deref(),
            Some("P2D1V-0002"),
            "原凭证要能带出红冲凭证号"
        );
        assert_eq!(rows[1].source_no.as_deref(), Some("IN-P2D1V"));
        assert_eq!(rows[1].source_doc_no.as_deref(), Some("PO-P2D1V"));
        assert!(
            rows[0].posted_at.unwrap() > rows[1].posted_at.unwrap(),
            "倒序断言：红字 posted_at 必须晚于蓝字"
        );
        // API DTO 映射：`is_red` / `status` / 两个关联凭证号都要出现在 JSON 里。
        let res = VoucherRes::from_row(&rows[0]);
        let json = serde_json::to_value(&res).expect("序列化");
        assert_eq!(json["is_red"], serde_json::json!(true));
        assert_eq!(json["status"], serde_json::json!("POSTED"));
        assert_eq!(json["reverse_of_no"], serde_json::json!("P2D1V-0001"));
        println!("[VTRACE-3] 按来源反查：蓝字 + 红字 2 张、倒序、关联号齐全 ✓");
    }

    /// ㉗ 按来源反查：单据没生成过凭证 → **空数组**（不是报错 / 不是 404）。
    #[tokio::test]
    async fn vtrace3_by_source_empty_when_no_voucher() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_by_source_fixture().await;
        let rows = vouchers_by_source("STOCK_IN", uid("d2d10002-0000-4000-8000-0000000000ff"))
            .await
            .expect("没有凭证不是错误");
        assert!(
            rows.is_empty(),
            "未生成凭证的单据必须返回空数组，实际 {} 行",
            rows.len()
        );
        // 换一个真实的来源类型、但该类型下没有这个 id 的凭证：同样是空数组。
        let rows = vouchers_by_source("COST_PERIOD", uid(BY_SOURCE_ID))
            .await
            .expect("按类型隔离查询");
        assert!(
            rows.is_empty(),
            "来源类型是过滤条件之一（STOCK_IN 的凭证不该出现在 COST_PERIOD 下）"
        );
        println!("[VTRACE-3] 无凭证 → 空数组 ✓");
    }

    /// ㉘ 按来源反查：非法 / 缺失 / 非 UUID 的入参一律 **400**（不静默查空）。
    #[test]
    fn vtrace3_by_source_query_validation() {
        let base = VoucherBySourceQuery {
            source_type: Some("STOCK_IN".to_string()),
            source_id: Some(BY_SOURCE_ID.to_string()),
        };
        let (kind, id) = base.validate().expect("合法入参应通过");
        assert_eq!(kind, "STOCK_IN");
        assert_eq!(id, uid(BY_SOURCE_ID));

        // ① 非法 source_type（值域外的字面量）→ 400 + 10001。
        let err = VoucherBySourceQuery {
            source_type: Some("PURCHASE_IN".to_string()),
            source_id: Some(BY_SOURCE_ID.to_string()),
        }
        .validate()
        .expect_err("值域外的 source_type 必须 400");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("PURCHASE_IN"), "消息要带回入参：{}", err.msg);
        assert!(
            err.msg.contains("COST_PERIOD"),
            "消息要列出完整值域（含 VTRACE-3 补登的 COST_PERIOD）：{}",
            err.msg
        );

        // ② 空 / 只有空白 → 400（不能当成「不过滤」）。
        for raw in ["", "   ", " "] {
            let err = VoucherBySourceQuery {
                source_type: Some(raw.to_string()),
                source_id: Some(BY_SOURCE_ID.to_string()),
            }
            .validate()
            .expect_err("空 source_type 必须 400");
            assert_eq!(err.status, 400);
            assert!(err.msg.contains("source_type"), "{}", err.msg);
        }
        let err = VoucherBySourceQuery {
            source_type: None,
            source_id: Some(BY_SOURCE_ID.to_string()),
        }
        .validate()
        .expect_err("缺 source_type 必须 400");
        assert_eq!(err.status, 400);

        // ③ source_id 不是 UUID → 400（与 `parse_uuid` 同口径）。
        for raw in ["", "not-a-uuid", "d2d10002-0000-4000-8000"] {
            let err = VoucherBySourceQuery {
                source_type: Some("STOCK_IN".to_string()),
                source_id: Some(raw.to_string()),
            }
            .validate()
            .expect_err("非法 source_id 必须 400");
            assert_eq!(err.status, 400);
            assert!(err.msg.contains("source_id"), "{}", err.msg);
        }
        // ④ 值域内 22 种全部能通过（VTRACE-3 补 COST_PERIOD；HR-06 词表扩底再补
        //    PAYROLL_ACCRUAL / PAYROLL_PAYMENT 两条工资凭证来源）。
        for kind in VoucherSourceType::ALL {
            let query = VoucherBySourceQuery {
                source_type: Some(kind.as_str().to_string()),
                source_id: Some(BY_SOURCE_ID.to_string()),
            };
            assert_eq!(query.validate().expect("值域内取值应通过").0, kind.as_str());
        }
        assert_eq!(
            VoucherSourceType::ALL.len(),
            22,
            "值域是 22 种（VTRACE-3 补了 COST_PERIOD，HR-06 补了两条 PAYROLL_*）"
        );
        println!("[VTRACE-3] 入参校验：非法值 / 空值 / 非 UUID 全部 400，22 种合法值全通过 ✓");
    }

    /// ㉙ 与单张版 `voucher::find_voucher_by_source` 的口径一致（防止两条查询逐渐漂开）。
    #[tokio::test]
    async fn vtrace3_by_source_agrees_with_find_voucher_by_source() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_by_source_fixture().await;
        let single = crate::business::erp::voucher::find_voucher_by_source(db(), "STOCK_IN", uid(BY_SOURCE_ID))
            .await
            .expect("单张反查失败")
            .expect("应能查到蓝字凭证");
        let rows = vouchers_by_source("STOCK_IN", uid(BY_SOURCE_ID))
            .await
            .expect("按来源反查");
        let first_blue = rows.iter().find(|row| row.is_red == 0).expect("清单里应有蓝字");
        assert_eq!(
            first_blue.id, single.id,
            "清单里的蓝字凭证必须与 find_voucher_by_source 返回的同一条"
        );
        assert_eq!(first_blue.voucher_no, single.voucher_no);
        println!("[VTRACE-3] 与 find_voucher_by_source 口径一致 ✓");
    }

    /// ㉕ 收尾清场：本模块的**科目夹具**（`P2D1A` / `P2D1B`）必须删掉（t_06c528fc）。
    ///
    /// `ledger` 与 `voucher_query` 共用这套夹具；科目没有分类行，留在共享库里会让
    /// `cash_flow::tests::r3_real_config_covers_all_accounts` 在**第二遍**全量时红
    /// （「分类配置漏科目」）。`biz_account` 无外键引用，直接删安全。
    #[tokio::test]
    async fn zz_teardown_purges_fixture_accounts() {
        let Some(_guard) = setup().await else {
            return;
        };
        // VTRACE-3 的反查夹具（`P2D1V` 两张）
        db().execute_with_params("DELETE FROM biz_voucher WHERE voucher_word = 'P2D1V'", vec![])
            .await
            .expect("清场删反查夹具失败");
        db().execute_with_params("DELETE FROM biz_account WHERE account_code LIKE 'P2D1%'", vec![])
            .await
            .expect("清场删夹具科目失败");
        let left: i64 = db()
            .scalar_sql(
                "SELECT count(*) FROM biz_account WHERE account_code LIKE 'P2D1%'".to_string(),
                vec![],
            )
            .await
            .expect("计数失败");
        assert_eq!(left, 0, "夹具科目必须删掉（留着会让第二遍全量变红）");
    }

    // ==================== VTRACE-7：分录辅助核算的名字 ====================

    /// 本组凭证字（`setup()` 的 6 张用 `P2D1`，这里另起一个，避免污染那边按前缀的断言）。
    const VT7_WORD: &str = "VT7";
    /// 本组凭证 id。
    const VT7_VOUCHER: &str = "d2d10007-0000-4000-8000-0000000000a0";
    /// 在用供应商（名字故意取卡面验收里那个真实样例）。
    const VT7_SUP: &str = "d2d10007-0000-4000-8000-000000000001";
    /// 在用物料。
    const VT7_MAT: &str = "d2d10007-0000-4000-8000-000000000002";
    /// 在用仓库。
    const VT7_WH: &str = "d2d10007-0000-4000-8000-000000000003";
    /// 在用部门。
    const VT7_DEPT: &str = "d2d10007-0000-4000-8000-000000000004";
    /// 已软删的供应商。
    const VT7_SUP_GONE: &str = "d2d10007-0000-4000-8000-000000000011";
    /// 已软删的物料。
    const VT7_MAT_GONE: &str = "d2d10007-0000-4000-8000-000000000012";
    /// 已软删的仓库。
    const VT7_WH_GONE: &str = "d2d10007-0000-4000-8000-000000000013";
    /// 已软删的部门。
    const VT7_DEPT_GONE: &str = "d2d10007-0000-4000-8000-000000000014";

    /// 清本组痕迹（凭证 → 分录级联；主数据按固定 id 删）。开工前 + 收尾各跑一次，重复跑不积累。
    async fn vtrace7_cleanup() {
        exec(&format!("DELETE FROM biz_voucher WHERE voucher_word = '{VT7_WORD}'")).await;
        exec(&format!(
            "DELETE FROM biz_supplier WHERE id IN ('{VT7_SUP}'::uuid, '{VT7_SUP_GONE}'::uuid)"
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_material WHERE id IN ('{VT7_MAT}'::uuid, '{VT7_MAT_GONE}'::uuid)"
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_warehouse WHERE id IN ('{VT7_WH}'::uuid, '{VT7_WH_GONE}'::uuid)"
        ))
        .await;
        exec(&format!(
            "DELETE FROM sys_dept WHERE id IN ('{VT7_DEPT}'::uuid, '{VT7_DEPT_GONE}'::uuid)"
        ))
        .await;
    }

    /// 建夹具：4 条在用主数据 + 4 条已软删主数据 + 一张两行凭证。
    ///
    /// | 行 | 借贷 | 辅助核算 |
    /// |----|------|----------|
    /// | 1 | 借 100 | 四个维度全指向**在用**主数据（名字必须取到） |
    /// | 2 | 贷 100 | 四个维度全指向**已软删**主数据（id 照给、名字必须 `null`，不许报错） |
    async fn vtrace7_seed() {
        vtrace7_cleanup().await;
        exec(&format!(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
             VALUES ('{VT7_SUP}'::uuid, 'VTRACE7-S1', '宁波华联包装材料厂', 1), \
                    ('{VT7_SUP_GONE}'::uuid, 'VTRACE7-S9', '已删除的旧供应商', 1)"
        ))
        .await;
        exec(&format!(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, status) \
             VALUES ('{VT7_MAT}'::uuid, 'VTRACE7-M1', '五层瓦楞纸箱 400×300×250', \
                     'd2d10007-0000-4000-8000-0000000000f1'::uuid, 1), \
                    ('{VT7_MAT_GONE}'::uuid, 'VTRACE7-M9', '已删除的旧物料', \
                     'd2d10007-0000-4000-8000-0000000000f1'::uuid, 1)"
        ))
        .await;
        exec(&format!(
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf) \
             VALUES ('{VT7_WH}'::uuid, 'VTRACE7-W1', '包材成品仓', '/{VT7_WH}/', 1, 1), \
                    ('{VT7_WH_GONE}'::uuid, 'VTRACE7-W9', '已删除的旧仓库', '/{VT7_WH_GONE}/', 1, 1)"
        ))
        .await;
        exec(&format!(
            "INSERT INTO sys_dept (id, dept_code, dept_name, status) \
             VALUES ('{VT7_DEPT}'::uuid, 'VTRACE7D', '采购部', 1), \
                    ('{VT7_DEPT_GONE}'::uuid, 'VTRACE7D9', '已删除的旧部门', 1)"
        ))
        .await;
        // 软删：deleted_at 非空即「已删除」（`sys_dept` / 三张主数据都是软删表）。
        for sql in [
            format!("UPDATE biz_supplier SET deleted_at = now() WHERE id = '{VT7_SUP_GONE}'::uuid"),
            format!("UPDATE biz_material SET deleted_at = now() WHERE id = '{VT7_MAT_GONE}'::uuid"),
            format!("UPDATE biz_warehouse SET deleted_at = now() WHERE id = '{VT7_WH_GONE}'::uuid"),
            format!("UPDATE sys_dept SET deleted_at = now() WHERE id = '{VT7_DEPT_GONE}'::uuid"),
        ] {
            exec(&sql).await;
        }
        exec(&format!(
            "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type, \
             source_type, source_no, summary, total_debit, total_credit, entry_count, status, is_red) \
             VALUES ('{VT7_VOUCHER}', '{VT7_WORD}', '{VT7_WORD}-0001', '2032-05', '2032-05-20', 'MANUAL', \
                     'MANUAL', 'VTRACE7-0001', 'VTRACE-7 辅助核算名字夹具', 100, 100, 2, 'POSTED', 0)"
        ))
        .await;
        // 科目快照直接写死：`biz_voucher_entry.account_id` 无外键，分录语义不依赖科目主数据。
        exec(&format!(
            "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, \
             account_name, debit_amount, credit_amount, aux_supplier_id, aux_material_id, \
             aux_warehouse_id, aux_dept_id, aux_qty) \
             VALUES ('{VT7_VOUCHER}', 1, 'VTRACE7 在用主数据', \
                     'd2d10007-0000-4000-8000-0000000000c1'::uuid, 'VTRACE7A', 'VTRACE7 存货', 100, 0, \
                     '{VT7_SUP}'::uuid, '{VT7_MAT}'::uuid, '{VT7_WH}'::uuid, '{VT7_DEPT}'::uuid, 3), \
                    ('{VT7_VOUCHER}', 2, 'VTRACE7 主数据已删', \
                     'd2d10007-0000-4000-8000-0000000000c2'::uuid, 'VTRACE7B', 'VTRACE7 应付', 0, 100, \
                     '{VT7_SUP_GONE}'::uuid, '{VT7_MAT_GONE}'::uuid, '{VT7_WH_GONE}'::uuid, \
                     '{VT7_DEPT_GONE}'::uuid, NULL)"
        ))
        .await;
    }

    /// 用例体（拆出来是为了让收尾清场挂在 `catch_unwind` 上，panic 也能清干净）。
    async fn vtrace7_body() {
        vtrace7_seed().await;
        let detail = voucher_detail(uid(VT7_VOUCHER)).await.expect("凭证详情");
        assert_eq!(detail.entries.len(), 2, "夹具 2 行分录必须都回来");

        // ① 在用主数据：id 与 name 同时下发（约定 13），名字是主档现值。
        let live = &detail.entries[0];
        assert_eq!(live.aux_supplier_id.as_deref(), Some(VT7_SUP));
        assert_eq!(
            live.aux_supplier_name.as_deref(),
            Some("宁波华联包装材料厂"),
            "供应商名必须从 biz_supplier 取出（卡面验收样例）"
        );
        assert_eq!(live.aux_material_name.as_deref(), Some("五层瓦楞纸箱 400×300×250"));
        assert_eq!(live.aux_warehouse_name.as_deref(), Some("包材成品仓"));
        assert_eq!(live.aux_dept_name.as_deref(), Some("采购部"));
        assert_eq!(live.aux_qty, Some(Decimal::from(3)), "aux_qty 语义未动");
        // 序列化后的键名就是前端要消费的名字（snake_case，`Option` → null）。
        let json = serde_json::to_value(live).expect("序列化分录");
        assert_eq!(json["aux_supplier_id"], serde_json::json!(VT7_SUP));
        assert_eq!(json["aux_supplier_name"], serde_json::json!("宁波华联包装材料厂"));
        assert_eq!(json["aux_dept_name"], serde_json::json!("采购部"));

        // ② 主数据已软删：**id 照给、名字为 null、不报错**（前端短 id 兜底）。
        let gone = &detail.entries[1];
        assert_eq!(gone.aux_supplier_id.as_deref(), Some(VT7_SUP_GONE), "弱引用 id 不能丢");
        assert_eq!(gone.aux_material_id.as_deref(), Some(VT7_MAT_GONE));
        assert_eq!(gone.aux_warehouse_id.as_deref(), Some(VT7_WH_GONE));
        assert_eq!(gone.aux_dept_id.as_deref(), Some(VT7_DEPT_GONE));
        assert!(gone.aux_supplier_name.is_none(), "软删供应商 → 名字 null（不是报错）");
        assert!(gone.aux_material_name.is_none());
        assert!(gone.aux_warehouse_name.is_none());
        assert!(gone.aux_dept_name.is_none());
        let json = serde_json::to_value(gone).expect("序列化分录");
        // 字段必须始终在响应里（null 也要有）：缺席会让前端以为「后端没这列」。
        for key in [
            "aux_supplier_name",
            "aux_material_name",
            "aux_warehouse_name",
            "aux_dept_name",
        ] {
            assert!(json.get(key).is_some(), "字段 `{key}` 必须始终出现在响应里");
            assert!(json[key].is_null(), "`{key}` 对软删主数据应为 null");
        }

        // ③ 主数据改名 → 详情立刻显示新名（实时 join，不是快照）。
        exec(&format!(
            "UPDATE biz_supplier SET supplier_name = '宁波华联包装（更名后）' WHERE id = '{VT7_SUP}'::uuid"
        ))
        .await;
        let renamed = voucher_detail(uid(VT7_VOUCHER)).await.expect("改名后再查详情");
        assert_eq!(
            renamed.entries[0].aux_supplier_name.as_deref(),
            Some("宁波华联包装（更名后）"),
            "主数据关联要 join 实时取名（约定 13），不是存快照"
        );

        // ④ 与分录级读接口一致：`entries_of` 单独调也一样带名字（详情就是用它拼的）。
        let rows = entries_of(uid(VT7_VOUCHER)).await.expect("单独读分录");
        assert_eq!(rows[0].aux_material_name.as_deref(), Some("五层瓦楞纸箱 400×300×250"));
        assert!(rows[1].aux_dept_name.is_none());

        // ⑤ 列表接口不受影响：`voucher_page` 的 DTO 没有这四列（列表不显示辅助核算，不扩影响面）。
        let (page, _) = voucher_page(&VoucherFilter {
            voucher_no: Some(format!("{VT7_WORD}-0001")),
            page: 1,
            page_size: 10,
            ..VoucherFilter::default()
        })
        .await
        .expect("列表");
        assert_eq!(page.len(), 1, "夹具凭证应能在列表里查到");
        println!("[VTRACE-7] 四个 aux_*_name 取到 / 软删为 null / 改名实时生效 ✓");
    }

    /// ㉚ 真库：分录辅助核算的四个名字一次 JOIN 取齐；软删主数据名字为 `null` 而不是报错。
    #[tokio::test]
    async fn vtrace7_entry_aux_names_joined() {
        use futures_util::FutureExt;
        let Some(_guard) = setup().await else {
            return;
        };
        // AssertUnwindSafe 同 `aux_tests::case`：清场只认本组固定 id 与前缀，panic 后状态不再使用。
        let outcome = std::panic::AssertUnwindSafe(vtrace7_body()).catch_unwind().await;
        vtrace7_cleanup().await;
        if let Err(payload) = outcome {
            std::panic::resume_unwind(payload);
        }
    }

    /// ㉛ 结构闸（不连库）：名字必须来自**一次** `LEFT JOIN`，不是逐行回查、也不是内连接。
    ///
    /// 内连接会把「辅助核算指向不存在的主数据」的行整条吞掉（凭证详情少分录），
    /// 所以这里同时断言四条 JOIN 都是 `LEFT JOIN` 且带 `deleted_at IS NULL`。
    #[test]
    fn vtrace7_entry_sql_is_single_left_join() {
        assert!(
            ENTRY_SOURCE.starts_with(ENTRY_TABLE),
            "FROM 子句应以分录表开头：{ENTRY_SOURCE}"
        );
        for (table, alias, id_col, name_col, out_col) in [
            (
                "biz_supplier",
                "sup",
                "aux_supplier_id",
                "supplier_name",
                "aux_supplier_name",
            ),
            (
                "biz_material",
                "mat",
                "aux_material_id",
                "material_name",
                "aux_material_name",
            ),
            (
                "biz_warehouse",
                "wh",
                "aux_warehouse_id",
                "warehouse_name",
                "aux_warehouse_name",
            ),
            ("sys_dept", "dept", "aux_dept_id", "dept_name", "aux_dept_name"),
        ] {
            let on_clause =
                format!("LEFT JOIN {table} {alias} ON {alias}.id = e.{id_col} AND {alias}.deleted_at IS NULL");
            assert!(ENTRY_SOURCE.contains(&on_clause), "缺少这条 LEFT JOIN：{on_clause}");
            assert!(
                ENTRY_COLUMNS.contains(&format!("{alias}.{name_col} AS {out_col}")),
                "`{out_col}` 必须显式 AS 出来（from_row 按别名取值）"
            );
            // 同一张表不许出现第二次 JOIN（内连接写法 `JOIN x alias ON` 不在 LEFT JOIN 里也要被抓）。
            assert_eq!(
                ENTRY_SOURCE.matches(&format!("JOIN {table} {alias} ")).count(),
                1,
                "`{table}` 只允许 JOIN 一次（一次取齐，不许逐行查）"
            );
        }
        assert_eq!(
            ENTRY_SOURCE.matches("JOIN").count(),
            ENTRY_SOURCE.matches("LEFT JOIN").count(),
            "四条关联都必须是 LEFT JOIN（内连接会让弱引用指向已删主数据的行整条消失）"
        );
        // 分录自己的列必须带 `e.` 前缀：FROM 里还有四张表的同名 id 列，不带前缀会歧义。
        assert!(ENTRY_COLUMNS.contains("e.aux_supplier_id") && ENTRY_COLUMNS.contains("e.line_no"));
        println!("[VTRACE-7] SQL 结构：4 条 LEFT JOIN 一次取齐 ✓");
    }
}

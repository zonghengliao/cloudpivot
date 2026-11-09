//! 凭证 + 分录 + 科目映射规则（批次 D · P2-C1）：**入库过账即生成凭证**。
//!
//! 表结构以 `backend/migrations/business/V0149__erp_voucher.up.sql` 为准
//! （`\(\d` 实测口径见迁移头注释）：
//!
//! ```text
//! biz_voucher        id, voucher_word(默认「记」), voucher_no, period(yyyy-MM), voucher_date,
//!                    voucher_type(AUTO/MANUAL), source_type, source_id, source_no, summary,
//!                    total_debit, total_credit, entry_count, status(POSTED/DRAFT/AUDITED/CANCELLED),
//!                    is_red, reverse_of, reversed_by, posted_at, posted_by, remark + 审计五件套
//!                    CHECK total_debit = total_credit            ← ★ 借贷平衡（硬红线 3）
//!                    CHECK entry_count >= 2
//! biz_voucher_entry  id, voucher_id, line_no, summary, account_id, account_code, account_name,
//!                    debit_amount, credit_amount, aux_supplier_id, aux_material_id,
//!                    aux_warehouse_id, aux_dept_id, aux_qty, aux_extra, created_at
//!                    CHECK (debit=0 AND credit>0) OR (debit>0 AND credit=0)  ← 单边（硬红线 4）
//! biz_voucher_rule   id, rule_code, biz_type, debit_account_id, credit_account_id,
//!                    tax_account_id, is_active + 审计五件套
//! ```
//!
//! # 生成时机（PRD §3.3 方案 A）
//!
//! 收货单过账（`DRAFT → POSTED`）时，在**同一个事务**里（[`crate::business::erp::stockin_handler`]
//! 的 `post_in_tx` 第 ⑦ 步）生成，状态直接 `POSTED`（**不是 DRAFT**）：库存已动，
//! 账必须同步动，否则月末账实不符（PRD §3.3.3）。
//!
//! # 金额口径（硬红线 5：取自单据，不重算）
//!
//! | 分录 | 科目 | 金额 |
//! |------|------|------|
//! | 借 | 存货（规则 → 物料 `inventory_account_id` → `1405`） | `header.total_amount`（**不含税**） |
//! | 借 | 进项税（规则 → `2221.01`） | `header.total_tax`（税率为 0 时**不产生该行**） |
//! | 贷 | 应付（规则 → 供应商 `payable_account_id` → `2202`） | `header.total_with_tax`（价税合计） |
//!
//! 三行合计天然平衡：`biz_stock_in` 上有 `ck_biz_si_tax CHECK (total_with_tax = total_amount + total_tax)`。
//! 落库前仍在本模块再断言一次（`InvalidValue`），因为凭证表那条 `CHECK` 是最后一道闸，不是第一道。
//!
//! # 多物料单据：**按去重物料拆分录**（P2-1b / `docs/ERP-辅助核算口径.md` §4）
//!
//! 存货借方不再是一条汇总行，而是「**每个去重物料一行**」（粒度 `MATERIAL`，不是明细行）：
//! 收货含税 `M + 2` 行（`1405`×M + `2221.01`×1 + `2202`×1）、零税 `M + 1` 行、出库 `2M` 行。
//! `M = 1` 时行数与拆分前**逐字相同**（收货 3 / 出库 2），这是回归面的底线。
//!
//! 三条硬约束（违反任一条，物料的辅助账就会记错而不是记缺）：
//!
//! 1. **金额取明细、单头不重算**：`Σ(存货行) ≡ 单头 total_amount`，尾差由
//!    [`absorb_tail_diff`] 吸收到按 `material_id` 升序的最后一行（吸收不了 → 报错，不静默）；
//! 2. **拆分只写在共享 builder 里**（[`purchase_in_lines`] / [`create_for_stock_out`]）：
//!    过账路径与联动配置路径都走 [`create_by_rule`]，同改同变 ⇒ D3 影子对比不会因拆分报差异；
//! 3. **科目逐行解析**（规则 `debit_account_id` → **该行物料** `inventory_account_id` → `1405`）：
//!    批量一条 SQL 取回，不做 N 次单查。以前只解析首个物料的科目，规则没配 `debit_account_id`
//!    且两种物料各有存货科目时整笔会挂到首个物料的科目上 —— 那是**记错科目**。
//!
//! 贷方应付、进项税**仍按 `DOC` 一条**：应付对供应商、进项税对发票，按物料拆没有会计意义。
//!
//! # ★ 红冲口径：借贷互换，不用负数（与卡面 §五 的一处差异）
//!
//! 卡面写「红字凭证金额取负」，但同一张卡面要求 `CHECK debit_amount >= 0 AND credit_amount >= 0`
//! —— 负数写不进库。架构 §2.5.2 已选定**方案 B（借贷互换）**：原「借 100」在红字凭证里是
//! 「贷 100」，金额恒为正。净效果仍等价于负数（`Σ借 − Σ贷` 为负），但试算平衡、报表取数、
//! 对外导出都不需要特判负号。落地形态：
//!
//! * 红字凭证：`is_red=1`、`reverse_of=原凭证.id`、`status='POSTED'`；
//! * 原凭证：`status` **保持 `POSTED`**（卡面 §五），只打 `reversed_by=红字凭证.id`；
//! * 分录：原 `debit_amount` / `credit_amount` **互换**，行号从 1 重排，摘要加「红冲 」前缀。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 凭证 / 分录 / 科目映射规则是会计台账（V0149 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:voucher:* 权限码控制（P2-D1 落地）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列；biz_account 是会计科目表，属全局主数据。
//! ```
//!
//! # ★ 记账科目口径：**只允许末级科目**（收口在 [`post_voucher`]）
//!
//! 决议（ADR · 卡 t_c38dbe54，与 ARCH-6 §1.3 / §6.6.2 的既定设计一致）：
//! **本系统不允许往非末级（`is_leaf = 0`）科目记账**，任何记账入口一旦出现非末级科目
//! 一律 400 拒绝，并点名科目编码。
//!
//! 为什么选「拒绝」而不是「让年结把父科目余额也结平」：
//!
//! 1. **口径唯一**：期初台账（`year_end::account_balances`）、报表（`report::match_account`
//!    按「科目 + 全部下级」前缀匹配）、预算额度都是**末级口径**。父科目同时可记账 ⇒
//!    同一笔费用既在父科目又在子科目，报表按前缀汇总会父子双计，预算按末级累加会漏掉父级；
//! 2. **结账正确性**：年结只取 `is_leaf = 1`（见 `year_end::account_balances`），父科目上的
//!    余额**不会被结转**，会留在账上，直接把「下年期初借贷相等」打破。历史事故：
//!    FIN-1（V0226）给 `5602` 挂了 8 个末级子科目并把父科目置 `is_leaf = 0` 之后，
//!    年结用例夹具仍往 `5602` 本级记 400 元费用 ⇒ 漏结 400、净利润多算 400、跨年期初差 150；
//! 3. **与既有文档一致**：ARCH-6 §6.6.2 的风险表原文就是「有人继续往 5602 本级记账 ⇒
//!    `voucher::account_required` 已校验 `is_leaf`，翻转后自动拦住」。当时的代码**并没有**
//!    这条校验（`account_required` 只查 `status = 1 AND deleted_at IS NULL`），本函数把
//!    文档描述的闸补上。
//!
//! 闸放在 [`post_voucher`] 而不是 `account_required` / `account_by_code`，因为：
//!
//! * 自动凭证（收货 / 应付 / 付款 / 折旧 / 结转）与手工凭证**都**走 `post_voucher`，
//!   这里是唯一收口，不存在「某条路径忘了查科目」；
//! * 手工凭证的分录按 `account_id` 提交（`account_by_id`），校验只看 `account_required`
//!   会被 id 路径绕过。
//!
//! 配套的两道防线（不在本文件）：
//!
//! * `year_end::close_year` 在结转前做一次 **fail-closed 兜底**：库里若已有非末级科目
//!   带余额（V0226 之前的历史分录），直接报错而不是静默漏结；
//! * 前端科目选择器必须带 `is_leaf=1`（`GET /business/erp/account/page` 已支持该过滤位），
//!   让用户选不到父科目，而不是选完再吃 400。
//!
//! # 事务边界
//!
//! 所有写函数吃 `&impl SqlExecutor`（`Db` / `Transaction` 都实现），**由调用方开事务**：
//! 过账的「库存 + 应付 + 凭证 + 回写引用」必须同一个事务（硬红线 2），
//! 任何一步失败整单回滚（卡面 ⑭）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use std::collections::BTreeMap;

pub use crate::business::erp::currency::BaseAmounts;
use crate::business::erp::serial_model;
use crate::business::erp::stockin_model::StockInRow;
use crate::business::shared::state_machine;

// ==================== 表名 / 常量 ====================

/// 凭证主表。
pub const TABLE: &str = "biz_voucher";
/// 凭证分录表。
pub const ENTRY_TABLE: &str = "biz_voucher_entry";
/// 科目映射规则表。
pub const RULE_TABLE: &str = "biz_voucher_rule";

/// 凭证字：记（本卡只产生「记」字凭证）。
pub const WORD_DEFAULT: &str = "记";
/// 凭证来源类型：收货入库单。
pub const SOURCE_STOCK_IN: &str = "STOCK_IN";
/// 凭证业务类型（映射规则按它查）：采购入库。
pub const BIZ_TYPE_PURCHASE_IN: &str = "PURCHASE_IN";
/// 凭证类型：自动生成。
pub const VOUCHER_TYPE_AUTO: &str = "AUTO";
/// 凭证类型：手工录入（P7）。
pub const VOUCHER_TYPE_MANUAL: &str = "MANUAL";
/// 凭证类型：年度损益结转（ERP-R4，一个会计年度一张）。
///
/// 白名单在库层（`ck_biz_voucher_type`，V0190 扩成 `AUTO|MANUAL|OPENING|YEAR_END`），
/// 一个期间最多一张（部分唯一索引 `uk_biz_voucher_year_end_period`）。
///
/// **它不进「本期发生额」**（`report::load_accounts` 与 `OPENING` 一起被排除）：
/// 结转分录把 5xxx / 6xxx 结平，若算进损益发生额，年结之后的利润表会把整年的
/// 收入费用对冲成 0 —— 那不是「这一年的业绩」，是账务的内部搬运。
pub const VOUCHER_TYPE_YEAR_END: &str = "YEAR_END";
/// 凭证来源类型：年度损益结转（ERP-R4）。
pub const SOURCE_TYPE_YEAR_END: &str = "YEAR_END";
/// 凭证来源类型：付款登记（P7，`biz_payment`）。
pub const SOURCE_PAYMENT: &str = "PAYMENT";
/// 凭证来源类型：手工录入（P7）。
pub const SOURCE_MANUAL: &str = "MANUAL";
/// 凭证状态：已记账（自动凭证直接进总账，PRD §3.3.3）。
pub const STATUS_POSTED: &str = "POSTED";
/// 凭证状态：草稿（P0-FIX-4A）。手工凭证的落库状态 —— 制单只是「写了一张单」，
/// 还没人复核、更没进账，总账 / 余额表 / 报表一律不算它（取数只认 [`STATUS_POSTED`]）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 凭证状态：已审核（P0-FIX-4A）。复核人看过这一版金额与分录，但仍**不进账**：
/// 记账是独立的一步（`AUDITED → POSTED`），记完才进总账 / 报表。
pub const STATUS_AUDITED: &str = "AUDITED";

/// 凭证的合法状态边（与 `state_machine::VOUCHER_EDGES` 同源，**唯一真相在状态机**）。
///
/// `transition()` 会拿本常量与注册表逐条核对：谁想在这里「顺手多放一条边」，
/// 立刻 500（不会静默放行）。`DRAFT → POSTED` 刻意不存在（制单 ≠ 审核）。
pub const LEGAL_EDGES: [(&str, &str); 3] = state_machine::VOUCHER_EDGES;
/// 会计期间状态：已结账（结账后不允许再红冲 / 记账）。
pub const PERIOD_CLOSED: &str = "CLOSED";
/// 会计期间状态：已开启（手工凭证只允许记到 OPEN 期间）。
pub const PERIOD_OPEN: &str = "OPEN";
/// 红字标记。
pub const IS_RED_YES: i16 = 1;
/// 蓝字标记。
pub const IS_RED_NO: i16 = 0;

/// 凭证号前缀（`PZ-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "PZ";
/// `biz_seq.seq_type`。
pub const SEQ_TYPE: &str = "PZ";
/// 流水号补零宽度。
pub const NO_SEQ_WIDTH: usize = 6;

/// 默认存货科目编码（`biz_account.account_code`）。
pub const DEFAULT_INVENTORY_CODE: &str = "1405";
/// 默认应付科目编码。
pub const DEFAULT_PAYABLE_CODE: &str = "2202";
/// 默认进项税科目编码。
pub const DEFAULT_TAX_CODE: &str = "2221.01";
/// 科目编码：待处理财产损溢（无来源入库的贷方、报废损失的借方）。
///
/// 由 `V0160__erp_serial_init_accounts` 补种（ADR §6 R2）：全库原有 5 个科目里没有它，
/// 缺了凭证就写不出来 ⇒ 成本账一加金额，`erp_invariants.py` 的 B2（BLOCKER）必红。
pub const ACCOUNT_PENDING_GAIN_LOSS: &str = "1901";
/// 科目编码：主营业务成本（序列号出库结转存货成本的借方）。
pub const ACCOUNT_MAIN_COST: &str = "6401";

// ---- 销售收入确认（ARCH-7 SO-22）用到的三个科目 ----
//
// 稿面（ARCH-7 §5.5 / §7.2）写的是「贷 6001 主营业务收入」，但**本仓科目表里没有 6001**：
// `V0183__erp_balance_sheet` 种的损益类是 `5xxx`，主营业务收入 = `5001`
// （`V0184__erp_income_statement` 也按 `5001,5051` 归集「一、营业收入」）。
// 照稿面写 6001 会直接吃 `account_required` 的「科目不存在」（SO-20 已踩过，卡内汇报留了证）。
/// 科目编码：应收账款（收入确认凭证的借方）。
pub const ACCOUNT_AR: &str = "1122";
/// 科目编码：主营业务收入（收入确认凭证的贷方；稿面 6001 在本库不存在，见上）。
pub const ACCOUNT_REVENUE: &str = "5001";
/// 科目编码：应交税费-应交增值税-销项税额（叶子科目）。
///
/// `2221` 是**汇总**科目，记账会被 [`post_voucher`] 的「只允许末级科目」闸拒掉。
pub const ACCOUNT_OUTPUT_TAX: &str = "2221.02";

/// 科目编码：财务费用（汇兑损益的对方科目，ARCH-7 §15.6.4 ②）。
///
/// 卡面写「6603 财务费用-汇兑损益」，但**本仓科目表是小企业准则**：损益类只有 `5xxx`，
/// 财务费用 = `5603`（`V0183` 资产负债表映射 + `V0184` 利润表 `FINANCE_EXPENSE` 都按 5603 取数）。
/// 与既有三次同类决策同口径（V0226 把 6602→5602、V0229 把 6602→5602.01、
/// V0311 把 6601→5601）：新种 66xx 会让财务费用在利润表与预算科目树里凭空多出一份、
/// 而 5603 那一份恒为 0。故落 `5603`（末级目录接用），
/// 摘要统一写「汇兑损益」，人在总账里按摘要即可分辨（若要单列子科目，应由财务主数据维护，
/// 不在本卡硬种 —— 科目编码一旦用过永久占用）。
pub const ACCOUNT_FX_GAIN_LOSS: &str = "5603";

/// 凭证来源类型：销售发票（`INVOICE`，与 `VoucherSourceType::Invoice` 同值）。
///
/// 收入确认的触发点是**开票**（ARCH-7 SO-22 口径「开票时确认收入」，符合税法：
/// 增值税纳税义务发生在开票当天），所以 `source_id` 落发票 id、`source_no` 落发票号，
/// 凭证↔发票可按号直接对账。
pub const SOURCE_INVOICE: &str = "INVOICE";

/// 借贷平衡容差：**差额 < 0.01 视为平衡**。
///
/// 记账的最小单位是「分」，4 位定标下 `0.00x` 的残差是拆行 / 定标带来的尾差，
/// 不该让整张凭证写不出来；它会被吸收到最后一笔贷方行（见 [`create_for_sales_revenue`]）。
/// 差额 ≥ 0.01 则是**真·不平**，必须拒绝 —— 这里绝不「算了」。
pub const BALANCE_TOLERANCE: Decimal = Decimal::from_parts(1, 0, 0, false, 2);

/// 凭证来源类型：序列号批量生成（= 无来源入库 / 期初）。
pub const SOURCE_SERIAL_BATCH: &str = "SERIAL_BATCH";
/// 凭证来源类型：序列号出库（与流水 `biz_type` 同名，凭证↔流水可按号对账）。
pub const SOURCE_SERIAL_OUT: &str = "SERIAL_OUT";
/// 凭证来源类型：在库序列号报废（同上）。
pub const SOURCE_SERIAL_SCRAP: &str = "SERIAL_SCRAP";
/// 凭证来源类型：出库单（领用 / 报废 / 盘亏 / 其他，ARCH-3 DC-13）。
///
/// 与 `biz_stock_out` 一一对应：一张出库单最多一张蓝字凭证（作废后再加一张红字）。
/// `biz_voucher.source_type` 是 `VARCHAR(24)` 且无 CHECK 白名单，新增取值不需要迁移。
pub const SOURCE_STOCK_OUT: &str = "STOCK_OUT";
/// 凭证来源类型：盘点单（ARCH-3 DC-14，盘点差异凭证）。
pub const SOURCE_STOCK_TAKE: &str = "STOCK_TAKE";

// ==================== 取号（通用：`{前缀}-yyyyMMdd-NNNNNN`） ====================

/// 一条取号规则（`biz_seq` 的 4 个可配字段）。
#[derive(Debug, Clone, Copy)]
pub struct SeqSpec<'a> {
    /// `biz_seq.seq_type`（唯一）。
    pub seq_type: &'a str,
    /// 单号前缀（`AP` / `PZ`）。
    pub prefix: &'a str,
    /// 规则名（首次建行时写入）。
    pub seq_name: &'a str,
    /// 备注（首次建行时写入）。
    pub remark: &'a str,
}

/// 组装单号：`{prefix}-{period_key}-{no:0NO_SEQ_WIDTH$}`。
pub fn format_doc_no(prefix: &str, period_key: &str, no: i64) -> String {
    format!("{prefix}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// `biz_seq(seq_type=?)` 缺失时幂等补一行。
///
/// **必须是 `ON CONFLICT … DO NOTHING`，不能靠「捕获唯一键冲突」**：一条失败的语句会把
/// 当前事务打成 aborted（PG `25P02`），之后同事务里所有语句都报
/// 「current transaction is aborted」—— 取号发生在建单事务内，等于把「首次并发建单」
/// 变成 500。`ON CONFLICT (seq_type) WHERE deleted_at IS NULL` 与
/// `uk_biz_seq_type` 这个**部分唯一索引**逐字对应（FIX-8 实测）。
async fn ensure_seq_row(ex: &impl SqlExecutor, spec: &SeqSpec<'_>) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, ?, ?, ?, 'yyyyMMdd', '', 0, 1, '-', 1, ? \
         ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(spec.seq_type.to_string()),
            Value::Text(spec.seq_name.to_string()),
            Value::Text(spec.prefix.to_string()),
            Value::Text(spec.remark.to_string()),
        ],
    )
    .await
    .or_else(|err| match err {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq` 一个号（三分支：同日累加 / 新日重置 / 时钟回拨**绝不**重置）。
///
/// 第三支是 W2b 实测的撞号地雷：`CASE WHEN period_key = ? THEN +1 ELSE 1` 会在
/// 「A→B→A（NTP 回拨 / 单测用假日期）」时把 A 当天已发的号整段撞掉。
async fn bump_seq(ex: &impl SqlExecutor, seq_type: &str, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上，biz_seq 是全局取号规则字典。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = ? AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::Text(seq_type.to_string()),
            ],
        )
        .await?;
    rows.first().map(|row| row.get_typed("current_no")).transpose()
}

/// 取一个单号（`{prefix}-yyyyMMdd-NNNNNN`，周期键按北京时间）。
pub async fn next_doc_no(ex: &impl SqlExecutor, spec: &SeqSpec<'_>, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = serial_model::period_key_of(now);
    ensure_seq_row(ex, spec).await?;
    match bump_seq(ex, spec.seq_type, &period_key).await? {
        Some(no) => Ok(format_doc_no(spec.prefix, &period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='{}') 行不存在（period_key={period_key}）",
            spec.seq_type
        ))),
    }
}

/// 凭证取号规则（`PZ-yyyyMMdd-000001`）。
pub const VOUCHER_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "凭证号",
    remark: "凭证号：PZ-yyyyMMdd-000001，按天（CST）滚动",
};

/// 取一个凭证号。
pub async fn next_voucher_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    next_doc_no(ex, &VOUCHER_SEQ, now).await
}

/// 红字凭证号前缀（`PZR-yyyyMMdd-000001`）。
///
/// **独立号段**（卡面 §二 ③）：红字凭证不能和蓝字凭证抢同一段 `biz_seq`，
/// 否则「今天最后一张蓝字凭证」与「今天第一张红字凭证」的号码语义混在一起，
/// 月末按号段排查「今天记了几张凭证 / 红冲了几张」就说不清。
pub const RED_NO_PREFIX: &str = "PZR";
/// 红字号段的 `biz_seq.seq_type`。
pub const RED_SEQ_TYPE: &str = "PZR";

/// 红字凭证取号规则。
pub const RED_VOUCHER_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: RED_SEQ_TYPE,
    prefix: RED_NO_PREFIX,
    seq_name: "红字凭证号",
    remark: "红字凭证号：PZR-yyyyMMdd-000001，按天（CST）滚动（独立于 PZ 号段）",
};

/// 取一个红字凭证号。
pub async fn next_red_voucher_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    next_doc_no(ex, &RED_VOUCHER_SEQ, now).await
}

// ==================== 会计期间 ====================

/// 某日期所属会计期间的 `yyyy-MM`。
///
/// 先按 `biz_accounting_period` 的 `[start_date, end_date]` 区间查（期间可能不按自然月切）；
/// **查不到就退化成日期的 `yyyy-MM`** —— 入库日期落在未开期间时，凭证仍要能记账
/// （fail-open 只影响"期间是否已开"的校验，那属结账模块；本卡不拦过账）。
pub async fn period_of(ex: &impl SqlExecutor, date: NaiveDate) -> DbResult<String> {
    // SCOPE-EXEMPT: biz_accounting_period 是全局会计期间字典，没有归属列。
    let rows = ex
        .query_sql(
            "SELECT period_code FROM biz_accounting_period \
              WHERE start_date <= ?::text::date AND end_date >= ?::text::date \
              ORDER BY period_code DESC LIMIT 1"
                .to_string(),
            vec![Value::Text(date.to_string()), Value::Text(date.to_string())],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("period_code"),
        None => Ok(date.format("%Y-%m").to_string()),
    }
}

/// 某期间的结账状态（`biz_accounting_period.status`）。
///
/// 期间不存在 → `None`（仓库里 `period_of` 对未开期间是 fail-open 的，这里同样不拦）。
pub async fn period_status(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_accounting_period 是全局会计期间字典，没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT status FROM biz_accounting_period WHERE period_code = ?".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(row.get_typed("status")?)),
        None => Ok(None),
    }
}

/// 期间是否已结账（红冲 / 记账的最后一道闸）。
pub async fn is_period_closed(ex: &impl SqlExecutor, period_code: &str) -> DbResult<bool> {
    Ok(period_status(ex, period_code)
        .await?
        .as_deref()
        .is_some_and(|status| status == PERIOD_CLOSED))
}

/// 北京时间「今天」（周期键 `yyyyMMdd` → 日期）。
///
/// 红字凭证的日期用「作废当天」，**必须按 CST 算**：`Utc::now().date_naive()` 在凌晨
/// （北京 00:00–08:00）还是前一天，会把凭证记到错误的期间。
pub fn today_cst(now: DateTime<Utc>) -> NaiveDate {
    let key = serial_model::period_key_of(now);
    NaiveDate::parse_from_str(&key, "%Y%m%d").unwrap_or_else(|_| now.date_naive())
}

// ==================== 科目解析 ====================
/// 科目引用（id + 编码 + 名称；分录要落编码/名称快照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRef {
    /// `biz_account.id`。
    pub id: Uuid,
    /// 科目编码（如 `1405`）。
    pub code: String,
    /// 科目名称（如 `库存商品`）。
    pub name: String,
}

/// 采购入库要用的三个科目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSet {
    /// 借：存货。
    pub inventory: AccountRef,
    /// 贷：应付账款。
    pub payable: AccountRef,
    /// 借：进项税（可空 = 系统里没有进项税科目；税率 0 时不需要它）。
    pub tax: Option<AccountRef>,
}

fn account_missing(what: &str, source: &str) -> DbError {
    DbError::InvalidValue(format!(
        "科目映射失败：{what}（{source}）不存在 / 已停用 / 已删除。\
         请先启用该科目，或在 biz_voucher_rule 里配置合法的映射规则"
    ))
}

async fn load_account_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AccountRef>> {
    // SCOPE-EXEMPT: biz_account 是会计科目主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT id, account_code, account_name FROM biz_account \
              WHERE id = ?::uuid AND status = 1 AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(account_of).transpose()
}

async fn load_account_by_code(ex: &impl SqlExecutor, code: &str) -> DbResult<Option<AccountRef>> {
    // SCOPE-EXEMPT: 同上。
    let row = ex
        .query_first_sql(
            "SELECT id, account_code, account_name FROM biz_account \
              WHERE account_code = ? AND status = 1 AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(code.to_string())],
        )
        .await?;
    row.as_ref().map(account_of).transpose()
}

fn account_of(row: &Row) -> DbResult<AccountRef> {
    Ok(AccountRef {
        id: row.get_typed("id")?,
        code: row.get_typed("account_code")?,
        name: row.get_typed("account_name")?,
    })
}

/// 按科目编码取启用中的科目（可空）。
///
/// 供付款凭证（2202 / 1002）与手工凭证按 `account_id` 回填编码 / 名称快照复用。
pub async fn account_by_code(ex: &impl SqlExecutor, code: &str) -> DbResult<Option<AccountRef>> {
    load_account_by_code(ex, code).await
}

/// 按科目编码取科目，取不到就报**点名科目编码**的明确错误（卡面 §一 ②：科目不存在要报明确错误）。
pub async fn account_required(ex: &impl SqlExecutor, code: &str, what: &str) -> DbResult<AccountRef> {
    load_account_by_code(ex, code)
        .await?
        .ok_or_else(|| account_missing(what, &format!("科目编码 {code}")))
}

/// 按科目 id 取启用中的科目（可空），供手工凭证按 `account_id` 取快照。
pub async fn account_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AccountRef>> {
    load_account_by_id(ex, id).await
}

/// 科目声明的辅助核算维度（`biz_account.aux_dimensions`，逗号分隔，如 `DEPT` / `SUPPLIER,MATERIAL`）。
///
/// `None` = 该科目不挂辅助核算（不需要任何 `aux_*`）。
/// 只给**手工凭证**的提交校验用：自动凭证的维度由各业务路径自己决定（付款挂部门、入库挂物料），
/// 不按科目声明卡（`docs/ERP-辅助核算口径.md` §2）。
pub async fn account_aux_dimensions(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_account 是会计科目主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT aux_dimensions FROM biz_account \
              WHERE id = ?::uuid AND status = 1 AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match row {
        Some(row) => row.get_typed::<Option<String>>("aux_dimensions"),
        // 科目不存在 / 已停用：由调用方的 `account_by_id` 出 400，这里不重复报
        None => Ok(None),
    }
}

/// 分录里落在**非末级**科目上的科目编码（去重、编码升序；空 = 全部可记账）。
///
/// 与 [`post_voucher`] 的闸配套，理由见模块文档「记账科目口径」。按**编码**批查一次
/// （不做 N 次单查）：手工凭证按 `account_id` 提交、自动凭证按编码解析，最后都用编码
/// 回表比对，两边同一口径。
async fn non_leaf_codes(ex: &impl SqlExecutor, lines: &[EntryLine]) -> DbResult<Vec<String>> {
    let mut codes: Vec<String> = lines.iter().map(|line| line.account.code.clone()).collect();
    codes.sort();
    codes.dedup();
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; codes.len()].join(", ");
    // SCOPE-EXEMPT: biz_account 是会计科目主数据（全局），没有归属列。
    let sql = format!(
        "SELECT account_code FROM biz_account \
          WHERE account_code IN ({placeholders}) AND deleted_at IS NULL AND is_leaf = 0 \
          ORDER BY account_code"
    );
    let params: Vec<Value> = codes.into_iter().map(Value::Text).collect();
    let rows = ex.query_sql(sql, params).await?;
    rows.iter().map(|row| row.get_typed::<String>("account_code")).collect()
}

/// 一条映射规则（三个科目 id 都已解析成 `AccountRef`；税科目可空）。
#[derive(Debug, Clone)]
struct ResolvedRule {
    debit: Option<AccountRef>,
    credit: Option<AccountRef>,
    tax: Option<AccountRef>,
}

/// 按 `biz_type` 取启用的映射规则（`rule_code` 唯一，取最早一条）。
async fn load_rule(ex: &impl SqlExecutor, biz_type: &str) -> DbResult<Option<ResolvedRule>> {
    // SCOPE-EXEMPT: biz_voucher_rule 是全局科目映射配置，没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT debit_account_id, credit_account_id, tax_account_id \
               FROM biz_voucher_rule \
              WHERE biz_type = ? AND is_active = 1 AND deleted_at IS NULL \
              ORDER BY created_at ASC LIMIT 1"
                .to_string(),
            vec![Value::Text(biz_type.to_string())],
        )
        .await?;
    resolve_rule(ex, row).await
}

/// 按 `rule_code`（= 配置里的「凭证模板」）取启用的映射规则。
///
/// 【D3】联动动作 `GENERATE_VOUCHER` 的 `template_code` 参数走这条：
/// 与 `biz_type` 同表同栏位，只是换一个查询键 —— **不新增第二套映射表**。
async fn load_rule_by_code(ex: &impl SqlExecutor, rule_code: &str) -> DbResult<Option<ResolvedRule>> {
    // SCOPE-EXEMPT: 同上。
    let row = ex
        .query_first_sql(
            "SELECT debit_account_id, credit_account_id, tax_account_id \
               FROM biz_voucher_rule \
              WHERE rule_code = ? AND is_active = 1 AND deleted_at IS NULL \
              ORDER BY created_at ASC LIMIT 1"
                .to_string(),
            vec![Value::Text(rule_code.to_string())],
        )
        .await?;
    resolve_rule(ex, row).await
}

/// 把一行 `biz_voucher_rule` 解析成三个科目（`None` = 规则不存在）。
///
/// `load_rule`（按 biz_type）与 `load_rule_by_code`（按模板码）共用本函数：
/// 「规则 → 科目」的解析口径只能有一处，否则模板路径与默认路径的科目会分叉。
async fn resolve_rule(ex: &impl SqlExecutor, row: Option<nik::prelude::Row>) -> DbResult<Option<ResolvedRule>> {
    let Some(row) = row else {
        return Ok(None);
    };
    let debit_id: Option<Uuid> = row.get_typed("debit_account_id")?;
    let credit_id: Option<Uuid> = row.get_typed("credit_account_id")?;
    let tax_id: Option<Uuid> = row.get_typed("tax_account_id")?;
    let debit = match debit_id {
        Some(id) => load_account_by_id(ex, id).await?,
        None => None,
    };
    let credit = match credit_id {
        Some(id) => load_account_by_id(ex, id).await?,
        None => None,
    };
    let tax = match tax_id {
        Some(id) => load_account_by_id(ex, id).await?,
        None => None,
    };
    Ok(Some(ResolvedRule { debit, credit, tax }))
}

/// 物料的存货科目（`biz_material.inventory_account_id`，可空）。
async fn material_inventory_account(ex: &impl SqlExecutor, material_id: Uuid) -> DbResult<Option<AccountRef>> {
    // SCOPE-EXEMPT: biz_material 是物料主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT inventory_account_id FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&material_id)],
        )
        .await?;
    match row
        .as_ref()
        .map(|row| row.get_typed::<Option<Uuid>>("inventory_account_id"))
    {
        Some(Ok(Some(id))) => load_account_by_id(ex, id).await,
        Some(Ok(None)) | None => Ok(None),
        Some(Err(err)) => Err(err),
    }
}

/// 一个物料的存货科目解析结果（逐物料出行用；`label` 只用于分行摘要）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialInventoryRef {
    /// 摘要用的物料标签（`物料 {material_code}`；物料已软删 / 取不到编码 → `物料 {id 前 8 位}`）。
    pub label: String,
    /// 该物料的存货科目（规则 → 物料 `inventory_account_id` → `1405` 的**最终**结果）。
    pub account: AccountRef,
}

/// **一条 SQL** 批量解析本单每个物料的存货科目（P2-1b §4.4）。
///
/// 退化顺序与 [`resolve_accounts_with`] 逐字一致，只是把「首个物料的科目」换成「**这一行物料**
/// 的科目」：
///
/// ```text
/// 规则 debit_account_id  →  该物料 biz_material.inventory_account_id  →  1405 库存商品
/// ```
///
/// # 为什么必须一次取完
///
/// 一张 200 行的收货单去重后可能几十个物料；逐行查 `biz_material` + `biz_account` 就是几十次
/// 往返，而凭证是在**过账事务**里生成的（持着收货单行锁），慢查询会直接拖长锁等待。
///
/// # 退化与报错
///
/// * 规则配了 `debit_account_id`（实测三个库的活跃规则 100% 有值）⇒ 全部物料都用它
///   （仍会取一次物料编码做分行摘要，但不再看物料的科目）；
/// * 物料没配科目 / 科目被停用 / 物料已软删 ⇒ 该行退到默认 `1405`；
/// * 默认 `1405` 也不存在 ⇒ `account_missing`（宁可拦住，不许兜底，PRD §3.4.3）。
async fn material_inventory_accounts(
    ex: &impl SqlExecutor,
    rule_debit: Option<&AccountRef>,
    material_lines: &[VoucherMaterialLine],
) -> DbResult<BTreeMap<Uuid, MaterialInventoryRef>> {
    let mut resolved: BTreeMap<Uuid, MaterialInventoryRef> = BTreeMap::new();
    if material_lines.is_empty() {
        return Ok(resolved);
    }
    // 物料编码 + 该物料自己的存货科目（科目必须**启用且未删**，与 `load_account_by_id` 同口径）
    let mut sql = String::from(
        "SELECT m.id, m.material_code, a.id AS account_id, a.account_code, a.account_name \
           FROM biz_material m \
           LEFT JOIN biz_account a \
             ON a.id = m.inventory_account_id AND a.status = 1 AND a.deleted_at IS NULL \
          WHERE m.deleted_at IS NULL AND m.id IN (",
    );
    for index in 0..material_lines.len() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("?::uuid");
    }
    sql.push(')');
    let params: Vec<Value> = material_lines
        .iter()
        .map(|line| Value::from(&line.material_id))
        .collect();
    let rows = ex.query_sql(sql, params).await?;
    // 中间态：`material_id → (物料编码, 该物料自己的存货科目)`，后一步再套「规则 → 物料 → 1405」。
    //
    // **物料编码与科目解耦**（FIX-9 ②）：以前这一层只在「物料配了启用的存货科目」时才把
    // 编码一起存下来（`account.map(|a| (code, a))`），物料没配科目（`inventory_account_id`
    // 为空，演示数据 `DEMO-P101/102` 就是）时编码被一起丢掉 → 分录摘要退化成
    // 「存货 物料 01a1b5d2（第 1 行）」这种 UUID 前 8 位。摘要的可读性与科目解析结果无关，
    // 所以编码**只要物料行在就一定留**，科目缺席只影响科目退化链。
    let mut from_material: BTreeMap<Uuid, (String, Option<AccountRef>)> = BTreeMap::new();
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let code: String = row.get_typed("material_code")?;
        let account_id: Option<Uuid> = row.get_typed("account_id")?;
        let account = match account_id {
            Some(_) => Some(AccountRef {
                id: row.get_typed("account_id")?,
                code: row.get_typed("account_code")?,
                name: row.get_typed("account_name")?,
            }),
            None => None,
        };
        from_material.insert(id, (code, account));
    }
    // 默认 1405 只在真的需要时取一次（多数单子走不到这一层）
    let mut default_account: Option<AccountRef> = None;
    for line in material_lines {
        let (label, material_account) = match from_material.get(&line.material_id) {
            Some((code, account)) => (format!("物料 {code}"), account.clone()),
            // 物料已软删 / 不存在：分录照样要出（金额在明细上），摘要退回 id 前 8 位
            None => (format!("物料 {}", short_id(line.material_id)), None),
        };
        let account = match rule_debit {
            Some(account) => account.clone(),
            None => match material_account {
                Some(account) => account,
                None => match &default_account {
                    Some(account) => account.clone(),
                    None => {
                        let loaded = load_account_by_code(ex, DEFAULT_INVENTORY_CODE).await?.ok_or_else(|| {
                            account_missing(
                                "默认存货科目 1405 库存商品",
                                "biz_voucher_rule 无 PURCHASE_IN 规则且物料未配存货科目",
                            )
                        })?;
                        default_account = Some(loaded.clone());
                        loaded
                    }
                },
            },
        };
        resolved.insert(line.material_id, MaterialInventoryRef { label, account });
    }
    Ok(resolved)
}

/// 主键的短形式（前 8 位）：物料已软删 / 取不到编码时，摘要里至少能对上 id。
fn short_id(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

/// 分录摘要里的物料标签：`物料 {编码}`（取不到编码 → `物料 {id 前 8 位}`）。
fn material_label(codes: &BTreeMap<Uuid, String>, material_id: Uuid) -> String {
    match codes.get(&material_id) {
        Some(code) => format!("物料 {code}"),
        None => format!("物料 {}", short_id(material_id)),
    }
}

/// 批量取物料编码（出库凭证的分行摘要用；**一条 SQL**，不给每行查一次）。
async fn material_codes(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<BTreeMap<Uuid, String>> {
    let mut codes: BTreeMap<Uuid, String> = BTreeMap::new();
    if ids.is_empty() {
        return Ok(codes);
    }
    let mut sql = String::from("SELECT id, material_code FROM biz_material WHERE deleted_at IS NULL AND id IN (");
    for index in 0..ids.len() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("?::uuid");
    }
    sql.push(')');
    // SCOPE-EXEMPT: biz_material 是物料主数据（全局），没有归属列。
    let params: Vec<Value> = ids.iter().map(Value::from).collect();
    for row in &ex.query_sql(sql, params).await? {
        let id: Uuid = row.get_typed("id")?;
        let code: String = row.get_typed("material_code")?;
        codes.insert(id, code);
    }
    Ok(codes)
}

/// 供应商的应付科目（`biz_supplier.payable_account_id`，可空）。
async fn supplier_payable_account(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<Option<AccountRef>> {
    // SCOPE-EXEMPT: biz_supplier 是供应商主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT payable_account_id FROM biz_supplier \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    match row
        .as_ref()
        .map(|row| row.get_typed::<Option<Uuid>>("payable_account_id"))
    {
        Some(Ok(Some(id))) => load_account_by_id(ex, id).await,
        Some(Ok(None)) | None => Ok(None),
        Some(Err(err)) => Err(err),
    }
}

/// 解析本次业务要用的三个科目（PRD §3.4 / 卡面 §四）。
///
/// 优先级（**逐项独立**，规则里缺哪项就单独退化，不会整条规则作废）：
///
/// ```text
/// 借存货：biz_voucher_rule.debit_account_id  → 物料 inventory_account_id → 1405
/// 贷应付：biz_voucher_rule.credit_account_id → 供应商 payable_account_id → 2202
/// 借进项税：biz_voucher_rule.tax_account_id  → 2221.01（可缺；税率 0 时用不到）
/// ```
///
/// 存货 / 应付科目最终解析不到 → `InvalidValue`（**宁可拦住，不许兜底**，PRD §3.4.3）。
pub async fn resolve_accounts(
    ex: &impl SqlExecutor,
    biz_type: &str,
    material_id: Option<Uuid>,
    supplier_id: Option<Uuid>,
) -> DbResult<AccountSet> {
    let rule = load_rule(ex, biz_type).await?;
    resolve_accounts_with(ex, rule, material_id, supplier_id).await
}

/// 科目解析的**唯一实现**（规则已取好，只做「规则 → 物料 → 默认」三层退化）。
///
/// `rule = None` 表示没有可用映射规则（按 `biz_type` 或模板码都没取到）——
/// 这时候走物料 / 供应商 / 默认科目退化，与拆分前逐字一致。
async fn resolve_accounts_with(
    ex: &impl SqlExecutor,
    rule: Option<ResolvedRule>,
    material_id: Option<Uuid>,
    supplier_id: Option<Uuid>,
) -> DbResult<AccountSet> {
    let inventory = match rule.as_ref().and_then(|r| r.debit.clone()) {
        Some(account) => account,
        None => {
            let from_material = match material_id {
                Some(id) => material_inventory_account(ex, id).await?,
                None => None,
            };
            match from_material {
                Some(account) => account,
                None => load_account_by_code(ex, DEFAULT_INVENTORY_CODE).await?.ok_or_else(|| {
                    account_missing(
                        "默认存货科目 1405 库存商品",
                        "biz_voucher_rule 无 PURCHASE_IN 规则且物料未配存货科目",
                    )
                })?,
            }
        }
    };

    let payable = match rule.as_ref().and_then(|r| r.credit.clone()) {
        Some(account) => account,
        None => {
            let from_supplier = match supplier_id {
                Some(id) => supplier_payable_account(ex, id).await?,
                None => None,
            };
            match from_supplier {
                Some(account) => account,
                None => load_account_by_code(ex, DEFAULT_PAYABLE_CODE).await?.ok_or_else(|| {
                    account_missing(
                        "默认应付科目 2202 应付账款",
                        "biz_voucher_rule 无 PURCHASE_IN 规则且供应商未配应付科目",
                    )
                })?,
            }
        }
    };

    let tax = match rule.as_ref().and_then(|r| r.tax.clone()) {
        Some(account) => Some(account),
        None => load_account_by_code(ex, DEFAULT_TAX_CODE).await?,
    };

    Ok(AccountSet {
        inventory,
        payable,
        tax,
    })
}

// ==================== 分录（纯逻辑） ====================

/// 一张单据要入账的三个金额（**全部取自单头，不重算**）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoucherAmounts {
    /// 不含税金额（`biz_stock_in.total_amount`）→ 借存货。
    pub amount: Decimal,
    /// 税额（`total_tax`）→ 借进项税（0 时不出行）。
    pub tax: Decimal,
    /// 价税合计（`total_with_tax`）→ 贷应付。
    pub with_tax: Decimal,
}

impl VoucherAmounts {
    /// 由收货单单头三列构造。
    pub fn of_stock_in(header: &StockInRow) -> Self {
        Self {
            amount: header.total_amount.round_dp(4),
            tax: header.total_tax.round_dp(4),
            with_tax: header.total_with_tax.round_dp(4),
        }
    }

    /// 借贷合计（蓝字 = 价税合计）。
    pub fn total(&self) -> Decimal {
        self.with_tax
    }
}

/// 一条「按物料拆分」的存货分录来源（P2-1b，`docs/ERP-辅助核算口径.md` §4.2）。
///
/// 粒度是**去重物料**（`MATERIAL`）而不是明细行 —— 同一 `material_id` 的多行必须先在
/// [`VoucherMaterialLine::normalize`] 里合并（金额相加、数量相加、行号取 min/max），
/// 所以这里的 `amount` 已经是「该物料的 Σ 明细金额」，不是单行金额。
///
/// 金额只来自明细（收货 `biz_stock_in_item.amount`、出库账上真实扣减额），单头金额**不在这里
/// 重算**：`Σ(存货行) ≡ 单头 total_amount` 的尾差由 [`absorb_tail_diff`] 吸收。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherMaterialLine {
    /// 物料（`biz_material.id`；也是出行的**升序键**，与批次 1 技术架构的加锁顺序同口径）。
    pub material_id: Uuid,
    /// 该物料的 Σ 数量。
    ///
    /// **不写进分录**：`aux_qty` 本批留空（§4.6：没有数量账消费方，科目上也没有「是否数量核算」
    /// 开关）。留在这里是为了日志与「行数/数量对得上」的排查。
    pub qty: Decimal,
    /// 该物料的 Σ 明细金额（收货是不含税行金额，出库是账上真实扣减金额）。
    pub amount: Decimal,
    /// 该物料覆盖的最小明细行号（摘要写「第 k–j 行」，与明细双向追溯）。
    pub line_from: i32,
    /// 该物料覆盖的最大明细行号。
    pub line_to: i32,
}

impl VoucherMaterialLine {
    /// 归一化成「按 `material_id` 升序、每个物料恰好一行」。
    ///
    /// 三条要求都在这一个地方保证，调用方（过账路径 / 配置路径 / 出库路径）不需要各自再排一次：
    /// * **升序**：同一张单两次生成的凭证行序必须一致（D3 影子对比逐行比对）；
    /// * **去重**：同一物料的多行明细先合并（金额、数量相加，行号取区间端点）；
    /// * **幂等**：已经是归一化结果再走一遍不变。
    pub fn normalize(lines: impl IntoIterator<Item = Self>) -> Vec<Self> {
        let mut merged: BTreeMap<Uuid, Self> = BTreeMap::new();
        for line in lines {
            match merged.get_mut(&line.material_id) {
                Some(existing) => {
                    existing.qty += line.qty;
                    existing.amount += line.amount;
                    existing.line_from = existing.line_from.min(line.line_from);
                    existing.line_to = existing.line_to.max(line.line_to);
                }
                None => {
                    merged.insert(line.material_id, line);
                }
            }
        }
        merged.into_values().collect()
    }
}

/// 把「单头金额 − Σ 明细金额」的尾差吸收到按 `material_id` 升序的最后一行。
///
/// # 为什么要吸收而不是重算单头
///
/// 凭证的硬约束是 `Σ借 = Σ贷` 与 `Σ(存货行) = 单头 total_amount`（`post_voucher` 与
/// `erp_invariants` A1/B2）。单头金额是权威值，明细是取数来源；两者只可能因为**非 API 写入**
/// （历史数据 / 手工 SQL / 测试夹具）而不等，这时候差额必须落在账上，不能悄悄丢掉。
///
/// # 口径（§4.2）
///
/// * `diff ≠ 0` ⇒ `tracing::warn!` 点名**单号 + 差额**（正常业务恒为 0，非 0 = 数据质量信号）；
/// * 先加到**最后一个物料**那行；如果吸收后该行金额 ≤ 0（异常数据），往前退一个物料；
/// * 所有行都吸收不了 ⇒ **报错**（`InvalidValue`），绝不静默吞掉差额。
///
/// # 参数
///
/// * `header_amount`：单头金额（权威值，已 `round_dp(4)`）；
/// * `amounts`：按物料升序的逐物料金额（长度与物料行一致，**已剔除 0 元物料**）。
pub fn absorb_tail_diff(doc_no: &str, header_amount: Decimal, amounts: &[Decimal]) -> DbResult<Vec<Decimal>> {
    let header = header_amount.round_dp(4);
    let material_sum: Decimal = amounts.iter().copied().sum();
    let diff = (header - material_sum).round_dp(4);
    if diff.is_zero() {
        return Ok(amounts.to_vec());
    }
    tracing::warn!(
        doc_no,
        header_amount = %header,
        material_sum = %material_sum.round_dp(4),
        tail_diff = %diff,
        "按物料拆分录的尾差不为 0：差额已吸收到最后一行（正常业务恒为 0，非 0 = 数据质量信号）"
    );
    let mut adjusted = amounts.to_vec();
    for index in (0..adjusted.len()).rev() {
        let candidate = (adjusted[index] + diff).round_dp(4);
        if candidate > Decimal::ZERO {
            adjusted[index] = candidate;
            return Ok(adjusted);
        }
    }
    Err(DbError::InvalidValue(format!(
        "凭证生成失败：按物料拆分录的尾差吸收不了（单据 {doc_no} 单头金额 {header}、\
         明细合计 {}、尾差 {diff}；按物料升序从后往前没有一行吸收后仍 > 0）。\
         这是数据质量问题（单头与明细不一致），请先核对单据金额",
        material_sum.round_dp(4)
    )))
}

/// 一条待落库的分录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryLine {
    /// 行号（从 1 起）。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目（编码 / 名称要快照进分录）。
    pub account: AccountRef,
    /// 借方金额。
    pub debit: Decimal,
    /// 贷方金额。
    pub credit: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<Uuid>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<Uuid>,
    /// 辅助核算：部门（"谁花的钱"；只有付款凭证写，见模块文档 §辅助核算）。
    pub aux_dept_id: Option<Uuid>,
}

/// 分录构造参数（避免 `too_many_arguments`）。
#[derive(Debug, Clone, Copy)]
pub struct LineContext {
    /// 借方存货行的辅助核算：仓库。
    pub warehouse_id: Option<Uuid>,
    /// **回退分支**（`material_lines` 为空）的存货行物料辅助核算。
    ///
    /// 按物料拆分录时，每行的物料来自 [`VoucherMaterialLine::material_id`]，不读这个字段
    /// （P2-1b 起多物料单不再留空，而是逐物料出行）。
    pub material_id: Option<Uuid>,
    /// 贷方应付行的辅助核算：供应商。
    pub supplier_id: Option<Uuid>,
}

/// 采购入库蓝字分录（PRD §3.4.2 / 卡面 §三 ④ / P2-1b §4.3）。
///
/// ```text
/// 借 库存商品        Σ(该物料明细金额)  × M   （每行 aux_material_id = 该行物料 + aux_warehouse_id）
/// 借 应交税费-进项税  total_tax          × 1   （税率为 0 时不产生该行）
/// 贷 应付账款        total_with_tax     × 1   （aux_supplier_id，仍按 DOC 一条）
/// ```
///
/// # 两条分支
///
/// * `material_lines` 为空 ⇒ **回退**到「单头一行 1405」（`material_id = ctx.material_id`）。
///   非 `STOCK_IN` 的调用方与历史数据走这里，行为与 P2-1b 之前**逐字相同**；
/// * 非空 ⇒ **每个去重物料一行**（`M + 2` / 零税 `M + 1` 行）：金额取该物料的 Σ 明细金额，
///   尾差由 [`absorb_tail_diff`] 吸收到最后一行，科目用 `material_accounts` 里**该物料**的科目。
///   金额为 0 的物料不出行（0 元赠品行不占金额，数量仍在库存流水里）。
///
/// # 为什么拆分写在这里（而不是 handler 里）
///
/// 过账路径（`create_for_stock_in`）与联动配置路径（`linkage::actions::commit_voucher`）都调
/// [`create_by_rule`]，再调本函数 —— **只有一份拆分实现**，所以 D3 影子对比（逐行比 `lines` /
/// `entry_count`）不会因为两条路径各写一套而报差异。谁在 handler 里另写一套，影子立刻红。
///
/// # 参数
///
/// * `doc_no`：单号（只用于尾差 WARN / 报错点名单据）；
/// * `material_accounts`：逐物料存货科目（必须覆盖 `material_lines` 的全部物料，缺了报错）；
/// * 其它参数同拆分前。
pub fn purchase_in_lines(
    accounts: &AccountSet,
    amounts: VoucherAmounts,
    summary: &str,
    ctx: LineContext,
    material_lines: &[VoucherMaterialLine],
    material_accounts: &BTreeMap<Uuid, MaterialInventoryRef>,
    doc_no: &str,
) -> DbResult<Vec<EntryLine>> {
    let materials = VoucherMaterialLine::normalize(material_lines.iter().cloned());
    let mut lines: Vec<EntryLine> = Vec::with_capacity(materials.len() + 2);
    if materials.is_empty() {
        // ---- 回退：单头一行 1405（与 P2-1b 之前逐字相同）----
        if amounts.amount > Decimal::ZERO {
            lines.push(EntryLine {
                line_no: 0,
                summary: format!("{summary} 存货"),
                account: accounts.inventory.clone(),
                debit: amounts.amount,
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: ctx.material_id,
                aux_warehouse_id: ctx.warehouse_id,
                // 采购入库**故意不写部门**：`biz_purchase_order` / `biz_stock_in` 都没有
                // `dept_id`，"谁买的"在系统里没有权威字段（DC-11 / DC-16）。按申请人部门
                // 猜一个值等于给财务一张错的部门辅助账，不如留空（见模块文档 §辅助核算）。
                aux_dept_id: None,
            });
        }
    } else {
        // ---- 按物料拆：每个物料一行 1405，金额取明细、末行吸尾差 ----
        let mut positives: Vec<&VoucherMaterialLine> =
            materials.iter().filter(|line| line.amount > Decimal::ZERO).collect();
        // `normalize` 已按 material_id 升序；这里再排一次是廉价的自证（顺序是契约的一部分）
        positives.sort_by_key(|line| line.material_id);
        let amounts_of_materials: Vec<Decimal> = positives.iter().map(|line| line.amount).collect();
        let split = absorb_tail_diff(doc_no, amounts.amount, &amounts_of_materials)?;
        for (line, amount) in positives.iter().zip(split) {
            let resolved = material_accounts.get(&line.material_id).ok_or_else(|| {
                DbError::InvalidValue(format!(
                    "凭证生成失败：单据 {doc_no} 的物料 {} 没有解析出存货科目（内部错误：\
                     逐物料科目批取与物料行对不上）",
                    line.material_id
                ))
            })?;
            let rows = line_range_text(line.line_from, line.line_to);
            lines.push(EntryLine {
                line_no: 0,
                summary: format!("{summary} 存货 {}（{rows}）", resolved.label),
                account: resolved.account.clone(),
                debit: amount,
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: Some(line.material_id),
                aux_warehouse_id: ctx.warehouse_id,
                // 同回退分支：采购入库不写部门（单据上没有权威部门字段）
                aux_dept_id: None,
            });
        }
    }
    if amounts.tax > Decimal::ZERO
        && let Some(tax_account) = accounts.tax.clone()
    {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{summary} 进项税额"),
            account: tax_account,
            debit: amounts.tax,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    if amounts.with_tax > Decimal::ZERO {
        lines.push(EntryLine {
            line_no: 0,
            summary: summary.to_string(),
            account: accounts.payable.clone(),
            debit: Decimal::ZERO,
            credit: amounts.with_tax,
            aux_supplier_id: ctx.supplier_id,
            aux_material_id: None,
            aux_dept_id: None,
            aux_warehouse_id: None,
        });
    }
    for (index, line) in lines.iter_mut().enumerate() {
        line.line_no = i32::try_from(index + 1).unwrap_or(0);
    }
    Ok(lines)
}

/// 明细行号区间的摘要文字：`第 3 行` / `第 3–5 行`。
///
/// 只写区间不写逐行，是因为凭证的物料粒度到「物料」为止；行级金额本来就在
/// `biz_stock_in_item` / `biz_stock_out_item` 上可查，摘要给出区间即可双向追溯。
fn line_range_text(line_from: i32, line_to: i32) -> String {
    if line_from == line_to {
        format!("第 {line_from} 行")
    } else {
        format!("第 {line_from}–{line_to} 行")
    }
}

/// 红字分录：**借贷互换**（架构 §2.5.2 方案 B），金额原样、行号重排、摘要加「红冲 」。
pub fn reversed_lines(original: &[EntryRow], reason: Option<&str>) -> Vec<EntryLine> {
    let prefix = match reason.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => format!("红冲（{text}）"),
        None => "红冲".to_string(),
    };
    original
        .iter()
        .enumerate()
        .map(|(index, row)| EntryLine {
            line_no: i32::try_from(index + 1).unwrap_or(0),
            summary: format!("{prefix}{}", row.summary),
            account: row.account.clone(),
            debit: row.credit,
            credit: row.debit,
            aux_supplier_id: row.aux_supplier_id,
            aux_material_id: row.aux_material_id,
            aux_warehouse_id: row.aux_warehouse_id,
            // 红冲必须原样复制辅助核算（技术架构 §3.1.7 第 4 条）：按辅助项查明细
            // 时，红冲行要落在**同一个**辅助项下，否则红冲会"冲不掉"那一项。
            aux_dept_id: row.aux_dept_id,
        })
        .collect()
}

/// 分录合计（借方 / 贷方）。
pub fn totals_of(lines: &[EntryLine]) -> (Decimal, Decimal) {
    let debit = lines.iter().fold(Decimal::ZERO, |acc, l| acc + l.debit);
    let credit = lines.iter().fold(Decimal::ZERO, |acc, l| acc + l.credit);
    (debit.round_dp(4), credit.round_dp(4))
}

// ==================== 行结构（读） ====================

/// `biz_voucher` 列清单。
pub const VOUCHER_COLUMNS: &str = "id, voucher_word, voucher_no, period, voucher_date, \
     voucher_type, source_type, source_id, source_no, source_doc_no, summary, total_debit, total_credit, \
     entry_count, status, is_red, reverse_of, reversed_by, posted_at, posted_by, remark, \
     pending_reverse_instance_id, reverse_reason, \
     created_at, updated_at, create_by, update_by, deleted_at";

/// 凭证行。
#[derive(Debug, Clone)]
pub struct VoucherRow {
    /// 主键。
    pub id: Uuid,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 会计期间（`yyyy-MM`）。
    pub period: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证类型（AUTO / MANUAL）。
    pub voucher_type: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Option<Uuid>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 业务单据号（`source_doc_no`）：红冲凭证**原样继承**原凭证的这一列，
    /// 否则「按退货单号查凭证」会漏掉红冲它的那张。
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
    /// 是否红字。
    pub is_red: i16,
    /// 红字凭证指向的原凭证。
    pub reverse_of: Option<Uuid>,
    /// 原凭证指向的红字凭证。
    pub reversed_by: Option<Uuid>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 过账人。
    pub posted_by: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 红冲审批实例（P-FLOW-3B）：有值 = 正在审批红冲（`status` 仍是 `POSTED`，报表口径不变）。
    pub pending_reverse_instance_id: Option<Uuid>,
    /// 红冲原因（P-FLOW-3B）：提交审批时填写，通过后成为红字凭证的分录摘要。
    pub reverse_reason: Option<String>,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl VoucherRow {
    fn from_row(row: &Row) -> DbResult<Self> {
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
        })
    }
}

/// `biz_voucher_entry` 列清单。
pub const ENTRY_COLUMNS: &str = "id, voucher_id, line_no, summary, account_id, account_code, \
     account_name, debit_amount, credit_amount, aux_supplier_id, aux_material_id, \
     aux_warehouse_id, aux_dept_id, created_at";

/// 分录行（读回用）。
#[derive(Debug, Clone)]
pub struct EntryRow {
    /// 主键。
    pub id: Uuid,
    /// 所属凭证。
    pub voucher_id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目（id / 编码 / 名称快照）。
    pub account: AccountRef,
    /// 借方金额。
    pub debit: Decimal,
    /// 贷方金额。
    pub credit: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<Uuid>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<Uuid>,
    /// 辅助核算：部门。
    pub aux_dept_id: Option<Uuid>,
}

impl EntryRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            voucher_id: row.get_typed("voucher_id")?,
            line_no: row.get_typed("line_no")?,
            summary: row.get_typed("summary")?,
            account: AccountRef {
                id: row.get_typed("account_id")?,
                code: row.get_typed("account_code")?,
                name: row.get_typed("account_name")?,
            },
            debit: row.get_typed("debit_amount")?,
            credit: row.get_typed("credit_amount")?,
            aux_supplier_id: row.get_typed("aux_supplier_id")?,
            aux_material_id: row.get_typed("aux_material_id")?,
            aux_warehouse_id: row.get_typed("aux_warehouse_id")?,
            aux_dept_id: row.get_typed("aux_dept_id")?,
        })
    }
}

/// 按 id 取凭证。
pub async fn find_voucher(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<VoucherRow>> {
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT {VOUCHER_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(VoucherRow::from_row).transpose()
}

/// 按 id 取凭证并**加行锁**（`SELECT ... FOR UPDATE`，红冲用：防同一张凭证被并发红冲两次）。
pub async fn find_voucher_locked(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<VoucherRow>> {
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {VOUCHER_COLUMNS} FROM {TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(VoucherRow::from_row).transpose()
}

/// 取凭证的**制单人**（`biz_voucher.create_by`）。
///
/// P0-FIX-4A 的「制单人不能审核自己的凭证」要用它：`VoucherRow` 不带 `create_by`
/// （它是审计列，不是业务字段），单独取一次比给全仓的 `VOUCHER_COLUMNS` 加列更安全 ——
/// 那个常量被列表 / 详情 / 反查三处共用，加列会牵动每条 SELECT 的解码。
///
/// 返回 `None` = 历史行 / 系统生成（没有制单人可比，谈不上「自己审自己」）。
pub async fn creator_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<Uuid>> {
    // SCOPE-EXEMPT: 同上（按主键精确取 1 列）。
    let row = ex
        .query_first_sql(
            format!("SELECT create_by FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    match row {
        Some(row) => row.get_typed("create_by"),
        None => Ok(None),
    }
}

/// 按来源单据取真凭证（`is_red = 0`，取最早一张）。
pub async fn find_voucher_by_source(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
) -> DbResult<Option<VoucherRow>> {
    // SCOPE-EXEMPT: 同上。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {VOUCHER_COLUMNS} FROM {TABLE} \
                  WHERE source_type = ? AND source_id = ?::uuid AND is_red = 0 AND deleted_at IS NULL \
                  ORDER BY created_at ASC LIMIT 1"
            ),
            vec![Value::Text(source_type.to_string()), Value::from(&source_id)],
        )
        .await?;
    row.as_ref().map(VoucherRow::from_row).transpose()
}

/// 取一张凭证的全部分录（行号升序）。
pub async fn entries_of(ex: &impl SqlExecutor, voucher_id: Uuid) -> DbResult<Vec<EntryRow>> {
    // SCOPE-EXEMPT: 同上。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ENTRY_COLUMNS} FROM {ENTRY_TABLE} WHERE voucher_id = ?::uuid \
                 ORDER BY line_no ASC"
            ),
            vec![Value::from(&voucher_id)],
        )
        .await?;
    rows.iter().map(EntryRow::from_row).collect()
}

// ==================== 写：生成凭证 ====================

/// 一张业务单据生成凭证所需的**全部上下文**（取数结果，不含算法）。
///
/// # 【D3】为什么要有这个结构（「先抽公共函数，再接管」的那一步）
///
/// 「取数」在凭证链路上必须只有一份口径：金额只从单头拿（**不重算**）、
/// 摘要只由「单号 + 价税合计」拼、存货辅助核算取首个物料、凭证日期取单据业务日期。
/// 收货过账（`stockin_handler::post_in_tx` ⑦）与联动动作 `GENERATE_VOUCHER`
/// 如果各写一遍取数，同一张单会按「哪条路径先跑」生成两张不一样的凭证 —— 那就是双真相。
///
/// 所以取数只有 [`VoucherBizCtx::of_stock_in`] 一处：
/// - 过账路径用它把**内存里已经加锁的单头**转成上下文（不再查库）；
/// - 配置路径用 `stockin_model::voucher_biz_ctx` 从库里取单头，再走**同一个**函数。
///
/// 取号 / 借贷平衡 / 落库仍然全部在 [`create_by_rule`] → [`post_voucher`]，
/// 本结构只承载数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherBizCtx {
    /// 来源单据类型（`STOCK_IN` / `PAYMENT` / …）。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: String,
    /// 凭证日期（默认 = 单据业务日期；配置可用 `account_date` 覆盖）。
    pub voucher_date: NaiveDate,
    /// 三个金额（取自单头）。
    pub amounts: VoucherAmounts,
    /// 凭证摘要（整单级）。
    pub summary: String,
    /// 映射规则用的业务类型（`PURCHASE_IN`）。
    pub biz_type: String,
    /// 存货科目退化用的物料（= 本单**首个**物料，按 `material_id` 升序）。
    ///
    /// 保留这个字段是因为 [`resolve_accounts_with`] 仍需一个物料来做「规则没配 → 物料科目」
    /// 的退化；按物料拆行之后，每行的科目由 [`material_inventory_accounts`] 逐物料解析。
    pub material_id: Option<Uuid>,
    /// **按去重物料聚合的存货行**（P2-1b §4.2：粒度 `MATERIAL`，按 `material_id` 升序）。
    ///
    /// 空 = 回退到「单头一行 1405」（非 `STOCK_IN` 的调用方与历史数据）；
    /// 非空 = 每个物料一行存货分录，`entry_count = M + 2`（含税）/ `M + 1`（零税）。
    ///
    /// 取数只有两条路（口径逐字一致）：过账路径在手内存明细上聚合
    /// （`stockin_handler_post`），配置路径走 `stockin_query::material_lines_of` 的一条聚合 SQL。
    pub material_lines: Vec<VoucherMaterialLine>,
    /// 应付科目退化用的供应商。
    pub supplier_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub warehouse_id: Option<Uuid>,
}

impl VoucherBizCtx {
    /// **取数的唯一实现**：收货单单头 + 按物料聚合的明细 → 凭证上下文。
    ///
    /// 与拆分前逐字相同（金额 `round_dp(4)`、摘要格式、`biz_type = PURCHASE_IN`、
    /// 供应商 / 仓库辅助核算），只多了一件事：物料维度从「首个物料 + 种类数」换成
    /// **逐物料的 `(material_id, Σqty, Σamount, 行号区间)`**。
    ///
    /// `material_lines` 会先过一遍 [`VoucherMaterialLine::normalize`]（升序 + 同物料合并），
    /// 所以两条取数路径即使给的顺序不同，生成的分录也逐行一致（D3 影子对比的前提）。
    pub fn of_stock_in(header: &StockInRow, material_lines: Vec<VoucherMaterialLine>) -> Self {
        let material_lines = VoucherMaterialLine::normalize(material_lines);
        Self {
            source_type: SOURCE_STOCK_IN.to_string(),
            source_id: header.id,
            source_no: header.in_no.clone(),
            voucher_date: header.in_date,
            amounts: VoucherAmounts::of_stock_in(header),
            summary: format!("采购入库 {} 价税合计 {}", header.in_no, header.total_with_tax),
            biz_type: BIZ_TYPE_PURCHASE_IN.to_string(),
            material_id: material_lines.first().map(|line| line.material_id),
            material_lines,
            supplier_id: header.supplier_id,
            warehouse_id: Some(header.warehouse_id),
        }
    }

    /// 换一个记账日期（配置参数 `account_date` 的覆盖口径：只动日期，不动金额与摘要）。
    pub fn with_voucher_date(&self, date: NaiveDate) -> Self {
        let mut ctx = self.clone();
        ctx.voucher_date = date;
        ctx
    }
}

/// 模板（`biz_voucher_rule.rule_code`）是否启用：联动动作的第 3 层校验用。
///
/// 停用的模板 = 「target 不存在」→ 调用方跳过该动作（不是失败，更不能凭它生成凭证）。
pub async fn template_active(ex: &impl SqlExecutor, rule_code: &str) -> DbResult<bool> {
    Ok(load_rule_by_code(ex, rule_code).await?.is_some())
}

/// **按模板 / 上下文驱动的唯一凭证生成实现**（取数 → 取号 → 平衡校验 → 落库）。
///
/// # 参数
///
/// - `template_code`：`Some` = 用 `biz_voucher_rule.rule_code` 指定映射模板（配置的
///   `template_code` 参数）；`None` = 按上下文里的 `biz_type` 取默认模板 ——
///   与拆分前 `create_auto` 的行为**逐字相同**。
/// - `ctx`：取数结果（[`VoucherBizCtx`]）。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`（`Db` / `Transaction`），**不 begin / 不 commit**；
/// 调用方（过账事务 / 联动引擎的 `InTx` 动作）负责收口。
///
/// # 谁在调用
///
/// - [`create_for_stock_in`]（薄封装，收货过账 ⑦ 的旧路径）；
/// - `linkage::actions::generate_voucher`（配置路径，第 3 步接管）。
///
/// 两条路走同一个函数 ⇒ 凭证张数、编号、借贷行、金额不可能分叉。
pub async fn create_by_rule(
    ex: &impl SqlExecutor,
    template_code: Option<&str>,
    ctx: &VoucherBizCtx,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let rule = match template_code {
        Some(code) => Some(load_rule_by_code(ex, code).await?.ok_or_else(|| {
            DbError::InvalidValue(format!(
                "凭证生成失败：模板 {code} 未启用（biz_voucher_rule 里没有这条存活规则），\
                 单据 {} 无法生成凭证",
                ctx.source_no
            ))
        })?),
        None => load_rule(ex, &ctx.biz_type).await?,
    };
    let accounts = resolve_accounts_with(ex, rule.clone(), ctx.material_id, ctx.supplier_id).await?;

    // 税率 > 0 但系统里没有进项税科目 → 拦住（不许静默漏记进项税）
    if ctx.amounts.tax > Decimal::ZERO && accounts.tax.is_none() {
        return Err(account_missing(
            format!("进项税科目 {}", DEFAULT_TAX_CODE).as_str(),
            "本次入库有税额，但系统里没有可用的进项税科目",
        ));
    }

    // 逐物料的存货科目（一条 SQL 批取，P2-1b §4.4）：规则 debit → 该物料科目 → 1405。
    // 批取是硬要求：凭证在过账事务里生成（持着收货单行锁），N 次单查会把锁等待拖长。
    let material_accounts =
        material_inventory_accounts(ex, rule.as_ref().and_then(|r| r.debit.as_ref()), &ctx.material_lines).await?;
    if !ctx.material_lines.is_empty() {
        // 多物料单不再是「汇总行 + 物料留空」，而是逐物料出行 —— 这条日志让财务能按单号
        // 确认「这张单的存货成本按物料查得到 M 条」。
        tracing::info!(
            source_no = %ctx.source_no,
            material_count = ctx.material_lines.len(),
            "凭证按物料拆分录：{} 行存货分录（每个物料一行）",
            ctx.material_lines.len()
        );
    }
    let lines = purchase_in_lines(
        &accounts,
        ctx.amounts,
        &ctx.summary,
        LineContext {
            warehouse_id: ctx.warehouse_id,
            material_id: ctx.material_id,
            supplier_id: ctx.supplier_id,
        },
        &ctx.material_lines,
        &material_accounts,
        &ctx.source_no,
    )?;
    let (debit, credit) = totals_of(&lines);
    if lines.len() < 2 {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：单据 {} 的金额全为 0（分录只有 {} 行），会计上不构成一张凭证",
            ctx.source_no,
            lines.len()
        )));
    }
    if debit != credit {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：借贷不平衡（借 {debit} / 贷 {credit}），单据 {}",
            ctx.source_no
        )));
    }

    post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: &ctx.source_type,
            source_id: Some(ctx.source_id),
            source_no: Some(&ctx.source_no),
            source_doc_no: None,
            voucher_date: ctx.voucher_date,
            summary: ctx.summary.clone(),
            lines,
            remark: None,
            red_of: None,
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
}

// ==================== 并行对比：凭证快照 ====================

/// 一行分录的快照（对比用，只留「应该相同」的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherLineSnapshot {
    /// 行号。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目编码（名称可能因主数据改名而变，编码才是账的口径）。
    pub account_code: String,
    /// 借方。
    pub debit: Decimal,
    /// 贷方。
    pub credit: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<Uuid>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<Uuid>,
    /// 辅助核算：部门。
    pub aux_dept_id: Option<Uuid>,
}

/// 一张凭证的逐字段快照 —— 「旧路径发的凭证」与「配置发的凭证」的对比物。
///
/// 【D3】SHADOW 阶段的判据就是它：[`compare_snapshots`] 返回空 = 逐字段一致。
/// 字段选择口径：凡是会在报表 / 下钻 / 审计里被看到的东西都进快照
/// （字号、期间、来源、借贷合计、行数、逐行科目与金额、辅助核算），
/// 只有 id 类主键与时间戳不比（换个事务就必然不同）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherSnapshot {
    /// 同一来源单据下的凭证张数（正常应为 1；红冲后为 2）。
    pub voucher_count: i64,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 凭证类型（`AUTO`）。
    pub voucher_type: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Option<Uuid>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 会计期间。
    pub period: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 摘要。
    pub summary: String,
    /// 借方合计。
    pub total_debit: Decimal,
    /// 贷方合计。
    pub total_credit: Decimal,
    /// 分录行数。
    pub entry_count: i32,
    /// 是否红字。
    pub is_red: i16,
    /// 逐行分录。
    pub lines: Vec<VoucherLineSnapshot>,
}

/// 取一张凭证的快照（读主表 + 分录 + 同一来源的凭证张数）。
pub async fn snapshot_of(ex: &impl SqlExecutor, voucher_id: Uuid) -> DbResult<VoucherSnapshot> {
    let Some(row) = find_voucher(ex, voucher_id).await? else {
        return Err(DbError::InvalidValue(format!("凭证快照失败：凭证 {voucher_id} 不存在")));
    };
    let voucher_count = match (row.source_type.as_str(), row.source_id) {
        (source_type, Some(source_id)) => {
            // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）；这里只数同一来源单据的凭证张数。
            ex.scalar_sql(
                format!(
                    "SELECT count(*) FROM {TABLE} \
                      WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL"
                ),
                vec![Value::Text(source_type.to_string()), Value::from(&source_id)],
            )
            .await?
        }
        _ => 1,
    };
    let lines = entries_of(ex, voucher_id)
        .await?
        .into_iter()
        .map(|entry| VoucherLineSnapshot {
            line_no: entry.line_no,
            summary: entry.summary,
            account_code: entry.account.code,
            debit: entry.debit,
            credit: entry.credit,
            aux_supplier_id: entry.aux_supplier_id,
            aux_material_id: entry.aux_material_id,
            aux_warehouse_id: entry.aux_warehouse_id,
            aux_dept_id: entry.aux_dept_id,
        })
        .collect();
    Ok(VoucherSnapshot {
        voucher_count,
        voucher_word: row.voucher_word,
        voucher_no: row.voucher_no,
        voucher_type: row.voucher_type,
        source_type: row.source_type,
        source_id: row.source_id,
        source_no: row.source_no,
        period: row.period,
        voucher_date: row.voucher_date,
        summary: row.summary,
        total_debit: row.total_debit,
        total_credit: row.total_credit,
        entry_count: row.entry_count,
        is_red: row.is_red,
        lines,
    })
}

/// 逐字段对比两张凭证快照，返回**不一致的字段名**（空 = 逐字段一致）。
///
/// 这是 SHADOW 阶段「并行跑一段、对比一致再切换」的判据：日志里 `matched` / `diffs`
/// 是可 grep 的事实，不是口头保证。
pub fn compare_snapshots(configured: &VoucherSnapshot, legacy: &VoucherSnapshot) -> Vec<&'static str> {
    let mut diffs: Vec<&'static str> = Vec::new();
    if configured.voucher_count != legacy.voucher_count {
        diffs.push("voucher_count");
    }
    if configured.voucher_word != legacy.voucher_word {
        diffs.push("voucher_word");
    }
    if configured.voucher_no != legacy.voucher_no {
        diffs.push("voucher_no");
    }
    if configured.voucher_type != legacy.voucher_type {
        diffs.push("voucher_type");
    }
    if configured.source_type != legacy.source_type {
        diffs.push("source_type");
    }
    if configured.source_id != legacy.source_id {
        diffs.push("source_id");
    }
    if configured.source_no != legacy.source_no {
        diffs.push("source_no");
    }
    if configured.period != legacy.period {
        diffs.push("period");
    }
    if configured.voucher_date != legacy.voucher_date {
        diffs.push("voucher_date");
    }
    if configured.summary != legacy.summary {
        diffs.push("summary");
    }
    if configured.total_debit != legacy.total_debit {
        diffs.push("total_debit");
    }
    if configured.total_credit != legacy.total_credit {
        diffs.push("total_credit");
    }
    if configured.entry_count != legacy.entry_count {
        diffs.push("entry_count");
    }
    if configured.is_red != legacy.is_red {
        diffs.push("is_red");
    }
    if configured.lines != legacy.lines {
        diffs.push("lines");
    }
    diffs
}

/// 批量写分录（单条语句，行号唯一键冲突即失败）。
async fn write_entries(ex: &impl SqlExecutor, voucher_id: Uuid, lines: &[EntryLine]) -> DbResult<()> {
    for line in lines {
        // SCOPE-EXEMPT: 凭证分录无归属列（见模块文档）。
        ex.exec_sql(
            format!(
                "INSERT INTO {ENTRY_TABLE} \
                    (voucher_id, line_no, summary, account_id, account_code, account_name, \
                     debit_amount, credit_amount, aux_supplier_id, aux_material_id, aux_warehouse_id, \
                     aux_dept_id) \
                 VALUES (?::uuid, ?, ?, ?::uuid, ?, ?, NULLIF(?::text,'')::numeric, \
                         NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::uuid)"
            ),
            vec![
                Value::from(&voucher_id),
                Value::from(line.line_no),
                Value::Text(line.summary.clone()),
                Value::from(&line.account.id),
                Value::Text(line.account.code.clone()),
                Value::Text(line.account.name.clone()),
                Value::Text(line.debit.to_string()),
                Value::Text(line.credit.to_string()),
                Value::Text(line.aux_supplier_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(line.aux_material_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(line.aux_warehouse_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(line.aux_dept_id.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 覆盖一张凭证的全部分录（期初凭证「覆盖式更新」专用）。
///
/// 会计上凭证一经记账不可改，唯一例外是**期初凭证**：同一期间只有一张，重复录入是
/// 「把这张凭证的分录整组换掉」，凭证号保持不变（卡面 §三 硬红线 2）。
/// 删除 + 重写放在同一个函数里，避免调用方只做了一半（删了老分录、新分录没写进去）。
pub async fn replace_entries(ex: &impl SqlExecutor, voucher_id: Uuid, lines: &[EntryLine]) -> DbResult<()> {
    ex.exec_sql(
        format!("DELETE FROM {ENTRY_TABLE} WHERE voucher_id = ?::uuid"),
        vec![Value::from(&voucher_id)],
    )
    .await?;
    write_entries(ex, voucher_id, lines).await
}

/// 凭证摘要里的**实体显示名**（AGENTS.md 约定 13：摘要里不许出现 UUID）。
///
/// 名称查到就用名称；主数据已被物理删除 / 名字是空白 → 退化成
/// `客户已删除(01a1bdda)`（只留 8 位短 id 方便工程定位，**绝不写整段 36 位 UUID**：
/// 摘要会呈现给财务和审计，UUID 在那里既读不懂也搜不到）。
pub fn party_label(name: Option<String>, kind: &str, id: Uuid) -> String {
    match name.map(|text| text.trim().to_string()).filter(|text| !text.is_empty()) {
        Some(name) => name,
        None => {
            let short: String = id.to_string().chars().take(8).collect();
            format!("{kind}已删除({short})")
        }
    }
}

/// 落一张凭证的全部入参（自动 / 手工 / 红字**共用同一个写入口**）。
#[derive(Debug, Clone)]
pub struct PostVoucher<'a> {
    /// 凭证类型（`AUTO` / `MANUAL`）。
    pub voucher_type: &'a str,
    /// 来源单据类型（`STOCK_IN` / `PAYMENT` / `MANUAL`）。
    pub source_type: &'a str,
    /// 来源单据 id（手工凭证为 `None`）。
    pub source_id: Option<Uuid>,
    /// 来源单号（手工凭证为 `None`）。
    pub source_no: Option<&'a str>,
    /// **业务单据号**（`biz_voucher.source_doc_no`，E2E-FIX-5 ②）。
    ///
    /// 语义与 [`Self::source_no`] 不同：
    /// * `source_no` = 「`source_id` 那张单的单号」= **记账依据 / 子单据**；
    /// * `source_doc_no` = 「人对外的业务单号」= 用户拿来查凭证的那个号。
    ///
    /// 大多数单据两者相同（收款单的 source_no 就是收款单号），传 `None` 即可 ——
    /// 落库时按 `COALESCE(source_doc_no, source_no)` 兜底。只有「一张业务单据派生多张
    /// 子单据 → 多张凭证」的**退货**场景才需要显式传（两张凭证的 source_no 分别是入库单号 /
    /// 红字发票号，但 source_doc_no 都必须是退货单号，否则按退货单号查凭证 0 行）。
    pub source_doc_no: Option<&'a str>,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证摘要。
    pub summary: String,
    /// 分录（≥ 2 行、借贷必须平衡）。
    pub lines: Vec<EntryLine>,
    /// 备注。
    pub remark: Option<&'a str>,
    /// `Some(原凭证 id)` → 落红字凭证（`is_red = 1`、`reverse_of = 原凭证`、走 `PZR` 独立号段）。
    pub red_of: Option<Uuid>,
    /// 落库的初始状态：`POSTED`（自动凭证 / 红冲）/ `DRAFT`（手工凭证，P0-FIX-4A）。
    ///
    /// 只有 [`STATUS_POSTED`] 与 [`STATUS_DRAFT`] 两个合法取值（`AUDITED` 不能直接落库：
    /// 它表示「有人复核过」，而落库这一刻还没有复核这回事）。取值由本函数校验，
    /// 不在「调用点各自写字符串」的口子上留余地。
    pub status: &'a str,
}

/// 落一张凭证 + 它的分录（**不 commit**，由调用方的事务收口）。
///
/// 三条写路径（收货自动凭证 / 付款凭证 / 手工凭证）与红冲**共用这一个函数**：
/// 校验（≥ 2 行、借贷平衡）与落库（主表 + 分录 + 取号）只有一份实现，
/// 不存在「某条路径忘了校验平衡」的可能。
pub async fn post_voucher(
    ex: &impl SqlExecutor,
    req: &PostVoucher<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    if req.lines.len() < 2 {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：至少要 2 条分录（当前 {} 行），会计上不构成一张凭证",
            req.lines.len()
        )));
    }
    // 初始状态白名单（P0-FIX-4A）：落库只允许「草稿」或「已记账」两种起点。
    // `AUDITED` 落库意味着「没人审核却显示已审核」，直接把双人复核掏空，所以拒绝。
    if req.status != STATUS_POSTED && req.status != STATUS_DRAFT {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：落库状态只能是 {STATUS_DRAFT}（手工凭证待复核）或 {STATUS_POSTED}\
             （自动凭证 / 红字凭证直接记账），收到「{}」",
            req.status
        )));
    }
    let (debit, credit) = totals_of(&req.lines);
    if debit != credit {
        let diff = debit - credit;
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：借贷不平衡（借 {debit} / 贷 {credit}，差额 {diff}）"
        )));
    }
    if debit <= Decimal::ZERO {
        return Err(DbError::InvalidValue(
            "凭证生成失败：借贷合计为 0，金额全为 0 的单据不构成一张凭证".to_string(),
        ));
    }

    // 记账科目口径：**只允许末级科目**（模块文档「记账科目口径」）。
    //
    // 放在借贷平衡之后、期间检查之前：科目不合法是**参数错误**（400），与期间开闭无关；
    // 而且这一闸必须覆盖自动凭证与手工凭证的全部入口 —— `post_voucher` 就是那个唯一收口。
    let non_leaf = non_leaf_codes(ex, &req.lines).await?;
    if !non_leaf.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "凭证生成失败：科目 {} 是非末级（汇总）科目，不能直接记账 —— 记账、报表与年结\
             都按末级科目结算，记到上级会让余额被漏结（跨年试算不平）。请改记它下面的末级明细科目",
            non_leaf.join("、")
        )));
    }

    let period = period_of(ex, req.voucher_date).await?;
    // ERP-P1（卡面 §一 ②「结账后」）：已结账的期间**只读**。
    // 闸放在这里而不是各业务入口，是因为自动凭证（收货 / 应付 / 付款）与手工凭证
    // 都走 `post_voucher` —— 在唯一收口拦一次，就没有「某条路径忘了查期间」的可能。
    // 语义是 400（参数/状态不允许这次操作），由各调用点的 write_error 翻成
    // 400 + 原文；红冲的 409 前置闸在 `reverse_posted` 里，比这里更早生效。
    if period_status(ex, &period)
        .await?
        .as_deref()
        .is_some_and(|status| status == PERIOD_CLOSED)
    {
        // 唯一的例外：**年末损益结转凭证**（ERP-R4）。
        //
        // 年结的前置条件就是「本年 12 个月全部结账」，所以结转凭证要落的 12 月
        // 必然是 CLOSED —— 这不是绕闸，是设计决议（卡面 §四 ⚠️ 的第二个选项）：
        // 结转凭证的 period 允许写入已结账期间，而白名单在库层
        // （`ck_biz_voucher_type` 只放 AUTO|MANUAL|OPENING|YEAR_END），
        // 只有 `year_end` 模块会传这个类型。反年结会把这张凭证整张删掉，
        // 12 月重新解封前账务是一致的。
        if req.voucher_type != VOUCHER_TYPE_YEAR_END || req.source_type != SOURCE_TYPE_YEAR_END {
            return Err(DbError::InvalidValue(format!(
                "会计期间 {period} 已结账，不能再生成凭证（凭证日期 {}）：\
                 已封账的月份只读，请在下一个未结账的期间做调整（或先反结账）",
                req.voucher_date
            )));
        }
        tracing::info!(
            period = %period,
            "年末损益结转凭证写入已结账期间（YEAR_END 的既定例外）"
        );
    }
    let voucher_no = match req.red_of {
        Some(_) => next_red_voucher_no(ex, now).await?,
        None => next_voucher_no(ex, now).await?,
    };
    let voucher_no_text = voucher_no.clone();
    let entry_count = i32::try_from(req.lines.len()).unwrap_or(0);
    let is_red = if req.red_of.is_some() { IS_RED_YES } else { IS_RED_NO };
    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
    // 落库即记账（POSTED）才盖「记账时间 / 记账人」戳；草稿（DRAFT）两列留 NULL ——
    // 否则列表 / 报表里会出现「草稿却有记账人」的自相矛盾行，审计第一个问的就是这个。
    let posted = req.status == STATUS_POSTED;
    let posted_at = if posted { Value::from(now) } else { Value::Null };
    let posted_by = if posted {
        Value::Text(operator_text.clone())
    } else {
        Value::Null
    };

    // 业务单据号（E2E-FIX-5 ②）：显式给的优先（退货过账传退货单号），否则等同来源单号。
    // 两者相同的单据占绝大多数 —— 单独一列是为了「按业务单号查凭证」这条查询面永远可用。
    let source_doc_no = req.source_doc_no.or(req.source_no);

    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    let inserted = ex
        .query_first_sql(
            format!(
                "INSERT INTO {TABLE} \
                    (voucher_word, voucher_no, period, voucher_date, voucher_type, source_type, \
                     source_id, source_no, source_doc_no, summary, total_debit, total_credit, entry_count, \
                     status, is_red, reverse_of, posted_at, posted_by, remark, \
                     created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?, ?, NULLIF(?::text,'')::date, ?, ?, NULLIF(?::text,'')::uuid, ?, \
                         NULLIF(?::text,''), ?, \
                         NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?, ?, ?, \
                         NULLIF(?::text,'')::uuid, ?::timestamptz, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                         ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
                 RETURNING id"
            ),
            vec![
                Value::Text(WORD_DEFAULT.to_string()),
                Value::Text(voucher_no),
                Value::Text(period),
                Value::Text(req.voucher_date.to_string()),
                Value::Text(req.voucher_type.to_string()),
                Value::Text(req.source_type.to_string()),
                Value::Text(req.source_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(req.source_no.unwrap_or_default().to_string()),
                Value::Text(source_doc_no.unwrap_or_default().to_string()),
                Value::Text(req.summary.clone()),
                Value::Text(debit.to_string()),
                Value::Text(credit.to_string()),
                Value::from(entry_count),
                Value::Text(req.status.to_string()),
                Value::I16(is_red),
                Value::Text(req.red_of.map(|v| v.to_string()).unwrap_or_default()),
                posted_at,
                posted_by,
                Value::Text(req.remark.unwrap_or_default().to_string()),
                Value::from(now),
                Value::from(now),
                Value::Text(operator_text.clone()),
                Value::Text(operator_text),
            ],
        )
        .await?;
    let voucher_id: Uuid = match inserted.as_ref() {
        Some(row) => row.get_typed("id")?,
        None => {
            return Err(DbError::InvalidValue(format!(
                "凭证写入失败：INSERT 没有返回 id（{}）",
                req.source_no.unwrap_or("手工凭证")
            )));
        }
    };

    write_entries(ex, voucher_id, &req.lines).await?;
    // P10 铁律 ③：凭证建单留痕（`NULL → DRAFT` / `NULL → POSTED`）。
    // 凭证的「原地状态流转」只有 P0-FIX-4A 的三条边（审核 / 反审核 / 记账），
    // 建单本身不是流转，所以这里只留「谁在什么时候、以什么状态落了这张凭证」。
    //
    // ARCH-3 DC-01：红字凭证的日志 reason 带上**红冲原因**（`红字冲销凭证｜<原因>`）——
    // 红冲没有状态流转可写，"为什么这次红冲没走复核"（显式跳过审批）就靠这段文字留痕；
    // 蓝字凭证仍为 `None`（建单不是业务动作，reason 无意义）。
    let log_reason = match req.red_of {
        None => None,
        Some(_) => {
            let own = req.remark.map(str::trim).filter(|text| !text.is_empty());
            Some(match own {
                Some(text) => format!("红字冲销凭证｜{text}"),
                None => "红字冲销凭证".to_string(),
            })
        }
    };
    state_machine::log_created(
        ex,
        state_machine::ENTITY_VOUCHER,
        voucher_id,
        &voucher_no_text,
        req.status,
        operator,
        log_reason.as_deref(),
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok(voucher_id)
}

/// 红冲入参。
#[derive(Debug, Clone, Copy)]
pub struct ReversalInput<'a> {
    /// 红冲原因（写进红字凭证分录摘要与备注）。
    pub reason: Option<&'a str>,
    /// 红字凭证日期（业务口径：作废 / 更正当天）。
    pub voucher_date: NaiveDate,
}

/// 红冲一张**已记账**凭证：生成红字凭证（借贷互换）+ 回写原凭证 `reversed_by`。
///
/// # 校验（卡面 §二 ②）
///
/// ```text
/// ① status 必须 POSTED
/// ② 本身是红字凭证（is_red = 1）的不能再红冲
/// ③ 已红冲过（reversed_by 非空）的拒绝，文案点名红字凭证号
/// ④ 所属期间已结账（CLOSED）的拒绝
/// ```
///
/// 原凭证 `status` **保持 `POSTED`**（与 P2-C1 同口径），只打 `reversed_by`。
pub async fn reverse_posted(
    ex: &impl SqlExecutor,
    original: &VoucherRow,
    input: ReversalInput<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    if original.is_red == IS_RED_YES {
        return Err(DbError::InvalidValue(format!(
            "红冲失败：凭证 {} 本身是红字凭证，红字凭证不能再红冲（要更正请直接补一张蓝字凭证）",
            original.voucher_no
        )));
    }
    if original.status != STATUS_POSTED {
        return Err(DbError::InvalidValue(format!(
            "红冲失败：凭证 {} 当前状态是 {}，只有已记账（POSTED）的凭证能红冲",
            original.voucher_no, original.status
        )));
    }
    if let Some(red_id) = original.reversed_by {
        let red_no = find_voucher(ex, red_id)
            .await?
            .map(|row| row.voucher_no)
            .unwrap_or_else(|| red_id.to_string());
        return Err(DbError::InvalidValue(format!(
            "红冲失败：该凭证 {} 已经红冲（红字凭证 {red_no}），不能重复红冲",
            original.voucher_no
        )));
    }
    if is_period_closed(ex, &original.period).await? {
        return Err(DbError::InvalidValue(format!(
            "红冲失败：凭证 {} 所属期间 {} 已结账，不能再红冲（请先反结账）",
            original.voucher_no, original.period
        )));
    }

    let entries = entries_of(ex, original.id).await?;
    if entries.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "红冲失败：原凭证 {} 没有任何分录",
            original.voucher_no
        )));
    }
    let lines = reversed_lines(&entries, input.reason);
    let summary = format!("红冲凭证 {} 借贷合计 {}", original.voucher_no, original.total_debit);

    let red_id = post_voucher(
        ex,
        &PostVoucher {
            voucher_type: &original.voucher_type,
            source_type: &original.source_type,
            source_id: original.source_id,
            source_no: original.source_no.as_deref(),
            // 红字凭证继承原凭证的业务单据号：红冲一张退货凭证后，按退货单号仍能查到两张。
            source_doc_no: original.source_doc_no.as_deref(),
            voucher_date: input.voucher_date,
            summary,
            lines,
            remark: input.reason.map(str::trim).filter(|text| !text.is_empty()),
            red_of: Some(original.id),
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await?;

    // 原凭证保持 POSTED（卡面 §二 ②），只打反向指针
    // SCOPE-EXEMPT: 凭证台账无归属列（见模块文档）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET reversed_by = ?::uuid, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&red_id),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&original.id),
        ],
    )
    .await?;

    Ok(red_id)
}

/// 收货单过账时生成蓝字凭证（卡面 §三 ②③④）。
///
/// # 【D3】薄封装
///
/// 「取数 → 取号 → 借贷平衡 → 落库」已经全部在 [`create_by_rule`]；
/// 本函数只做一件事：把**内存里已经加锁的收货单头**转成 [`VoucherBizCtx`]
/// （走 [`VoucherBizCtx::of_stock_in`]，与配置路径同一份取数），然后调它。
///
/// 留在原地的原因是「调用点语义」：过账路径手上有锁好的单头，不该为此再查一次库；
/// 而配置路径（联动动作）手上只有单据 id，走 `stockin_model::voucher_biz_ctx` 取数。
/// 两条路上的**算法只有一份**。
pub async fn create_for_stock_in(
    ex: &impl SqlExecutor,
    header: &StockInRow,
    material_lines: Vec<VoucherMaterialLine>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let ctx = VoucherBizCtx::of_stock_in(header, material_lines);
    create_by_rule(ex, None, &ctx, operator, now).await
}

/// 某个用户**当前**所属部门（`sys_user.dept_id`；用户不存在 / 已软删 / 没配部门 → `None`）。
///
/// # 为什么凭证的部门从这里取（DC-11 的「部门从哪来」）
///
/// `biz_payment` 只有 `create_by`（付款登记人 = 付款凭证的申请人），没有 `dept_id` 列；
/// 系统里唯一有权威部门的是 `sys_user.dept_id`。所以「付款凭证的部门」= **申请人所属部门**。
///
/// 这是一份**快照**，与 `biz_task.dept_id`（"创建时负责人所属部门"）同口径：
/// 人调部门后，历史凭证的部门辅助账不跟着漂移 —— 凭证是审计证据，不允许事后变。
///
/// # 为什么查不到就返回 None 而不是报错
///
/// 部门辅助核算是**增强维度**，不是记账前提。系统账号（AI 用户 / 集成账号）本来
/// 可能没有部门，为此拒绝付款入账是错的；留空 + 由调用方打 WARN 即可。
pub async fn user_dept_of(ex: &impl SqlExecutor, user_id: Uuid) -> DbResult<Option<Uuid>> {
    // SCOPE-EXEMPT: 按主键查单个用户的部门（登录主体已知），不产出用户可见行集。
    let row = ex
        .query_first_sql(
            "SELECT dept_id FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&user_id)],
        )
        .await?;
    match row.as_ref().map(|row| row.get_typed::<Option<Uuid>>("dept_id")) {
        Some(Ok(dept_id)) => Ok(dept_id),
        Some(Err(err)) => Err(err),
        None => Ok(None),
    }
}

// ==================== 写：序列号侧三种「无来源」库存变动 ====================

/// 序列号的三种库存变动（ADR-20261031 §6 R1，规格 v3 §3.10）。
///
/// 三者的共同点是「没有供应商、没有采购单」——借方必须是成本类 / 资产类科目而不是应付，
/// 所以既不能复用 [`create_auto`]（它按 `biz_voucher_rule` 走采购入库映射），
/// 也不该在调用点各写一遍分录：本枚举 + [`create_for_serial_movement`] 是**唯一写入口**。
#[derive(Debug, Clone, Copy)]
pub enum SerialMovement<'a> {
    /// 期初 / 其他入库（批量生成）：借 `1405 库存商品` / 贷 `1901 待处理财产损溢`。
    Inbound {
        /// 物料。
        material_id: Uuid,
        /// 仓库。
        warehouse_id: Uuid,
        /// 本次生成件数（只进摘要，金额由调用方按账上差额传入）。
        count: i64,
    },
    /// 出库（序列号发货）：借 `6401 主营业务成本` / 贷 `1405 库存商品`。
    Outbound {
        /// 物料。
        material_id: Uuid,
        /// 仓库。
        warehouse_id: Uuid,
        /// 序列号（进摘要与来源单号）。
        serial_no: &'a str,
    },
    /// 报废：借 `1901 待处理财产损溢` / 贷 `1405 库存商品`。
    Scrapped {
        /// 物料。
        material_id: Uuid,
        /// 仓库。
        warehouse_id: Uuid,
        /// 序列号（进摘要与来源单号）。
        serial_no: &'a str,
    },
}

/// 序列号库存变动 → 凭证（**同事务**；`amount` 为账上真实差额，不在本函数重算）。
///
/// # 为什么金额必须由调用方传入
///
/// 生成侧 = `apply_cost` 的 `after.amount − before.amount`（该桶的实际增量）；
/// 出库 / 报废侧 = 仓级 `before.amount − after.amount`。两处都是 `numeric(18,4)`，
/// 谁都不该在 Rust 里再乘一次 `N × unit_cost` —— 4 位舍入一错，
/// `erp_invariants.py` 的 B2（容差 `0.0001`）就会报 0.00x 的差。
///
/// # 差额 ≤ 0 直接跳过（不是错误）
///
/// 改判前生成的历史号是零成本（`qty>0 / avg_cost=0 / amount=0`），它们出库 / 报废时
/// 差额恰好为 0。`post_voucher` 在 `借贷合计 <= 0` 时会报错（那是对**单据**的护栏），
/// 所以这里提前 `Ok(None)` 并打 WARN 点名 —— 这是「存量不补账」的过渡口径
/// （规格 v3 §8.2），财务可以按这条日志判断要不要补账。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`（`Db` / `Transaction`），由调用方开事务；本函数不 `begin`
/// 不 `commit`，也绝不出现「事务里再开事务」。
pub async fn create_for_serial_movement(
    ex: &impl SqlExecutor,
    kind: SerialMovement<'_>,
    doc_id: Uuid,
    doc_no: &str,
    amount: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let amount = amount.round_dp(4);
    let (material_id, warehouse_id, source_type, summary, debit_side) = match kind {
        SerialMovement::Inbound {
            material_id,
            warehouse_id,
            count,
        } => (
            material_id,
            warehouse_id,
            SOURCE_SERIAL_BATCH,
            format!("期初入库：序列号批量生成 {count} 件"),
            DebitSide::Inventory,
        ),
        SerialMovement::Outbound {
            material_id,
            warehouse_id,
            serial_no,
        } => (
            material_id,
            warehouse_id,
            SOURCE_SERIAL_OUT,
            format!("序列号出库结转成本：{serial_no}"),
            DebitSide::Counterpart(ACCOUNT_MAIN_COST, "主营业务成本"),
        ),
        SerialMovement::Scrapped {
            material_id,
            warehouse_id,
            serial_no,
        } => (
            material_id,
            warehouse_id,
            SOURCE_SERIAL_SCRAP,
            format!("序列号报废损失：{serial_no}"),
            DebitSide::Counterpart(ACCOUNT_PENDING_GAIN_LOSS, "待处理财产损溢"),
        ),
    };
    if amount <= Decimal::ZERO {
        tracing::warn!(
            material_id = %material_id,
            warehouse_id = %warehouse_id,
            doc_no,
            amount = %amount,
            source_type,
            "序列号变动金额为 0（历史零成本存量，ADR 口径不补账）：跳过凭证生成，只有流水没有分录"
        );
        return Ok(None);
    }

    let inventory = account_required(ex, DEFAULT_INVENTORY_CODE, "库存商品").await?;
    // 存货行带「物料 + 仓库」辅助核算（账上能按仓按料拆开看）；对方科目只带物料 ——
    // 1901 / 6401 是全公司口径的过渡 / 损益科目，挂仓库会把报表拆碎。
    let inventory_aux_warehouse = Some(warehouse_id);
    let lines = match debit_side {
        DebitSide::Inventory => {
            let counterpart = account_required(ex, ACCOUNT_PENDING_GAIN_LOSS, "待处理财产损溢").await?;
            vec![
                EntryLine {
                    line_no: 1,
                    summary: summary.clone(),
                    account: inventory,
                    debit: amount,
                    credit: Decimal::ZERO,
                    aux_supplier_id: None,
                    aux_dept_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: inventory_aux_warehouse,
                },
                EntryLine {
                    line_no: 2,
                    summary: summary.clone(),
                    account: counterpart,
                    debit: Decimal::ZERO,
                    credit: amount,
                    aux_dept_id: None,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: None,
                },
            ]
        }
        DebitSide::Counterpart(code, name) => {
            let counterpart = account_required(ex, code, name).await?;
            vec![
                EntryLine {
                    line_no: 1,
                    summary: summary.clone(),
                    account: counterpart,
                    debit: amount,
                    aux_dept_id: None,
                    credit: Decimal::ZERO,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: None,
                },
                EntryLine {
                    line_no: 2,
                    summary: summary.clone(),
                    account: inventory,
                    aux_dept_id: None,
                    debit: Decimal::ZERO,
                    credit: amount,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: inventory_aux_warehouse,
                },
            ]
        }
    };

    let voucher_id = post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type,
            source_id: Some(doc_id),
            source_no: Some(doc_no),
            source_doc_no: None,
            voucher_date: today_cst(now),
            summary,
            lines,
            remark: None,
            red_of: None,
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await?;
    Ok(Some(voucher_id))
}

/// 借方落在哪一侧（决定分录两行的科目配对）。
#[derive(Debug, Clone, Copy)]
enum DebitSide {
    /// 借存货、贷对方科目（期初入库）。
    Inventory,
    /// 借对方科目、贷存货（出库 / 报废）。
    Counterpart(&'static str, &'static str),
}

// ==================== 写：出库单成本结转凭证（ARCH-3 DC-13） ====================

/// 出库单过账 → 蓝字凭证的入参（一张单**一张**凭证；行数按物料拆，P2-1b §4.3）。
///
/// 打包成结构体是必须的：`post_voucher` 之外还有 9 个业务字段，
/// 摊平会直接撞 clippy 的 `too_many_arguments`（-D warnings 下是硬错）。
#[derive(Debug, Clone)]
pub struct StockOutVoucher<'a> {
    /// 出库单 id（`biz_voucher.source_id`）。
    pub doc_id: Uuid,
    /// 出库单号。
    pub doc_no: &'a str,
    /// 凭证日期（= 出库单的 `out_date`，决定会计期间）。
    pub voucher_date: NaiveDate,
    /// 出库类型（`ISSUE` / `SCRAP` / `LOSS` / `OTHER`，进摘要）。
    pub out_type: &'a str,
    /// 借方科目编码（报废·盘亏 1901 / 其余 6401）。
    pub debit_code: &'a str,
    /// 借方科目名（写进分录摘要，便于人直接看懂）。
    pub debit_name: &'a str,
    /// **按物料聚合的出库行**（P2-1b：粒度 `MATERIAL`，`amount` = 该物料账上真实扣减金额）。
    ///
    /// 非空 ⇒ 借成本科目 `M` 行 + 贷 `1405` `M` 行（`2M` 行），每行带该行物料的辅助核算；
    /// 空 ⇒ 回退到「借 1 行 + 贷 1 行」的汇总行（历史行为），此时用 [`Self::aux_material_id`]。
    pub material_lines: Vec<VoucherMaterialLine>,
    /// **回退分支**（`material_lines` 为空）的存货行物料辅助核算值。
    ///
    /// 按物料拆分录时每行的物料来自 [`Self::material_lines`]，本字段不参与。
    pub aux_material_id: Option<Uuid>,
    /// 出库仓库（存货行的 `aux_warehouse_id`：账上能按仓拆开看）。
    pub warehouse_id: Uuid,
    /// **账上真实差额**（Σ 各明细 `biz_inventory.amount` 的变动量），
    /// 由调用方从 `RETURNING` 的 before / after 算出，本函数**绝不重算** `qty × avg`。
    pub amount: Decimal,
    /// 凭证摘要。
    pub summary: String,
}

/// 出库单过账时生成蓝字凭证：借 6401 / 1901，贷 1405。
///
/// # 金额口径（硬红线）
///
/// `amount` 必须是**账上真实差额**（`before.amount − after.amount` 的合计），
/// 不是 `Σ qty × avg_cost`。理由与序列号出库同一条（见
/// [`create_for_serial_movement`]）：多桶 / 4 位舍入下两者不相等，
/// 拿重算值去贷 1405 会让 `erp_invariants.py` 的 B2（库存金额 = 存货科目余额）
/// 报出 0.00x 的差 —— 那是 BLOCKER。
///
/// # 差额 ≤ 0 直接跳过（不是错误）
///
/// 零成本存量（`qty>0 / avg_cost=0 / amount=0`）出库时差额恰好为 0；
/// `post_voucher` 在 `借贷合计 <= 0` 时会报错（那是对**单据**的护栏），
/// 所以这里提前 `Ok(None)` 并打 WARN 点名单据 —— 财务按这条日志判断要不要补账。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`（`Db` / `Transaction`），由调用方开事务；不 `begin` 不 `commit`。
pub async fn create_for_stock_out(
    ex: &impl SqlExecutor,
    req: &StockOutVoucher<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let amount = req.amount.round_dp(4);
    if amount <= Decimal::ZERO {
        tracing::warn!(
            doc_no = req.doc_no,
            doc_id = %req.doc_id,
            amount = %amount,
            out_type = req.out_type,
            "出库单成本差额为 0（零成本存量 / 账面金额已为 0）：跳过凭证生成，只动库存与流水"
        );
        return Ok(None);
    }

    let inventory = account_required(ex, DEFAULT_INVENTORY_CODE, "库存商品").await?;
    let debit = account_required(ex, req.debit_code, req.debit_name).await?;
    // 按物料拆（P2-1b §4.3）：`M` 个物料 ⇒ 借成本科目 `M` 行 + 贷 1405 `M` 行。
    // 金额 ≤ 0 的物料不出行（零成本存量的物料不占分录，数量仍在库存流水里）。
    let materials = VoucherMaterialLine::normalize(req.material_lines.iter().cloned());
    let positives: Vec<VoucherMaterialLine> = materials
        .into_iter()
        .filter(|line| line.amount > Decimal::ZERO)
        .collect();
    let lines = if positives.is_empty() {
        // ---- 回退：借 1 行 + 贷 1 行（历史行为，`aux_material_id` 只在此时参与）----
        vec![
            EntryLine {
                line_no: 1,
                summary: req.summary.clone(),
                account: debit,
                debit: amount,
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: req.aux_material_id,
                // 借方（成本 / 待处理损溢）是全公司口径的科目，不挂仓库辅助核算。
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
            EntryLine {
                line_no: 2,
                summary: req.summary.clone(),
                account: inventory,
                debit: Decimal::ZERO,
                credit: amount,
                aux_supplier_id: None,
                aux_material_id: req.aux_material_id,
                aux_warehouse_id: Some(req.warehouse_id),
                aux_dept_id: None,
            },
        ]
    } else {
        // 逐物料金额 = 该物料的账上真实扣减额；Σ 与单头金额的尾差吸收到最后一行。
        // 出库侧单头就是逐行累加出来的（`stockout_handler` 的 `total_amount += applied.amount`），
        // 所以这里的 `diff` 恒为 0；非 0 说明有非 API 写入，`absorb_tail_diff` 会 WARN 点名。
        let amounts_of_materials: Vec<Decimal> = positives.iter().map(|line| line.amount).collect();
        let split = absorb_tail_diff(req.doc_no, amount, &amounts_of_materials)?;
        // 摘要里的物料编码：一条 SQL 批取（取不到 → id 前 8 位），不让每行各查一次
        let ids: Vec<Uuid> = positives.iter().map(|line| line.material_id).collect();
        let codes = material_codes(ex, &ids).await?;
        let mut lines: Vec<EntryLine> = Vec::with_capacity(positives.len() * 2);
        // 借：成本科目（6401 / 1901），每物料一行，**不挂仓库**
        for (line, material_amount) in positives.iter().zip(split.iter().copied()) {
            let label = material_label(&codes, line.material_id);
            lines.push(EntryLine {
                line_no: 0,
                summary: format!(
                    "{} {}（{}）",
                    req.summary,
                    label,
                    line_range_text(line.line_from, line.line_to)
                ),
                account: debit.clone(),
                debit: material_amount,
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: Some(line.material_id),
                // 借方（成本 / 待处理损溢）是全公司口径的科目，不挂仓库辅助核算。
                aux_warehouse_id: None,
                aux_dept_id: None,
            });
        }
        // 贷：1405 库存商品，每物料一行，**物料 + 出库仓**（账上能按仓按料拆开看）
        for (line, material_amount) in positives.iter().zip(split) {
            let label = material_label(&codes, line.material_id);
            lines.push(EntryLine {
                line_no: 0,
                summary: format!(
                    "{} 存货 {}（{}）",
                    req.summary,
                    label,
                    line_range_text(line.line_from, line.line_to)
                ),
                account: inventory.clone(),
                debit: Decimal::ZERO,
                credit: material_amount,
                aux_supplier_id: None,
                aux_material_id: Some(line.material_id),
                aux_warehouse_id: Some(req.warehouse_id),
                aux_dept_id: None,
            });
        }
        for (index, line) in lines.iter_mut().enumerate() {
            line.line_no = i32::try_from(index + 1).unwrap_or(0);
        }
        lines
    };

    let voucher_id = post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: SOURCE_STOCK_OUT,
            source_id: Some(req.doc_id),
            source_no: Some(req.doc_no),
            source_doc_no: None,
            voucher_date: req.voucher_date,
            summary: req.summary.clone(),
            lines,
            remark: None,
            red_of: None,
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await?;
    Ok(Some(voucher_id))
}

// ==================== 写：销售收入确认凭证（ARCH-7 §5.5 / SO-22） ====================

/// 收入确认凭证的入参（**开票时**确认收入，一张发票一张蓝字凭证）。
///
/// 打包成结构体与 [`StockOutVoucher`] 同一个理由：摊平会撞 clippy 的
/// `too_many_arguments`（`-D warnings` 下是硬错）。
#[derive(Debug, Clone)]
pub struct SalesRevenueVoucher<'a> {
    /// 发票 id（`biz_voucher.source_id`）。
    pub invoice_id: Uuid,
    /// 发票号（`biz_voucher.source_no`，同时对账键）。
    pub invoice_no: &'a str,
    /// 凭证日期（= 发票 `invoice_date`，决定会计期间；跨期必须走调整）。
    pub voucher_date: NaiveDate,
    /// **不含税**金额（`biz_invoice.amount`，4 位定标）。
    pub amount: Decimal,
    /// 税额（`biz_invoice.tax_amount`）。
    pub tax_amount: Decimal,
    /// 价税合计（`biz_invoice.total_with_tax`，= 借方的应收账款）。
    pub total_with_tax: Decimal,
    /// **按商品聚合的收入行**（粒度 `MATERIAL`，`amount` = 该行不含税金额）。
    ///
    /// 非空 ⇒ 贷 `5001` 拆成 `M` 行（每行带物料辅助核算，账上能按商品看收入结构）；
    /// 空 ⇒ 回退到「贷 1 行」的汇总行（本卡之前的行为）。
    /// 无论哪条路，合并 / 升序 / 尾差吸收都走 [`VoucherMaterialLine::normalize`]
    /// + [`absorb_tail_diff`]（与出库成本凭证同一套实现，不另写一份）。
    pub material_lines: Vec<VoucherMaterialLine>,
    /// 凭证摘要（调用方给，例如「开票 IV-20261106-000001 客户 xxx」）。
    pub summary: String,
    /// 备注（发票备注；`None` = 不写）。
    pub remark: Option<&'a str>,
    /// **本位币三连**（SO-41 / ARCH-7 §15.6.4 ②）：`Some` ⇒ 分录按本位币金额入账。
    ///
    /// 来源固定是单据的 `base_total_*` 三列（SO-40 在建单时按锁定汇率换算、尾差归含税列），
    /// 于是「USD 单据 ⇒ 凭证金额 = 原币 × 锁定汇率」是**数据驱动**的，
    /// 本函数不再自己乘汇率（汇率只有锁定那一刻算一次，后面任何汇率变动都不影响已开票金额）。
    ///
    /// `None` ⇒ 按原币入账（历史行为，兼容本位币单据：`rate = 1` 时两者逐分相等）。
    pub base: Option<BaseAmounts>,
}

/// 校验「价税合计 = 不含税 + 税额」是否自洽，并返回**吸收尾差后**的贷方税额。
///
/// * 差额 < [`BALANCE_TOLERANCE`]（= 0.01）⇒ 视为平衡：返回的税额取
///   `含税 − 不含税`（4 位定标），这样借方（含税）与贷方（不含税 + 税额）
///   **分毫不差**，`post_voucher` 的严格相等校验也过得去；
/// * 差额 ≥ 0.01 ⇒ `DbError::InvalidValue`，文案直接指出三个金额与差额。
///   这是数据质量闸：不是「差不多就行」，是「这张单的金额本身算错了」。
///
/// 纯函数（不碰库），因此无 `ERP_DB_URL` 也能跑单测。
pub fn sales_revenue_amounts(
    doc_no: &str,
    total_with_tax: Decimal,
    amount: Decimal,
    tax_amount: Decimal,
) -> DbResult<(Decimal, Decimal)> {
    let total = total_with_tax.round_dp(4);
    let revenue = amount.round_dp(4);
    let tax = tax_amount.round_dp(4);
    if revenue < Decimal::ZERO {
        return Err(DbError::InvalidValue(format!(
            "收入凭证生成失败：不含税金额不能为负（单据 {doc_no} 传 {revenue}）；\
             红字收入请走红字凭证（voucher::reverse_posted），不要拿负数硬写蓝字凭证"
        )));
    }
    if tax < Decimal::ZERO {
        return Err(DbError::InvalidValue(format!(
            "收入凭证生成失败：税额不能为负（单据 {doc_no} 传 {tax}）"
        )));
    }
    let derived_tax = (total - revenue).round_dp(4);
    let diff = (derived_tax - tax).abs();
    if diff >= BALANCE_TOLERANCE {
        return Err(DbError::InvalidValue(format!(
            "收入凭证生成失败：金额不自洽（单据 {doc_no}）—— 价税合计 {total} ≠ 不含税 {revenue} \
             + 税额 {tax}（差额 {diff} ≥ {BALANCE_TOLERANCE}），借贷无法平衡；请先核对单据金额"
        )));
    }
    if diff > Decimal::ZERO {
        tracing::warn!(
            doc_no,
            total_with_tax = %total,
            amount = %revenue,
            tax_amount = %tax,
            tail_diff = %diff,
            absorbed_tax = %derived_tax,
            "收入凭证的价税尾差 < 0.01：已吸收进销项税行（借贷分毫不差），非 0 = 单据定标信号"
        );
    }
    Ok((revenue, derived_tax))
}

/// 把**不含税合计**拆到各商品行：合并同物料 → 按物料升序 → 尾差吸收到最后一行。
///
/// 复用 [`VoucherMaterialLine::normalize`]（合并 + 排序）与 [`absorb_tail_diff`]
/// （尾差）—— 与出库成本结转凭证**同一套实现**，不另起一套拆分逻辑。
/// 金额 ≤ 0 的行不参与（零金额行会被 `post_voucher` 的分录校验咬）。
///
/// 返回空 vec ⇒ 调用方回退到「贷 1 行」。
pub fn sales_revenue_split(
    doc_no: &str,
    revenue_total: Decimal,
    material_lines: &[VoucherMaterialLine],
) -> DbResult<Vec<VoucherMaterialLine>> {
    let mut positives: Vec<VoucherMaterialLine> = VoucherMaterialLine::normalize(material_lines.iter().cloned())
        .into_iter()
        .filter(|line| line.amount > Decimal::ZERO)
        .collect();
    if positives.is_empty() {
        return Ok(Vec::new());
    }
    let amounts: Vec<Decimal> = positives.iter().map(|line| line.amount).collect();
    let split = absorb_tail_diff(doc_no, revenue_total, &amounts)?;
    for (line, amount) in positives.iter_mut().zip(split) {
        line.amount = amount;
    }
    Ok(positives)
}

/// 本位币三连的**入账前校验**：非负 + 恒等式 `with_tax = amount + tax`，返回定标后的值。
///
/// 三列都来自单据（SO-40 的 `base_total_*`），库层已有 `ck_biz_*_base_amount` 兜底；
/// 这里再校验一次的理由是**凭证是最终账**：单据列被非 API 写入绕过 CHECK 之前，
/// 我们不能把「借 1122 1000 / 贷 5001 700 + 贷 2221.02 200」这种不平的凭证写进总账。
///
/// 尾差处理与 [`BaseAmounts::of`] 同口径：`with_tax` 取前两项之和（差额 ≤ 1 个最小单位
/// 会落在这里，而不是让借贷差 0.0001 卡住结账）。
pub fn validate_base_amounts(doc_no: &str, base: BaseAmounts) -> DbResult<BaseAmounts> {
    let amount = base.amount.round_dp(4);
    let tax = base.tax.round_dp(4);
    let with_tax = base.with_tax.round_dp(4);
    if amount < Decimal::ZERO || tax < Decimal::ZERO || with_tax < Decimal::ZERO {
        return Err(DbError::InvalidValue(format!(
            "收入凭证生成失败：本位币金额不能为负（单据 {doc_no} 传 不含税 {amount} / 税额 {tax} / \
             含税 {with_tax}）；红字收入请走红字凭证，不要拿负数硬写蓝字凭证"
        )));
    }
    let diff = (with_tax - (amount + tax)).abs();
    if diff >= BALANCE_TOLERANCE {
        return Err(DbError::InvalidValue(format!(
            "收入凭证生成失败：本位币金额不自洽（单据 {doc_no}）—— 含税 {with_tax} ≠ 不含税 {amount} \
             + 税额 {tax}（差额 {diff} ≥ {BALANCE_TOLERANCE}），借贷无法平衡；请先核对单据的 base_* 三列"
        )));
    }
    Ok(BaseAmounts {
        amount,
        tax,
        // 尾差归含税（与 `currency::BaseAmounts::of` 同一口径，不另立一套）
        with_tax: amount + tax,
    })
}

/// 把**本位币不含税合计**按原币的商品构成拆到各物料行（SO-41）。
///
/// 商品行只有原币金额（发票/订单明细本来就只有原币），本位币合计却来自单据的
/// `base_total_amount`。两个金额之间只差一次汇率与逐项定标（≤ 每行 1 个最小单位），
/// 所以按 `base_revenue / revenue_original` 的比例缩放每一行，再把尾差交给
/// [`absorb_tail_diff`]（与出库 / 收入拆行**同一把尺子**）—— 不重算汇率、不另写拆分逻辑。
///
/// 三种退化：`material_lines` 为空 ⇒ 空 vec（调用方回退「贷 1 行」）；
/// 原币不含税 ≤ 0（`revenue_base` 却 > 0 的脏单据）⇒ 同样回退单行，
/// 宁可少拆行也不让比例除法把金额炸成天文数字。
pub fn sales_revenue_split_in_base(
    doc_no: &str,
    revenue_original: Decimal,
    revenue_base: Decimal,
    material_lines: &[VoucherMaterialLine],
) -> DbResult<Vec<VoucherMaterialLine>> {
    if material_lines.is_empty() {
        return Ok(Vec::new());
    }
    let original = revenue_original.round_dp(4);
    if original <= Decimal::ZERO {
        tracing::warn!(
            doc_no,
            revenue_original = %original,
            revenue_base = %revenue_base,
            "原币不含税金额 ≤ 0 却给了本位币金额：商品行无法按比例缩放，回退为贷 1 行汇总"
        );
        return Ok(Vec::new());
    }
    let ratio = revenue_base / original;
    let scaled: Vec<VoucherMaterialLine> = material_lines
        .iter()
        .map(|line| VoucherMaterialLine {
            material_id: line.material_id,
            qty: line.qty,
            amount: (line.amount * ratio).round_dp(4),
            line_from: line.line_from,
            line_to: line.line_to,
        })
        .collect();
    sales_revenue_split(doc_no, revenue_base, &scaled)
}

/// 开票（收入确认）时生成蓝字凭证：**借 1122 应收账款（价税合计）/ 贷 5001 主营业务收入
/// （不含税）+ 贷 2221.02 应交税费-销项税（税额）**。
///
/// # 触发点：开票时，不是挂账时
///
/// ARCH-7 SO-22 的二选一取「开票时确认收入」：增值税纳税义务发生在开票当天，
/// 挂账（出库转应收）只是债权成立、不是收入确认时点。所以 `source_type = INVOICE`、
/// `source_id` = 发票 id（作废时按同一来源红冲，见 `invoice::reverse_invoice_voucher`）。
///
/// # 借贷平衡的硬口径
///
/// 1. 三个金额先过 [`sales_revenue_amounts`]：差额 ≥ 0.01 直接拒绝（金额本身算错）；
/// 2. 差额 < 0.01 的尾差**吸收**掉（进销项税行 / 最后一笔贷方行），落库的凭证
///    `Σ借 == Σ贷`（`post_voucher` 要求严格相等，不是「近似相等」）；
/// 3. 拆行后的尾差走 [`absorb_tail_diff`]（与出库成本凭证同一把尺子）。
///
/// # 期间已关闭
///
/// 期间闸只有一处 —— [`post_voucher`] 里查 `biz_accounting_period`：已结账（`CLOSED`）
/// 的月份**只读**，返回 `DbError::InvalidValue`（文案含「已结账」）。本函数不重复查，
/// 免得出现「两个地方各自判一次、某天口径不一致」。
///
/// # 返回
///
/// * `Ok(Some(voucher_id))`：已生成并记账（`POSTED`）；
/// * `Ok(None)`：价税合计 ≤ 0（零金额发票不构成凭证，打 WARN 点名单据）。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`（`Db` / `Transaction`），**由调用方开事务**；不 `begin` 不 `commit`。
pub async fn create_for_sales_revenue(
    ex: &impl SqlExecutor,
    req: &SalesRevenueVoucher<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let total = req.total_with_tax.round_dp(4);
    if total <= Decimal::ZERO {
        tracing::warn!(
            doc_no = req.invoice_no,
            invoice_id = %req.invoice_id,
            total_with_tax = %total,
            "价税合计 ≤ 0（零金额发票）：跳过收入凭证生成，只留单据"
        );
        return Ok(None);
    }
    let (revenue, tax, total) = match req.base {
        // 本位币入账（SO-41 / ARCH-7 §15.6.4 ②）：外币单据的凭证金额 = 原币 × **锁定**汇率
        Some(base) => {
            let base = validate_base_amounts(req.invoice_no, base)?;
            (base.amount, base.tax, base.with_tax)
        }
        // 兼容路径：没有 base_* 的调用方按原币入账（本位币单据 rate = 1 时两者逐分相等）
        None => {
            let (revenue, tax) = sales_revenue_amounts(req.invoice_no, total, req.amount, req.tax_amount)?;
            (revenue, tax, total)
        }
    };

    let ar_account = account_required(ex, ACCOUNT_AR, "收入凭证的借方科目 1122 应收账款").await?;
    let revenue_account = account_required(ex, ACCOUNT_REVENUE, "收入凭证的贷方科目 5001 主营业务收入").await?;

    // 借：应收账款（价税合计）
    let mut lines: Vec<EntryLine> = vec![EntryLine {
        line_no: 1,
        summary: format!("{} 确认应收账款", req.summary),
        account: ar_account,
        debit: total,
        credit: Decimal::ZERO,
        aux_supplier_id: None,
        aux_material_id: None,
        aux_warehouse_id: None,
        aux_dept_id: None,
    }];

    // 贷：主营业务收入（不含税；按商品拆行时不带仓库辅助核算 —— 收入是全公司口径）
    let split = if req.base.is_some() {
        sales_revenue_split_in_base(req.invoice_no, req.amount, revenue, &req.material_lines)?
    } else {
        sales_revenue_split(req.invoice_no, revenue, &req.material_lines)?
    };
    if split.is_empty() {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{} 确认销售收入", req.summary),
            account: revenue_account,
            debit: Decimal::ZERO,
            credit: revenue,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    } else {
        let ids: Vec<Uuid> = split.iter().map(|line| line.material_id).collect();
        let codes = material_codes(ex, &ids).await?;
        for line in &split {
            let label = material_label(&codes, line.material_id);
            lines.push(EntryLine {
                line_no: 0,
                summary: format!(
                    "{} 收入 {}（{}）",
                    req.summary,
                    label,
                    line_range_text(line.line_from, line.line_to)
                ),
                account: revenue_account.clone(),
                debit: Decimal::ZERO,
                credit: line.amount,
                aux_supplier_id: None,
                aux_material_id: Some(line.material_id),
                // 收入科目不挂仓库：按仓拆收入会把损益表拆碎（与成本凭证的口径对称）
                aux_warehouse_id: None,
                aux_dept_id: None,
            });
        }
    }

    // 贷：应交税费-销项税（税额为 0 不出行 —— 零金额分录入不了库）
    if tax > Decimal::ZERO {
        let tax_account = account_required(ex, ACCOUNT_OUTPUT_TAX, "收入凭证的贷方科目 2221.02 销项税").await?;
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{} 销项税额", req.summary),
            account: tax_account,
            debit: Decimal::ZERO,
            credit: tax,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }

    for (index, line) in lines.iter_mut().enumerate() {
        line.line_no = i32::try_from(index + 1).unwrap_or(0);
    }

    // 最后一闸：借贷必须真的平（不靠调用方自证）
    let (debit, credit) = totals_of(&lines);
    let diff = (debit - credit).abs();
    if diff >= BALANCE_TOLERANCE {
        return Err(DbError::InvalidValue(format!(
            "收入凭证生成失败：借贷不平衡（单据 {}，借 {debit} / 贷 {credit}，差额 {diff} ≥ \
             {BALANCE_TOLERANCE}）",
            req.invoice_no
        )));
    }
    if diff > Decimal::ZERO {
        // < 0.01 的尾差落到最后一笔贷方行（上面已保证税额行是派生值，正常走不到这里）
        if let Some(last) = lines.iter_mut().rev().find(|line| line.credit > Decimal::ZERO) {
            last.credit = (last.credit + diff).round_dp(4);
            tracing::warn!(
                doc_no = req.invoice_no,
                tail_diff = %diff,
                "收入凭证的借贷尾差 < 0.01：已吸收到最后一笔贷方行"
            );
        }
    }

    let voucher_id = post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: SOURCE_INVOICE,
            source_id: Some(req.invoice_id),
            source_no: Some(req.invoice_no),
            source_doc_no: None,
            voucher_date: req.voucher_date,
            summary: req.summary.clone(),
            lines,
            remark: req.remark,
            red_of: None,
            status: crate::business::erp::voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await?;
    Ok(Some(voucher_id))
}

/// 按**来源单据**找原凭证并红冲（出库单作废用；找不到原凭证 → `Ok(None)`）。
///
/// 与 [`reverse_for_stock_in`] 的区别只有一处：不写死 `SOURCE_STOCK_IN`，
/// 而是把来源类型当参数 —— 这样出库单（`SOURCE_STOCK_OUT`）与将来的退货单
/// 不用各抄一份「找凭证 + 定日期 + 红冲」的胶水代码。
/// 红冲本身**只有一份实现**（[`reverse_posted`]）：原凭证保持 `POSTED` + 写
/// `reversed_by`，新凭证 `is_red = 1`。
///
/// # 返回
///
/// * `Ok(Some(red_id))`：已红冲；
/// * `Ok(None)`：这张单没有凭证（零成本存量 / 从未过账），不阻断作废。
pub async fn reverse_by_source(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
    voucher_date: NaiveDate,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let Some(original) = find_voucher_by_source(ex, source_type, source_id).await? else {
        return Ok(None);
    };
    let red_id = reverse_posted(ex, &original, ReversalInput { reason, voucher_date }, operator, now).await?;
    Ok(Some(red_id))
}

// ==================== 写：红冲 ====================

/// 收货单作废时生成红字凭证（卡面 §五）。
///
/// * 原凭证 `status` 保持 `POSTED`，只写 `reversed_by`；
/// * 红字凭证 `is_red=1`、`reverse_of=原凭证`，分录借贷互换；
/// * 找不到原凭证（未过账的历史脏数据）→ 返回 `Ok(None)`，不阻断作废。
pub async fn reverse_for_stock_in(
    ex: &impl SqlExecutor,
    header: &StockInRow,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>> {
    let Some(original) = find_voucher_by_source(ex, SOURCE_STOCK_IN, header.id).await? else {
        return Ok(None);
    };
    // ★ P-FLOW-3B 反向（L6 关联物级联）：收货单作废要联动红冲这张凭证 →
    //   先终止它在跑的**红冲审批**（否则审批中心留孤儿待办，pending 也永远挂着：
    //   入口会一直 409「已在红冲审批中」，且已红冲的凭证再批一次会撞「不能重复红冲」）。
    //   必须在 reverse_posted 之前做：`cancel_approval_of` 会清 pending，
    //   而 `ck_biz_voucher_pending_reverse` 要求「pending 非空 ⟹ reversed_by 为空」。
    crate::business::erp::voucher_bridge::cancel_approval_of(
        ex,
        original.id,
        &original.voucher_no,
        "收货单作废联动红冲凭证，红冲审批自动终止",
    )
    .await?;
    // 红字凭证日期：作废当天（CST）。已作废单用 cancelled_at，未作废的兜底「今天」。
    let voucher_date = header
        .cancelled_at
        .map(|at| at.date_naive())
        .unwrap_or_else(|| today_cst(now));
    // 红冲只保留一份实现（[`reverse_posted`]）；本函数只负责「找到原凭证 + 定日期」。
    let red_id = reverse_posted(ex, &original, ReversalInput { reason, voucher_date }, operator, now).await?;
    Ok(Some(red_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::stockin_dto::{CreateStockInReq, StockInItemReq, StockInStatusReq};
    use crate::business::erp::stockin_handler::{change_stock_in_status, create_stock_in};
    use crate::business::erp::{fixture_hygiene, payable, stockin_model};
    use nik::prelude::chrono::Duration;
    use std::str::FromStr;

    const SUPPLIER: &str = "b2c10001-0000-4000-8000-000000000002";
    const WAREHOUSE: &str = "b2c10001-0000-4000-8000-000000000003";
    const MATERIAL_A: &str = "b2c10001-0000-4000-8000-000000000011";
    /// 配了存货科目的物料（用于验证「规则缺省时退化到物料科目」）。
    const MATERIAL_B: &str = "b2c10001-0000-4000-8000-000000000012";
    /// 测试专用用户科目（规则指向它，证明「规则优先」）。
    const TEST_ACCOUNT: &str = "b2c10001-0000-4000-8000-000000000021";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('b2c10001-0000-4000-8000-000000000001', 'P2C1-PCS', '个', 1, NULL, 1.000000, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days)
         VALUES ('b2c10001-0000-4000-8000-000000000002', 'P2C1-SUP', 'P2C1 测试供应商', 1, 30)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
         VALUES ('b2c10001-0000-4000-8000-000000000003', 'P2C1-WH', 'P2C1 收货仓',
                 '/b2c10001-0000-4000-8000-000000000003/', 1, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
         VALUES ('b2c10001-0000-4000-8000-000000000011', 'P2C1-M1', 'P2C1 螺丝',
                 'b2c10001-0000-4000-8000-000000000001', 1, false),
                ('b2c10001-0000-4000-8000-000000000012', 'P2C1-M2', 'P2C1 垫片',
                 'b2c10001-0000-4000-8000-000000000001', 1, false)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                                  account_category, balance_direction, is_leaf, status)
         VALUES ('b2c10001-0000-4000-8000-000000000021', 'P2C1.1405', 'P2C1 测试存货科目', NULL, 1,
                 'ASSET', 'DEBIT', 1, 1)
         ON CONFLICT DO NOTHING",
        // 作废审批的发起人（P16 起作废走审批，必须有登录用户；**非超管**，避免自审被 FORBID 挡）
        "INSERT INTO sys_user (id, username, password, real_name)
         VALUES ('b2c10009-0000-4000-8000-000000000001', 'p2c1_keeper', 'p2c1-not-a-real-hash', 'P2C1 仓管')
         ON CONFLICT DO NOTHING",
    ];

    // 真库用例必须串行（共用全局 `db()`、同一批夹具与同一批 `biz_seq`，与 P2-B1 同范式）。
    //
    // 锁必须与 `cash_flow` 的全表断言共用同一把（`voucher_query::tests::DB_LOCK`）：
    // 本模块夹具在 `biz_account` 里造了 `P2C1.1405`（无现金流量分类行），而
    // `cash_flow::tests::r3_real_config_covers_all_accounts` 遍历 `biz_account` 全表
    // 断言「每个非现金科目都有分类行」—— 两把不同的锁时并行必撞。
    use crate::business::erp::voucher_query::tests::DB_LOCK;

    async fn ensure_db() -> bool {
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
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // `max_lifetime = 0` 逼池子每次新建连接（W1b 实测的「借到死连接」）。
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

    async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 清掉本模块夹具的**全部痕迹**（状态日志 → 凭证分录 → 凭证 → 应付 → 流水 → 库存 → 收货单），
    /// 并复位上一轮改过的映射规则 / 科目状态。
    ///
    /// 两处调用：每个用例开头的 `setup()`，以及模块里名字排最后的
    /// `zz_teardown_purges_finance_status_logs`（libtest 按名排序，`zz_` 必然收尾）。
    ///
    /// ⚠️ 状态日志必须**在删单据之前**按 id 清：`biz_status_log` 没有外键（`fixture_hygiene`）。
    /// 本模块的收货过账会同时产生 `STOCK_IN` / `PAYABLE` / `VOUCHER` 三类日志，
    /// 漏一类就是每跑一遍全量多 20 行孤儿（本卡实测）。
    async fn cleanup_fixtures() {
        let cleanups = [
            // P16 起作废走审批：先把本夹具的作废审批实例清掉（否则下一轮发起撞「已有在跑的审批单」）
            format!(
                "DELETE FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_CANCEL' AND biz_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            // 状态日志（无外键）：必须在删单据之前按 id 清，否则每轮留一批孤儿（`fixture_hygiene`）
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
                 (SELECT id FROM biz_voucher WHERE source_id IN \
                   (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid) \
                   OR source_no LIKE 'BROKEN-%' OR voucher_no LIKE '__P2C1%')"
            ),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'PAYABLE' AND entity_id IN \
                 (SELECT id FROM biz_payable WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND entity_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                 (SELECT id FROM biz_voucher WHERE source_id IN \
                   (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid))"
            ),
            format!(
                "DELETE FROM biz_voucher WHERE source_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!("DELETE FROM biz_payable WHERE supplier_id = '{SUPPLIER}'::uuid"),
            format!(
                "DELETE FROM biz_inventory_txn WHERE doc_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_inventory WHERE material_id IN \
                 ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid) AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            format!("DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid"),
            // 复位上一轮可能改过的映射规则 / 科目状态
            "UPDATE biz_voucher_rule SET deleted_at = NULL, is_active = 1, \
                    debit_account_id = (SELECT id FROM biz_account WHERE account_code = '1405' AND deleted_at IS NULL), \
                    credit_account_id = (SELECT id FROM biz_account WHERE account_code = '2202' AND deleted_at IS NULL), \
                    tax_account_id = (SELECT id FROM biz_account WHERE account_code = '2221.01' AND deleted_at IS NULL) \
              WHERE rule_code = 'PURCHASE_IN_DEFAULT'".to_string(),
            "UPDATE biz_account SET status = 1 WHERE account_code IN ('1405','2202','2221.01')".to_string(),
            format!("UPDATE biz_material SET inventory_account_id = NULL WHERE id = '{MATERIAL_B}'::uuid"),
            "DELETE FROM biz_voucher WHERE source_no LIKE 'BROKEN-%' OR voucher_no LIKE '__P2C1%'"
                .to_string(),
            // E2E-FIX-5 ②：`source_doc_no` 用例自己造的两张凭证（业务单据号 / 子单据号前缀）
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'P2C1-DOCNO%')"
                .to_string(),
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'P2C1-DOCNO%')"
                .to_string(),
            "DELETE FROM biz_voucher WHERE source_no LIKE 'P2C1-DOCNO%'".to_string(),
        ];
        for sql in cleanups {
            exec(&sql).await;
        }
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具 + 复位科目映射规则。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup_fixtures().await;
        for sql in FIXTURES {
            exec(sql).await;
        }
        Some(guard)
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    fn line_req(material: &str, qty: &str, price: &str, rate: &str) -> StockInItemReq {
        StockInItemReq {
            purchase_order_item_id: None,
            material_id: Some(material.to_string()),
            qty: Some(dec(qty)),
            price_excl: Some(dec(price)),
            tax_rate: Some(dec(rate)),
            location_id: None,
            location_dict_id: None,
            serial_ids: Vec::new(),
            remark: None,
        }
    }

    fn req(items: Vec<StockInItemReq>) -> CreateStockInReq {
        CreateStockInReq {
            source_order_id: None,
            purchase_order_id: None,
            supplier_id: Some(SUPPLIER.to_string()),
            // A4：没有来源采购单 ⇒ 必须显式声明「无源单据」+ 写明来源（AP-5）
            is_sourceless: Some(true),
            warehouse_id: Some(WAREHOUSE.to_string()),
            in_date: None,
            invoice_received: None,
            remark: Some("A4 夹具：无源入库（凭证链路用例）".to_string()),
            items,
            total_qty: None,
            total_amount: None,
            total_tax: None,
        }
    }

    fn post_req() -> StockInStatusReq {
        StockInStatusReq {
            status: Some("POSTED".to_string()),
            reason: None,
            serials: Vec::new(),
            ..Default::default()
        }
    }

    fn cancel_req(reason: &str) -> StockInStatusReq {
        StockInStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: Some(reason.to_string()),
            serials: Vec::new(),
            ..Default::default()
        }
    }

    fn req_with_date(items: Vec<StockInItemReq>, date: NaiveDate) -> CreateStockInReq {
        let mut r = req(items);
        r.in_date = Some(date);
        r
    }

    /// 建单 + 过账，返回收货单 id（过账失败直接 panic，测试里好定位）。
    async fn post_new(lines: Vec<StockInItemReq>) -> Uuid {
        let id = create_stock_in(&req(lines), None).await.unwrap();
        change_stock_in_status(id, &post_req(), None)
            .await
            .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
        id
    }

    /// 作废的发起人（**非超管**：审批人来源是 ADMIN，超管发起会撞 `self_approval=FORBID`）。
    const KEEPER: &str = "b2c10009-0000-4000-8000-000000000001";

    /// 作废 = **发起审批 → 复核通过 → 执行**（P16 之后作废不再一步到位）。
    ///
    /// 为什么要这个 helper：直接 `change_stock_in_status(id, &cancel_req(..), None)` 会
    /// `401 发起作废审批需要登录用户`（V0174 起作废先走审批，必须有发起人）。
    /// 这里把三步串起来，走的仍是**生产同一条路径**，所以旧断言（红字凭证 / 应付作废）不变。
    ///
    /// ⚠️ 复核节点是 `assignee_type = 'ROLE'`（`WAREHOUSE_MANAGER`，P1-FIX-1 起）
    /// + `empty_assignee=ERROR`（不兜底超管）⇒ **复核岗位必须先在册有人**，否则发起就 422。
    ///
    /// 角色成员是共享资源，这里只留本模块的复核人（`p2c1_reviewer`）。
    async fn cancel_via_approval(stock_in_id: Uuid, reason: &str) {
        for sql_text in [
            "DELETE FROM sys_user_role WHERE role_id = '20000000-0000-4000-8000-0000000f0004'::uuid \
                AND user_id <> 'b2c10009-0000-4000-8000-000000000002'::uuid",
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES ('b2c10009-0000-4000-8000-000000000002'::uuid, 'p2c1_reviewer', 'x', \
                     'P2C1 作废复核人', 1) ON CONFLICT (id) DO NOTHING",
            "INSERT INTO sys_user_role (user_id, role_id) \
             VALUES ('b2c10009-0000-4000-8000-000000000002'::uuid, \
                     '20000000-0000-4000-8000-0000000f0004'::uuid) ON CONFLICT DO NOTHING",
        ] {
            db().execute_with_params(sql_text, vec![])
                .await
                .unwrap_or_else(|err| panic!("作废复核岗位夹具失败：{err}"));
        }
        change_stock_in_status(stock_in_id, &cancel_req(reason), Some(uid(KEEPER)))
            .await
            .unwrap_or_else(|err| panic!("发起作废审批失败：{} {}", err.code, err.msg));
        let instance_id: Uuid = db()
            .scalar_with_params::<Option<Uuid>>(
                "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&stock_in_id)],
            )
            .await
            .expect("查作废实例失败")
            .expect("作废审批必须回写实例 id");
        let outs = crate::business::flow::test_support::approve_all_pending(instance_id, "单测：同意作废").await;
        assert!(
            !outs.is_empty(),
            "作废审批必须至少有一条 PENDING 待办可批（实例 {instance_id}）"
        );
        for out in outs {
            crate::business::erp::stockin_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;
        }
        crate::business::flow::test_support::assert_instance_status(instance_id, "APPROVED").await;
    }

    fn uid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    /// 取凭证（真凭证，非红字）。
    async fn voucher_of(stock_in_id: Uuid) -> VoucherRow {
        find_voucher_by_source(db(), SOURCE_STOCK_IN, stock_in_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("收货单 {stock_in_id} 没有生成凭证"))
    }

    async fn payable_of(stock_in_id: Uuid) -> payable::PayableRow {
        payable::find_by_source(db(), SOURCE_STOCK_IN, stock_in_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("收货单 {stock_in_id} 没有生成应付单"))
    }

    // ---------- ① 应付 ----------

    /// ① 过账生成应付：金额 = 价税合计（226），status=OPEN，未付 = 全额。
    #[tokio::test]
    async fn post_creates_open_payable_with_with_tax_amount() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let row = payable_of(id).await;
        assert_eq!(row.amount, dec("226"), "应付金额 = 价税合计（不是不含税 200）");
        assert_eq!(row.paid_amount, Decimal::ZERO);
        assert_eq!(row.unpaid_amount, dec("226"));
        assert_eq!(row.status, payable::STATUS_OPEN);
        assert_eq!(row.source_type, "STOCK_IN");
        assert_eq!(row.source_id, id);
        assert!(row.payable_no.starts_with("AP-"), "{}", row.payable_no);
        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(
            row.period_code,
            period_of(db(), header.in_date).await.unwrap(),
            "期间跟收货日期走"
        );
        assert_eq!(row.credit_days, 30, "账期快照来自供应商档案");
        assert_eq!(
            row.due_date,
            header.in_date + Duration::days(30),
            "到期日 = 入库日期 + 账期"
        );
    }

    // ---------- ② 凭证状态 / ③ 分录 / ④ 平衡 ----------

    /// ② 过账生成的凭证直接 `POSTED`（不是 DRAFT），且是自动凭证。
    #[tokio::test]
    async fn post_creates_posted_auto_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let voucher = voucher_of(id).await;
        assert_eq!(voucher.status, STATUS_POSTED, "自动凭证必须直接已记账");
        assert_eq!(voucher.voucher_type, VOUCHER_TYPE_AUTO);
        assert_eq!(voucher.is_red, IS_RED_NO);
        assert_eq!(voucher.voucher_word, WORD_DEFAULT);
        assert!(voucher.voucher_no.starts_with("PZ-"), "{}", voucher.voucher_no);
        assert_eq!(voucher.source_no.as_deref(), Some(id_to_in_no(id).await.as_str()));
        assert_eq!(voucher.entry_count, 3);
    }

    async fn id_to_in_no(id: Uuid) -> String {
        stockin_model::find_stock_in(id)
            .await
            .unwrap()
            .expect("收货单不存在")
            .in_no
    }

    /// ③ 分录 3 行：借存货(200) / 借进项税(26) / 贷应付(226)，科目编码逐一核对。
    #[tokio::test]
    async fn post_creates_three_entries_debit_inventory_debit_tax_credit_payable() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let voucher = voucher_of(id).await;
        let entries = entries_of(db(), voucher.id).await.unwrap();
        assert_eq!(entries.len(), 3, "借存货 + 借进项税 + 贷应付");

        let inventory = &entries[0];
        assert_eq!(inventory.line_no, 1);
        assert_eq!(inventory.account.code, DEFAULT_INVENTORY_CODE);
        assert_eq!(inventory.account.name, "库存商品");
        assert_eq!(inventory.debit, dec("200"));
        assert_eq!(inventory.credit, Decimal::ZERO);
        assert_eq!(inventory.aux_warehouse_id, Some(Uuid::parse_str(WAREHOUSE).unwrap()));

        let tax = &entries[1];
        assert_eq!(tax.line_no, 2);
        assert_eq!(tax.account.code, DEFAULT_TAX_CODE);
        assert_eq!(tax.debit, dec("26"));
        assert_eq!(tax.credit, Decimal::ZERO);

        let ap = &entries[2];
        assert_eq!(ap.line_no, 3);
        assert_eq!(ap.account.code, DEFAULT_PAYABLE_CODE);
        assert_eq!(ap.debit, Decimal::ZERO);
        assert_eq!(ap.credit, dec("226"));
        assert_eq!(ap.aux_supplier_id, Some(Uuid::parse_str(SUPPLIER).unwrap()));
    }

    /// ④ 借贷平衡：主表两列相等，且等于分录两列之和。
    #[tokio::test]
    async fn voucher_totals_are_balanced() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![
            line_req(MATERIAL_A, "100", "2", "13"),
            line_req(MATERIAL_B, "3", "10.5", "13"),
        ])
        .await;
        let voucher = voucher_of(id).await;
        assert_eq!(voucher.total_debit, voucher.total_credit);
        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(
            voucher.total_debit, header.total_with_tax,
            "借贷合计 = 单头价税合计（金额取自单据，不重算）"
        );
        let entries = entries_of(db(), voucher.id).await.unwrap();
        let (debit, credit) = totals_of(
            &entries
                .iter()
                .map(|row| EntryLine {
                    line_no: row.line_no,
                    summary: row.summary.clone(),
                    aux_dept_id: None,
                    account: row.account.clone(),
                    debit: row.debit,
                    credit: row.credit,
                    aux_supplier_id: None,
                    aux_material_id: None,
                    aux_warehouse_id: None,
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(debit, voucher.total_debit);
        assert_eq!(credit, voucher.total_credit);
    }

    /// ⑤ 税率 0：只有 2 行，不产生进项税行。
    #[tokio::test]
    async fn zero_tax_receipt_has_two_entries() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "10", "2", "0")]).await;
        let voucher = voucher_of(id).await;
        let entries = entries_of(db(), voucher.id).await.unwrap();
        assert_eq!(entries.len(), 2, "税率 0 → 无进项税行");
        assert!(
            entries.iter().all(|row| row.account.code != DEFAULT_TAX_CODE),
            "不应出现进项税科目"
        );
        assert_eq!(voucher.total_debit, dec("20"));
        assert_eq!(voucher.entry_count, 2);
    }

    /// ⑥ 金额口径：存货用不含税、应付用价税合计。
    #[tokio::test]
    async fn amount_basis_inventory_excludes_tax_payable_includes_tax() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "7", "1.5", "13")]).await;
        let voucher = voucher_of(id).await;
        let entries = entries_of(db(), voucher.id).await.unwrap();
        let inventory = entries
            .iter()
            .find(|row| row.account.code == DEFAULT_INVENTORY_CODE)
            .unwrap();
        let tax = entries.iter().find(|row| row.account.code == DEFAULT_TAX_CODE).unwrap();
        let ap = entries
            .iter()
            .find(|row| row.account.code == DEFAULT_PAYABLE_CODE)
            .unwrap();
        // 金额一律等于单头三列（7 × 1.5 = 10.5；税额由建单侧算，本模块不重算）
        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(inventory.debit, header.total_amount);
        assert_eq!(tax.debit, header.total_tax);
        assert_eq!(ap.credit, header.total_with_tax);
        assert_eq!(inventory.debit, dec("10.5"), "存货 = 不含税金额");
        assert_eq!(ap.credit, inventory.debit + tax.debit, "应付 = 存货 + 进项税");
        let payable_row = payable_of(id).await;
        assert_eq!(payable_row.amount, ap.credit, "应付金额 = 贷方合计");
    }

    // ---------- ⑦ 回写 / ⑧ 期间 ----------

    /// ⑦ 回写 `stock_in.voucher_id` / `payable_id`。
    #[tokio::test]
    async fn post_writes_back_voucher_and_payable_ids() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "1", "1", "0")]).await;
        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        let voucher = voucher_of(id).await;
        let payable_row = payable_of(id).await;
        assert_eq!(header.voucher_id, Some(voucher.id));
        assert_eq!(header.payable_id, Some(payable_row.id));
        assert_eq!(payable_row.voucher_id, Some(voucher.id), "应付单也要指向同一张凭证");
    }

    /// ⑧ 凭证 period = 收货日期所属会计期间（落在未开期间时退化成 yyyy-MM）。
    #[tokio::test]
    async fn voucher_period_follows_stock_in_date() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 2026-10 在 biz_accounting_period 里是 OPEN 期间 → 按区间命中
        let id = create_stock_in(
            &req_with_date(
                vec![line_req(MATERIAL_A, "1", "1", "0")],
                NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
            ),
            None,
        )
        .await
        .unwrap();
        change_stock_in_status(id, &post_req(), None).await.unwrap();
        assert_eq!(voucher_of(id).await.period, "2026-10");
        assert_eq!(payable_of(id).await.period_code, "2026-10");

        // 2025-03 没有期间记录 → 退化 yyyy-MM，不阻断过账
        let id2 = create_stock_in(
            &req_with_date(
                vec![line_req(MATERIAL_A, "1", "1", "0")],
                NaiveDate::from_ymd_opt(2025, 3, 15).unwrap(),
            ),
            None,
        )
        .await
        .unwrap();
        change_stock_in_status(id2, &post_req(), None).await.unwrap();
        assert_eq!(voucher_of(id2).await.period, "2025-03");
    }

    // ---------- ⑨ 规则优先 / ⑩ 默认科目 ----------

    /// ⑨ 科目映射：优先用 `biz_voucher_rule` 的规则（把借方改到测试科目）。
    #[tokio::test]
    async fn account_mapping_uses_voucher_rule() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        exec(&format!(
            "UPDATE biz_voucher_rule SET debit_account_id = '{TEST_ACCOUNT}'::uuid \
              WHERE rule_code = 'PURCHASE_IN_DEFAULT'"
        ))
        .await;
        let id = post_new(vec![line_req(MATERIAL_A, "10", "2", "0")]).await;
        let entries = entries_of(db(), voucher_of(id).await.id).await.unwrap();
        assert_eq!(
            entries[0].account.code, "P2C1.1405",
            "规则里的借方科目必须优先于默认 1405"
        );
        assert_eq!(entries[1].account.code, DEFAULT_PAYABLE_CODE);
    }

    /// ⑩ 找不到规则 → 用默认科目（1405 / 2221.01 / 2202）。
    #[tokio::test]
    async fn missing_rule_falls_back_to_default_accounts() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        exec("UPDATE biz_voucher_rule SET deleted_at = now() WHERE rule_code = 'PURCHASE_IN_DEFAULT'").await;
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let entries = entries_of(db(), voucher_of(id).await.id).await.unwrap();
        let codes: Vec<&str> = entries.iter().map(|row| row.account.code.as_str()).collect();
        assert_eq!(
            codes,
            vec![DEFAULT_INVENTORY_CODE, DEFAULT_TAX_CODE, DEFAULT_PAYABLE_CODE]
        );
    }

    /// ⑩b 规则与物料科目都没有 → 供应商 / 物料档案里的科目优先于硬编码默认值。
    #[tokio::test]
    async fn material_account_is_used_when_rule_missing() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        exec("UPDATE biz_voucher_rule SET deleted_at = now() WHERE rule_code = 'PURCHASE_IN_DEFAULT'").await;
        exec(&format!(
            "UPDATE biz_material SET inventory_account_id = '{TEST_ACCOUNT}'::uuid \
              WHERE id = '{MATERIAL_B}'::uuid"
        ))
        .await;
        let id = post_new(vec![line_req(MATERIAL_B, "10", "2", "0")]).await;
        let entries = entries_of(db(), voucher_of(id).await.id).await.unwrap();
        assert_eq!(entries[0].account.code, "P2C1.1405", "物料配的存货科目优先于 1405");
    }

    // ---------- ⑪⑫⑬ 红冲 ----------

    /// ⑪ 作废 → 生成红字凭证；借贷互换 ⇒ 净效果等于原凭证取负。
    #[tokio::test]
    async fn cancel_creates_red_voucher_with_negative_net_effect() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "100", "2", "13")]).await;
        let original = voucher_of(id).await;
        cancel_via_approval(id, "重复入库").await;

        // 红字凭证 = 同一来源、is_red=1 的另一张
        let red = db()
            .query_first_sql(
                format!(
                    "SELECT {VOUCHER_COLUMNS} FROM {TABLE} \
                      WHERE source_id = ?::uuid AND is_red = 1 AND deleted_at IS NULL \
                      ORDER BY created_at DESC LIMIT 1"
                ),
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .expect("作废必须生成红字凭证");
        let red = VoucherRow::from_row(&red).unwrap();
        assert_eq!(red.status, STATUS_POSTED);
        assert_eq!(red.reverse_of, Some(original.id));
        assert_eq!(red.total_debit, original.total_debit, "红字凭证金额与原凭证等额");

        let red_entries = entries_of(db(), red.id).await.unwrap();
        assert_eq!(red_entries.len(), 3);
        // 借贷互换：原来的借方(1405/2221.01) 变成贷方，原来的贷方(2202) 变成借方
        let debit_codes: Vec<&str> = red_entries
            .iter()
            .filter(|row| row.debit > Decimal::ZERO)
            .map(|row| row.account.code.as_str())
            .collect();
        assert_eq!(debit_codes, vec![DEFAULT_PAYABLE_CODE], "红字凭证借方 = 原贷方");
        let credit_total: Decimal = red_entries.iter().map(|row| row.credit).sum();
        assert_eq!(credit_total, dec("226"));

        // 净效果：Σ借 − Σ贷 = −226（等价于「金额取负」）
        let net = red.total_debit - red.total_credit;
        assert_eq!(net, Decimal::ZERO, "红字凭证自身仍然借贷平衡");
        let (rd, rc) = totals_of(
            &red_entries
                .iter()
                .map(|row| EntryLine {
                    line_no: row.line_no,
                    aux_dept_id: None,
                    summary: row.summary.clone(),
                    account: row.account.clone(),
                    debit: row.debit,
                    credit: row.credit,
                    aux_supplier_id: None,
                    aux_material_id: None,
                    aux_warehouse_id: None,
                })
                .collect::<Vec<_>>(),
        );
        // 原凭证 + 红字凭证合起来：每个科目净额为 0（蓝字 200 借 / 红字 200 贷）
        let original_entries = entries_of(db(), original.id).await.unwrap();
        let combine = |code: &str| -> Decimal {
            let d1: Decimal = original_entries
                .iter()
                .filter(|r| r.account.code == code)
                .map(|r| r.debit - r.credit)
                .sum();
            let d2: Decimal = red_entries
                .iter()
                .filter(|r| r.account.code == code)
                .map(|r| r.debit - r.credit)
                .sum();
            d1 + d2
        };
        assert_eq!(combine(DEFAULT_INVENTORY_CODE), Decimal::ZERO);
        assert_eq!(combine(DEFAULT_TAX_CODE), Decimal::ZERO);
        assert_eq!(combine(DEFAULT_PAYABLE_CODE), Decimal::ZERO);
        assert_eq!(rd - rc, Decimal::ZERO);
    }

    /// ⑫ 红字凭证 `reverse_of` 指向原凭证；原凭证 `reversed_by` 指向红字凭证、状态仍 POSTED。
    #[tokio::test]
    async fn red_voucher_links_back_to_original() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "5", "2", "0")]).await;
        let original = voucher_of(id).await;
        cancel_via_approval(id, "供应商取消").await;

        let refreshed = find_voucher(db(), original.id).await.unwrap().unwrap();
        assert_eq!(refreshed.status, STATUS_POSTED, "原凭证状态保持 POSTED（卡面 §五）");
        let red_id = refreshed.reversed_by.expect("原凭证必须打 reversed_by");
        let red = find_voucher(db(), red_id).await.unwrap().unwrap();
        assert_eq!(red.reverse_of, Some(original.id));
        assert_eq!(red.is_red, IS_RED_YES);
        assert_eq!(red.source_id, Some(id));
        // 重复红冲必须被拒（同一事务语义下不该出现两张红字凭证）
        let err = reverse_for_stock_in(
            db(),
            &stockin_model::find_stock_in(id).await.unwrap().unwrap(),
            Some("再来一次"),
            None,
            Utc::now(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, DbError::InvalidValue(ref msg) if msg.contains("已经红冲")),
            "{err:?}"
        );
    }

    /// ⑬ 作废 → 应付单 `CANCELLED`。
    #[tokio::test]
    async fn cancel_marks_payable_cancelled() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "10", "2", "13")]).await;
        assert_eq!(payable_of(id).await.status, payable::STATUS_OPEN);
        cancel_via_approval(id, "质检不合格退货").await;
        let row = payable_of(id).await;
        assert_eq!(row.status, payable::STATUS_CANCELLED);
        assert_eq!(row.amount, dec("22.6"), "作废不改金额，只改状态");
    }

    // ---------- ⑭ 事务性 / ⑮ DB CHECK ----------

    /// ⑭ 事务性：凭证生成失败 → 库存也不变（整单回滚，收货单回到 DRAFT）。
    #[tokio::test]
    async fn voucher_failure_rolls_back_inventory() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 停用进项税科目 → 解析科目失败 → create_auto 报错 → 整单回滚
        exec("UPDATE biz_account SET status = 0 WHERE account_code = '2221.01'").await;
        let id = create_stock_in(&req(vec![line_req(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        let err = change_stock_in_status(id, &post_req(), None)
            .await
            .expect_err("进项税科目停用时过账必须失败");
        assert!(err.msg.contains("科目"), "{}", err.msg);

        // 库存、流水、凭证、应付：一条都不能留
        let qty: Decimal = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(qty),0) FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![
                    Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
                    Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(qty, Decimal::ZERO, "凭证失败 → 库存回滚");
        let txns: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(txns, 0, "凭证失败 → 库存流水回滚");
        let vouchers: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(vouchers, 0);
        let payables: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_payable WHERE source_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(payables, 0);

        let header = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(header.status, "DRAFT", "过账失败必须回到 DRAFT（可重试）");
        assert_eq!(header.posted_at, None);

        // 恢复科目后可正常过账（同一张单重试成功）
        exec("UPDATE biz_account SET status = 1 WHERE account_code = '2221.01'").await;
        change_stock_in_status(id, &post_req(), None).await.unwrap();
        assert_eq!(voucher_of(id).await.entry_count, 3);
    }

    /// ⑮ 借贷不平衡的脏数据被 DB CHECK 拦住（绕过引擎直接写表也不行）。
    #[tokio::test]
    async fn db_rejects_unbalanced_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = db()
            .execute_with_params(
                "INSERT INTO biz_voucher (voucher_no, period, voucher_date, total_debit, total_credit, entry_count)
                 VALUES ('__P2C1_UNBALANCED__', '2026-10', DATE '2026-10-01', 100, 99, 2)",
                vec![],
            )
            .await
            .expect_err("借贷不平衡必须被 ck_biz_voucher_balance 拦住");
        assert!(
            matches!(err, DbError::CheckViolation { .. }),
            "期望 CheckViolation，实际 {err:?}"
        );
    }

    /// ⑮b 分录借贷双边同时有值被 DB CHECK 拦住。
    #[tokio::test]
    async fn db_rejects_entry_with_both_sides() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line_req(MATERIAL_A, "1", "1", "0")]).await;
        let voucher = voucher_of(id).await;
        let err = db()
            .execute_with_params(
                &format!(
                    "INSERT INTO biz_voucher_entry \
                       (voucher_id, line_no, summary, account_id, account_code, account_name, \
                        debit_amount, credit_amount) \
                     VALUES ('{}'::uuid, 99, '__P2C1_BOTH__', gen_random_uuid(), 'X', 'X', 10, 10)",
                    voucher.id
                ),
                vec![],
            )
            .await
            .expect_err("双边分录必须被 ck_biz_voucher_entry_side 拦住");
        assert!(
            matches!(err, DbError::CheckViolation { .. }),
            "期望 CheckViolation，实际 {err:?}"
        );
    }

    // ---------- 纯逻辑 ----------

    /// 凭证号格式：`PZ-yyyyMMdd-000001`（6 位补零，周期键按北京时间）。
    #[test]
    fn voucher_no_format_is_pz_prefix_with_six_digits() {
        assert_eq!(format_doc_no("PZ", "20261031", 1), "PZ-20261031-000001");
        assert_eq!(format_doc_no("PZ", "20261031", 123456), "PZ-20261031-123456");
        assert_eq!(format_doc_no("AP", "20261031", 7), "AP-20261031-000007");
        assert_eq!(VOUCHER_SEQ.prefix, "PZ");
        assert_eq!(VOUCHER_SEQ.seq_type, "PZ");
    }

    /// 纯逻辑：红字分录 = 借贷互换 + 行号重排 + 摘要加前缀。
    #[test]
    fn reversed_lines_swap_sides_and_keep_amounts() {
        let accounts = AccountSet {
            inventory: AccountRef {
                id: Uuid::nil(),
                code: "1405".to_string(),
                name: "库存商品".to_string(),
            },
            payable: AccountRef {
                id: Uuid::nil(),
                code: "2202".to_string(),
                name: "应付账款".to_string(),
            },
            tax: None,
        };
        let lines = purchase_in_lines(
            &accounts,
            VoucherAmounts {
                amount: dec("200"),
                tax: Decimal::ZERO,
                with_tax: dec("200"),
            },
            "采购入库 IN-1",
            LineContext {
                warehouse_id: None,
                material_id: None,
                supplier_id: None,
            },
            &[],
            &BTreeMap::new(),
            "IN-1",
        )
        .expect("回退分支必须能构造分录");
        assert_eq!(lines.len(), 2);
        let original: Vec<EntryRow> = lines
            .iter()
            .map(|line| EntryRow {
                id: Uuid::nil(),
                voucher_id: Uuid::nil(),
                aux_dept_id: line.aux_dept_id,
                line_no: line.line_no,
                summary: line.summary.clone(),
                account: line.account.clone(),
                debit: line.debit,
                credit: line.credit,
                aux_supplier_id: line.aux_supplier_id,
                aux_material_id: line.aux_material_id,
                aux_warehouse_id: line.aux_warehouse_id,
            })
            .collect();
        let red = reversed_lines(&original, Some("退货"));
        assert_eq!(red[0].debit, Decimal::ZERO);
        assert_eq!(red[0].credit, dec("200"));
        assert_eq!(red[1].debit, dec("200"));
        assert_eq!(red[1].credit, Decimal::ZERO);
        assert!(red[0].summary.starts_with("红冲（退货）"));
        assert_eq!(red[1].line_no, 2);
    }

    /// ㉖ 收尾清场：本模块的**科目夹具**（`P2C1.1405`）必须删掉（t_06c528fc）。
    ///
    /// 科目没有分类行，留在共享库里会让 `cash_flow` 的「分类配置覆盖全科目」断言
    /// 在**第二遍**全量时红。`biz_account` 无外键引用，直接删安全。
    #[tokio::test]
    async fn zz_teardown_purges_fixture_accounts() {
        let Some(_guard) = setup().await else {
            return;
        };
        db().execute_with_params("DELETE FROM biz_account WHERE account_code LIKE 'P2C1%'", vec![])
            .await
            .expect("清场删夹具科目失败");
        let left: i64 = db()
            .scalar_sql(
                "SELECT count(*) FROM biz_account WHERE account_code LIKE 'P2C1%'".to_string(),
                vec![],
            )
            .await
            .expect("计数失败");
        assert_eq!(left, 0, "夹具科目必须删掉（留着会让第二遍全量变红）");
    }

    /// ㉗ 收尾清场：本模块的**状态日志**（`STOCK_IN` / `PAYABLE` / `VOUCHER`）必须按 id 收干。
    ///
    /// 收货过账会同时写三类日志，而 `biz_status_log` 没有外键：清场里漏掉「删单据前先删
    /// 日志」这 20 行就成孤儿，永远收不回来（本卡实测 `voucher::tests` 每遍 +20 PAYABLE
    /// +20 VOUCHER）。`zz_` 排在本模块最后，是这一块的收口。
    #[tokio::test]
    async fn zz_teardown_purges_finance_status_logs() {
        let Some(_guard) = setup().await else {
            return;
        };
        let stock_in_id = post_new(vec![line_req(MATERIAL_A, "2", "100", "13")]).await;
        let payable = payable_of(stock_in_id).await;
        let voucher = voucher_of(stock_in_id).await;

        let count_logs = |entity_type: &'static str, target: Uuid| async move {
            db().scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = ? AND entity_id = ?::uuid",
                vec![Value::from(entity_type), Value::from(&target)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0)
        };
        let targets = [
            ("STOCK_IN", stock_in_id),
            ("PAYABLE", payable.id),
            ("VOUCHER", voucher.id),
        ];
        for (label, target) in targets {
            assert!(
                count_logs(label, target).await > 0,
                "清场前必须有 {label} 状态日志，否则这条用例证明不了什么"
            );
        }

        // 走模块真实的清场
        cleanup_fixtures().await;

        for (label, target) in targets {
            assert_eq!(
                count_logs(label, target).await,
                0,
                "清场后 {label} 状态日志必须为 0（日志无外键，必须按 id 先删）"
            );
        }

        // 兜底：整库不再有 PAYABLE / VOUCHER 的孤儿行
        // 兜底：整库最终不许留 PAYABLE / VOUCHER 的孤儿行。
        // 并行跑时别的模块可能正处在「单据已删、它自己的日志还没删」的毫秒级中间态，
        // 一次断言会把别人的中间态判成本模块假红 ⇒ 收敛式判定（`settle_orphans`）：
        // 真孤儿等多久都还在，仍然会红；只吸收别人正在删的瞬时窗口。
        let left = fixture_hygiene::settle_orphans(fixture_hygiene::FINANCE, std::time::Duration::from_secs(5)).await;
        assert_eq!(
            left, 0,
            "整库不许留 PAYABLE / VOUCHER 的孤儿状态日志（收敛等待 5s 后仍非 0）"
        );
    }

    // ==================== SO-22：销售收入确认凭证 ====================

    /// 清掉本卡用例造的凭证（分录 → 状态日志 → 凭证）。
    ///
    /// 红字凭证的 `source_no` 继承原单号，所以一个前缀就能把蓝红两张都带走。
    async fn cleanup_sales_revenue() {
        let sqls = [
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'SO22-%')",
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'SO22-%')",
            "DELETE FROM biz_voucher WHERE source_no LIKE 'SO22-%'",
        ];
        for sql in sqls {
            exec(sql).await;
        }
    }

    fn so22_invoice(no: u32) -> Uuid {
        Uuid::from_str(&format!("b2c10022-0000-4000-8000-{no:012}")).expect("测试发票 id 非法")
    }

    /// `(line_no, account_code, debit, credit)` 逐行读回来。
    async fn so22_entries(voucher_id: &Uuid) -> Vec<(i32, String, Decimal, Decimal)> {
        let rows = db()
            .query_sql(
                "SELECT line_no, account_code, COALESCE(debit_amount, 0) AS d, \
                        COALESCE(credit_amount, 0) AS c \
                   FROM biz_voucher_entry WHERE voucher_id = ?::uuid ORDER BY line_no"
                    .to_string(),
                vec![Value::from(voucher_id)],
            )
            .await
            .expect("查凭证分录");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed::<i32>("line_no").expect("line_no"),
                    row.get_typed::<String>("account_code").expect("account_code"),
                    row.get_typed::<Decimal>("d").expect("debit"),
                    row.get_typed::<Decimal>("c").expect("credit"),
                )
            })
            .collect()
    }

    /// 纯函数：三个金额自洽 ⇒ 通过；差额 < 0.01 ⇒ 吸收；≥ 0.01 ⇒ 拒绝。
    #[test]
    fn sales_revenue_amounts_absorbs_under_a_cent_and_rejects_a_cent() {
        let (revenue, tax) = sales_revenue_amounts("SO22-U-1", dec("1130"), dec("1000"), dec("130"))
            .expect("1130 = 1000 + 130 必须通过");
        assert_eq!(revenue, dec("1000"));
        assert_eq!(tax, dec("130"));
        assert_eq!(revenue + tax, dec("1130"), "借贷必须相等");

        // 尾差 0.004 < 0.01 ⇒ 吸收进销项税（含税 − 不含税），借贷分毫不差
        let (revenue, tax) = sales_revenue_amounts("SO22-U-2", dec("1130"), dec("1000"), dec("129.9960"))
            .expect("尾差 < 0.01 应吸收而不是报错");
        assert_eq!(revenue, dec("1000"));
        assert_eq!(revenue + tax, dec("1130"), "吸收后借贷必须严格相等");
        assert_eq!(tax, dec("130"));

        // 差额恰好 0.01 ⇒ 拒绝（「< 0.01 才算平衡」，等于 0.01 就不算）
        let err =
            sales_revenue_amounts("SO22-U-3", dec("1130"), dec("1000"), dec("129.99")).expect_err("差额 0.01 必须拒绝");
        let text = format!("{err}");
        assert!(text.contains("不自洽"), "文案要说清金额不自洽：{text}");

        // 负数一律拒绝（红字必须走红字凭证，不许拿负数写蓝字）
        assert!(
            sales_revenue_amounts("SO22-U-4", dec("1130"), dec("-1000"), dec("130")).is_err(),
            "负数不含税必须拒绝"
        );
        assert!(
            sales_revenue_amounts("SO22-U-5", dec("1130"), dec("1000"), dec("-130")).is_err(),
            "负数税额必须拒绝"
        );
        println!("[SO22-①] 价税自洽 / 尾差吸收 / 0.01 拒绝 / 负数拒绝 全部通过 ✓");
    }

    /// 纯函数：拆行复用 `normalize`（合并同物料 + 升序）+ 尾差吸收。
    #[test]
    fn sales_revenue_split_merges_same_material_and_absorbs_tail() {
        let mat_a = Uuid::from_str(MATERIAL_A).expect("物料 A");
        let mat_b = Uuid::from_str(MATERIAL_B).expect("物料 B");
        let line = |material_id: Uuid, amount: &str, from: i32, to: i32| VoucherMaterialLine {
            material_id,
            qty: Decimal::ZERO,
            amount: dec(amount),
            line_from: from,
            line_to: to,
        };
        // 明细：A 400 + A 100（同物料 ⇒ 合并 500）+ B 499.99；表头不含税 1000
        let split = sales_revenue_split(
            "SO22-U-6",
            dec("1000"),
            &[
                line(mat_b, "499.99", 3, 3),
                line(mat_a, "400", 1, 1),
                line(mat_a, "100", 2, 2),
            ],
        )
        .expect("拆分应成功");
        assert_eq!(split.len(), 2, "同物料必须合并成一行");
        assert_eq!(split[0].material_id, mat_a, "必须按物料 id 升序");
        assert_eq!(split[0].amount, dec("500"), "A 的 400 + 100 应合并成 500");
        assert_eq!(split[0].line_from, 1);
        assert_eq!(split[0].line_to, 2);
        let sum: Decimal = split.iter().map(|l| l.amount).sum();
        assert_eq!(sum, dec("1000"), "拆行后合计必须等于表头不含税金额（尾差吸收）");
        println!("[SO22-②] 合并 400+100=500 / 升序 / 尾差 0.01 吸收 → Σ=1000 ✓");
    }

    /// 真库：正常凭证 —— 借 1122 1130 / 贷 5001 1000 / 贷 2221.02 130，借贷相等。
    #[tokio::test]
    async fn sales_revenue_voucher_posts_balanced_entries() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup_sales_revenue().await;
        let now = Utc::now();
        let voucher_id = create_for_sales_revenue(
            db(),
            &SalesRevenueVoucher {
                invoice_id: so22_invoice(1),
                invoice_no: "SO22-T-0001",
                voucher_date: today_cst(now),
                amount: dec("1000"),
                tax_amount: dec("130"),
                total_with_tax: dec("1130"),
                material_lines: Vec::new(),
                summary: "SO22 单测：正常收入凭证".to_string(),
                remark: Some("SO22 单测"),
                base: None,
            },
            None,
            now,
        )
        .await
        .expect("收入凭证应生成")
        .expect("应有凭证 id");

        let entries = so22_entries(&voucher_id).await;
        assert_eq!(entries.len(), 3, "应为 借1 + 贷2 共 3 行：{entries:?}");
        assert_eq!(entries[0].1, ACCOUNT_AR);
        assert_eq!(entries[0].2, dec("1130"), "借 1122 应为价税合计");
        assert_eq!(entries[0].3, Decimal::ZERO);
        assert_eq!(entries[1].1, ACCOUNT_REVENUE);
        assert_eq!(entries[1].3, dec("1000"), "贷 5001 应为不含税金额");
        assert_eq!(entries[2].1, ACCOUNT_OUTPUT_TAX);
        assert_eq!(entries[2].3, dec("130"), "贷 2221.02 应为税额");
        let debit: Decimal = entries.iter().map(|e| e.2).sum();
        let credit: Decimal = entries.iter().map(|e| e.3).sum();
        assert_eq!(debit, credit, "借贷必须相等");
        assert_eq!(debit, dec("1130"));

        let header = db()
            .query_first_sql(
                "SELECT source_type, source_no, status, total_debit, total_credit, entry_count \
                   FROM biz_voucher WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查凭证头")
            .expect("凭证头应存在");
        assert_eq!(header.get_typed::<String>("source_type").unwrap(), SOURCE_INVOICE);
        assert_eq!(header.get_typed::<String>("source_no").unwrap(), "SO22-T-0001");
        assert_eq!(header.get_typed::<String>("status").unwrap(), STATUS_POSTED);
        assert_eq!(header.get_typed::<i32>("entry_count").unwrap(), 3);
        cleanup_sales_revenue().await;
        println!("[SO22-③] 真库 3 行分录 借1122 1130 / 贷5001 1000 / 贷2221.02 130，借贷各 1130 ✓");
    }

    /// 真库：多行科目 —— 按商品拆 2 行收入 + 1 行税，合计仍等于不含税 1000。
    #[tokio::test]
    async fn sales_revenue_voucher_splits_revenue_by_material() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup_sales_revenue().await;
        let now = Utc::now();
        let line = |material: &str, amount: &str| VoucherMaterialLine {
            material_id: Uuid::from_str(material).expect("物料 id"),
            qty: Decimal::ZERO,
            amount: dec(amount),
            line_from: 1,
            line_to: 1,
        };
        let voucher_id = create_for_sales_revenue(
            db(),
            &SalesRevenueVoucher {
                invoice_id: so22_invoice(2),
                invoice_no: "SO22-T-0002",
                voucher_date: today_cst(now),
                amount: dec("1000"),
                tax_amount: dec("130"),
                total_with_tax: dec("1130"),
                material_lines: vec![line(MATERIAL_B, "600"), line(MATERIAL_A, "400")],
                summary: "SO22 单测：多行科目".to_string(),
                remark: None,
                base: None,
            },
            None,
            now,
        )
        .await
        .expect("多行收入凭证应生成")
        .expect("应有凭证 id");

        let entries = so22_entries(&voucher_id).await;
        assert_eq!(entries.len(), 4, "应为 借1 + 贷(收入2) + 贷(税1) = 4 行：{entries:?}");
        let revenue_lines: Vec<&(i32, String, Decimal, Decimal)> =
            entries.iter().filter(|e| e.1 == ACCOUNT_REVENUE).collect();
        assert_eq!(revenue_lines.len(), 2, "收入必须按商品拆成 2 行");
        let revenue_sum: Decimal = revenue_lines.iter().map(|e| e.3).sum();
        assert_eq!(revenue_sum, dec("1000"), "两行收入合计必须等于不含税金额");
        let debit: Decimal = entries.iter().map(|e| e.2).sum();
        let credit: Decimal = entries.iter().map(|e| e.3).sum();
        assert_eq!(debit, credit, "拆分后借贷仍必须相等");
        assert_eq!(credit, dec("1130"));
        cleanup_sales_revenue().await;
        println!("[SO22-④] 真库多行科目：收入拆 2 行 Σ=1000 + 税 130，借贷各 1130 ✓");
    }

    /// 真库：期间已结账 ⇒ 拒绝（文案点明「已结账」），且库里不留半张凭证。
    #[tokio::test]
    async fn sales_revenue_voucher_rejects_closed_period() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup_sales_revenue().await;
        let period = "2031-03";
        exec(&format!(
            "DELETE FROM biz_accounting_period WHERE period_code = '{period}'"
        ))
        .await;
        exec(&format!(
            "INSERT INTO biz_accounting_period \
                (id, period_year, period_month, period_code, start_date, end_date, status) \
             VALUES (gen_random_uuid(), 2031, 3, '{period}', '2031-03-01', '2031-03-31', 'CLOSED')"
        ))
        .await;
        let now = Utc::now();
        let err = create_for_sales_revenue(
            db(),
            &SalesRevenueVoucher {
                invoice_id: so22_invoice(3),
                invoice_no: "SO22-T-0003",
                voucher_date: NaiveDate::from_ymd_opt(2031, 3, 15).expect("合法日期"),
                amount: dec("1000"),
                tax_amount: dec("130"),
                total_with_tax: dec("1130"),
                material_lines: Vec::new(),
                summary: "SO22 单测：已结账期间".to_string(),
                remark: None,
                base: None,
            },
            None,
            now,
        )
        .await
        .expect_err("期间已结账必须拒绝");
        let text = format!("{err}");
        assert!(text.contains("已结账"), "文案必须说明期间已结账：{text}");
        assert!(text.contains(period), "文案必须点名期间 {period}：{text}");
        let leaked = db()
            .query_first_sql(
                "SELECT COUNT(*) AS n FROM biz_voucher WHERE source_no = 'SO22-T-0003'".to_string(),
                vec![],
            )
            .await
            .expect("查残留凭证")
            .expect("count 行");
        assert_eq!(
            leaked.get_typed::<i64>("n").unwrap(),
            0,
            "被拒的凭证不能落库（半张凭证 = 账实不符）"
        );
        exec(&format!(
            "DELETE FROM biz_accounting_period WHERE period_code = '{period}'"
        ))
        .await;
        cleanup_sales_revenue().await;
        println!("[SO22-⑤] 真库已结账期间拒绝：{text} ✓（无残留凭证）");
    }

    /// 真库：价税合计为 0 ⇒ 不生成凭证（`Ok(None)`），也不报错。
    #[tokio::test]
    async fn sales_revenue_voucher_skips_zero_amount() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup_sales_revenue().await;
        let now = Utc::now();
        let got = create_for_sales_revenue(
            db(),
            &SalesRevenueVoucher {
                invoice_id: so22_invoice(4),
                invoice_no: "SO22-T-0004",
                voucher_date: today_cst(now),
                amount: Decimal::ZERO,
                tax_amount: Decimal::ZERO,
                total_with_tax: Decimal::ZERO,
                material_lines: Vec::new(),
                summary: "SO22 单测：零金额".to_string(),
                remark: None,
                base: None,
            },
            None,
            now,
        )
        .await
        .expect("零金额不该报错");
        assert!(got.is_none(), "零金额发票不该产出凭证");
        cleanup_sales_revenue().await;
        println!("[SO22-⑥] 真库零金额 → Ok(None)，不留凭证 ✓");
    }

    /// 真库：出库作废冲销应收（`CANCELLED`）时，收入凭证必须一起红冲 ——
    /// 否则「应收冲销了、总账上 1122 还挂着」。
    #[tokio::test]
    async fn receivable_cancel_reds_the_revenue_voucher() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup_sales_revenue().await;
        exec(
            "DELETE FROM biz_status_log WHERE entity_type = 'RECEIVABLE' AND entity_id IN \
             (SELECT id FROM biz_receivable WHERE ar_no LIKE 'SO22-AR-%')",
        )
        .await;
        exec("DELETE FROM biz_receivable WHERE ar_no LIKE 'SO22-AR-%'").await;

        let now = Utc::now();
        let customer = "b2c10022-0000-4000-8000-000000000099";
        exec(&format!(
            "INSERT INTO biz_customer (id, code, name, status) \
             VALUES ('{customer}', 'SO22-CUST', 'SO22 单测客户', 'ACTIVE') ON CONFLICT DO NOTHING"
        ))
        .await;

        // ① 开票时生成的蓝字收入凭证
        let blue = create_for_sales_revenue(
            db(),
            &SalesRevenueVoucher {
                invoice_id: so22_invoice(9),
                invoice_no: "SO22-T-0009",
                voucher_date: today_cst(now),
                amount: dec("1000"),
                tax_amount: dec("130"),
                total_with_tax: dec("1130"),
                material_lines: Vec::new(),
                summary: "SO22 单测：应收冲销的红冲".to_string(),
                remark: None,
                base: None,
            },
            None,
            now,
        )
        .await
        .expect("蓝字收入凭证应生成")
        .expect("应有凭证 id");

        // ② 一条「已开票、一分没收」的应收，挂上收入凭证
        let ar_id = db()
            .query_first_sql(
                "INSERT INTO biz_receivable \
                    (ar_no, customer_id, source_type, source_id, source_no, currency_code, amount, \
                     received_amount, unsettled_amount, credit_days, due_date, period_code, status, \
                     voucher_id) \
                 VALUES ('SO22-AR-0001', ?::uuid, 'STOCK_OUT', gen_random_uuid(), 'SO22-SO-0001', \
                         'CNY', 1130, 0, 1130, 30, ?::text::date, '2026-11', 'OPEN', ?::uuid) \
                 RETURNING id"
                    .to_string(),
                vec![
                    Value::from(&Uuid::from_str(customer).expect("客户 id")),
                    Value::Text(today_cst(now).to_string()),
                    Value::from(&blue),
                ],
            )
            .await
            .expect("建应收")
            .expect("应收 id")
            .get_typed::<Uuid>("id")
            .expect("应收主键");

        // ③ 出库作废 ⇒ 应收冲销（真实入口，内部同一事务里红冲收入凭证）
        let row = crate::business::erp::receivable::cancel_for_stock_out(db(), ar_id, "出库作废：SO22 单测", None, now)
            .await
            .expect("应收冲销应成功");
        assert_eq!(row.status, "CANCELLED");

        // ④ 蓝 + 红两张凭证，红字反向
        let rows = db()
            .query_sql(
                "SELECT id, is_red, reverse_of FROM biz_voucher \
                  WHERE source_no = 'SO22-T-0009' ORDER BY is_red"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查蓝红凭证");
        assert_eq!(rows.len(), 2, "应有一张蓝字 + 一张红字：{rows:?}");
        let blue_id = rows[0].get_typed::<Uuid>("id").expect("蓝字 id");
        let red_id = rows[1].get_typed::<Uuid>("id").expect("红字 id");
        assert_eq!(rows[0].get_typed::<i16>("is_red").unwrap(), IS_RED_NO);
        assert_eq!(rows[1].get_typed::<i16>("is_red").unwrap(), IS_RED_YES);
        assert_eq!(
            rows[1].get_typed::<Uuid>("reverse_of").ok(),
            Some(blue_id),
            "红字凭证必须指回蓝字原凭证"
        );

        let blue_entries = so22_entries(&blue_id).await;
        let red_entries = so22_entries(&red_id).await;
        assert_eq!(red_entries.len(), blue_entries.len(), "红字行数应与蓝字一致");
        for (blue_line, red_line) in blue_entries.iter().zip(red_entries.iter()) {
            assert_eq!(blue_line.1, red_line.1, "科目顺序应一致");
            assert_eq!(blue_line.2, red_line.3, "红字的贷方 = 蓝字的借方（反向）");
            assert_eq!(blue_line.3, red_line.2, "红字的借方 = 蓝字的贷方（反向）");
        }

        // ⑤ 清场
        exec(
            "DELETE FROM biz_status_log WHERE entity_type = 'RECEIVABLE' AND entity_id IN \
             (SELECT id FROM biz_receivable WHERE ar_no LIKE 'SO22-AR-%')",
        )
        .await;
        exec("DELETE FROM biz_receivable WHERE ar_no LIKE 'SO22-AR-%'").await;
        cleanup_sales_revenue().await;
        println!("[SO22-⑦] 真库出库作废 → 应收 CANCELLED + 收入凭证红冲（反向分录逐行核对） ✓");
    }

    // ==================== SO-41：凭证本位币入账（ARCH-7 §15.6.4 ②） ====================

    /// 清掉本卡用例造的凭证（分录 → 状态日志 → 凭证）。
    async fn cleanup_so41() {
        let sqls = [
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'SO41-%')",
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'SO41-%')",
            "DELETE FROM biz_voucher WHERE source_no LIKE 'SO41-%'",
        ];
        for sql in sqls {
            exec(sql).await;
        }
    }

    /// `(account_code, debit, credit)` 逐行读回来。
    async fn so41_entries(voucher_id: &Uuid) -> Vec<(String, Decimal, Decimal)> {
        let rows = db()
            .query_sql(
                "SELECT line_no, account_code, COALESCE(debit_amount, 0) AS d, \
                        COALESCE(credit_amount, 0) AS c \
                   FROM biz_voucher_entry WHERE voucher_id = ?::uuid ORDER BY line_no"
                    .to_string(),
                vec![Value::from(voucher_id)],
            )
            .await
            .expect("查凭证分录");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed::<String>("account_code").expect("account_code"),
                    row.get_typed::<Decimal>("d").expect("debit"),
                    row.get_typed::<Decimal>("c").expect("credit"),
                )
            })
            .collect()
    }

    /// 纯逻辑：本位币三连的入账前校验（非负 + 恒等式 + 尾差归含税）。
    #[test]
    fn so41_validate_base_amounts_enforces_identity_and_sign() {
        let ok = validate_base_amounts(
            "SO41-T-0001",
            BaseAmounts {
                amount: dec("7100"),
                tax: dec("923"),
                with_tax: dec("8023"),
            },
        )
        .expect("恒等式成立的三连必须通过");
        assert_eq!(ok.amount, dec("7100"));
        assert_eq!(ok.tax, dec("923"));
        assert_eq!(ok.with_tax, dec("8023"));

        let tail = validate_base_amounts(
            "SO41-T-0002",
            BaseAmounts {
                amount: dec("7100.0001"),
                tax: dec("923.0001"),
                with_tax: dec("8023.0044"),
            },
        )
        .expect("尾差 < 0.01 必须被吸收而不是拒绝");
        assert_eq!(
            tail.with_tax,
            dec("8023.0002"),
            "含税取前两项之和（与 currency::BaseAmounts::of 同口径，尾差落含税列）"
        );

        let err = validate_base_amounts(
            "SO41-T-0003",
            BaseAmounts {
                amount: dec("7100"),
                tax: dec("923"),
                with_tax: dec("8000"),
            },
        )
        .expect_err("含税 ≠ 不含税 + 税额（差 123）必须拒绝");
        let text = format!("{err}");
        assert!(text.contains("不自洽"), "文案必须点明不自洽：{text}");
        assert!(text.contains("SO41-T-0003"), "文案必须点名单据：{text}");

        let err = validate_base_amounts(
            "SO41-T-0004",
            BaseAmounts {
                amount: dec("-1"),
                tax: dec("0"),
                with_tax: dec("-1"),
            },
        )
        .expect_err("负数本位币金额必须拒绝（红字走红冲）");
        assert!(format!("{err}").contains("不能为负"));

        println!("[SO41-⓪] 纯逻辑：本位币三连校验（非负 / 恒等式 / 尾差归含税） ✓");
    }

    /// 纯逻辑：本位币收入按原币商品构成等比拆分，尾差吸收后 Σ = 本位币不含税。
    #[test]
    fn so41_split_in_base_scales_material_lines() {
        let air = |material: &str, amount: &str| VoucherMaterialLine {
            material_id: Uuid::from_str(material).expect("物料 id"),
            qty: dec("1"),
            amount: dec(amount),
            line_from: 1,
            line_to: 1,
        };
        // 原币 1000（400 + 600）@ 7.1 ⇒ 本位币 7100（2840 + 4260）
        let split = sales_revenue_split_in_base(
            "SO41-T-0005",
            dec("1000"),
            dec("7100"),
            &[air(MATERIAL_A, "400"), air(MATERIAL_B, "600")],
        )
        .expect("按比例拆分应成功");
        assert_eq!(split.len(), 2, "两个物料 ⇒ 两行收入：{split:?}");
        let sum: Decimal = split.iter().map(|line| line.amount).sum();
        assert_eq!(sum, dec("7100"), "拆行合计必须等于本位币不含税金额");
        assert_eq!(split[0].amount, dec("2840"), "400 × 7.1 = 2840");
        assert_eq!(split[1].amount, dec("4260"), "600 × 7.1 = 4260");

        // 尾差场景：原币 300 拆 100 + 200，本位币 2131（比例为 7.1033…）
        let split = sales_revenue_split_in_base(
            "SO41-T-0006",
            dec("300"),
            dec("2131"),
            &[air(MATERIAL_A, "100"), air(MATERIAL_B, "200")],
        )
        .expect("尾差应被吸收");
        let sum: Decimal = split.iter().map(|line| line.amount).sum();
        assert_eq!(sum, dec("2131"), "尾差必须吸收到最后一行");

        // 退化：没有商品行 ⇒ 空 vec（调用方回退「贷 1 行」）
        assert!(
            sales_revenue_split_in_base("SO41-T-0007", dec("1000"), dec("7100"), &[])
                .expect("空行不该报错")
                .is_empty()
        );
        // 退化：原币不含税 ≤ 0 ⇒ 也不拆（不许除零/天文数字）
        assert!(
            sales_revenue_split_in_base("SO41-T-0008", dec("0"), dec("7100"), &[air(MATERIAL_A, "10")])
                .expect("原币为 0 不该报错")
                .is_empty()
        );

        println!("[SO41-⓪] 纯逻辑：本位币收入等比拆分 + 尾差吸收 + 两种退化 ✓");
    }

    /// 真库：**USD 发票 ⇒ 凭证金额 = 原币 × 锁定汇率**（1000 USD @ 7.1 =
    /// 借 1122 8023 / 贷 5001 7100 / 贷 2221.02 923），不是原币金额。
    #[tokio::test]
    async fn so41_revenue_voucher_books_base_currency_amounts() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup_so41().await;
        let now = Utc::now();
        // 原币 1000 + 税 130 = 1130 USD；锁定汇率 7.1 ⇒ 本位币 7100 / 923 / 8023
        let base = BaseAmounts::of(dec("1000"), dec("130"), dec("7.1"));
        assert_eq!(base.with_tax, dec("8023"), "USD 1130 × 7.1 = 8023");

        let voucher_id = create_for_sales_revenue(
            db(),
            &SalesRevenueVoucher {
                invoice_id: so22_invoice(41),
                invoice_no: "SO41-T-0001",
                voucher_date: today_cst(now),
                amount: dec("1000"),
                tax_amount: dec("130"),
                total_with_tax: dec("1130"),
                material_lines: Vec::new(),
                summary: "SO41 单测：USD 发票按本位币入账".to_string(),
                remark: None,
                base: Some(base),
            },
            None,
            now,
        )
        .await
        .expect("USD 收入凭证应生成")
        .expect("应有凭证 id");

        let entries = so41_entries(&voucher_id).await;
        assert_eq!(entries.len(), 3, "借 1122 + 贷 5001 + 贷 2221.02：{entries:?}");
        assert_eq!(entries[0].0, ACCOUNT_AR, "借方必须是 1122 应收账款");
        assert_eq!(
            entries[0].1,
            dec("8023"),
            "借方 = 价税合计 × 汇率（8023，不是原币 1130）"
        );
        assert_eq!(entries[1].0, ACCOUNT_REVENUE);
        assert_eq!(entries[1].2, dec("7100"), "贷方收入 = 1000 × 7.1");
        assert_eq!(entries[2].0, ACCOUNT_OUTPUT_TAX);
        assert_eq!(entries[2].2, dec("923"), "贷方税额 = 130 × 7.1");
        let debit: Decimal = entries.iter().map(|entry| entry.1).sum();
        let credit: Decimal = entries.iter().map(|entry| entry.2).sum();
        assert_eq!(debit, credit, "借贷必须相等");

        cleanup_so41().await;
        println!("[SO41-①] 真库 USD 发票：借 1122 8023 / 贷 5001 7100 / 贷 2221.02 923 ✓");
    }

    /// 真库：出库成本结转凭证**本来就是本位币**（成本账 `biz_inventory.amount` 无币种），
    /// 所以外币订单的出库单也**不许**再乘一次汇率 —— 凭证金额 = 账上真实扣减额。
    #[tokio::test]
    async fn so41_stock_out_voucher_stays_in_base_currency() {
        let Some(_guard) = setup().await else {
            return;
        };
        cleanup_so41().await;
        let now = Utc::now();
        let doc_id = Uuid::from_str("b2c10041-0000-4000-8000-000000000001").expect("单据 id");
        let voucher_id = create_for_stock_out(
            db(),
            &StockOutVoucher {
                doc_id,
                doc_no: "SO41-T-0100",
                voucher_date: today_cst(now),
                out_type: "ISSUE",
                debit_code: ACCOUNT_MAIN_COST,
                debit_name: "主营业务成本",
                // 账上真实扣减额：500（本位币）。即使来源订单是 USD，成本账也不分币种，
                // 传递进来的就必须是人民币，本函数不做任何换算。
                material_lines: vec![VoucherMaterialLine {
                    material_id: Uuid::from_str(MATERIAL_A).expect("物料 id"),
                    qty: dec("5"),
                    amount: dec("500"),
                    line_from: 1,
                    line_to: 1,
                }],
                aux_material_id: Some(Uuid::from_str(MATERIAL_A).expect("物料 id")),
                warehouse_id: Uuid::from_str(WAREHOUSE).expect("仓库 id"),
                amount: dec("500"),
                summary: "SO41 单测：出库成本结转（本位币）".to_string(),
            },
            None,
            now,
        )
        .await
        .expect("成本结转凭证应生成")
        .expect("应有凭证 id");

        let entries = so41_entries(&voucher_id).await;
        assert_eq!(entries.len(), 2, "借 6401 + 贷 1405：{entries:?}");
        assert_eq!(entries[0].0, ACCOUNT_MAIN_COST);
        assert_eq!(entries[0].1, dec("500"), "借方成本 = 账上扣减额（不得再乘汇率）");
        assert_eq!(entries[1].0, DEFAULT_INVENTORY_CODE);
        assert_eq!(entries[1].2, dec("500"));

        cleanup_so41().await;
        println!("[SO41-②] 真库出库成本凭证：借 6401 500 / 贷 1405 500（本位币，不换算） ✓");
    }

    // ==================== FIX-8：取号器并发 + 按单据类型独立（t_6ddb0036） ====================
    //
    // 卡面问题 1「取号器是不是全局单行」的结论：**不是**。`biz_seq` 是「一个 `seq_type` 一行」
    // （`uk_biz_seq_type` 部分唯一索引，V0137 §1.1），调拨单号跳到 `TK-20261107-000093`
    // 是并行的单测 / 演练把 `TK` 这一行推上去了 —— 跳号允许（ERP 只禁重号）。
    //
    // 但「本来没坏」不等于「有证据」：取号代码是 `UPDATE … RETURNING`（单语句原子），
    // 一旦有人把它改成「先 SELECT 再 UPDATE」就会同时冒重号，且只看代码看不出来。
    // 所以把口径钉成两条可执行断言：
    //   ① 同一号段 10 个**并发事务**取号 → 号码两两不同，计数器正好前进 10；
    //   ② 不同 `seq_type` 各自递增、互不影响（A 抢 6 个号时 B 一个号都不动）。
    //
    // 都走 `next_doc_no`（生产唯一入口：盘点 / 凭证 / 应付 / 销售 / 调拨……全部经它），
    // 因此证明的是框架级取号语义，不是某个单据的私有实现。

    /// `biz_seq` 的读状态（`(period_key, current_no)`），`None` = 这行不存在。
    async fn seq_state(seq_type: &str) -> Option<(String, i64)> {
        let row = db()
            .query_first_sql(
                "SELECT period_key, current_no FROM biz_seq \
                  WHERE seq_type = ? AND deleted_at IS NULL"
                    .to_string(),
                vec![Value::Text(seq_type.to_string())],
            )
            .await
            .unwrap_or_else(|err| panic!("读 biz_seq({seq_type}) 失败：{err}"));
        row.as_ref().map(|row| {
            (
                row.get_typed::<String>("period_key").expect("period_key"),
                row.get_typed::<i64>("current_no").expect("current_no"),
            )
        })
    }

    /// 全部号段的 `(seq_type, current_no)` 快照（用来自证「别的号段纹丝不动」）。
    async fn seq_snapshot() -> std::collections::BTreeMap<String, i64> {
        let rows = db()
            .query_sql(
                "SELECT seq_type, current_no FROM biz_seq WHERE deleted_at IS NULL".to_string(),
                vec![],
            )
            .await
            .expect("读 biz_seq 全表失败");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed::<String>("seq_type").expect("seq_type"),
                    row.get_typed::<i64>("current_no").expect("current_no"),
                )
            })
            .collect()
    }

    /// FIX-8 用例专用号段（`seq_type` 无白名单约束；用例开头结尾都删，不污染共享库）。
    const FIX8_CONC: SeqSpec<'static> = SeqSpec {
        seq_type: "FIX8_CONC",
        prefix: "F8",
        seq_name: "FIX-8 并发取号",
        remark: "FIX-8 单测专用号段：10 并发取号不重号",
    };
    const FIX8_A: SeqSpec<'static> = SeqSpec {
        seq_type: "FIX8_A",
        prefix: "FA",
        seq_name: "FIX-8 号段 A",
        remark: "FIX-8 单测专用号段：验证按单据类型独立递增",
    };
    const FIX8_B: SeqSpec<'static> = SeqSpec {
        seq_type: "FIX8_B",
        prefix: "FB",
        seq_name: "FIX-8 号段 B",
        remark: "FIX-8 单测专用号段：验证按单据类型独立递增",
    };

    async fn drop_fix8_seq_rows() {
        exec("DELETE FROM biz_seq WHERE seq_type LIKE 'FIX8\\_%'").await;
    }

    /// 验收①：10 个**并发事务**取同一个号段，号码不许重号。
    ///
    /// 用「事务 + 提交」而不是裸语句，是因为生产路径（建单）就是这样：事务持有 `biz_seq`
    /// 那一行的行锁直到提交，后来者必须等 —— 这正是「重号」最容易漏掉的地方。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fix8_concurrent_takes_never_duplicate() {
        let Some(_guard) = setup().await else {
            return;
        };
        let now = Utc::now();
        let period_key = serial_model::period_key_of(now);
        drop_fix8_seq_rows().await;

        let mut handles = Vec::new();
        for _ in 0..10 {
            handles.push(tokio::spawn(async move {
                let tx = db().begin().await.expect("开事务");
                let no = next_doc_no(&tx, &FIX8_CONC, now).await.expect("并发取号不应失败");
                tx.commit().await.expect("提交");
                no
            }));
        }
        let mut numbers = Vec::new();
        for handle in handles {
            numbers.push(handle.await.expect("取号任务 panic"));
        }

        let mut unique = numbers.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            10,
            "10 个并发取号必须两两不同（重号是生产事故）：{numbers:?}"
        );
        // 号码必须是同一号段的连续 1..=10：不是「各自从 0 起」也不是「跳着走」
        let expected: Vec<String> = (1..=10).map(|no| format_doc_no("F8", &period_key, no)).collect();
        assert_eq!(unique, expected, "并发取到的应是 {period_key} 号段的 1..=10");
        // 计数器只前进 10 步：多推 = 有人读了旧值再写（丢号不算错，但说明实现退化）
        let (key, current) = seq_state("FIX8_CONC").await.expect("号段行应存在");
        assert_eq!(key, period_key, "周期键应停在本次取号的周期");
        assert_eq!(current, 10, "10 次取号后计数器必须正好是 10");

        drop_fix8_seq_rows().await;
        println!("[FIX-8 ①] 10 并发取号 {unique:?} 两两不同，计数器 = 10 ✓");
    }

    /// 验收②：取号器**按单据类型分行**，A 号段抢号时 B 号段一个号都不动。
    ///
    /// 这条同时是「不是全局单行」的回归护栏：如果哪天有人把号段改成共用一行，
    /// A 取 6 个号会让 B 从 7 开始，断言立刻红。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fix8_seq_types_advance_independently() {
        let Some(_guard) = setup().await else {
            return;
        };
        let now = Utc::now();
        let period_key = serial_model::period_key_of(now);
        drop_fix8_seq_rows().await;
        let before = seq_snapshot().await;

        let mut handles = Vec::new();
        for idx in 0..10 {
            let spec = if idx % 5 < 3 { FIX8_A } else { FIX8_B }; // A 6 个号 / B 4 个号，交替发起
            handles.push(tokio::spawn(async move {
                let tx = db().begin().await.expect("开事务");
                let no = next_doc_no(&tx, &spec, now).await.expect("取号");
                tx.commit().await.expect("提交");
                no
            }));
        }
        let mut numbers = Vec::new();
        for handle in handles {
            numbers.push(handle.await.expect("取号任务 panic"));
        }
        let mut a: Vec<String> = numbers.iter().filter(|no| no.starts_with("FA-")).cloned().collect();
        let mut b: Vec<String> = numbers.iter().filter(|no| no.starts_with("FB-")).cloned().collect();
        a.sort();
        b.sort();
        let expected_a: Vec<String> = (1..=6).map(|no| format_doc_no("FA", &period_key, no)).collect();
        let expected_b: Vec<String> = (1..=4).map(|no| format_doc_no("FB", &period_key, no)).collect();
        assert_eq!(a, expected_a, "A 号段应独立从 1 递增到 6：{numbers:?}");
        assert_eq!(b, expected_b, "B 号段应独立从 1 递增到 4：{numbers:?}");
        assert_eq!(seq_state("FIX8_A").await.expect("A 号段行"), (period_key.clone(), 6));
        assert_eq!(seq_state("FIX8_B").await.expect("B 号段行"), (period_key.clone(), 4));

        // 其它号段（PO / TK / STOCK_TAKE / PZ …）纹丝不动
        let after = seq_snapshot().await;
        for (seq_type, current) in &before {
            if seq_type.starts_with("FIX8_") {
                continue;
            }
            assert_eq!(
                after.get(seq_type),
                Some(current),
                "取 FIX8 的号不该推进别的号段 {seq_type}（{current} → {:?}）",
                after.get(seq_type)
            );
        }

        drop_fix8_seq_rows().await;
        println!("[FIX-8 ②] 按单据类型独立递增：A=1..=6 / B=1..=4，其它 24 个号段不动 ✓");
    }

    /// FIX-9 ②：物料**没配存货科目**时，分录摘要也必须写物料编码，不许退化成 UUID 前 8 位。
    ///
    /// 回归目标：旧实现 `from_material.insert(id, account.map(|a| (code, a)))` 把「物料编码」
    /// 和「该物料的存货科目」绑在一起 —— 物料 `inventory_account_id IS NULL` 时两者一起被丢，
    /// 摘要于是写成「存货 物料 01a1b5d2（第 1 行）」（P0-02 演练 `PZ-20261107-000007` 实测）。
    /// 演示数据 `DEMO-P101/P102` 正是没配科目的形状，所以这条不是假想。
    #[tokio::test]
    async fn fix9_voucher_summary_uses_material_code_without_inventory_account() {
        let Some(_guard) = setup().await else {
            return;
        };
        // 先钉住前置条件：夹具物料确实没配存货科目（否则这条用例证明不了退化链）
        let row = db()
            .query_first_sql(
                "SELECT inventory_account_id FROM biz_material WHERE id = ?::uuid".to_string(),
                vec![Value::from(&uid(MATERIAL_A))],
            )
            .await
            .expect("查物料")
            .expect("夹具物料必须存在");
        let configured: Option<Uuid> = row.get_typed("inventory_account_id").expect("读科目列");
        assert!(
            configured.is_none(),
            "前置条件：MATERIAL_A 不该配存货科目（现在是 {configured:?}）"
        );

        let material_line = VoucherMaterialLine {
            material_id: uid(MATERIAL_A),
            qty: dec("2"),
            amount: dec("20"),
            line_from: 1,
            line_to: 1,
        };
        let rule_account = AccountRef {
            id: uid(TEST_ACCOUNT),
            code: "P2C1.1405".to_string(),
            name: "P2C1 测试存货科目".to_string(),
        };
        let map = material_inventory_accounts(db(), Some(&rule_account), std::slice::from_ref(&material_line))
            .await
            .expect("逐物料科目解析");
        let resolved = map.get(&material_line.material_id).expect("物料必须解析出结果");
        assert_eq!(
            resolved.label, "物料 P2C1-M1",
            "摘要标签必须是物料编码，不是 uuid 前 8 位"
        );

        // 真正的分录摘要文字（验收 ② 的字符串口径：`存货 物料 P2C1-M1（第 1 行）`）
        let lines = purchase_in_lines(
            &AccountSet {
                inventory: rule_account.clone(),
                payable: rule_account.clone(),
                tax: None,
            },
            VoucherAmounts {
                amount: dec("20"),
                tax: Decimal::ZERO,
                with_tax: dec("20"),
            },
            "采购入库 P2C1-IN-1 价税合计 20.0000",
            LineContext {
                warehouse_id: None,
                material_id: None,
                supplier_id: None,
            },
            std::slice::from_ref(&material_line),
            &map,
            "P2C1-IN-1",
        )
        .expect("构造采购入库分录");
        assert_eq!(lines.len(), 2, "零税单：借存货 1 行 + 贷应付 1 行");
        assert!(
            lines[0].summary.contains("存货 物料 P2C1-M1（第 1 行）"),
            "存货行摘要实际：{}",
            lines[0].summary
        );
        assert_eq!(
            lines[0].aux_material_id,
            Some(material_line.material_id),
            "辅助核算的物料维度不受影响"
        );
    }

    // ---------- E2E-FIX-5：摘要名称 + 业务单据号 ----------

    /// ① 摘要里的实体显示名：有名字用名字，没有**只留 8 位短 id**，绝不写 36 位 UUID。
    #[test]
    fn party_label_never_emits_full_uuid() {
        let id = uid("01a1bdda-4947-4000-8000-000000000001");
        assert_eq!(
            party_label(Some("北京云枢科技".to_string()), "客户", id),
            "北京云枢科技"
        );
        // 空白名字等同「没名字」（主数据名字被清空 / 空串）
        assert_eq!(
            party_label(Some("   ".to_string()), "客户", id),
            "客户已删除(01a1bdda)"
        );
        assert_eq!(party_label(None, "供应商", id), "供应商已删除(01a1bdda)");
        let fallback = party_label(None, "客户", id);
        assert!(
            !fallback.contains("01a1bdda-4947"),
            "退化文案不许带整段 UUID，实际：{fallback}"
        );
        assert_eq!(fallback.chars().filter(char::is_ascii_hexdigit).count(), 8);
    }

    /// 造一张两行的自动凭证（借 1405 / 贷 6401），返回凭证 id。
    ///
    /// `source_no` = 记账依据单据号；`source_doc_no` = 业务单据号（退货单号）。
    async fn post_probe_voucher(source_no: &str, source_doc_no: Option<&str>) -> Uuid {
        let inventory = account_required(db(), DEFAULT_INVENTORY_CODE, "库存商品")
            .await
            .expect("取 1405");
        let cogs = account_required(db(), ACCOUNT_MAIN_COST, "主营业务成本")
            .await
            .expect("取 6401");
        let lines = vec![
            EntryLine {
                line_no: 1,
                summary: format!("{source_no} 借方"),
                account: inventory,
                debit: dec("10"),
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
            EntryLine {
                line_no: 2,
                summary: format!("{source_no} 贷方"),
                account: cogs,
                debit: Decimal::ZERO,
                credit: dec("10"),
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
        ];
        post_voucher(
            db(),
            &PostVoucher {
                voucher_type: VOUCHER_TYPE_AUTO,
                source_type: SOURCE_STOCK_IN,
                source_id: None,
                source_no: Some(source_no),
                source_doc_no,
                voucher_date: Utc::now().date_naive(),
                summary: format!("P2C1-DOCNO {source_no}"),
                lines,
                remark: None,
                red_of: None,
                status: STATUS_POSTED,
            },
            None,
            Utc::now(),
        )
        .await
        .unwrap_or_else(|err| panic!("落凭证失败：{err}"))
    }

    /// ② 业务单据号（E2E-FIX-5 ② / V0357）：
    /// · 不传 → 落库时等同来源单号（既有单据的兼容口径）；
    /// · 传了 → 原样落库（退货过账：两张凭证的 source_no 是子单据号，业务单号都是退货单号）；
    /// · 按业务单号**能查到凭证**（精确筛 + 关键词 LIKE 两条查询面都通）。
    #[tokio::test]
    async fn source_doc_no_records_business_doc_and_is_queryable() {
        let Some(_guard) = setup().await else {
            return;
        };
        use crate::business::erp::voucher_query::{self, VoucherFilter};

        // ① 不传 → 与来源单号同值（收货 / 收款 / 开票等「业务单据 = 记账依据」的通用口径）
        let plain = post_probe_voucher("P2C1-DOCNO-IN1", None).await;
        let row = find_voucher(db(), plain).await.expect("查凭证").expect("凭证必须存在");
        assert_eq!(
            row.source_doc_no.as_deref(),
            Some("P2C1-DOCNO-IN1"),
            "不传 source_doc_no 时必须回落成 source_no"
        );

        // ② 退货过账的形状：一张退货单 → 两张凭证（入库单号 / 红字发票号各一张）
        let business_no = "SR-P2C1-DOCNO-000001";
        let cost_voucher = post_probe_voucher("P2C1-DOCNO-IN2", Some(business_no)).await;
        let revenue_voucher = post_probe_voucher("P2C1-DOCNO-IV2", Some(business_no)).await;
        for id in [cost_voucher, revenue_voucher] {
            let row = find_voucher(db(), id).await.expect("查凭证").expect("凭证必须存在");
            assert_eq!(
                row.source_doc_no.as_deref(),
                Some(business_no),
                "退货凭证的业务单据号必须是退货单号"
            );
            assert_ne!(
                row.source_doc_no, row.source_no,
                "退货场景两条链必须分流：source_no 是子单据号"
            );
        }

        // ③ 按业务单号查凭证：精确筛选命中且**恰好两张**
        let filter = VoucherFilter {
            source_doc_no: Some(business_no.to_string()),
            ..VoucherFilter::default()
        };
        let (rows, total) = voucher_query::voucher_page(&filter).await.expect("按业务单号分页");
        assert_eq!(rows.len(), 2, "一张退货单的两张凭证都要查得到");
        assert_eq!(total, 2);
        assert!(
            rows.iter()
                .all(|row| row.source_doc_no.as_deref() == Some(business_no)),
            "返回行的业务单据号必须都是退货单号"
        );
        let mut source_nos: Vec<String> =
            rows.iter().filter_map(|row| row.source_no.clone()).collect();
        source_nos.sort();
        assert_eq!(source_nos, vec!["P2C1-DOCNO-IN2", "P2C1-DOCNO-IV2"]);

        // ④ 关键词（前端搜索框只传 voucher_no）也能按业务单号命中
        let by_keyword = VoucherFilter {
            voucher_no: Some(business_no.to_string()),
            ..VoucherFilter::default()
        };
        let (rows, _) = voucher_query::voucher_page(&by_keyword).await.expect("关键词分页");
        assert_eq!(rows.len(), 2, "关键词要同时匹配 source_doc_no");

        println!("[E2E-FIX-5 ②] source_doc_no 落库 + 按业务单号查凭证（精确 / 关键词）✓");
    }
}

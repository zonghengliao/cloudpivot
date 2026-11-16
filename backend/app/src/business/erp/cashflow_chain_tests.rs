//! M1-17 现金流底稿单测：**录入 → 勾稽 → 出表 → 出表闸** 一条链走完（闭合正例 + 四种不闭合反例）。
//!
//! # 为什么 M1-14 / M1-16 已有用例，还要这一套
//!
//! M1-13 建表、M1-14 录入 + 勾稽、M1-16 由底稿出表，每张卡都钉住了**自己那一段**的形状。
//! 但「每一段都对」不等于「连起来对」：钱从底稿流到报表再流到导出件，中间有几处口径可以
//! 各自正确而合起来错 —— 而且错了不报错，只是报表少一个数、多算一遍、或者上期数错配到本期。
//!
//! | 可能漂的地方 | 单卡用例为什么抓不到 | 本文件断在哪 |
//! |:--|:--|:--|
//! | 「上期」取错期间（只减月份不减年） | M1-16 的纯逻辑用例直接喂一个上期 `WorksheetRes`，绕过了 `previous_period_of` | 纯逻辑① + 场景六（真库 `1911-01` → 上期 `1910-12`） |
//! | 底稿闭合但报表**小计 / 净额加错**（流出被当成负数加、分区漏项） | 底稿勾稽只聚合 `FLOW` 行，根本不看报表小计 | 纯逻辑②（把「流入 + 流出」当净增加额喂进 `build_check`） |
//! | 未闭合时「页面要能开」与「不许导出」混成一件事 | M1-14 只测 POST 校验、M1-16 只测导出闸，没人对**同一份**不闭合底稿把两条入口都走一遍 | 场景三 / 四 / 五 |
//! | 上期底稿不闭合把本期出表也卡死（或反过来静默放过） | 两卡各测各的期间 | 场景五（只吼不拦） |
//! | 上期独有的行在本期报表里消失（历史编制结果丢数） | M1-16 纯逻辑用例手工造 `WorksheetRes`，不经过「配置行被删」这条路 | 场景七（真库删配置行） |
//! | 已结账期间的报表出不来（历史报表导不出） | 期间闸在 M1-14 里测的是「写被拒」，没人测「读 / 出表仍要能」 | 场景八 |
//!
//! # 断言口径：**绝对值**，不用增量
//!
//! 夹具期间用 **1912-11 / 1912-12 / 1911-01**：`1913-xx` 已被 M1-14 / M1-16 的内联用例占用，
//! `1999-xx` 被 M1-10 占用，`2026-xx` 起是生产期间与别卡的夹具。1912 及以前没有任何用例造数
//! （模板库 `cloudpivot_test` 里最老的期间是 2030-02），所以本文件可以对金额写死断言 ——
//! 取数窗口写错、上期错配、小计漏项都会立刻红，而不是被「增量相等」这种弱断言放过。
//!
//! # 夹具（本文件独占，清场只按自己的期间 / 自己的临时配置项收）
//!
//! ```text
//! 期间（status = OPEN，除非用例自己改成 CLOSED；status 由 reset() 每次重铺）
//!   1912-11  上期（正例的对比列）
//!   1912-12  本期
//!   1911-01  本期，上期 1910-12 **故意不建**（跨年 + 没开账）
//!
//! 底稿（正例，两期都闭合）
//!   1912-11  期初    0   OPER_SALES_RECEIPTS 900(流入)  OPER_PURCHASE_PAYMENTS 400(流出)       期末  500
//!   1912-12  期初  500   OPER_SALES_RECEIPTS 1200(流入) FIN_CAPITAL_IN 300(流入)
//!                        OPER_PURCHASE_PAYMENTS 700(流出) INVEST_PURCHASES 250(流出)           期末 1050
//!
//! 现金流量项目（`biz_cashflow_item` 的模板行，不新建 —— 新建就是绕开配置口径）
//!   OPER_SALES_RECEIPTS     经营 / 流入 / sort 101
//!   OPER_PURCHASE_PAYMENTS  经营 / 流出 / sort 201
//!   INVEST_PURCHASES        投资 / 流出 / sort 351
//!   FIN_CAPITAL_IN          筹资 / 流入 / sort 401
//!   本文件自建的临时配置项 M117_PREV_ONLY_ITEM（场景七用完即删）
//! ```
//!
//! 数字为什么是这些：三个活动都非零（分区小计 / 净额 / 净增加额都要真算一次）；流入流出都是
//! 正数（「把流出取负」立刻会让 950 变 −950、净额从 550 变 2450）；上期只录了经营的两行
//! （「上期开账但没录」= `Some(0)` 与「上期没开账」= `None` 两种「没有数」被区分开）。
//!
//! # 真库用例需要
//!
//! ```bash
//! ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_m117_unit' \
//!   cargo test --lib business::erp::cashflow_chain_tests -- --test-threads=1
//! ```
//!
//! 库必须已应用到 `V0414`（`biz_cashflow_worksheet`）。缺 `ERP_DB_URL` 时真库用例全部
//! `SKIP`（打印 `SKIP:`），纯逻辑用例照常跑。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use crate::business::erp::cashflow_statement::{
    self, CURRENT_LABEL, CashStatementRes, PREVIOUS_LABEL, StatementAmountRes, StatementBalanceRes, StatementLineRes,
    StatementSectionRes,
};
use crate::business::erp::cashflow_worksheet::{
    self, BalanceIn, CheckRes, CheckWorksheetReq, DIRECTION_INFLOW, DIRECTION_OUTFLOW, LINE_CLOSING, LINE_FLOW,
    LINE_OPENING, SaveWorksheetReq, StoredRow, WorksheetItemIn, WorksheetRes,
};
use crate::business::erp::report;
use crate::business::erp::voucher::voucher_query::tests::{DB_LOCK, ensure_db};
use cloudpivot_kernel::shared::error_code;

// ==================== 夹具常量 ====================

/// 上期（本期 `1912-12` 的 `previous_period_of` 给的就是它）。
const PREV_PERIOD: &str = "1912-11";
/// 本期。
const CUR_PERIOD: &str = "1912-12";
/// 本期（跨年：上期是 `1910-12`，故意不建期间行）。
const LONE_PERIOD: &str = "1911-01";
/// `LONE_PERIOD` 的上期（**不建期间行**，用来证明「上期没开账 ⇒ 整列 null」）。
const LONE_PREV_PERIOD: &str = "1910-12";
/// 本文件自建的临时配置项（场景七：上期独有行）。
const SCRATCH_ITEM: &str = "M117_PREV_ONLY_ITEM";

/// 正例的上期底稿（绝对值，见文件头夹具表）。
const PREV_SALES: &str = "900";
const PREV_PURCHASE: &str = "400";
const PREV_OPENING: &str = "0";
const PREV_CLOSING: &str = "500";

/// 正例的本期底稿。
const CUR_SALES: &str = "1200";
const CUR_CAPITAL: &str = "300";
const CUR_PURCHASE: &str = "700";
const CUR_INVEST: &str = "250";
const CUR_OPENING: &str = "500";
const CUR_CLOSING: &str = "1050";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点数解析")
}

/// 构造一条「已录入的明细行」（用于纯逻辑用例：`compose_sheet` 只吃配置 + 已存行）。
fn flow_row(code: &str, name: &str, activity: &str, direction: &str, amount: &str, sort: i32) -> StoredRow {
    StoredRow {
        line_type: LINE_FLOW.to_string(),
        item_code: Some(code.to_string()),
        item_name: name.to_string(),
        activity: Some(activity.to_string()),
        direction: Some(direction.to_string()),
        amount: dec(amount),
        sort_order: sort,
        remark: None,
    }
}

/// 构造一条余额行（期初 / 期末）。
fn balance_row(line_type: &str, name: &str, amount: &str) -> StoredRow {
    StoredRow {
        line_type: line_type.to_string(),
        item_code: None,
        item_name: name.to_string(),
        activity: None,
        direction: None,
        amount: dec(amount),
        sort_order: if line_type == LINE_OPENING { 1 } else { 9999 },
        remark: None,
    }
}

/// 正例的「已存行」集合（与真库夹具同一份数，纯逻辑用例复用）。
fn canonical_rows() -> Vec<StoredRow> {
    vec![
        flow_row(
            "OPER_SALES_RECEIPTS",
            "销售商品、提供劳务收到的现金",
            "OPERATING",
            DIRECTION_INFLOW,
            CUR_SALES,
            101,
        ),
        flow_row(
            "FIN_CAPITAL_IN",
            "吸收投资收到的现金",
            "FINANCING",
            DIRECTION_INFLOW,
            CUR_CAPITAL,
            401,
        ),
        flow_row(
            "OPER_PURCHASE_PAYMENTS",
            "购买商品、接受劳务支付的现金",
            "OPERATING",
            DIRECTION_OUTFLOW,
            CUR_PURCHASE,
            201,
        ),
        flow_row(
            "INVEST_PURCHASES",
            "购建固定资产、无形资产和其他长期资产支付的现金",
            "INVESTING",
            DIRECTION_OUTFLOW,
            CUR_INVEST,
            351,
        ),
        balance_row(LINE_OPENING, cashflow_worksheet::OPENING_NAME, CUR_OPENING),
        balance_row(LINE_CLOSING, cashflow_worksheet::CLOSING_NAME, CUR_CLOSING),
    ]
}

// ==================== 纯逻辑：期间口径 ====================

/// ① 「上期」是**上一会计期间**，跨年要减年：`1913-01` → `1912-12`。
///    只减月份的实现会给出 `1913-00` 这种不存在的期间 —— 上期整列静默变空，报表看不出来，
///    只是永远没有对比数。
#[test]
fn previous_period_is_previous_accounting_period_across_years() {
    assert_eq!(report::previous_period_of("1913-01").as_deref(), Some("1912-12"));
    assert_eq!(report::previous_period_of("1912-12").as_deref(), Some("1912-11"));
    assert_eq!(report::previous_period_of("1912-11").as_deref(), Some("1912-10"));
    assert_eq!(report::previous_period_of("1912-02").as_deref(), Some("1912-01"));
    // 形状不合法的期间不推导（宁可不给上期，也不给一个假期间）
    assert_eq!(report::previous_period_of("1912-13"), None);
    assert_eq!(report::previous_period_of("1912-00"), None);
    assert_eq!(report::previous_period_of("1912-1"), None);

    // 期间起止日：12 月跨年 + 闰年 2 月（1912 是闰年，1912-02-29 必须存在）
    let (start, end) = report::month_range("1912-12").expect("12 月的月范围");
    assert_eq!(start.to_string(), "1912-12-01");
    assert_eq!(end.to_string(), "1912-12-31");
    let (_, feb_end) = report::month_range("1912-02").expect("闰年 2 月的月范围");
    assert_eq!(feb_end.to_string(), "1912-02-29", "1912 是闰年");
    assert_eq!(report::period_name_of("1912-12"), "1912年12月");
}

// ==================== 纯逻辑：出表闸的两条等式 ====================

/// ② 出表闸要两条等式都真才算闭合 —— 第二条（`净增加额 + 期初 = 期末`）真的在闸上，
///    不是装饰：底稿勾稽只聚合 `FLOW` 行，一张「小计加错」的表在底稿那侧仍然闭合。
///
/// 反例用最经典的那个错：把「流出」当负数加（`净增加额 = 流入 + 流出`）。底稿一分不动
/// （`check.current.balanced` 仍为 true），只有报表这条等式会红。若闸只看底稿勾稽，
/// 这张表会带着 `2450` 的净增加额导出去。
#[test]
fn statement_gate_needs_arithmetic_equality_not_only_worksheet_reconcile() {
    let sheet = cashflow_worksheet::compose_sheet(CUR_PERIOD, "OPEN", &[], &canonical_rows());
    assert!(sheet.check.balanced, "{}", sheet.check.message);
    assert_eq!(sheet.totals.inflow, dec("1500"), "流入合计 = 1200 + 300");
    assert_eq!(
        sheet.totals.outflow,
        dec("950"),
        "流出合计是正的 950（方向由 direction 表达）"
    );
    assert_eq!(sheet.totals.net, dec("550"));

    let body = cashflow_statement::compose_statement(CUR_PERIOD, &sheet, None, false);
    assert!(body.check.balanced, "{}", body.check.message);
    assert!(body.check.arithmetic_ok);
    assert_eq!(body.net_increase.current, dec("550"));
    assert_eq!(body.check.diff, Some(Decimal::ZERO));
    assert_eq!(body.check.previous_balanced, None, "没有上期底稿 ⇒ null（不是 false）");

    // ★ 反例：同一张底稿、同一对余额，只把「净增加额」换成「流入 + 流出」
    let wrong_net_increase = StatementAmountRes {
        current: dec("1500") + dec("950"),
        previous: None,
    };
    let check = cashflow_statement::build_check(&sheet, None, wrong_net_increase, &body.opening, &body.closing);
    assert!(!check.balanced, "小计加错的表不许过闸");
    assert!(!check.arithmetic_ok, "报表自身加减关系不平");
    assert!(check.current.balanced, "底稿这一侧仍然闭合（勾稽抓不到这个错）");
    assert_eq!(check.diff, Some(dec("1900")), "500 + 2450 − 1050 = 1900");
    assert!(check.message.contains("报表加减关系不平"), "{}", check.message);

    // 余额缺一端时这条等式**算不出来**，也必须算「不平」（不能默认放过）
    let no_closing = StatementBalanceRes {
        line_type: LINE_CLOSING.to_string(),
        item_name: cashflow_worksheet::CLOSING_NAME.to_string(),
        current: None,
        previous: None,
    };
    let check = cashflow_statement::build_check(&sheet, None, body.net_increase, &body.opening, &no_closing);
    assert!(!check.arithmetic_ok);
    assert_eq!(check.diff, None);
    assert!(!check.balanced);
    assert!(check.message.contains("算不出"), "{}", check.message);

    // 三块活动的小计不能互相抵消：投资区净额是 −250（不是 250）
    let investing = body
        .sections
        .iter()
        .find(|section| section.activity == "INVESTING")
        .expect("投资分区");
    assert_eq!(investing.outflow_total.current, dec("250"));
    assert_eq!(investing.net.current, dec("-250"));
}

// ==================== 真库：夹具 ====================

/// 真库用例串行锁（与凭证 / 汇率 / 底稿的内联用例共用同一把：都在写共享的期间表与底稿表）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    reset().await;
    Some(guard)
}

/// 清场 + 铺期间：三个期间（`OPEN`），并把本文件可能留下的行清干净。
///
/// 只按**自己的期间码 / 自己的临时配置项**收，绝不按年份通配（模板库里有 2030–2032 的期间）。
async fn reset() {
    for period in [PREV_PERIOD, CUR_PERIOD, LONE_PERIOD, LONE_PREV_PERIOD] {
        exec_sql(
            "DELETE FROM biz_cashflow_worksheet WHERE period_code = ?",
            vec![Value::Text(period.to_string())],
        )
        .await;
        exec_sql(
            "DELETE FROM biz_accounting_period WHERE period_code = ?",
            vec![Value::Text(period.to_string())],
        )
        .await;
    }
    exec_sql(
        "DELETE FROM biz_cashflow_item WHERE item_code = ?",
        vec![Value::Text(SCRATCH_ITEM.to_string())],
    )
    .await;

    for period in [PREV_PERIOD, CUR_PERIOD, LONE_PERIOD] {
        // 主数据夹具走 `test_seed`（P2-62 门禁：测试代码不许手写主数据 INSERT）。
        // `ensure_accounting_period` 只负责「这一行存在且 OPEN」，别的状态由用例自己 UPDATE。
        let year: i32 = period[..4].parse().expect("期间年");
        let month: u32 = period[5..].parse().expect("期间月");
        crate::test_seed::ensure_accounting_period(year, month).await;
    }
}

async fn exec_sql(sql: &str, params: Vec<Value>) {
    db().execute_with_params(sql, params)
        .await
        .unwrap_or_else(|error| panic!("SQL 失败：{error}\nSQL: {sql}"));
}

async fn set_period_status(period: &str, status: &str) {
    exec_sql(
        "UPDATE biz_accounting_period SET status = ? WHERE period_code = ?",
        vec![Value::Text(status.to_string()), Value::Text(period.to_string())],
    )
    .await;
}

/// 删掉某期间的期末余额行 —— 模拟「会计还没录期末」。`save_sheet` 的 `closing = None` 是
/// 「这一项不动」，所以「缺行」只能这样造（页面之外唯一的来源）。
async fn drop_closing_row(period: &str) {
    exec_sql(
        "DELETE FROM biz_cashflow_worksheet WHERE period_code = ? AND line_type = 'CLOSING'",
        vec![Value::Text(period.to_string())],
    )
    .await;
}

async fn stored_row_count(period: &str) -> i64 {
    let row = db()
        .query_first_sql(
            "SELECT count(*) AS n FROM biz_cashflow_worksheet WHERE period_code = ?".to_string(),
            vec![Value::Text(period.to_string())],
        )
        .await
        .expect("统计底稿行数")
        .expect("count(*) 必有一行");
    row.get_typed::<i64>("n").expect("n")
}

/// 铺一条本文件独占的分类配置（场景七用：先让它存在 → 录上期 → 再删掉它）。
async fn insert_scratch_item() {
    exec_sql(
        "INSERT INTO biz_cashflow_item \
             (id, item_code, item_name, activity, direction, account_pattern, sort_order, is_active) \
         VALUES (gen_random_uuid(), ?, 'M117 上期独有项目', 'INVESTING', 'INFLOW', 'T_117', 399, 1)",
        vec![Value::Text(SCRATCH_ITEM.to_string())],
    )
    .await;
}

// ==================== 真库：铺数 / 取数 ====================

fn balance_in(amount: &str) -> BalanceIn {
    BalanceIn {
        amount: dec(amount),
        remark: None,
    }
}

fn item_in(code: &str, amount: &str) -> WorksheetItemIn {
    WorksheetItemIn {
        item_code: code.to_string(),
        amount: dec(amount),
        remark: None,
    }
}

fn item_in_with_remark(code: &str, amount: &str, remark: &str) -> WorksheetItemIn {
    WorksheetItemIn {
        item_code: code.to_string(),
        amount: dec(amount),
        remark: Some(remark.to_string()),
    }
}

async fn save(period: &str, items: Vec<WorksheetItemIn>, opening: Option<&str>, closing: Option<&str>) -> WorksheetRes {
    let req = SaveWorksheetReq {
        period: period.to_string(),
        opening: opening.map(balance_in),
        closing: closing.map(balance_in),
        items,
    };
    cashflow_worksheet::save_sheet(&req, None)
        .await
        .unwrap_or_else(|error| panic!("保存底稿失败（{period}）：{} {}", error.code, error.msg))
}

/// 出表（走**接口用的同一条路**：`snapshot_or_error` 的 REPEATABLE READ 快照 + 期间校验）。
async fn report_of(period: &str) -> CashStatementRes {
    cashflow_statement::snapshot_or_error(period, false)
        .await
        .unwrap_or_else(|error| panic!("出表失败（{period}）：{} {}", error.code, error.msg))
}

/// 正例两期底稿（见文件头夹具表）。
async fn seed_closed_pair() {
    let prev = save(
        PREV_PERIOD,
        vec![
            item_in("OPER_SALES_RECEIPTS", PREV_SALES),
            item_in("OPER_PURCHASE_PAYMENTS", PREV_PURCHASE),
        ],
        Some(PREV_OPENING),
        Some(PREV_CLOSING),
    )
    .await;
    assert!(prev.check.balanced, "上期夹具必须闭合：{}", prev.check.message);

    let cur = save(
        CUR_PERIOD,
        vec![
            item_in_with_remark("OPER_SALES_RECEIPTS", CUR_SALES, "含代收代付"),
            item_in("FIN_CAPITAL_IN", CUR_CAPITAL),
            item_in("OPER_PURCHASE_PAYMENTS", CUR_PURCHASE),
            item_in("INVEST_PURCHASES", CUR_INVEST),
        ],
        Some(CUR_OPENING),
        Some(CUR_CLOSING),
    )
    .await;
    assert!(cur.check.balanced, "本期夹具必须闭合：{}", cur.check.message);
}

fn section<'a>(statement: &'a CashStatementRes, activity: &str) -> &'a StatementSectionRes {
    statement
        .sections
        .iter()
        .find(|section| section.activity == activity)
        .unwrap_or_else(|| panic!("报表缺 {activity} 分区"))
}

fn line<'a>(lines: &'a [StatementLineRes], code: &str) -> &'a StatementLineRes {
    lines
        .iter()
        .find(|line| line.item_code == code)
        .unwrap_or_else(|| panic!("报表缺 {code} 行"))
}

fn problem_codes(check: &CheckRes) -> Vec<&str> {
    check.problems.iter().map(|problem| problem.code.as_str()).collect()
}

/// 出表闸：未闭合必须 `400 + 66087`，且报文点名「差在哪 / 缺什么」。
fn assert_export_blocked(statement: &CashStatementRes, needles: &[&str]) {
    let error = cashflow_statement::ensure_exportable(statement).expect_err("未闭合必须拦住导出");
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_CASHFLOW_WORKSHEET_UNBALANCED);
    for needle in needles {
        assert!(error.msg.contains(needle), "报文必须点名「{needle}」：{}", error.msg);
    }
}

/// 导出表里按「项目」列的文字取一行。
fn export_row<'a>(
    rows: &'a [std::collections::HashMap<String, String>],
    name_part: &str,
) -> &'a std::collections::HashMap<String, String> {
    rows.iter()
        .find(|row| row["项目"].contains(name_part))
        .unwrap_or_else(|| panic!("导出表缺「{name_part}」行"))
}

// ==================== 真库：闭合正例 ====================

/// ③ **正例（本卡主线）**：两期底稿都闭合 → 报表两条等式都成立 → 导出闸放行 → 真 xlsx 出得来。
///
/// 一次断言整条链的恒等式，任何一环漂了都会红：
///
/// * 本期列取本期底稿、上期列取上期底稿（**错配**会把 900 印到本期、1200 印到上期）；
/// * 三个活动的流入 / 流出小计与净额各自独立（**把流出取负**会让 950 → −950）；
/// * 净增加额 = 三块净额之和 = 期末 − 期初；
/// * 报表合计 = 底稿合计（同一份数两条路径算出来必须逐位相等）；
/// * 「上期开账但没录这一项」⇒ 上期金额 `0` 且 `previous_entered = false`（与 `null` 是两件事）。
#[tokio::test]
async fn closed_pair_yields_balanced_statement_and_real_xlsx() {
    let Some(_guard) = setup().await else {
        return;
    };
    seed_closed_pair().await;

    // 底稿这一侧（M1-14 的读入口回读，确认夹具真的落了库）
    let cur_sheet = cashflow_worksheet::load_sheet(CUR_PERIOD).await.expect("回读本期底稿");
    assert!(cur_sheet.check.balanced, "{}", cur_sheet.check.message);
    assert_eq!(cur_sheet.totals.inflow, dec("1500"));
    assert_eq!(cur_sheet.totals.outflow, dec("950"));
    assert!(cur_sheet.editable);

    // ---------- 报表 ----------
    let statement = report_of(CUR_PERIOD).await;
    assert_eq!(statement.period, CUR_PERIOD);
    assert_eq!(statement.period_name, "1912年12月");
    assert_eq!(statement.period_status, "OPEN");
    assert_eq!(statement.previous_period.as_deref(), Some(PREV_PERIOD));
    assert_eq!(statement.previous_period_status.as_deref(), Some("OPEN"));
    assert_eq!(statement.sections.len(), 3, "经营 → 投资 → 筹资");

    // 两条等式都真
    assert!(statement.check.balanced, "{}", statement.check.message);
    assert!(statement.check.arithmetic_ok);
    assert_eq!(statement.check.previous_balanced, Some(true));
    assert_eq!(statement.check.diff, Some(Decimal::ZERO));
    assert!(
        statement.check.current.problems.is_empty(),
        "{:?}",
        problem_codes(&statement.check.current)
    );
    // 唯一该有的告警是「模板里还有 N 个配置项本期没录」（12 个模板项 - 本期录的 4 个 = 8）。
    // 上期未开账 / 上期未闭合 / 跨期口径不一致这三类告警在闭合正例里都不该出现。
    assert_eq!(
        statement.warnings.len(),
        1,
        "正例只该有一条「未录项」提示：{:?}",
        statement.warnings
    );
    assert!(
        statement.warnings[0].contains("个配置项目还没录"),
        "{:?}",
        statement.warnings
    );

    // 经营活动：本期 1200 − 700 = 500；上期 900 − 400 = 500
    let operating = section(&statement, "OPERATING");
    assert_eq!(operating.inflow_total.current, dec("1200"));
    assert_eq!(operating.inflow_total.previous, Some(dec("900")));
    assert_eq!(operating.outflow_total.current, dec("700"), "流出小计是正的 700");
    assert_eq!(operating.outflow_total.previous, Some(dec("400")));
    assert_eq!(operating.net.current, dec("500"));
    assert_eq!(operating.net.previous, Some(dec("500")));
    let sales = line(&operating.inflow, "OPER_SALES_RECEIPTS");
    assert_eq!(sales.current, dec("1200"));
    assert_eq!(sales.previous, Some(dec("900")));
    assert_eq!(sales.remark.as_deref(), Some("含代收代付"), "底稿备注要跟着上报表");

    // 投资活动：本期只有流出 250（净额 −250）；上期这一行 0（上期开账但没录）
    let investing = section(&statement, "INVESTING");
    assert_eq!(investing.outflow_total.current, dec("250"));
    assert_eq!(investing.net.current, dec("-250"));
    let invest = line(&investing.outflow, "INVEST_PURCHASES");
    assert_eq!(invest.previous, Some(Decimal::ZERO), "上期开账但没录 ⇒ 0");
    assert!(!invest.previous_entered, "0 与「录过 0」要区分开");

    // 筹资活动：本期流入 300
    let financing = section(&statement, "FINANCING");
    assert_eq!(financing.inflow_total.current, dec("300"));
    assert_eq!(financing.inflow_total.previous, Some(Decimal::ZERO));
    assert_eq!(financing.net.current, dec("300"));

    // 净增加额 = 三块净额之和 = 期末 − 期初
    assert_eq!(statement.net_increase.current, dec("550"), "500 − 250 + 300");
    assert_eq!(statement.net_increase.previous, Some(dec("500")), "上期 500 + 0 + 0");
    assert_eq!(statement.opening.current, Some(dec("500")));
    assert_eq!(statement.opening.previous, Some(dec("0")));
    assert_eq!(statement.closing.current, Some(dec("1050")));
    assert_eq!(statement.closing.previous, Some(dec("500")));
    assert_eq!(
        statement.closing.current.expect("期末") - statement.opening.current.expect("期初"),
        statement.net_increase.current,
        "期末 − 期初 必须等于净增加额"
    );

    // 报表合计 = 底稿合计（两条路径算同一份数）
    let inflow_sum: Decimal = statement.sections.iter().map(|item| item.inflow_total.current).sum();
    let outflow_sum: Decimal = statement.sections.iter().map(|item| item.outflow_total.current).sum();
    assert_eq!(inflow_sum, cur_sheet.totals.inflow);
    assert_eq!(outflow_sum, cur_sheet.totals.outflow);
    assert_eq!(inflow_sum - outflow_sum, statement.net_increase.current);

    // ---------- 出表闸放行 + 真 xlsx ----------
    cashflow_statement::ensure_exportable(&statement).expect("闭合的报表必须能出表");
    let (headers, rows) = cashflow_statement::export_table(&statement);
    assert_eq!(headers, vec!["项目", CURRENT_LABEL, PREVIOUS_LABEL]);
    assert_eq!(export_row(&rows, "经营活动产生的现金流量净额")["本期金额"], "500.00");
    assert_eq!(export_row(&rows, "经营活动产生的现金流量净额")["上期金额"], "500.00");
    assert_eq!(export_row(&rows, "投资活动产生的现金流量净额")["本期金额"], "-250.00");
    assert_eq!(export_row(&rows, "现金及现金等价物净增加额")["本期金额"], "550.00");
    assert_eq!(export_row(&rows, "加：期初现金及现金等价物余额")["本期金额"], "500.00");
    assert_eq!(export_row(&rows, "期末现金及现金等价物余额")["本期金额"], "1,050.00");
    assert_eq!(export_row(&rows, "期末现金及现金等价物余额")["上期金额"], "500.00");

    // 真件：xlsx 必须真能生成（出表闸放行 ≠ 文件能写出来，两步都验）
    let bytes = crate::business::impexp::engine::write_sheet(&headers, &rows).expect("生成 xlsx");
    assert!(bytes.len() > 2_000, "xlsx 不该是个空壳，实际 {} 字节", bytes.len());
    assert_eq!(&bytes[..2], b"PK", "xlsx 是 zip 容器（PK 魔数）");
}

/// ④ 幂等：同一张底稿重复提交不攒行、重出的报表逐行相同（upsert 键 `period_code + item_code`）。
///
/// 这里断的是**链上的幂等**：`save_sheet` 幂等（M1-14 已测），但「重出报表金额不变」只有把
/// 两位接起来才看得见 —— 若 `compose_sheet` 把同项目两行各算一次，重存后金额会翻倍。
#[tokio::test]
async fn resave_and_rebuild_are_idempotent() {
    let Some(_guard) = setup().await else {
        return;
    };
    seed_closed_pair().await;
    let before_rows = stored_row_count(CUR_PERIOD).await;
    assert_eq!(before_rows, 6, "4 条明细 + 期初 + 期末");
    let before = report_of(CUR_PERIOD).await;
    let (_, before_export) = cashflow_statement::export_table(&before);

    // 同一批再存一遍（页面「再点一次保存」）
    let again = save(
        CUR_PERIOD,
        vec![
            item_in_with_remark("OPER_SALES_RECEIPTS", CUR_SALES, "含代收代付"),
            item_in("FIN_CAPITAL_IN", CUR_CAPITAL),
            item_in("OPER_PURCHASE_PAYMENTS", CUR_PURCHASE),
            item_in("INVEST_PURCHASES", CUR_INVEST),
        ],
        Some(CUR_OPENING),
        Some(CUR_CLOSING),
    )
    .await;
    assert!(again.check.balanced, "{}", again.check.message);

    assert_eq!(stored_row_count(CUR_PERIOD).await, before_rows, "重存不许攒行");
    let after = report_of(CUR_PERIOD).await;
    assert_eq!(after.net_increase.current, before.net_increase.current);
    assert_eq!(after.closing.current, before.closing.current);
    assert_eq!(
        section(&after, "OPERATING").inflow_total.current,
        section(&before, "OPERATING").inflow_total.current
    );
    let (_, after_export) = cashflow_statement::export_table(&after);
    assert_eq!(after_export, before_export, "重出报表必须逐行相同");
}

// ==================== 真库：不闭合反例 ====================

/// ⑤ 反例 A · **期末录错**（期初 500 + 净增加额 550 = 应为 1050，实录 1150，差 −100）。
///
/// 三件事同时成立才算对：
///
/// 1. `save_sheet` **允许**存（工作稿本来就不该被拦），但 `check` 明确报差 —— 会计看得到；
/// 2. `GET …/statement` 仍然 **200**（页面要能打开去补，不是 500 也不是 404）；
/// 3. 出表闸 **400 + 66087**，报文点名期间与差额。
///
/// 最后再改回 1050：**用户自己就能救回来**（不需要工程师改库）—— 这是本卡对约定 11 的落点。
#[tokio::test]
async fn wrong_closing_keeps_page_open_but_blocks_export() {
    let Some(_guard) = setup().await else {
        return;
    };
    seed_closed_pair().await;

    // 改期末（只改一边：真实的「录错了期末」）
    let saved = save(CUR_PERIOD, vec![], None, Some("1150")).await;
    assert!(!saved.check.balanced, "差 100 必须报不平");
    assert_eq!(saved.check.diff, Some(dec("-100")), "应为 1050、实录 1150 ⇒ 差 −100");
    assert!(problem_codes(&saved.check).contains(&cashflow_worksheet::PROBLEM_IMBALANCED));

    // 页面语义：报表照样编得出来（200），带 problems 让人知道补什么
    let statement = report_of(CUR_PERIOD).await;
    assert!(!statement.check.balanced);
    assert!(!statement.check.arithmetic_ok, "期初 + 净增加额 − 期末 = −100");
    assert_eq!(statement.check.diff, Some(dec("-100")));
    assert_eq!(statement.check.current.diff, Some(dec("-100")));
    assert!(
        problem_codes(&statement.check.current).contains(&cashflow_worksheet::PROBLEM_IMBALANCED),
        "{:?}",
        problem_codes(&statement.check.current)
    );
    assert!(
        statement.check.message.contains("1050.0000"),
        "{}",
        statement.check.message
    );
    assert!(
        statement.check.message.contains("1150.0000"),
        "{}",
        statement.check.message
    );

    // 出表前的复核闸（M1-14 的 POST check）也拦同一份数据 —— 两条入口一个结论
    let recheck = cashflow_worksheet::check_sheet(&CheckWorksheetReq {
        period: CUR_PERIOD.to_string(),
        opening: None,
        closing: None,
        items: Vec::new(),
        require_balanced: true,
    })
    .await
    .expect_err("复核闸必须拦住不平的底稿");
    assert_eq!(recheck.status, 400);
    assert_eq!(recheck.code, error_code::ERP_CASHFLOW_WORKSHEET_UNBALANCED);

    // 出表闸
    assert_export_blocked(&statement, &["1912-12", "未闭合", "1150.0000", "-100.00"]);

    // ★ 改回正确期末 ⇒ 立刻恢复闭合且能出表（用户自愈，不用改库）
    let healed = save(CUR_PERIOD, vec![], None, Some(CUR_CLOSING)).await;
    assert!(healed.check.balanced, "{}", healed.check.message);
    let statement = report_of(CUR_PERIOD).await;
    assert!(statement.check.balanced, "{}", statement.check.message);
    assert!(statement.check.arithmetic_ok);
    cashflow_statement::ensure_exportable(&statement).expect("修回后必须能出表");
    let (_, rows) = cashflow_statement::export_table(&statement);
    assert_eq!(export_row(&rows, "期末现金及现金等价物余额")["本期金额"], "1,050.00");
}

/// ⑥ 反例 B · **期末余额行根本不存在**（会计还没录）。
///
/// 缺行 ≠ 0：报表 `closing = null`、`diff = null`（算不出）、`arithmetic_ok = false`，
/// `problems` 点名 `CLOSING_MISSING`，导出闸报文把「待补」原样带出来。
#[tokio::test]
async fn missing_closing_row_is_named_and_blocks_export() {
    let Some(_guard) = setup().await else {
        return;
    };
    seed_closed_pair().await;
    drop_closing_row(CUR_PERIOD).await;

    let statement = report_of(CUR_PERIOD).await;
    assert_eq!(statement.closing.current, None, "缺行必须是 null，不是 0");
    assert_eq!(statement.check.diff, None, "缺一端就推不出差额");
    assert!(!statement.check.arithmetic_ok);
    assert!(!statement.check.balanced);
    let codes = problem_codes(&statement.check.current);
    assert!(
        codes.contains(&cashflow_worksheet::PROBLEM_CLOSING_MISSING),
        "{codes:?}"
    );
    assert!(
        !codes.contains(&cashflow_worksheet::PROBLEM_IMBALANCED),
        "缺行不是「不平」，别混成一条：{codes:?}"
    );

    assert_export_blocked(
        &statement,
        &["待补", cashflow_worksheet::CLOSING_NAME, "请到「现金流底稿」页"],
    );

    // 补录末期末 ⇒ 恢复闭合
    let healed = save(CUR_PERIOD, vec![], None, Some(CUR_CLOSING)).await;
    assert!(healed.check.balanced, "{}", healed.check.message);
    assert!(report_of(CUR_PERIOD).await.check.balanced);
}

/// ⑦ 反例 C · **尾差 0.0001**：差一分钱的零头也**绝不四舍五入抹平**（抹平 = 报表错了还说闭合）。
///
/// 底稿侧与报表侧的差额都必须是精确的 `0.0001`，导出闸报文也要带这个数 —— 否则会计拿到
/// 「差 0.00」的提示根本查不出来。
#[tokio::test]
async fn sub_cent_tail_is_not_rounded_away() {
    let Some(_guard) = setup().await else {
        return;
    };
    seed_closed_pair().await;

    let saved = save(CUR_PERIOD, vec![], None, Some("1050.0001")).await;
    assert!(!saved.check.balanced, "差 0.0001 也不闭合");
    assert_eq!(saved.check.diff, Some(dec("-0.0001")));

    let statement = report_of(CUR_PERIOD).await;
    assert!(!statement.check.balanced);
    assert!(!statement.check.arithmetic_ok);
    assert_eq!(statement.check.diff, Some(dec("-0.0001")), "0.0001 不许被 round 成 0");
    assert_eq!(statement.closing.current, Some(dec("1050.0001")), "库里就是 4 位小数");

    assert_export_blocked(&statement, &["0.0001"]);
}

/// ⑧ 反例 D · **上期底稿不闭合**：只吼不拦。
///
/// 上期的错是上期的事，本期已经闭合就该能出表 —— 但要 `previous_balanced = false` + 一条
/// 点名告警，让人知道对比列不可信。反过来（上期错把本期卡死）会让会计无法交本期报表。
#[tokio::test]
async fn previous_unbalanced_warns_but_does_not_block_current() {
    let Some(_guard) = setup().await else {
        return;
    };
    seed_closed_pair().await;

    // 上期期末录错（应为 500，实录 600 ⇒ 差 −100）
    let prev = save(PREV_PERIOD, vec![], None, Some("600")).await;
    assert!(!prev.check.balanced);
    assert_eq!(prev.check.diff, Some(dec("-100")));

    let statement = report_of(CUR_PERIOD).await;
    assert!(
        statement.check.balanced,
        "本期闭合不受上期影响：{}",
        statement.check.message
    );
    assert!(statement.check.arithmetic_ok);
    assert_eq!(statement.check.previous_balanced, Some(false), "上期不闭合要如实标出来");
    let previous = statement.check.previous.as_ref().expect("上期勾稽要一起返回");
    assert!(!previous.balanced);
    assert_eq!(previous.diff, Some(dec("-100")));
    assert!(
        statement
            .warnings
            .iter()
            .any(|warning| warning.contains("上期底稿未闭合")),
        "{:?}",
        statement.warnings
    );
    // 上期数据仍照着出（仅供参考），只是不可信
    assert_eq!(statement.closing.previous, Some(dec("600")));

    cashflow_statement::ensure_exportable(&statement).expect("上期不闭合不阻塞本期出表");
    let (_, rows) = cashflow_statement::export_table(&statement);
    assert_eq!(export_row(&rows, "期末现金及现金等价物余额")["上期金额"], "600.00");
}

// ==================== 真库：上期口径 ====================

/// ⑨ 场景六 · **上期没开账（跨年）**：上期整列 `null` + 点名告警，本期照常出表。
///
/// 本期 `1911-01`，`previous_period_of` 给 `1910-12` —— 会计日历里没有这一期。此时：
/// 上期金额**整列 `null`**（渲染成「—」），导出件该列是**空串**（不是 `0.00`），
/// 告警点名 `1910-12`。绝不替用户推导一个不存在的报告期。
#[tokio::test]
async fn previous_period_not_opened_leaves_previous_column_null() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 上期期间行确实不存在（reset 已清过，留一条断言作证）
    let missing = cashflow_worksheet::load_sheet(LONE_PREV_PERIOD).await;
    let error = missing.expect_err("上期没开账时读底稿必须报期间不存在");
    assert_eq!(error.code, error_code::ERP_PERIOD_NOT_FOUND);

    let saved = save(
        LONE_PERIOD,
        vec![item_in("OPER_SALES_RECEIPTS", "100")],
        Some("0"),
        Some("100"),
    )
    .await;
    assert!(saved.check.balanced, "{}", saved.check.message);

    let statement = report_of(LONE_PERIOD).await;
    assert_eq!(statement.period, LONE_PERIOD);
    assert_eq!(statement.previous_period, None);
    assert_eq!(statement.previous_period_status, None);
    assert!(statement.check.balanced, "{}", statement.check.message);
    assert_eq!(statement.check.previous_balanced, None, "没有上期底稿 ⇒ null");
    assert!(statement.check.previous.is_none(), "没有上期底稿 ⇒ 上期勾稽 null");
    assert_eq!(statement.closing.current, Some(dec("100")));
    assert_eq!(statement.closing.previous, None, "上期没开账 ⇒ 上期余额 null（不是 0）");
    assert_eq!(statement.opening.previous, None);
    assert_eq!(statement.net_increase.previous, None);
    let operating = section(&statement, "OPERATING");
    assert_eq!(operating.inflow_total.previous, None);
    assert_eq!(line(&operating.inflow, "OPER_SALES_RECEIPTS").previous, None);
    assert!(
        statement
            .warnings
            .iter()
            .any(|warning| warning.contains(LONE_PREV_PERIOD)),
        "告警必须点名 1910-12：{:?}",
        statement.warnings
    );

    cashflow_statement::ensure_exportable(&statement).expect("上期没开账不影响本期出表");
    let (_, rows) = cashflow_statement::export_table(&statement);
    assert_eq!(
        export_row(&rows, "期末现金及现金等价物余额")["上期金额"],
        "",
        "上期无底稿 ⇒ 空串，不印 0.00"
    );
    assert_eq!(export_row(&rows, "期末现金及现金等价物余额")["本期金额"], "100.00");
}

/// ⑩ 场景七 · **上期独有行在配置被删后仍要出**（历史编制结果不静默丢数）。
///
/// 做法：临时配置项 → 录进上期 → 删掉配置行 → 本期报表里它必须仍在（本期 0、上期 30、
/// `item_active = false`），进 `warnings`，且**本期仍闭合**（一个 0 金额的参考行不该影响出表）。
#[tokio::test]
async fn previous_only_row_survives_after_config_deleted() {
    let Some(_guard) = setup().await else {
        return;
    };
    insert_scratch_item().await;

    let prev = save(
        PREV_PERIOD,
        vec![
            item_in("OPER_SALES_RECEIPTS", "900"),
            item_in("OPER_PURCHASE_PAYMENTS", "400"),
            item_in(SCRATCH_ITEM, "30"),
        ],
        Some("0"),
        Some("530"),
    )
    .await;
    assert!(prev.check.balanced, "上期 0 + 930 − 400 = 530：{}", prev.check.message);

    let cur = save(
        CUR_PERIOD,
        vec![
            item_in("OPER_SALES_RECEIPTS", "1200"),
            item_in("OPER_PURCHASE_PAYMENTS", "700"),
        ],
        Some("530"),
        Some("1030"),
    )
    .await;
    assert!(
        cur.check.balanced,
        "本期 530 + 1200 − 700 = 1030：{}",
        cur.check.message
    );

    // 配置行被删（项目下线 / 换版），但上期底稿里那行还在
    exec_sql(
        "DELETE FROM biz_cashflow_item WHERE item_code = ?",
        vec![Value::Text(SCRATCH_ITEM.to_string())],
    )
    .await;

    let statement = report_of(CUR_PERIOD).await;
    let investing = section(&statement, "INVESTING");
    let kept = line(&investing.inflow, SCRATCH_ITEM);
    assert_eq!(kept.current, Decimal::ZERO, "本期没编这一行 ⇒ 0");
    assert!(!kept.current_entered);
    assert_eq!(kept.previous, Some(dec("30")), "上期编制结果不许丢");
    assert!(kept.previous_entered);
    assert!(!kept.item_active, "配置已删 ⇒ item_active = false");
    assert_eq!(investing.inflow_total.previous, Some(dec("30")), "上期小计要含它");
    assert!(
        statement
            .warnings
            .iter()
            .any(|warning| warning.contains("只在上期底稿里存在")),
        "{:?}",
        statement.warnings
    );
    assert!(statement.check.balanced, "0 金额的参考行不影响本期闭合");
    assert!(statement.check.arithmetic_ok);
    cashflow_statement::ensure_exportable(&statement).expect("能出表");

    let (_, rows) = cashflow_statement::export_table(&statement);
    let row = export_row(&rows, "M117 上期独有项目");
    assert_eq!(row["本期金额"], "0.00");
    assert_eq!(row["上期金额"], "30.00");
}

/// ⑪ 场景八 · **已结账期间**：写被拒（66085），但报表**仍然读得出、导得出**。
///
/// 期间闸的意义是「不许再改」，不是「历史报表消失」。审计 / 报出留档要看的就是结账后的表，
/// 所以出表闸只看勾稽，不看期间状态。
#[tokio::test]
async fn closed_period_statement_is_readable_and_exportable_but_not_writable() {
    let Some(_guard) = setup().await else {
        return;
    };
    seed_closed_pair().await;
    set_period_status(CUR_PERIOD, "CLOSED").await;

    // 写：被期间闸拦住（M1-14 的 66085）
    let error = cashflow_worksheet::save_sheet(
        &SaveWorksheetReq {
            period: CUR_PERIOD.to_string(),
            opening: None,
            closing: Some(balance_in(CUR_CLOSING)),
            items: Vec::new(),
        },
        None,
    )
    .await
    .expect_err("已结账期间不许再改底稿");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::ERP_CASHFLOW_WORKSHEET_PERIOD_LOCKED);
    assert!(error.msg.contains(CUR_PERIOD), "报文要点名期间：{}", error.msg);

    // 读：底稿仍可读，但 editable = false（页面置灰而不是报错）
    let sheet = cashflow_worksheet::load_sheet(CUR_PERIOD)
        .await
        .expect("已结账期间底稿可读");
    assert!(!sheet.editable);
    assert_eq!(sheet.period_status, "CLOSED");
    assert!(sheet.check.balanced);

    // 出表：仍 200 + 仍可导出（历史留档）
    let statement = report_of(CUR_PERIOD).await;
    assert_eq!(statement.period_status, "CLOSED");
    assert!(statement.check.balanced, "{}", statement.check.message);
    assert!(statement.check.arithmetic_ok);
    cashflow_statement::ensure_exportable(&statement).expect("已结账期间的报表必须能导出");
    let (headers, rows) = cashflow_statement::export_table(&statement);
    let bytes = crate::business::impexp::engine::write_sheet(&headers, &rows).expect("生成 xlsx");
    assert_eq!(&bytes[..2], b"PK");
    assert_eq!(export_row(&rows, "现金及现金等价物净增加额")["本期金额"], "550.00");
}

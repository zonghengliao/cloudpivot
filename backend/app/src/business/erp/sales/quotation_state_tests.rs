// ============================================================================
// quotation_state_tests.rs —— 销售报价单「状态机 + 转单」单测（M3-05）
//
// 卡：t_a2ba25ed（M3-05【M3·销售】报价单：单测 / 含转单与状态机）
// 上游：M3-01 表（v0432）/ M3-02 CRUD+状态机 / M3-03 转销售订单 / M3-04 审批流接线
//
// # 这一份与另外三份用例的分工
//
// ```text
// quotation_model::tests        ← 表与行的**纯口径**（状态域 / 边表 / 金额定标 / 取号 / CAS / 分页）
// quotation_handler::tests      ← 入参校验 + 转单**纯函数**（ensure_convertible / plan_order_items）
//                                 + 转单主链与幂等的真库用例（M3-03 自带）
// quotation_approval_bridge::tests ← 审批桥的**静态契约**（词表 / 流程编码 / 表单快照）
// 本文件（M3-05）               ← **handler 三道状态守卫 × 状态机落库 × 转单** 的端到端单测
//                                 把「哪些边要 reason」「哪条边要审批」「什么状态锁单」
//                                 「作废要不要撤审批」「转单能不能并发」逐条钉死
// ```
//
// 也就是说本文件**不重测**已经在上面三处钉过的口径，它钉的是**写在 handler 里、
// 只有走 `change_status_with` / `convert_quotation` 才会执行**的那一层：
//
// * 非法边 / 终态 → 409，报文给「当前可流转到」而不是 500；
// * 需要原因的三条边缺 `reason` → 400（草稿作废**不要**原因）；
// * `DRAFT → SENT` 的审批闸：没 `APPROVED` 实例 → **409 + 66088**，且一个字都不许落库；
// * `Skipped` 闸（没配流程 + 显式跳过）→ 放出，理由进 `biz_status_log.reason`；
// * 有效期 / 已转单两道守卫：过期价不能被接受、已转单的报价不能被作废；
// * 审批人占着单据时状态推不动，但**作废要连审批一起撤**（不留孤儿待办）；
// * 「退发起人」（`RESUBMIT` 待办）**不锁单**（否则退回发起人 = 把单锁死）；
// * 转单并发下只建一张订单；软删后所有入口 404。
//
// # 审批用例为什么用 SQL 造实例（而不是走 `submit_approval`）
//
// 本文件验的是「**闸**对实例状态怎么反应」，不是「引擎能不能发起」——发起那一段
// 归 M3-04 的 E2E（`scripts/verify/m304_quotation_approval_e2e.sh`）与 flow 域自己的用例。
// 走 `submit_approval` 需要库里有已发布的 `quotation_approve` 流程定义 + 角色 UUID 对得上
// （V0435 的种子在角色不匹配时整条跳过），那会让本文件的真库用例**依赖环境**而随机变红；
// 所以这里只用 `biz_flow_instance` / `biz_flow_task` 的最小合法行造出四种事实：
// `APPROVED`（闸放行）、`PENDING + APPROVAL 待办`（审批人占着）、
// `PENDING + RESUBMIT 待办`（票在发起人手里）、`PENDING`（作废要撤的那一条）。
// 夹具流程定义是 `DRAFT` 且 `is_default = false`，不会被 `published_quotation_flow` 摘走。
//
// # 主数据为什么走 `crate::test_seed`
//
// P2-62（`scripts/verify/test_seed_gate.sh`）规定测试代码里的主数据 INSERT 只许降不许升，
// 客户夹具统一用 `test_seed::ensure_customer`。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与同一批 `M305-` 夹具），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m305_ut" cargo test --lib \
//   business::erp::sales::quotation_state_tests -- --test-threads=1
// ```
//
// 也可直接跑 `bash scripts/verify/m305_quotation_state_drill.sh`
// （克隆演练库 → 补迁移 V0405…V0435 → 跑本组 + 报价单整组回归）。
// ============================================================================

use super::{
    ConvertQuotationReq, IssueGate, QuotationStatusReq, change_quotation_status, change_status_with, convert_quotation,
    delete_quotation, ensure_acceptable, ensure_cancellable, ensure_convertible, ensure_sendable, get_quotation,
};
use crate::business::erp::fixture_hygiene;
use crate::business::erp::sales::quotation_model as model;
use crate::test_seed;
use cloudpivot_kernel::business::shared::state_machine;
use cloudpivot_kernel::shared::error_code;
use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::str::FromStr;

/// 定点字面量（`17.65` 这类在 f64 下不精确，一律走字符串）。
fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("定点可解析")
}

/// 固定日期（用例不依赖「今天」，只有真库里确实需要 today 的地方才用 `model::today_cst`）。
fn day(year: i32, month: u32, dom: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, dom).expect("合法日期")
}

/// 一张报价单（只有守卫用得上的字段有真实值，其余给中性默认）。
fn quote_row(status: &str, valid_until: NaiveDate) -> model::QuotationRow {
    model::QuotationRow {
        id: Uuid::now_v7(),
        quote_no: "QT-20261117-000001".to_string(),
        customer_id: Uuid::now_v7(),
        quote_date: day(2026, 11, 17),
        valid_until,
        status: status.to_string(),
        total_amount: dec("100.0000"),
        total_tax: dec("13.0000"),
        total_with_tax: dec("113.0000"),
        converted_so_id: None,
        converted_at: None,
        approval_instance_id: None,
        remark: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        create_by: None,
        update_by: None,
        deleted_at: None,
    }
}

// ============================================================================
// 一、纯逻辑（不需要数据库）
// ============================================================================

/// 空明细必须在**问客户之前**就被挡下（顺序即口径：空单与客户冻结是两件事，
/// 空单文案要说「没有明细」，不能变成一句莫名其妙的客户状态报错）。
#[tokio::test]
async fn send_guard_refuses_empty_lines_before_touching_customer() {
    let row = quote_row(model::STATUS_DRAFT, day(2026, 12, 17));
    let err = ensure_sendable(&row, &[], model::today_cst(Utc::now()))
        .await
        .expect_err("空明细不能发出");
    assert_eq!(err.status, 400, "空明细是入参问题，给 400 不是 409");
    assert!(err.msg.contains("至少要 1 行明细"), "文案要指名字段：{}", err.msg);
    assert!(err.msg.contains(&row.quote_no), "文案要带单号：{}", err.msg);
}

/// `SENT → ACCEPTED` 的有效期刀口：`valid_until` **当天仍可接受**，次日 409。
///
/// 「过期价不能被接受」是 M3-02 卡面原话：接受发生在过期之后 = 用旧价成交，
/// 财务/成本口径都会歪，所以必须是硬拦（409 冲突，不是 400 参数错）。
#[test]
fn accept_guard_expiry_boundary_is_inclusive_on_valid_until() {
    let expires = day(2026, 11, 30);
    let row = quote_row(model::STATUS_SENT, expires);

    // 有效期至当天：可以接受
    ensure_acceptable(&row, expires).expect("有效期当天仍可接受");
    ensure_acceptable(&row, day(2026, 11, 1)).expect("有效期内任何时候都可接受");

    // 次日：过期，409
    let err = ensure_acceptable(&row, day(2026, 12, 1)).expect_err("过期价不能被接受");
    assert_eq!(err.status, 409, "过期是状态冲突，不是参数错");
    assert!(err.msg.contains(&row.quote_no), "文案要带单号：{}", err.msg);
    assert!(err.msg.contains("2026-11-30"), "文案要带有效期：{}", err.msg);
    assert!(err.msg.contains("过期"), "{}", err.msg);
    // 文案要给出可执行的下一步（重报新单），不能只说「不许」
    assert!(err.msg.contains("重新报价"), "文案要给可执行提示：{}", err.msg);
}

/// `→ CANCELLED` 的守卫：已转销售订单的报价不许作废（作废它 = 订单失去来源）。
///
/// 与状态无关（已转单的单必然是 `ACCEPTED`，但守卫本身不靠状态判断）——
/// 判据只有 `converted_so_id`，因为那才是「下游有没有引用我」的事实。
#[test]
fn cancel_guard_refuses_converted_quote_and_names_the_order() {
    ensure_cancellable(&quote_row(model::STATUS_ACCEPTED, day(2026, 12, 17))).expect("没转过单的已接受报价可以作废");

    let mut row = quote_row(model::STATUS_ACCEPTED, day(2026, 12, 17));
    let so_id = Uuid::now_v7();
    row.converted_so_id = Some(so_id);
    let err = ensure_cancellable(&row).expect_err("已转单的报价不许作废");
    assert_eq!(err.status, 409);
    assert!(
        err.msg.contains(&so_id.to_string()),
        "文案要点名是哪张订单：{}",
        err.msg
    );
    assert!(err.msg.contains("失去来源"), "文案要说清代价：{}", err.msg);
    assert!(
        err.msg.contains("销售订单的作废流程"),
        "文案要给正确出口（去作废订单而不是作废报价）：{}",
        err.msg
    );
}

/// `parse_uuid` / `target_status` 两道前置校验都发生在**读库之前**：
/// 坏 id 与坏状态码不该变成一次数据库往返（也不该被吞成 500）。
#[tokio::test]
async fn status_request_validation_happens_before_any_db_read() {
    let id = Uuid::now_v7().to_string();

    // ① 域外状态：400，且把五个合法值全列出来（前端照它渲染按钮）
    let err = change_quotation_status(
        None,
        &id,
        QuotationStatusReq {
            target_status: "WIN".to_string(),
            reason: None,
        },
    )
    .await
    .expect_err("域外状态必须 400");
    assert_eq!(err.status, 400);
    assert_eq!(err.code, error_code::PARAM_INVALID);
    for status in model::STATUSES {
        assert!(err.msg.contains(status), "文案要列出 {status}：{}", err.msg);
    }

    // ② 空白状态（`trim` 后为空）同样 400，不是「没传就当没变」
    for blank in ["", "   "] {
        let err = change_quotation_status(
            None,
            &id,
            QuotationStatusReq {
                target_status: blank.to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("空白状态必须 400");
        assert_eq!(err.status, 400);
    }

    // ③ 非法 UUID：400 + 66006，并且**先于**状态校验（id 解析在最前面）
    let err = change_quotation_status(
        None,
        "not-a-uuid",
        QuotationStatusReq {
            target_status: "WIN".to_string(),
            reason: None,
        },
    )
    .await
    .expect_err("非法 id 必须 400");
    assert_eq!(err.status, 400);
    assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
    assert!(err.msg.contains("id"), "文案要指名字段：{}", err.msg);
}

/// 转单 ≠ 接受：**已接受但今天已过期**的报价仍可转单（接受发生在有效期内），
/// 而同一张单如果还没被接受，`SENT → ACCEPTED` 会被有效期挡住。
///
/// 这条不对称是 M3-03 的既定决定（见 `quotation_handler` 模块注释「客户」一行），
/// 单独立一条用例把它钉住 —— 否则下一次「顺手统一一下过期判断」就会把已成交的单卡死。
#[test]
fn convert_is_expiry_blind_while_accept_is_not() {
    let expired = day(2026, 1, 1);
    let today = day(2026, 11, 17);

    let accepted = quote_row(model::STATUS_ACCEPTED, expired);
    ensure_convertible(&accepted).expect("已接受但已过期：仍可转单（成交发生在有效期内）");
    assert!(accepted.expired_on(today), "前提：这张单今天确实过期了");

    let sent = quote_row(model::STATUS_SENT, expired);
    let err = ensure_acceptable(&sent, today).expect_err("未接受且已过期：不能标接受");
    assert_eq!(err.status, 409);
}

/// 三道守卫的「边归属」自检：哪条边有守卫、哪条边要原因，都不能靠注释维持。
///
/// 这里把 handler 的三道守卫 + 状态机的原因边并排断言一次 —— 新增一条
/// `→ CANCELLED` 的边时，如果忘了接守卫/原因，这条用例会先红。
#[test]
fn guard_and_reason_edges_line_up_with_the_state_machine() {
    let entity = state_machine::Entity::Quotation;

    // ① 原因边：只有「对外推翻承诺」的三条（客户拒绝 + 发出后两条作废）
    let mut reason_edges: Vec<(&str, &str)> = Vec::new();
    for from in model::STATUSES {
        for to in model::allowed_targets(from) {
            if entity.needs_reason(from, to) {
                reason_edges.push((from, to));
            }
        }
    }
    assert_eq!(
        reason_edges,
        vec![
            (model::STATUS_SENT, model::STATUS_REJECTED),
            (model::STATUS_SENT, model::STATUS_CANCELLED),
            (model::STATUS_ACCEPTED, model::STATUS_CANCELLED),
        ],
        "原因边集合变了：handler 的 needs_reason 分支与文案要一起改"
    );

    // ② 草稿作废**不要**原因（从没对外发出过，没有承诺可推翻）
    assert!(!entity.needs_reason(model::STATUS_DRAFT, model::STATUS_CANCELLED));

    // ③ 三条正向主链都不需要原因（发出 / 接受 / 拒绝以外的正向动作没有）
    for (from, to) in [
        (model::STATUS_DRAFT, model::STATUS_SENT),
        (model::STATUS_SENT, model::STATUS_ACCEPTED),
    ] {
        assert!(!entity.needs_reason(from, to), "{from} → {to} 不该要原因");
    }

    // ④ 守卫归属：唯一入口 DRAFT→SENT 有守卫；→CANCELLED 的守卫判据是 converted_so_id
    assert!(model::can_transition(model::STATUS_DRAFT, model::STATUS_SENT));
    for (from, _to) in reason_edges.iter().filter(|(_, to)| *to == model::STATUS_CANCELLED) {
        assert!(
            model::can_transition(from, model::STATUS_CANCELLED) || *from == model::STATUS_SENT,
            "{from} → CANCELLED 必须是合法边，否则守卫永远执行不到"
        );
    }
}

// ============================================================================
// 二、真库用例（状态机落库 + 审批闸 + 转单）
// ============================================================================

/// 真库用例必须串行（共用全局 `db()` 与同一批 `M305-` 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 夹具客户编码 —— 清场按它定位（单号是取号器生成的，不能按前缀删单据）。
const CUSTOMER_CODE: &str = "M305-CUST";

/// 夹具流程定义 / 版本的固定 id（幂等插入；`DRAFT` + `is_default = false`，
/// 不会被 `published_quotation_flow` 摘走，也不与 V0435 的默认流程撞唯一索引）。
const FIXTURE_DEF_ID: &str = "30000000-0000-4000-8000-0000000d3050";
const FIXTURE_VERSION_ID: &str = "30000000-0000-4000-8000-0000000d3051";

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
            match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
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

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 清场：审批待办 → 实例节点 → 实例 → 状态日志 → 销售订单（明细 CASCADE）→ 报价单（明细 CASCADE）→ 夹具客户。
///
/// 状态日志**先于**单据删（它没有外键，单据一删就再也定位不到 id —— `fixture_hygiene` 的
/// 孤儿判据因此永远收不走这批行）。
async fn cleanup() {
    let by_customer = "SELECT id FROM biz_customer WHERE code = ?";
    let quotation_of_customer = format!("SELECT id FROM biz_quotation WHERE customer_id IN ({by_customer})");
    let instance_of_customer = format!(
        "SELECT id FROM biz_flow_instance \
          WHERE biz_type = 'QUOTATION_APPROVE' AND biz_id IN ({quotation_of_customer})"
    );
    let statements: Vec<(String, Vec<Value>)> = vec![
        (
            format!("DELETE FROM biz_flow_task WHERE instance_id IN ({instance_of_customer})"),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!("DELETE FROM biz_flow_instance_node WHERE instance_id IN ({instance_of_customer})"),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!(
                "DELETE FROM biz_flow_instance \
                  WHERE biz_type = 'QUOTATION_APPROVE' AND biz_id IN ({quotation_of_customer})"
            ),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!(
                "DELETE FROM biz_status_log \
                  WHERE entity_type = 'QUOTATION' AND entity_id IN ({quotation_of_customer})"
            ),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!(
                // ⚠ 销售订单轨迹的 entity_type 是 `SALE_ORDER`（`DocKind::SaleOrder.as_str()`），
                // 不是 `SALES_ORDER` —— 写错等于这段清理永远删 0 行，日志全变孤儿（M3-08 实测抓到）
                "DELETE FROM biz_status_log \
                  WHERE entity_type = 'SALE_ORDER' AND entity_id IN ( \
                        SELECT id FROM biz_sales_order WHERE customer_id IN ({by_customer}))"
            ),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!("DELETE FROM biz_sales_order WHERE customer_id IN ({by_customer})"),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!("DELETE FROM biz_quotation WHERE customer_id IN ({by_customer})"),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            "DELETE FROM biz_customer WHERE code = ?".to_string(),
            vec![Value::from(CUSTOMER_CODE)],
        ),
    ];
    for (sql, params) in statements {
        if let Err(err) = db().execute_with_params(&sql, params).await {
            panic!("清理夹具失败：{err}\nSQL: {sql}");
        }
    }
}

/// 夹具客户（ACTIVE）—— 主数据走 `test_seed`（P2-62）。
async fn fixture_customer() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_CODE, "M3-05 单测夹具客户").await
}

fn new_line(qty: &str, price: &str, rate: &str) -> model::NewLine {
    let (qty, price, tax_rate) = (dec(qty), dec(price), dec(rate));
    let (amount, tax_amount) = model::line_amounts(qty, price, tax_rate);
    model::NewLine {
        material_id: Uuid::now_v7(),
        qty,
        price,
        tax_rate,
        amount,
        tax_amount,
        remark: None,
    }
}

/// 建一张报价单（走 `model::create_quotation`，与建单接口同一条写路径）。
///
/// `valid_days` 是有效期相对「今天」的天数（负 = 已经过期，用于有效期刀口）。
async fn create_quotation(customer: Uuid, lines: &[model::NewLine], valid_days: i64) -> (Uuid, String) {
    let today = model::today_cst(Utc::now());
    let totals = model::Totals::of(lines);
    let tx = db().begin().await.expect("开事务");
    let created = model::create_quotation(
        &tx,
        &model::NewQuotation {
            customer_id: customer,
            quote_date: today,
            valid_until: today + Duration::days(valid_days),
            remark: Some("M3-05 单测夹具"),
        },
        lines,
        &totals,
        state_machine::Operator::of(None),
        Utc::now(),
    )
    .await
    .expect("建报价单应成功");
    tx.commit().await.expect("提交");
    created
}

/// 按合法边逐跳推到 `target`（**不绕过状态机**：每一步都是 CAS）。
///
/// 只处理夹具需要的路径；`DRAFT → SENT` 走 `Skipped` 闸（本文件测状态机，不测审批闸）。
async fn advance_to(id: Uuid, target: &str) -> String {
    let row = model::find_quotation(id)
        .await
        .expect("查报价单")
        .expect("报价单必须存在");
    let path: &[&str] = match (row.status.as_str(), target) {
        (model::STATUS_DRAFT, model::STATUS_SENT) => &[model::STATUS_SENT],
        (model::STATUS_DRAFT, model::STATUS_ACCEPTED) => &[model::STATUS_SENT, model::STATUS_ACCEPTED],
        (model::STATUS_DRAFT, model::STATUS_REJECTED) => &[model::STATUS_SENT, model::STATUS_REJECTED],
        (model::STATUS_SENT, model::STATUS_ACCEPTED) => &[model::STATUS_ACCEPTED],
        (model::STATUS_SENT, model::STATUS_REJECTED) => &[model::STATUS_REJECTED],
        (current, target) if current == target => &[],
        (current, target) => panic!("夹具不支持 {current} → {target}"),
    };
    let mut from = row.status.clone();
    for step in path {
        let req = QuotationStatusReq {
            target_status: (*step).to_string(),
            // 需要原因的边（客户拒绝）一律给原因；其余边给了也无害
            reason: Some("M3-05 夹具：状态推进".to_string()),
        };
        let res = change_status_with(
            None,
            &id.to_string(),
            req,
            IssueGate::Skipped("M3-05 夹具：跳过审批".to_string()),
        )
        .await
        .unwrap_or_else(|err| panic!("夹具推进 {from} → {step} 失败（{}）：{}", err.status, err.msg));
        assert_eq!(res.status, *step, "推进后状态必须是 {step}");
        from = (*step).to_string();
    }
    from
}

/// 单表的审批 / 待办计数（(状态, PENDING 的 APPROVAL 待办数, PENDING 的 RESUBMIT 待办数)）。
async fn instance_snapshot(instance_id: Uuid) -> (String, i64, i64) {
    let row = db()
        .query_first_sql(
            "SELECT i.status, \
                    (SELECT count(*) FROM biz_flow_task t \
                      WHERE t.instance_id = i.id AND t.status = 'PENDING' \
                        AND t.task_kind = 'APPROVAL') AS approval_pending, \
                    (SELECT count(*) FROM biz_flow_task t \
                      WHERE t.instance_id = i.id AND t.status = 'PENDING' \
                        AND t.task_kind = 'RESUBMIT') AS resubmit_pending \
               FROM biz_flow_instance i WHERE i.id = ?::uuid"
                .to_string(),
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例快照应成功")
        .expect("实例必须存在");
    (
        row.get_typed("status").expect("status"),
        row.get_typed("approval_pending").expect("approval_pending"),
        row.get_typed("resubmit_pending").expect("resubmit_pending"),
    )
}

/// 造一条「报价单审批实例」的**最小合法行**（实例 + 当前节点 + 一条待办）。
///
/// 只造事实、不跑引擎：本文件验的是闸对实例状态的反应（见文件头「为什么用 SQL 造实例」）。
/// `task_kind = "APPROVAL"` → 审批人占着这张单；`"RESUBMIT"` → 票在发起人手里。
/// `instance_status` = `PENDING` 时留一条 PENDING 待办；终态实例（`APPROVED` 等）不留待办。
async fn seed_instance(quotation_id: Uuid, quote_no: &str, instance_status: &str, task_kind: &str) -> Uuid {
    let assignee: Uuid = db()
        .scalar_sql(
            "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY username LIMIT 1".to_string(),
            vec![],
        )
        .await
        .expect("查夹具用户应成功（演练库里至少要有一个 sys_user）");
    // 流程定义 / 版本（幂等）：`DRAFT` + `is_default = false` —— 不会被当成「已发布的报价审批流程」
    for sql in [
        format!(
            "INSERT INTO biz_flow_def \
                 (id, flow_code, flow_name, biz_type, status, is_default, current_version_no) \
             VALUES ('{FIXTURE_DEF_ID}'::uuid, 'm305_fixture_flow', 'M3-05 单测夹具流程', \
                     'QUOTATION_APPROVE', 'DRAFT', false, 1) \
             ON CONFLICT (id) DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_flow_def_version \
                 (id, def_id, version_no, version_status, is_current, node_count, edge_count) \
             VALUES ('{FIXTURE_VERSION_ID}'::uuid, '{FIXTURE_DEF_ID}'::uuid, 1, 'DRAFT', false, 1, 1) \
             ON CONFLICT (id) DO NOTHING"
        ),
    ] {
        db().exec_sql(sql, vec![]).await.expect("插夹具流程定义应成功");
    }

    let instance_id = Uuid::now_v7();
    let instance_no = format!(
        "FLW-M305-{}",
        instance_id
            .simple()
            .to_string()
            .get(26..32)
            .unwrap_or("000000")
            .to_uppercase()
    );
    // `ck_biz_flow_instance_end`：`PENDING` ⇔ `ended_at IS NULL`
    let ended_at = if instance_status == "PENDING" { "NULL" } else { "now()" };
    db().execute_with_params(
        &format!(
            "INSERT INTO biz_flow_instance \
                 (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
                  biz_type, biz_id, biz_no, title, status, initiator_id, ended_at) \
             VALUES (?::uuid, ?, '{FIXTURE_DEF_ID}'::uuid, '{FIXTURE_VERSION_ID}'::uuid, 1, \
                     'm305_fixture_flow', 'M3-05 单测夹具流程', 'QUOTATION_APPROVE', \
                     ?::uuid, ?, ?, ?, ?::uuid, {ended_at})"
        ),
        vec![
            Value::from(&instance_id),
            Value::Text(instance_no.clone()),
            Value::from(&quotation_id),
            Value::Text(quote_no.to_string()),
            Value::Text(format!("M3-05 夹具审批 {quote_no}")),
            Value::Text(instance_status.to_string()),
            Value::from(&assignee),
        ],
    )
    .await
    .expect("插夹具审批实例应成功");

    if instance_status == "PENDING" {
        let node_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_flow_instance_node \
                 (id, instance_id, node_key, node_name, node_type, round, status, activated_at) \
             VALUES (?::uuid, ?::uuid, 'n1', '夹具审批节点', 'APPROVAL', 1, 'ACTIVE', now())",
            vec![Value::from(&node_id), Value::from(&instance_id)],
        )
        .await
        .expect("插夹具实例节点应成功");
        let task_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_flow_task \
                 (id, instance_id, instance_node_id, node_key, round, task_kind, \
                  assignee_id, assignee_source, status) \
             VALUES (?::uuid, ?::uuid, ?::uuid, 'n1', 1, ?, ?::uuid, 'USER', 'PENDING')",
            vec![
                Value::from(&task_id),
                Value::from(&instance_id),
                Value::from(&node_id),
                Value::Text(task_kind.to_string()),
                Value::from(&assignee),
            ],
        )
        .await
        .expect("插夹具待办应成功");
    }
    instance_id
}

/// 报价单的状态日志（时间正序，每条 = (from, to, action, reason)）。
async fn status_logs(id: Uuid) -> Vec<(Option<String>, String, Option<String>, Option<String>)> {
    db().find_with_params(
        "SELECT from_status, to_status, action, reason FROM biz_status_log \
          WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid \
          ORDER BY created_at, id",
        vec![Value::from(&id)],
    )
    .await
    .expect("查状态日志应成功")
    .iter()
    .map(|row| {
        (
            row.get_typed::<Option<String>>("from_status").unwrap_or(None),
            row.get_typed::<String>("to_status").unwrap_or_default(),
            row.get_typed::<Option<String>>("action").unwrap_or(None),
            row.get_typed::<Option<String>>("reason").unwrap_or(None),
        )
    })
    .collect()
}

async fn status_of(id: Uuid) -> String {
    model::find_quotation(id)
        .await
        .expect("查报价单")
        .expect("报价单必须存在")
        .status
}

async fn order_count(customer: Uuid) -> i64 {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_sales_order WHERE customer_id = ? AND deleted_at IS NULL",
        vec![Value::from(&customer)],
    )
    .await
    .expect("统计订单数应成功")
}

fn status_req(target: &str, reason: Option<&str>) -> QuotationStatusReq {
    QuotationStatusReq {
        target_status: target.to_string(),
        reason: reason.map(str::to_string),
    }
}

/// ① 审批闸：没 `APPROVED` 实例 → 409 + 66088，且**一个字都不落库**。
///
/// 这是 M3-04 的核心口径（审批是发出的前置条件）。失败路径最容易写成「日志先写了、
/// 状态没落」—— 所以这里同时断言：状态仍是 DRAFT、没有 SENT 轨迹、没有写 approval 指针、
/// 也没有 `update_by`。
#[tokio::test]
async fn issue_without_approved_instance_is_409_66088_and_writes_nothing() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (id, quote_no) = create_quotation(customer, &[new_line("1", "100", "13")], 14).await;
    let before = model::find_quotation(id).await.expect("查单").expect("存在");

    let err = change_quotation_status(None, &id.to_string(), status_req(model::STATUS_SENT, None))
        .await
        .expect_err("没批过的报价不能发出");
    assert_eq!(err.status, 409, "审批缺失是状态冲突");
    assert_eq!(
        err.code,
        error_code::ERP_QUOTATION_APPROVAL_REQUIRED,
        "错误码必须是 66088（前端按它给「去提交审批」）"
    );
    assert!(err.msg.contains(&quote_no), "文案要带单号：{}", err.msg);
    assert!(err.msg.contains("submit-approval"), "文案要给可执行出口：{}", err.msg);

    let after = model::find_quotation(id).await.expect("查单").expect("存在");
    assert_eq!(after.status, model::STATUS_DRAFT, "被闸挡下后必须还是草稿");
    assert!(after.approval_instance_id.is_none(), "不许写审批指针");
    assert_eq!(after.update_by, before.update_by, "被挡下时不许落 update_by");
    let logs = status_logs(id).await;
    assert_eq!(logs.len(), 1, "只该有建单那一行日志：{logs:?}");
    assert_eq!(logs[0].1, model::STATUS_DRAFT);
    assert!(
        !logs.iter().any(|(_, to, _, _)| to == model::STATUS_SENT),
        "不许留 SENT 轨迹"
    );
}

/// ② 有 `APPROVED` 实例 → 闸放行，发出的理由写进状态日志（能追溯是哪一轮批的）。
///
/// 这条同时是约定 11 的兜底路径：审批通过后自动发出若失败（如有效期已过），
/// 人工 `PATCH status=SENT` 必须仍然放行 —— 否则「批了却永远发不出去」。
#[tokio::test]
async fn approved_instance_unlocks_issue_and_reason_names_the_instance() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (id, quote_no) = create_quotation(customer, &[new_line("2", "50", "13")], 14).await;
    let instance_id = seed_instance(id, &quote_no, "APPROVED", "APPROVAL").await;

    let res = change_quotation_status(None, &id.to_string(), status_req(model::STATUS_SENT, None))
        .await
        .expect("有 APPROVED 实例就该放行");
    assert_eq!(res.status, model::STATUS_SENT);
    assert_eq!(res.status_label, "已发出");

    let logs = status_logs(id).await;
    let hop = logs
        .iter()
        .find(|(from, to, _, _)| from.as_deref() == Some(model::STATUS_DRAFT) && to == model::STATUS_SENT)
        .expect("必须有 DRAFT → SENT 轨迹");
    let reason = hop.3.clone().unwrap_or_default();
    assert!(reason.contains("审批通过"), "理由要说明是靠审批发出：{reason}");
    assert!(reason.contains("FLW-M305-"), "理由要能追到具体审批单号：{reason}");

    // 详情页的 approval 段（前端判「待批 / 已批」只看这里，不看 status）
    let detail = get_quotation(&id.to_string()).await.expect("查详情");
    let approval = detail.approval.expect("详情必须带审批摘要");
    assert_eq!(approval.instance_id, instance_id.to_string());
    assert_eq!(approval.status, "APPROVED");
    assert_eq!(approval.status_label, "已通过");
    assert_eq!(approval.pending_tasks, 0);
}

/// ③ `Skipped` 闸（没配流程 + 显式跳过）：直接发出，跳过理由进状态日志。
///
/// 这条路径的存在意义是「报价单不能永远发不出去」：跳过必须是**显式**的
/// （理由由 `approval_gate` 产出并落日志），不是悄悄绕过。
#[tokio::test]
async fn skip_gate_issues_and_persists_the_skip_reason() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (id, _no) = create_quotation(customer, &[new_line("1", "10", "0")], 14).await;

    let skip_reason = "M3-05 单测：库内无 quotation_approve 流程，显式跳过（理由 ≥10 字）";
    let res = change_status_with(
        None,
        &id.to_string(),
        status_req(model::STATUS_SENT, None),
        IssueGate::Skipped(skip_reason.to_string()),
    )
    .await
    .expect("显式跳过应放行");
    assert_eq!(res.status, model::STATUS_SENT);

    let logs = status_logs(id).await;
    let hop = logs
        .iter()
        .find(|(from, to, _, _)| from.as_deref() == Some(model::STATUS_DRAFT) && to == model::STATUS_SENT)
        .expect("必须有 DRAFT → SENT 轨迹");
    assert_eq!(
        hop.3.as_deref(),
        Some(skip_reason),
        "跳过理由必须原样落库（审计要能回答「凭什么没批就发了」）"
    );
}

/// ④ 非法边 / 终态 → 409，并且报文给出「当前可流转到」（不是一句「失败」）。
///
/// 406/500 的区别很实际：前端拿 `allowed_targets` 渲染按钮，拿文案提示用户下一步。
#[tokio::test]
async fn illegal_edges_and_terminal_states_are_409_with_targets() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;

    // 跳步：草稿直接接受
    let (draft, _) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    let err = change_quotation_status(None, &draft.to_string(), status_req(model::STATUS_ACCEPTED, None))
        .await
        .expect_err("DRAFT → ACCEPTED 不是合法边");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("不在合法边表里"), "文案要说清是非法迁移：{}", err.msg);
    // 非法迁移的文案给「当前可流转到」（前端照它渲染按钮）；单号在终态文案里才带
    assert!(err.msg.contains("当前可流转到"), "文案要给出路：{}", err.msg);
    for target in model::allowed_targets(model::STATUS_DRAFT) {
        assert!(err.msg.contains(target), "文案要列出 {target}：{}", err.msg);
    }
    // 跳步的名词也要说人话（中文标签 + 状态码并排）
    assert!(
        err.msg.contains("已接受") && err.msg.contains(model::STATUS_ACCEPTED),
        "文案要给中文标签与状态码：{}",
        err.msg
    );

    // 终态：拒绝之后什么都不许再做
    let (rejected, rejected_no) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    advance_to(rejected, model::STATUS_REJECTED).await;
    for target in [model::STATUS_DRAFT, model::STATUS_SENT, model::STATUS_CANCELLED] {
        let err = change_quotation_status(None, &rejected.to_string(), status_req(target, Some("终态再流转")))
            .await
            .expect_err("终态不许再流转");
        assert_eq!(err.status, 409, "REJECTED → {target} 必须 409");
        assert!(
            err.msg.contains("终态") && err.msg.contains(&rejected_no),
            "终态文案要说清是终态并带单号：{}",
            err.msg
        );
    }
    assert_eq!(status_of(rejected).await, model::STATUS_REJECTED);
}

/// ⑤ 三条「对外推翻承诺」的边缺 `reason` → 400；草稿作废**不要**原因。
#[tokio::test]
async fn reason_is_required_on_commitment_reversing_edges_only() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;

    // 客户拒绝：无原因 400、有原因 200
    let (sent, _) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    advance_to(sent, model::STATUS_SENT).await;
    for blank in [None, Some("   ")] {
        let err = change_quotation_status(None, &sent.to_string(), status_req(model::STATUS_REJECTED, blank))
            .await
            .expect_err("客户拒绝必须给原因");
        assert_eq!(err.status, 400, "缺原因是入参问题（400），不是冲突");
        assert!(err.msg.contains("原因"), "文案要说清缺什么：{}", err.msg);
    }
    let res = change_quotation_status(
        None,
        &sent.to_string(),
        status_req(model::STATUS_REJECTED, Some("客户嫌贵，转竞品")),
    )
    .await
    .expect("给原因后应拒绝成功");
    assert_eq!(res.status, model::STATUS_REJECTED);

    // 发出后作废：同样要原因
    let (sent2, _) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    advance_to(sent2, model::STATUS_SENT).await;
    let err = change_quotation_status(None, &sent2.to_string(), status_req(model::STATUS_CANCELLED, None))
        .await
        .expect_err("发出后作废必须给原因");
    assert_eq!(err.status, 400);

    // 已接受后作废：也要原因
    let (accepted, _) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    advance_to(accepted, model::STATUS_ACCEPTED).await;
    let err = change_quotation_status(None, &accepted.to_string(), status_req(model::STATUS_CANCELLED, None))
        .await
        .expect_err("已接受后作废必须给原因");
    assert_eq!(err.status, 400);

    // 草稿作废：**不要**原因（从没对外发出过）
    let (draft, _) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    let res = change_quotation_status(None, &draft.to_string(), status_req(model::STATUS_CANCELLED, None))
        .await
        .expect("草稿作废不要原因");
    assert_eq!(res.status, model::STATUS_CANCELLED);
    assert!(res.allowed_targets.is_empty(), "作废是终态，前端不该再有按钮");
}

/// ⑥ 已发出但过期的报价不能被接受；把有效期改回来就能接受（失败不留死结）。
#[tokio::test]
async fn expired_quote_cannot_be_accepted_but_recovers_after_fixing_the_date() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (id, quote_no) = create_quotation(customer, &[new_line("1", "100", "13")], 14).await;
    advance_to(id, model::STATUS_SENT).await;

    // 时间事实：把报价日 / 有效期一起推到过去（模拟「报价发出去放了一个月客户才回」）。
    //
    // 两个日期必须**一起推**：`ck_biz_quotation_valid_until` 要求 `valid_until >= quote_date`，
    // 只改有效期会撞库层 CHECK —— 真实世界里也没有「今天报价、昨天到期」的单。
    let yesterday = model::today_cst(Utc::now()) - Duration::days(1);
    let quote_day = yesterday - Duration::days(14);
    db().execute_with_params(
        "UPDATE biz_quotation SET quote_date = ?::text::date, valid_until = ?::text::date \
          WHERE id = ?::uuid",
        vec![
            Value::Text(quote_day.to_string()),
            Value::Text(yesterday.to_string()),
            Value::from(&id),
        ],
    )
    .await
    .expect("把报价日 / 有效期推到过去应成功");

    let err = change_quotation_status(None, &id.to_string(), status_req(model::STATUS_ACCEPTED, None))
        .await
        .expect_err("过期价不能被接受");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains(&quote_no) && err.msg.contains("过期"), "{}", err.msg);
    assert_eq!(status_of(id).await, model::STATUS_SENT, "被挡下后状态不变");

    // 修好有效期 → 同一条边能走通（不存在「过期一次就永远接受不了」的死结）
    let later = model::today_cst(Utc::now()) + Duration::days(30);
    db().execute_with_params(
        "UPDATE biz_quotation SET valid_until = ?::text::date WHERE id = ?::uuid",
        vec![Value::Text(later.to_string()), Value::from(&id)],
    )
    .await
    .expect("续期应成功");
    let res = change_quotation_status(None, &id.to_string(), status_req(model::STATUS_ACCEPTED, None))
        .await
        .expect("续期后应能接受");
    assert_eq!(res.status, model::STATUS_ACCEPTED);
}

/// ⑦ 已转销售订单的报价不许作废；没转过的可以作废。
///
/// 「转单」与「作废」是两条会互相踩的路径：作废已转单的报价，订单就失去了来源。
/// 这条用例把 `convert_quotation` 与 `ensure_cancellable` 串起来验一次。
#[tokio::test]
async fn converted_quote_cannot_be_cancelled_but_plain_accepted_can() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;

    // 转过的：作废 409，文案点名订单
    let (converted, _) = create_quotation(customer, &[new_line("2", "30", "13")], 14).await;
    advance_to(converted, model::STATUS_ACCEPTED).await;
    let order = convert_quotation(None, &converted.to_string(), ConvertQuotationReq::default())
        .await
        .expect("已接受应能转单");
    let err = change_quotation_status(
        None,
        &converted.to_string(),
        status_req(model::STATUS_CANCELLED, Some("客户反悔，想作废报价")),
    )
    .await
    .expect_err("已转单的报价不能作废");
    assert_eq!(err.status, 409);
    assert!(
        err.msg.contains(&order.header.so_id),
        "文案要点名是哪张订单：{}",
        err.msg
    );
    assert_eq!(status_of(converted).await, model::STATUS_ACCEPTED, "状态不许被改");
    // 转单本身也不改状态：仍是已接受
    let row = model::find_quotation(converted).await.expect("查单").expect("存在");
    assert_eq!(row.converted_so_id.map(|id| id.to_string()), Some(order.header.so_id));

    // 没过转单的：作废 200
    let (plain, _) = create_quotation(customer, &[new_line("1", "10", "0")], 14).await;
    advance_to(plain, model::STATUS_ACCEPTED).await;
    let res = change_quotation_status(
        None,
        &plain.to_string(),
        status_req(model::STATUS_CANCELLED, Some("客户取消，未转单")),
    )
    .await
    .expect("没转过单的已接受报价可以作废");
    assert_eq!(res.status, model::STATUS_CANCELLED);
}

/// ⑧ 审批人占着单据时状态推不动（409 带审批单号）；作废要**连审批一起撤**。
///
/// 判据是「有没有 `PENDING` 的 `APPROVAL` 待办」，不是「实例在不在跑」——
/// 后者的反例见下一条用例（退发起人）。
#[tokio::test]
async fn approver_pending_locks_status_but_cancel_cascades_the_approval() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (id, quote_no) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    let instance_id = seed_instance(id, &quote_no, "PENDING", "APPROVAL").await;
    let (instance_status, approval_pending, _) = instance_snapshot(instance_id).await;
    assert_eq!(instance_status, "PENDING");
    assert_eq!(approval_pending, 1, "前提：有一个审批人在等这一票");

    // 审批中不许换状态（403 会让人以为没权限，409 才说得清是状态冲突）
    let err = change_status_with(
        None,
        &id.to_string(),
        status_req(model::STATUS_SENT, None),
        IssueGate::Skipped("M3-05：审批中，试着发出".to_string()),
    )
    .await
    .expect_err("审批中不许发出");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("审批中"), "文案要说清是审批中：{}", err.msg);
    assert!(err.msg.contains("FLW-M305-"), "文案要点名审批单：{}", err.msg);
    assert_eq!(status_of(id).await, model::STATUS_DRAFT);

    // 作废是审批中唯一允许的流转，且同一事务里把审批撤掉（不留孤儿待办）
    let res = change_quotation_status(None, &id.to_string(), status_req(model::STATUS_CANCELLED, None))
        .await
        .expect("审批中可以作废");
    assert_eq!(res.status, model::STATUS_CANCELLED);
    assert!(
        res.approval_instance_id.is_none(),
        "作废后审批指针必须清空（否则列表页显示「审批中」而单已作废）"
    );

    let (after_status, approval_pending, _) = instance_snapshot(instance_id).await;
    assert_eq!(after_status, "CANCELLED", "在跑的审批必须被一起终止");
    assert_eq!(approval_pending, 0, "不许留 PENDING 待办（孤儿待办的典型来源）");
    let comment: Option<String> = db()
        .scalar_with_params(
            "SELECT result_comment FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例终止理由应成功");
    assert!(
        comment.unwrap_or_default().contains("作废"),
        "终止理由要写清是「报价单作废」触发的"
    );
}

/// ⑨ 「退发起人」（`RESUBMIT` 待办）**不锁单**：票在发起人手里，单据必须可改可推进。
///
/// 这是 M3-04 踩过的坑：按「实例还在跑」判锁，退回发起人后单据既改不了也提交不了，
/// 只能找工程师改库。判据只能是「有没有审批人在等这一票」。
#[tokio::test]
async fn resubmit_todo_keeps_the_quote_editable_and_issuable() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (id, quote_no) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    let instance_id = seed_instance(id, &quote_no, "PENDING", "RESUBMIT").await;
    let (instance_status, approval_pending, resubmit_pending) = instance_snapshot(instance_id).await;
    assert_eq!(instance_status, "PENDING", "实例仍是 PENDING（引擎口径）");
    assert_eq!(approval_pending, 0, "前提：没有审批人在等这一票");
    assert_eq!(resubmit_pending, 1, "前提：票在发起人手里");

    // 单据没被锁：能改、能推进
    let res = change_status_with(
        None,
        &id.to_string(),
        status_req(model::STATUS_SENT, None),
        IssueGate::Skipped("M3-05：退发起人后重新走一遍".to_string()),
    )
    .await
    .expect("退发起人不该把单据锁死");
    assert_eq!(res.status, model::STATUS_SENT);
}

/// ⑩ 软删之后所有入口都是 404（不是 409、更不是「查到了但不许改」）。
#[tokio::test]
async fn soft_deleted_quote_is_404_on_every_entry() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (id, _) = create_quotation(customer, &[new_line("1", "10", "13")], 14).await;
    delete_quotation(None, &id.to_string()).await.expect("草稿应能软删");

    let err = get_quotation(&id.to_string()).await.expect_err("详情必须 404");
    assert_eq!(err.status, 404);
    let err = change_quotation_status(None, &id.to_string(), status_req(model::STATUS_SENT, None))
        .await
        .expect_err("已删的单不能流转");
    assert_eq!(err.status, 404);
    let err = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect_err("已删的单不能转单");
    assert_eq!(err.status, 404);
    assert_eq!(order_count(customer).await, 0, "失败路径不许留订单");
}

/// ⑪ 转单并发：两个请求同时点「一键转单」只建**一张**订单。
///
/// `SELECT … FOR UPDATE` 锁报价单行 → 第二个请求在锁上排队，拿到锁后看到
/// `converted_so_id` 已回填 → 走幂等路径返回同一张订单（不是 409，也不是第二张单）。
#[tokio::test]
async fn concurrent_convert_builds_exactly_one_order() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (id, _) = create_quotation(customer, &[new_line("3", "17.655", "13")], 14).await;
    advance_to(id, model::STATUS_ACCEPTED).await;

    let id_text = id.to_string();
    let (first, second) = tokio::join!(
        convert_quotation(None, &id_text, ConvertQuotationReq::default()),
        convert_quotation(None, &id_text, ConvertQuotationReq::default()),
    );
    let first = first.expect("并发转单：第一笔应成功");
    let second = second.expect("并发转单：第二笔应幂等成功（不是 409）");
    assert_eq!(first.header.so_id, second.header.so_id, "两笔必须回同一张订单");
    assert_eq!(first.header.so_no, second.header.so_no);

    let newly = [&first, &second]
        .iter()
        .filter(|res| res.meta.extra.get("already_converted") == Some(&serde_json::json!(false)))
        .count();
    assert_eq!(newly, 1, "必须恰好一笔是真建单，另一笔是幂等回读");
    assert_eq!(order_count(customer).await, 1, "并发转单绝不能建出第二张订单");
}

/// ⑫ 状态日志：每一次流转都留一行（建立 → 发出 → 接受），操作人快照跟着落。
///
/// 审计要求「事后能回答是谁在什么时候把状态推成什么」，所以 operator 不能丢
/// （AGENTS.md 约定 13：审计类信息存快照）。
#[tokio::test]
async fn status_log_records_every_hop_with_operator_snapshot() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let operator = Uuid::now_v7();
    let lines = [new_line("1", "10", "13")];

    // 建单（带操作人）→ 发出 → 接受，全部带同一个操作人
    let today = model::today_cst(Utc::now());
    let tx = db().begin().await.expect("开事务");
    let (id, _no) = model::create_quotation(
        &tx,
        &model::NewQuotation {
            customer_id: customer,
            quote_date: today,
            valid_until: today + Duration::days(14),
            remark: Some("M3-05 日志夹具"),
        },
        &lines,
        &model::Totals::of(&lines),
        state_machine::Operator::of(Some(operator)),
        Utc::now(),
    )
    .await
    .expect("建单应成功");
    tx.commit().await.expect("提交");

    let req = status_req(model::STATUS_SENT, None);
    change_status_with(
        Some(operator),
        &id.to_string(),
        req,
        IssueGate::Skipped("M3-05 日志夹具：跳过审批".to_string()),
    )
    .await
    .expect("发出应成功");
    change_quotation_status(
        Some(operator),
        &id.to_string(),
        status_req(model::STATUS_ACCEPTED, None),
    )
    .await
    .expect("接受应成功");

    let logs = status_logs(id).await;
    assert_eq!(logs.len(), 3, "建立 + 发出 + 接受 = 3 行，实际：{logs:?}");
    assert_eq!(logs[0].0, None, "建单行 from_status 必须是 NULL");
    assert_eq!(logs[0].1, model::STATUS_DRAFT);
    assert_eq!(
        (logs[1].0.as_deref(), logs[1].1.as_str()),
        (Some(model::STATUS_DRAFT), model::STATUS_SENT)
    );
    assert_eq!(
        (logs[2].0.as_deref(), logs[2].1.as_str()),
        (Some(model::STATUS_SENT), model::STATUS_ACCEPTED)
    );
    assert!(
        logs.iter().all(|(_, _, action, _)| action.is_none()),
        "流转不是 CONVERT 动作"
    );

    let operators: Vec<Option<Uuid>> = db()
        .find_with_params(
            "SELECT operator_id FROM biz_status_log \
              WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid ORDER BY created_at, id",
            vec![Value::from(&id)],
        )
        .await
        .expect("查操作人应成功")
        .iter()
        .map(|row| row.get_typed::<Option<Uuid>>("operator_id").unwrap_or(None))
        .collect();
    assert_eq!(operators, vec![Some(operator); 3], "每一跳都要留操作人");
}

// ============================================================================
// 三、清场（`zz_` 前缀 = 本组最后一条）
// ============================================================================

/// 跑完不留夹具：客户 / 报价单 / 销售订单 / 审批实例全清，状态日志的孤儿一并收干。
///
/// 为什么需要它：`cleanup()` 在每条用例**开始时**跑（保证用例从干净库起步），
/// 于是**最后一条用例的夹具会留在库里** —— 演练库跑完不复位，下一组用例就可能踩到
/// `M305-` 的单据（尤其 `converted_so_id` 的唯一索引）。`zz_` 排序最后，正好收尾。
#[tokio::test]
async fn zz_teardown_purges_fixtures_and_status_log_orphans() {
    let Some(_guard) = setup().await else { return };
    // `setup()` 已经跑过一次 `cleanup()`；这里再确认「真的什么都没有」
    for (label, sql) in [
        (
            "夹具客户",
            "SELECT count(*) AS n FROM biz_customer WHERE code = 'M305-CUST'".to_string(),
        ),
        (
            "本组报价单",
            "SELECT count(*) AS n FROM biz_quotation q \
               JOIN biz_customer c ON c.id = q.customer_id WHERE c.code = 'M305-CUST'"
                .to_string(),
        ),
        (
            "本组审批实例",
            "SELECT count(*) AS n FROM biz_flow_instance WHERE flow_code = 'm305_fixture_flow'".to_string(),
        ),
        (
            "本组审批待办",
            "SELECT count(*) AS n FROM biz_flow_task t \
               JOIN biz_flow_instance i ON i.id = t.instance_id \
              WHERE i.flow_code = 'm305_fixture_flow'"
                .to_string(),
        ),
    ] {
        let left: i64 = db()
            .scalar_sql(sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("查{label}残留失败：{err}"));
        assert_eq!(left, 0, "清场后仍有{label}残留：{left} 行");
    }

    // 状态日志没有外键：报价单 / 销售订单的日志一条孤儿都不许剩
    // （`fixture_hygiene::SALES` 是本卡补登记的销售链两类）
    let purged = fixture_hygiene::purge_orphans(fixture_hygiene::SALES).await;
    let orphans = fixture_hygiene::orphans(fixture_hygiene::SALES).await;
    assert_eq!(orphans, 0, "收尾后仍有孤儿状态日志：{orphans} 行");
    println!("[M3-05] 清场：夹具 0 残留，销售链孤儿状态日志 0 条（本次顺手收干 {purged} 条）");
}

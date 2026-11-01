//! P-FLOW-3B 的真库单测：**应付作废审批** + **凭证红冲审批**（≥14 条）。
//!
//! 跑的是 **V0191 的真种子**（`payable_cancel`：3 节点 2 边；`voucher_reverse`：5 节点 5 边、
//! 含 `form.amount > 10000` 的会签分支），不是另造测试图 —— 所以这些用例同时验证
//! 「种子能发起 / 阈值真的落在条件边上 / 会签要两人 / 内控闸生效 / 反向能撤」。
//!
//! ```bash
//! export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_3b'
//! cargo test --lib pflow3b_ -- --test-threads=1
//! ```
//!
//! 夹具全部带 `3B` 前缀（应付单号 `AP-3B-…`、凭证 `source_no = 'P3B-…'`、用户 `p3b_*`），
//! 每次 `setup()` 先按前缀清掉上一轮，重复跑不积累。
//!
//! ⚠️ 与其它真库用例共用全局 `db()` 与同一批 `biz_seq`，所以必须 `--test-threads=1` 串行跑。

use std::str::FromStr;

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::voucher_write;
use crate::business::erp::{payable, payable_bridge, payable_payment, voucher, voucher_bridge};
use crate::business::flow::engine;

/// 财务经理角色（V0191 种子里的固定 UUID）。
const ROLE_FM: &str = "20000000-0000-4000-8000-0000000f0005";
/// 财务总监角色。
const ROLE_FD: &str = "20000000-0000-4000-8000-0000000f0006";
/// 制单人（**同时**是财务经理：用来验证「制单人不能自批」）。
const U_MAKER: &str = "b3b30001-0000-4000-8000-000000000001";
/// 复核人（纯财务经理：正常审批路径用它）。
const U_FM: &str = "b3b30002-0000-4000-8000-000000000002";
/// 财务总监（会签第二票）。
const U_FD: &str = "b3b30003-0000-4000-8000-000000000003";
/// 夹具供应商。
const SUPPLIER: &str = "b3b30010-0000-4000-8000-000000000010";
/// 夹具科目：库存现金（借）。
const ACCOUNT_DEBIT: &str = "b3b30020-0000-4000-8000-000000000020";
/// 夹具科目：应付账款（贷）。
const ACCOUNT_CREDIT: &str = "b3b30021-0000-4000-8000-000000000021";

/// 真库用例必须串行（共用全局 `db()`、同一批夹具与同一批 `biz_seq`）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("测试金额字面量非法")
}

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
            // `max_lifetime = 0` 逼池子每次新建连接，避免借到上一个 runtime 的死连接。
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

async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 清本卡夹具（顺序受外键约束：通知 → 实例 → 待办 → 凭证/分录 → 应付 → 主数据 → 用户）。
///
/// 两处调用：每个用例开头的 `setup()`（保证入口干净），以及模块里**名字排最后**的
/// `pflow3b_zz_teardown_purges_fixtures` —— 全量套件是同一个库串行跑，
/// 本模块若把 P3B 科目 / 供应商留在库里，后面按「科目全集」断言的用例会误红
/// （`cash_flow::tests::r3_real_config_covers_all_accounts`）。
async fn purge_fixtures() {
    let cleanups = [
        "DELETE FROM sys_notice_receiver WHERE notice_id IN \
           (SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
              (SELECT id::text FROM biz_flow_instance WHERE biz_type IN \
                 ('PAYABLE_CANCEL','VOUCHER_REVERSE')))"
            .to_string(),
        "DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
           (SELECT id::text FROM biz_flow_instance WHERE biz_type IN \
              ('PAYABLE_CANCEL','VOUCHER_REVERSE'))"
            .to_string(),
        "UPDATE biz_flow_instance SET prev_instance_id = NULL \
          WHERE prev_instance_id IN \
            (SELECT id FROM biz_flow_instance WHERE biz_type IN \
               ('PAYABLE_CANCEL','VOUCHER_REVERSE'))"
            .to_string(),
        "DELETE FROM biz_flow_instance WHERE biz_type IN ('PAYABLE_CANCEL','VOUCHER_REVERSE')"
            .to_string(),
        "DELETE FROM biz_flow_task WHERE assignee_id IN \
           (SELECT id FROM sys_user WHERE username LIKE 'p3b_%')"
            .to_string(),
        "UPDATE biz_flow_instance SET prev_instance_id = NULL \
          WHERE initiator_id IN (SELECT id FROM sys_user WHERE username LIKE 'p3b_%')"
            .to_string(),
        "DELETE FROM biz_flow_instance \
          WHERE initiator_id IN (SELECT id FROM sys_user WHERE username LIKE 'p3b_%')"
            .to_string(),
        "DELETE FROM biz_status_log WHERE entity_id IN \
           (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-3B-%')"
            .to_string(),
        "DELETE FROM biz_status_log WHERE entity_id IN \
           (SELECT id FROM biz_voucher WHERE source_no LIKE 'P3B-%')"
            .to_string(),
        "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
           (SELECT id FROM biz_voucher WHERE source_no LIKE 'P3B-%')"
            .to_string(),
        "DELETE FROM biz_voucher WHERE source_no LIKE 'P3B-%'".to_string(),
        "DELETE FROM biz_payable WHERE payable_no LIKE 'AP-3B-%'".to_string(),
        // 科目 / 供应商也是共享主数据：留着会让「全量套件」里按科目全集的用例误红
        // （`cash_flow::tests::r3_real_config_covers_all_accounts`：分类配置漏科目）。
        "DELETE FROM biz_opening_balance WHERE account_id IN \
           (SELECT id FROM biz_account WHERE account_code LIKE 'P3B.%')"
            .to_string(),
        "DELETE FROM biz_account WHERE account_code LIKE 'P3B.%'".to_string(),
        "DELETE FROM biz_supplier WHERE supplier_code = 'P3B-SUP'".to_string(),
        "DELETE FROM sys_user_role WHERE role_id IN \
           ('20000000-0000-4000-8000-0000000f0005'::uuid, \
            '20000000-0000-4000-8000-0000000f0006'::uuid)"
            .to_string(),
        "DELETE FROM sys_user_role WHERE user_id IN \
           (SELECT id FROM sys_user WHERE username LIKE 'p3b_%')"
            .to_string(),
        "DELETE FROM sys_user WHERE username LIKE 'p3b_%'".to_string(),
    ];
    for sql in cleanups {
        exec(&sql).await;
    }
}

/// 拿锁 + 连库 + 清上一轮 + 建夹具。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    // 清上一轮
    purge_fixtures().await;

    // 供应商 + 两个科目（凭证分录的外键）
    exec(&format!(
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days) \
         VALUES ('{SUPPLIER}'::uuid, 'P3B-SUP', 'P3B 财务链测试供应商', 1, 30) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    exec(&format!(
        "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, \
            account_category, balance_direction, is_leaf, status) VALUES \
         ('{ACCOUNT_DEBIT}'::uuid, 'P3B.1002', 'P3B 银行存款', NULL, 1, 'ASSET', 'DEBIT', 1, 1), \
         ('{ACCOUNT_CREDIT}'::uuid, 'P3B.2202', 'P3B 应付账款', NULL, 1, 'LIABILITY', 'CREDIT', 1, 1) \
         ON CONFLICT DO NOTHING"
    ))
    .await;

    // 三个账号：制单人（也是财务经理）/ 纯财务经理 / 财务总监
    exec(&format!(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES \
         ('{U_MAKER}'::uuid, 'p3b_maker', 'x', 'P3B 制单人', 1), \
         ('{U_FM}'::uuid, 'p3b_fm', 'x', 'P3B 财务经理', 1), \
         ('{U_FD}'::uuid, 'p3b_fd', 'x', 'P3B 财务总监', 1) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    // 角色是共享资源：只留本卡夹具的人（否则「派给谁 / 几条待办」不可判定）
    exec(&format!(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES \
         ('{U_MAKER}'::uuid, '{ROLE_FM}'::uuid), \
         ('{U_FM}'::uuid, '{ROLE_FM}'::uuid), \
         ('{U_FD}'::uuid, '{ROLE_FD}'::uuid) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    Some(guard)
}

// ==================== 夹具：应付单 / 凭证 ====================

/// 造一张应付单（`status` 由调用方指定；`paid` 同步算出 `unpaid`，满足库侧恒等）。
async fn payable_with(amount: &str, paid: &str, status: &str, creator: &str) -> Uuid {
    let now = Utc::now();
    let supplier_id = uid(SUPPLIER);
    let due = NaiveDate::from_ymd_opt(2026, 12, 31).expect("日期非法");
    let period = voucher::period_of(db(), due).await.expect("取会计期间失败");
    let id = payable::insert(
        db(),
        &payable::NewPayable {
            supplier_id,
            source_type: payable::SOURCE_STOCK_IN,
            source_id: Uuid::now_v7(),
            source_no: "P3B-IN",
            amount: dec(amount),
            credit_days: 30,
            due_date: due,
            period_code: &period,
            voucher_id: None,
        },
        Some(uid(creator)),
        now,
    )
    .await
    .unwrap_or_else(|err| panic!("造应付单失败：{err}"));
    // 直接写状态（夹具不测状态机）：金额恒等必须一起写，否则 ck_biz_payable_unpaid 违约
    exec(&format!(
        "UPDATE biz_payable SET status = '{status}', paid_amount = {paid}, \
            unpaid_amount = {amount} - {paid} WHERE id = '{id}'::uuid"
    ))
    .await;
    id
}

/// 造一张金额 = `amount` 的蓝字凭证（`POSTED`，两行：借 银行存款 / 贷 应付账款）。
async fn posted_voucher(amount: &str, creator: &str) -> Uuid {
    let now = Utc::now();
    let date = NaiveDate::from_ymd_opt(2026, 11, 2).expect("日期非法");
    let total = dec(amount);
    let lines = vec![
        voucher::EntryLine {
            line_no: 1,
            summary: "P3B 测试分录（借）".to_string(),
            account: voucher::AccountRef {
                id: uid(ACCOUNT_DEBIT),
                code: "P3B.1002".to_string(),
                name: "P3B 银行存款".to_string(),
            },
            debit: total,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
        },
        voucher::EntryLine {
            line_no: 2,
            summary: "P3B 测试分录（贷）".to_string(),
            account: voucher::AccountRef {
                id: uid(ACCOUNT_CREDIT),
                code: "P3B.2202".to_string(),
                name: "P3B 应付账款".to_string(),
            },
            debit: Decimal::ZERO,
            credit: total,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
        },
    ];
    voucher::post_voucher(
        db(),
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_MANUAL,
            source_type: voucher::SOURCE_MANUAL,
            source_id: None,
            source_no: Some("P3B-凭证"),
            voucher_date: date,
            summary: "P3B 红冲审批测试凭证".to_string(),
            lines,
            remark: None,
            red_of: None,
        },
        Some(uid(creator)),
        now,
    )
    .await
    .unwrap_or_else(|err| panic!("造凭证失败：{err}"))
}

// ==================== 夹具：读状态 ====================

async fn payable_row(id: Uuid) -> payable::PayableRow {
    let sql = format!(
        "SELECT {} FROM biz_payable WHERE id = ?::uuid",
        payable::COLUMNS
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&id)])
        .await
        .expect("查应付单失败");
    let row = rows.first().expect("应付单不存在");
    payable::PayableRow {
        id: row.get_typed("id").unwrap(),
        payable_no: row.get_typed("payable_no").unwrap(),
        supplier_id: row.get_typed("supplier_id").unwrap(),
        source_type: row.get_typed("source_type").unwrap(),
        source_id: row.get_typed("source_id").unwrap(),
        source_no: row.get_typed("source_no").unwrap(),
        invoice_no: row.get_typed("invoice_no").unwrap(),
        invoice_date: row.get_typed("invoice_date").unwrap(),
        currency_code: row.get_typed("currency_code").unwrap(),
        amount: row.get_typed("amount").unwrap(),
        paid_amount: row.get_typed("paid_amount").unwrap(),
        unpaid_amount: row.get_typed("unpaid_amount").unwrap(),
        credit_days: row.get_typed("credit_days").unwrap(),
        due_date: row.get_typed("due_date").unwrap(),
        period_code: row.get_typed("period_code").unwrap(),
        status: row.get_typed("status").unwrap(),
        settled_at: row.get_typed("settled_at").unwrap(),
        voucher_id: row.get_typed("voucher_id").unwrap(),
        remark: row.get_typed("remark").unwrap(),
        cancel_instance_id: row.get_typed("cancel_instance_id").unwrap(),
        cancel_reason: row.get_typed("cancel_reason").unwrap(),
        created_at: row.get_typed("created_at").unwrap(),
        updated_at: row.get_typed("updated_at").unwrap(),
    }
}

async fn voucher_row(id: Uuid) -> voucher::VoucherRow {
    voucher::find_voucher(db(), id)
        .await
        .expect("查凭证失败")
        .expect("凭证不存在")
}

async fn instance_of(biz_type: &str, biz_id: Uuid) -> Option<String> {
    let rows = db()
        .find_with_params(
            "SELECT status FROM biz_flow_instance \
              WHERE biz_type = ? AND biz_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY started_at DESC LIMIT 1",
            vec![Value::Text(biz_type.to_string()), Value::from(&biz_id)],
        )
        .await
        .expect("查审批实例失败");
    rows.first().map(|row| row.get_typed("status").unwrap())
}

async fn instance_id_of(biz_type: &str, biz_id: Uuid) -> Uuid {
    db().scalar_with_params::<Option<Uuid>>(
        "SELECT id FROM biz_flow_instance \
          WHERE biz_type = ? AND biz_id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL \
          LIMIT 1",
        vec![Value::Text(biz_type.to_string()), Value::from(&biz_id)],
    )
    .await
    .expect("查审批实例失败")
    .expect("应当有在跑的审批实例")
}

/// 某实例的 `PENDING` 待办：`(task_id, assignee_id)`，按 assignee 排序。
async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY assignee_id, id",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("读待办失败");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("id").unwrap(),
                row.get_typed("assignee_id").unwrap(),
            )
        })
        .collect()
}

/// 某人在该实例上的待办（**不限状态**）：`(task_id, status)`。
async fn task_of(instance_id: Uuid, assignee: Uuid) -> Option<(Uuid, String)> {
    let rows = db()
        .find_with_params(
            "SELECT id, status FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND assignee_id = ?::uuid ORDER BY created_at LIMIT 1",
            vec![Value::from(&instance_id), Value::from(&assignee)],
        )
        .await
        .expect("读待办失败");
    rows.first().map(|row| {
        (
            row.get_typed("id").unwrap(),
            row.get_typed("status").unwrap(),
        )
    })
}

/// 以某人身份同意一条待办（**只动引擎**，回调由调用点自己调）。
async fn approve(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
    db().transaction(|tx| async move {
        engine::approve(&tx, task_id, operator, Some("同意（P3B 单测）")).await
    })
    .await
    .unwrap_or_else(|err| panic!("审批同意失败：{err}"))
}

/// 以某人身份驳回并终止一条待办。
async fn reject_final(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
    db().transaction(|tx| async move {
        engine::reject(
            &tx,
            task_id,
            operator,
            "驳回：P3B 单测（金额/单据不对，请复核后重提）",
            engine::ReturnTo::Final,
        )
        .await
    })
    .await
    .unwrap_or_else(|err| panic!("审批驳回失败：{err}"))
}

/// 红字凭证（`reverse_of = 原凭证`）的 id 列表。
async fn red_children(original: Uuid) -> Vec<Uuid> {
    db().find_with_params(
        "SELECT id FROM biz_voucher WHERE reverse_of = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&original)],
    )
    .await
    .expect("查红字凭证失败")
    .iter()
    .map(|row| row.get_typed::<Uuid>("id").unwrap())
    .collect()
}

// ==================== 应付作废审批（7 条） ====================

/// ① `OPEN` 且 `paid = 0` → 提交成功：状态 `PENDING_CANCEL`、挂实例、**欠款一分没动**。
#[tokio::test]
async fn pflow3b_payable_submit_cancel_enters_pending() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = payable_with("1000", "0", payable::STATUS_OPEN, U_FM).await;
    let before = payable_row(id).await;

    let out = payable_payment::submit_cancel(id, Some("供应商重复开票"), Some(uid(U_FM)))
        .await
        .unwrap_or_else(|err| panic!("提交作废失败：{} {}", err.code, err.msg));
    assert_eq!(out.outcome, payable_payment::OUTCOME_CANCEL_PENDING);
    assert!(out.approval_instance_id.is_some(), "必须回写审批实例 id");

    let after = payable_row(id).await;
    assert_eq!(after.status, payable::STATUS_PENDING_CANCEL);
    assert!(
        after.cancel_instance_id.is_some(),
        "必须挂 cancel_instance_id"
    );
    assert_eq!(
        after.cancel_reason.as_deref(),
        Some("供应商重复开票"),
        "作废原因要落库（审批人据此判断）"
    );
    // 账一分没动
    assert_eq!(after.amount, before.amount);
    assert_eq!(after.paid_amount, before.paid_amount);
    assert_eq!(after.unpaid_amount, before.unpaid_amount);
    // 状态日志（P10 铁律③：唯一入口必须留痕）
    let logs: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_status_log \
              WHERE entity_id = ?::uuid AND from_status = 'OPEN' AND to_status = 'PENDING_CANCEL'",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(logs, 1, "状态变更必须写 biz_status_log");

    // 待办：只有财务经理（制单人是 U_FM 本人？不是 —— 这张单的 create_by = U_FM 被内控排除，
    // 所以待办落到同角色的另一个人 U_MAKER 上）
    let instance = instance_id_of(payable::CANCEL_BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;
    assert_eq!(tasks.len(), 1, "应只有一条待办（另一名财务经理）");
    assert_eq!(tasks[0].1, uid(U_MAKER), "制单人（U_FM）必须被内控排除");
    // 被排除的人有 SKIPPED 留痕（审计要能回答「谁没批、为什么」）
    let (_, status) = task_of(instance, uid(U_FM)).await.expect("应有留痕行");
    assert_eq!(status, "SKIPPED", "制单人的待办必须落 SKIPPED 留痕");
}

/// ② 审批通过 → `CANCELLED`（欠款消失、实例引用摘掉、状态日志齐）。
#[tokio::test]
async fn pflow3b_payable_approve_cancels() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = payable_with("1000", "0", payable::STATUS_OPEN, U_FM).await;
    payable_payment::submit_cancel(id, Some("重复开票"), Some(uid(U_FM)))
        .await
        .expect("提交作废失败");
    let instance = instance_id_of(payable::CANCEL_BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;
    assert_eq!(tasks.len(), 1);

    let out = approve(tasks[0].0, uid(U_MAKER)).await;
    assert_eq!(out.instance_status, "APPROVED");
    payable_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;

    let after = payable_row(id).await;
    assert_eq!(
        after.status,
        payable::STATUS_CANCELLED,
        "审批通过必须真作废"
    );
    assert!(
        after.cancel_instance_id.is_none(),
        "作废后必须摘掉实例引用（ck_biz_payable_pending_cancel 要求）"
    );
    assert!(
        after
            .remark
            .as_deref()
            .unwrap_or_default()
            .contains("重复开票"),
        "作废原因要写进 remark：{:?}",
        after.remark
    );
    // 金额列不动：作废是「状态 + 红冲进项凭证」，不是把金额抹成 0
    //（应付列表默认展示全部状态 —— 财务对账要看得见作废单；账面后果靠红冲凭证体现）
    assert_eq!(after.amount, dec("1000"));
    assert_eq!(after.paid_amount, Decimal::ZERO);
    assert_eq!(after.unpaid_amount, dec("1000"));
    assert_eq!(after.status, payable::STATUS_CANCELLED);
}

/// ③ 审批驳回 → 回到 `OPEN`（账一分没动），可以再次提交。
#[tokio::test]
async fn pflow3b_payable_reject_restores_open() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = payable_with("1000", "0", payable::STATUS_OPEN, U_FM).await;
    payable_payment::submit_cancel(id, Some("重复开票"), Some(uid(U_FM)))
        .await
        .expect("提交作废失败");
    let instance = instance_id_of(payable::CANCEL_BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;

    let out = reject_final(tasks[0].0, uid(U_MAKER)).await;
    assert_eq!(out.instance_status, "REJECTED");
    payable_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;

    let after = payable_row(id).await;
    assert_eq!(after.status, payable::STATUS_OPEN, "驳回必须回到未付");
    assert!(after.cancel_instance_id.is_none(), "驳回必须摘掉实例引用");
    assert_eq!(after.unpaid_amount, dec("1000"), "欠款还在（一分没动）");

    // 还能再提交一次（孤儿实例/引用没清干净的话这里会 409）
    payable_payment::submit_cancel(id, Some("再提一次"), Some(uid(U_FM)))
        .await
        .unwrap_or_else(|err| panic!("驳回后重提失败：{} {}", err.code, err.msg));
    assert_eq!(payable_row(id).await.status, payable::STATUS_PENDING_CANCEL);
}

/// ④ `PARTIAL_PAID` 提交作废 → **409**（卡面决策：已付过钱的必须走付款冲销）。
#[tokio::test]
async fn pflow3b_payable_partial_paid_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = payable_with("1000", "300", payable::STATUS_PARTIAL_PAID, U_FM).await;
    let err = payable_payment::submit_cancel(id, Some("想作废"), Some(uid(U_FM)))
        .await
        .expect_err("部分已付必须拒绝");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(
        err.msg.contains("已付") || err.msg.contains("部分已付"),
        "文案要点名已付：{}",
        err.msg
    );
    assert_eq!(
        payable_row(id).await.status,
        payable::STATUS_PARTIAL_PAID,
        "被拒后状态不能变"
    );
    assert!(
        instance_of(payable::CANCEL_BIZ_TYPE, id).await.is_none(),
        "被拒后不许留下审批实例"
    );
}

/// ⑤ `SETTLED` 提交作废 → **409**。
#[tokio::test]
async fn pflow3b_payable_settled_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = payable_with("1000", "1000", payable::STATUS_SETTLED, U_FM).await;
    let err = payable_payment::submit_cancel(id, Some("想作废"), Some(uid(U_FM)))
        .await
        .expect_err("已结清必须拒绝");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert_eq!(payable_row(id).await.status, payable::STATUS_SETTLED);
    assert!(
        instance_of(payable::CANCEL_BIZ_TYPE, id).await.is_none(),
        "被拒后不许留下审批实例"
    );
}

/// ⑥ 制单人自批 → **403**（内控闸，事务之前就拦）。
#[tokio::test]
async fn pflow3b_payable_self_approval_forbidden() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 制单人 = U_FM（他也挂着财务经理角色）
    let id = payable_with("1000", "0", payable::STATUS_OPEN, U_FM).await;
    payable_payment::submit_cancel(id, Some("重复开票"), Some(uid(U_FM)))
        .await
        .expect("提交作废失败");
    let instance = instance_id_of(payable::CANCEL_BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;
    assert_eq!(tasks[0].1, uid(U_MAKER), "待办已派给别人（制单人被排除）");

    // 制单人拿着**别人的**待办来批 → 403（防转办 / 加签 / 老数据绕过）
    let err = payable_bridge::guard_self_approval(tasks[0].0, uid(U_FM))
        .await
        .expect_err("制单人自批必须 403");
    assert_eq!(err.status, 403, "{}", err.msg);
    assert_eq!(
        err.code,
        crate::shared::error_code::FLOW_SELF_APPROVAL_FORBIDDEN
    );
    assert!(err.msg.contains("内控"), "{}", err.msg);
    // 别人来批：放行
    payable_bridge::guard_self_approval(tasks[0].0, uid(U_MAKER))
        .await
        .expect("非制单人应当放行");
}

/// ⑦ 审批中删除应付单 → **撤审批**（孤儿实例回收，P11 教训的机器化兜底）。
#[tokio::test]
async fn pflow3b_payable_delete_cancels_approval() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = payable_with("1000", "0", payable::STATUS_OPEN, U_FM).await;
    payable_payment::submit_cancel(id, Some("重复开票"), Some(uid(U_FM)))
        .await
        .expect("提交作废失败");
    let instance = instance_id_of(payable::CANCEL_BIZ_TYPE, id).await;
    assert_eq!(pending_tasks(instance).await.len(), 1);

    // 模拟「审批中单据被删」（软删；本仓应付单没有删除接口，删除由运维/外部导入路径触发）
    exec(&format!(
        "UPDATE biz_payable SET deleted_at = now() WHERE id = '{id}'::uuid"
    ))
    .await;

    let cancelled = payable_bridge::reconcile_orphan_payable_cancels()
        .await
        .expect("孤儿回收失败");
    assert!(cancelled >= 1, "必须回收这条孤儿实例");
    assert_eq!(
        instance_of(payable::CANCEL_BIZ_TYPE, id).await.as_deref(),
        Some("CANCELLED"),
        "单据被删后审批实例必须终止"
    );
    assert!(
        pending_tasks(instance).await.is_empty(),
        "终止后不许留 PENDING 待办（否则审批中心点进去 404）"
    );
}

/// ⑦b 反向：发起人自己**撤回** → 回到 `OPEN`（正向配反向，L4）。
#[tokio::test]
async fn pflow3b_payable_withdraw_restores_open() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = payable_with("1000", "0", payable::STATUS_OPEN, U_FM).await;
    payable_payment::submit_cancel(id, Some("重复开票"), Some(uid(U_FM)))
        .await
        .expect("提交作废失败");
    payable_payment::withdraw_cancel(id, None, Some(uid(U_FM)))
        .await
        .unwrap_or_else(|err| panic!("撤回失败：{} {}", err.code, err.msg));
    let after = payable_row(id).await;
    assert_eq!(after.status, payable::STATUS_OPEN, "撤回必须回到未付");
    assert!(after.cancel_instance_id.is_none());
    assert_eq!(after.unpaid_amount, dec("1000"));
    // 待办清零（撤回后审批中心不该还挂着这条）
    assert!(
        instance_of(payable::CANCEL_BIZ_TYPE, id).await.as_deref() == Some("WITHDRAWN"),
        "实例状态必须是 WITHDRAWN"
    );
}

// ==================== 凭证红冲审批（7 条） ====================

/// ⑧ `POSTED` 提交红冲 → `pending_reverse_instance_id` 有值、**`status` 仍 `POSTED`**。
#[tokio::test]
async fn pflow3b_voucher_submit_reverse_flags_pending_only() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = posted_voucher("8000", U_FM).await;
    let out = voucher_write::submit_reverse(id, Some("凭证金额录错"), Some(uid(U_FM)))
        .await
        .unwrap_or_else(|err| panic!("提交红冲失败：{} {}", err.code, err.msg));
    assert_eq!(out.outcome, voucher_write::OUTCOME_REVERSE_PENDING);

    let row = voucher_row(id).await;
    assert_eq!(
        row.status,
        voucher::STATUS_POSTED,
        "硬红线 3：原凭证状态必须保持 POSTED（否则报表取数会漏掉这张已入账凭证）"
    );
    assert!(
        row.pending_reverse_instance_id.is_some(),
        "必须挂 pending_reverse_instance_id"
    );
    assert!(row.reversed_by.is_none(), "送审时不能红冲");
    assert_eq!(row.reverse_reason.as_deref(), Some("凭证金额录错"));
    // 8000 ≤ 10000 → 单人审批（财务经理）
    let instance = instance_id_of(voucher_bridge::BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;
    assert_eq!(tasks.len(), 1, "1 万以内走财务经理单人审批");
    assert_eq!(
        tasks[0].1,
        uid(U_MAKER),
        "制单人（U_FM）被内控排除，派给另一名财务经理"
    );
}

/// ⑨ 审批通过 → 生成红字凭证 + `reversed_by` 回写 + pending 清空。
#[tokio::test]
async fn pflow3b_voucher_approve_generates_red() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = posted_voucher("8000", U_FM).await;
    voucher_write::submit_reverse(id, Some("凭证金额录错"), Some(uid(U_FM)))
        .await
        .expect("提交红冲失败");
    let instance = instance_id_of(voucher_bridge::BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;

    let out = approve(tasks[0].0, uid(U_MAKER)).await;
    assert_eq!(out.instance_status, "APPROVED");
    voucher_bridge::on_reverse_settled(out.instance_id, &out.instance_status).await;

    let original = voucher_row(id).await;
    assert_eq!(original.status, voucher::STATUS_POSTED, "原凭证仍 POSTED");
    assert!(
        original.pending_reverse_instance_id.is_none(),
        "通过后必须清空 pending"
    );
    let red_id = original.reversed_by.expect("必须回写 reversed_by");
    let reds = red_children(id).await;
    assert_eq!(reds.len(), 1, "必须且只生成一张红字凭证");
    assert_eq!(reds[0], red_id);

    let red = voucher_row(red_id).await;
    assert_eq!(red.is_red, voucher::IS_RED_YES, "红字凭证 is_red=1");
    assert_eq!(red.reverse_of, Some(id), "红字凭证 reverse_of 指向原凭证");
    assert_eq!(red.total_debit, original.total_credit, "借贷互换（借）");
    assert_eq!(red.total_credit, original.total_debit, "借贷互换（贷）");
}

/// ⑩ 审批驳回 → pending 清空、**无红字**、原凭证仍 `POSTED`。
#[tokio::test]
async fn pflow3b_voucher_reject_clears_pending() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = posted_voucher("8000", U_FM).await;
    voucher_write::submit_reverse(id, Some("凭证金额录错"), Some(uid(U_FM)))
        .await
        .expect("提交红冲失败");
    let instance = instance_id_of(voucher_bridge::BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;

    let out = reject_final(tasks[0].0, uid(U_MAKER)).await;
    assert_eq!(out.instance_status, "REJECTED");
    voucher_bridge::on_reverse_settled(out.instance_id, &out.instance_status).await;

    let original = voucher_row(id).await;
    assert_eq!(original.status, voucher::STATUS_POSTED);
    assert!(
        original.pending_reverse_instance_id.is_none(),
        "驳回必须清 pending（否则这张凭证永远提交不了红冲）"
    );
    assert!(original.reversed_by.is_none(), "驳回不许生成红字");
    assert!(
        red_children(id).await.is_empty(),
        "库里的红字凭证数必须是 0"
    );

    // 驳回后还能再提交（pending 没清干净的话这里会 409）
    voucher_write::submit_reverse(id, Some("重新提交"), Some(uid(U_FM)))
        .await
        .unwrap_or_else(|err| panic!("驳回后重提失败：{} {}", err.code, err.msg));
}

/// ⑪ 已红冲的再提交 → **409**。
#[tokio::test]
async fn pflow3b_voucher_already_reversed_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = posted_voucher("8000", U_FM).await;
    // 先走一遍完整审批，把它冲掉
    voucher_write::submit_reverse(id, Some("凭证金额录错"), Some(uid(U_FM)))
        .await
        .expect("提交红冲失败");
    let instance = instance_id_of(voucher_bridge::BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;
    let out = approve(tasks[0].0, uid(U_MAKER)).await;
    voucher_bridge::on_reverse_settled(out.instance_id, &out.instance_status).await;
    assert!(voucher_row(id).await.reversed_by.is_some());

    let err = voucher_write::submit_reverse(id, Some("再红冲一次"), Some(uid(U_FM)))
        .await
        .expect_err("已红冲必须 409");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("已经红冲"), "{}", err.msg);
}

/// ⑫ 金额 > 1 万 → 走**会签**（两人都批才通过）。阈值来自 V0191 的条件边。
#[tokio::test]
async fn pflow3b_voucher_large_amount_requires_countersign() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = posted_voucher("20000", U_FM).await;
    voucher_write::submit_reverse(id, Some("大额凭证红冲"), Some(uid(U_FM)))
        .await
        .expect("提交红冲失败");
    let instance = instance_id_of(voucher_bridge::BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;
    assert_eq!(
        tasks.len(),
        2,
        "2 万 > 1 万必须走会签（财务经理 + 财务总监，制单人被排除后剩 CFO 与另一名 FM 之外的第二票）"
    );

    // 第一票：实例还在跑，不生成红字
    let first = approve(tasks[0].0, tasks[0].1).await;
    assert_eq!(first.instance_status, "PENDING", "会签只批一票不许通过");
    voucher_bridge::on_reverse_settled(first.instance_id, &first.instance_status).await;
    assert!(
        voucher_row(id).await.reversed_by.is_none(),
        "会签未凑齐时不许红冲"
    );

    // 第二票：通过
    let second = approve(tasks[1].0, tasks[1].1).await;
    assert_eq!(second.instance_status, "APPROVED");
    voucher_bridge::on_reverse_settled(second.instance_id, &second.instance_status).await;
    let original = voucher_row(id).await;
    assert!(original.reversed_by.is_some(), "两票齐了才红冲");
    assert_eq!(red_children(id).await.len(), 1);
}

/// ⑬ 制单人自批 → **403**。
#[tokio::test]
async fn pflow3b_voucher_self_approval_forbidden() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = posted_voucher("8000", U_FM).await;
    voucher_write::submit_reverse(id, Some("凭证金额录错"), Some(uid(U_FM)))
        .await
        .expect("提交红冲失败");
    let instance = instance_id_of(voucher_bridge::BIZ_TYPE, id).await;
    let tasks = pending_tasks(instance).await;

    let err = voucher_bridge::guard_self_approval(tasks[0].0, uid(U_FM))
        .await
        .expect_err("制单人自批必须 403");
    assert_eq!(err.status, 403, "{}", err.msg);
    assert_eq!(
        err.code,
        crate::shared::error_code::FLOW_SELF_APPROVAL_FORBIDDEN
    );
    voucher_bridge::guard_self_approval(tasks[0].0, uid(U_MAKER))
        .await
        .expect("非制单人应当放行");
}

/// ⑭ 审批中删除凭证 → **撤审批**（孤儿回收），pending 清空。
#[tokio::test]
async fn pflow3b_voucher_delete_cancels_approval() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = posted_voucher("8000", U_FM).await;
    voucher_write::submit_reverse(id, Some("凭证金额录错"), Some(uid(U_FM)))
        .await
        .expect("提交红冲失败");
    let instance = instance_id_of(voucher_bridge::BIZ_TYPE, id).await;
    assert_eq!(pending_tasks(instance).await.len(), 1);

    exec(&format!(
        "UPDATE biz_voucher SET deleted_at = now() WHERE id = '{id}'::uuid"
    ))
    .await;
    let cancelled = voucher_bridge::reconcile_orphan_reverses()
        .await
        .expect("孤儿回收失败");
    assert!(cancelled >= 1, "必须回收这条孤儿实例");
    assert_eq!(
        instance_of(voucher_bridge::BIZ_TYPE, id).await.as_deref(),
        Some("CANCELLED")
    );
    assert!(pending_tasks(instance).await.is_empty());
}

/// ⑭b 反向：发起人**撤回**红冲 → 只清 pending，凭证一直 `POSTED`、无红字。
#[tokio::test]
async fn pflow3b_voucher_withdraw_clears_pending() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = posted_voucher("8000", U_FM).await;
    voucher_write::submit_reverse(id, Some("凭证金额录错"), Some(uid(U_FM)))
        .await
        .expect("提交红冲失败");
    voucher_write::withdraw_reverse(id, None, Some(uid(U_FM)))
        .await
        .unwrap_or_else(|err| panic!("撤回失败：{} {}", err.code, err.msg));
    let row = voucher_row(id).await;
    assert_eq!(row.status, voucher::STATUS_POSTED);
    assert!(
        row.pending_reverse_instance_id.is_none(),
        "撤回必须清 pending"
    );
    assert!(red_children(id).await.is_empty());
    assert_eq!(
        instance_of(voucher_bridge::BIZ_TYPE, id).await.as_deref(),
        Some("WITHDRAWN")
    );
}

// ==================== 种子自检（graph_hash 必须与行一致） ====================

/// ⑮ V0191 两个种子的 `graph_snapshot` / `graph_hash` 必须与**行数据重算**逐字节一致。
///
/// 这条是「手写 hash 写错了」的唯一机器化防线：`graph::load_graph` 从 `biz_flow_node` /
/// `biz_flow_edge` / `biz_flow_node_assignee` 重建图，`graph_hash()` 重算 sha256，
/// 与迁移里写死的值比对（与 `flow::maintain` 的每日自检同一口径）。
#[tokio::test]
async fn pflow3b_seeded_flows_hash_matches_rows() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    for (flow_code, expect_nodes, expect_edges) in [
        ("payable_cancel", 3usize, 2usize),
        ("voucher_reverse", 5, 5),
    ] {
        let rows = db()
            .find_with_params(
                "SELECT d.current_version_id AS version_id, v.graph_hash, v.node_count, v.edge_count \
                   FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id \
                  WHERE d.flow_code = ? AND d.deleted_at IS NULL",
                vec![Value::Text(flow_code.to_string())],
            )
            .await
            .expect("查流程定义失败");
        let row = rows
            .first()
            .unwrap_or_else(|| panic!("V0191 没种出流程 {flow_code}"));
        let version_id: Uuid = row.get_typed("version_id").unwrap();
        let stored: String = row.get_typed("graph_hash").unwrap();
        let nodes: i32 = row.get_typed("node_count").unwrap();
        let edges: i32 = row.get_typed("edge_count").unwrap();
        assert_eq!(nodes as usize, expect_nodes, "{flow_code} 节点数不对");
        assert_eq!(edges as usize, expect_edges, "{flow_code} 边数不对");

        let graph = crate::business::flow::model::load_graph(version_id)
            .await
            .expect("从行重建流程图失败");
        assert_eq!(
            graph.graph_hash(),
            stored,
            "{flow_code} 的 graph_hash 与行数据重算不一致（快照写错了）"
        );
    }
}

/// ⑯ 种子里的「金额 > 1 万会签」必须真的落在**条件边**上（不是代码里 if/else）。
#[tokio::test]
async fn pflow3b_seed_threshold_is_on_edge() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let rows = db()
        .find_with_params(
            "SELECT e.condition_expr->>'field' AS field, e.condition_expr->>'cmp' AS cmp, \
                    (e.condition_expr->>'value') AS value \
               FROM biz_flow_edge e JOIN biz_flow_def d ON d.current_version_id = e.version_id \
              WHERE d.flow_code = 'voucher_reverse' AND NOT e.is_default \
                AND e.condition_expr IS NOT NULL",
            vec![],
        )
        .await
        .expect("查条件边失败");
    assert_eq!(rows.len(), 1, "voucher_reverse 应只有一条条件边");
    let row = &rows[0];
    assert_eq!(row.get_typed::<String>("field").unwrap(), "form.amount");
    assert_eq!(row.get_typed::<String>("cmp").unwrap(), "gt");
    assert_eq!(row.get_typed::<String>("value").unwrap(), "10000");
}

/// ⑰ **跑在最后**（名字按字母序排在本模块末尾）：把夹具清干净再退场。
///
/// 全量套件是同一个库串行跑，本模块留下的 P3B 科目 / 供应商会让
/// `cash_flow::tests::r3_real_config_covers_all_accounts`（按科目全集校验分类配置）
/// 之类的用例误红；留下的 PENDING 实例会顶住兄弟模块的 `uk_biz_flow_instance_biz_active`。
/// 用例改名时务必保持 `pflow3b_zz_` 前缀在最后。
#[tokio::test]
async fn pflow3b_zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else {
        return;
    };
    purge_fixtures().await;

    for (label, sql) in [
        (
            "科目",
            "SELECT count(*) AS n FROM biz_account WHERE account_code LIKE 'P3B.%'",
        ),
        (
            "供应商",
            "SELECT count(*) AS n FROM biz_supplier WHERE supplier_code = 'P3B-SUP'",
        ),
        (
            "用户",
            "SELECT count(*) AS n FROM sys_user WHERE username LIKE 'p3b_%'",
        ),
        (
            "本卡实例",
            "SELECT count(*) AS n FROM biz_flow_instance \
              WHERE biz_type IN ('PAYABLE_CANCEL','VOUCHER_REVERSE')",
        ),
        (
            "本卡待办",
            "SELECT count(*) AS n FROM biz_flow_task t JOIN biz_flow_instance i \
                ON i.id = t.instance_id \
              WHERE i.biz_type IN ('PAYABLE_CANCEL','VOUCHER_REVERSE')",
        ),
    ] {
        let rows = db()
            .find_with_params(sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("查{label}残留失败：{err}"));
        let n: i64 = rows[0].get_typed("n").unwrap();
        assert_eq!(n, 0, "teardown 后仍有{label}残留：{n} 行");
    }
}
